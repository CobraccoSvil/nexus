//! Endpoint internal `/api/internal/routing/*`.
//!
//! Questo modulo espone le decisioni di routing del Rust orchestrator al
//! brain Python e ad altri client interni (es. worker schedulati). Lo scopo
//! e' eliminare la duplicazione della routing matrix tra `orchestrator.rs`
//! e `brain/router/service.py::_ROUTING_MATRIX`.
//!
//! Il brain non deve piu' decidere in proprio quale provider/model usare:
//! deve consultare questo endpoint e rispettare la decisione. Mantiene la
//! sua logica di embedding-based intent classification come **complemento**
//! locale (informazione che non viaggia in rete), ma la decisione finale
//! di routing e' autoritativa lato Rust.
//!
//! Auth: lo stesso pattern di `/internal/settings/:key` (no JWT layer).
//! Convenzione del repo: route `/internal/...` sono raggiungibili solo
//! tramite localhost o reti trusted del cluster.

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use sqlx::PgPool;

use crate::AppState;

// ── Il bridge `POST /api/internal/provider-error` non esiste piu' ──────────
//
// Era il TERZO scrittore di esclusioni, e classificava con la stessa forma di
// difetto degli altri due: `derive_error_class` accettava una `error_class`
// dichiarata dal chiamante oppure — quando mancava — la deduceva dalla PROSA
// (`provider_error_classifier::classify_with_status` su `error_text`), e da li'
// poteva mettere un fornitore in cooldown LUNGO, sei ore, per tutto il sistema.
// La portata era sempre il fornitore intero: il payload non aveva un modello.
//
// Il suo unico client era `brain/cooldown_bridge.py`, e il brain Python non
// esiste piu' (porting zero-Python, mig 0462/0532): nel repo non resta una sola
// chiamata a quella rotta. Chi conosce classe e portata di un rifiuto e' il
// gateway, e le dichiara sul wire a ogni chiamata
// (`EsclusioneDichiarata` -> `registra_esclusione_dichiarata`).

#[derive(Debug, Deserialize)]
pub struct PurposeQuery {
    pub purpose: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PurposeResolveResponse {
    pub purpose: String,
    pub provider: String,
    pub model: String,
    pub rationale: String,
    /// True quando il purpose NON e' risolvibile su un provider disponibile:
    /// il (provider, model) configurato e' in cooldown billing/quota e non
    /// esiste alternativa capable (tier-based) fuori cooldown. In questo caso
    /// l'handler ritorna HTTP 503 e il chiamante (brain) DEVE saltare il
    /// fallback su quel purpose invece di ritentare un provider morto
    /// (ADR 0020). Niente blocco silenzioso: errore esplicito.
    #[serde(default)]
    pub no_capable_provider: bool,
}

/// Esito della risoluzione di un purpose model. PUNTO UNICO (regola L): sia
/// l'handler HTTP `resolve_purpose` sia i worker in-process (es.
/// `wiki::code_docs_enricher`) devono passare da qui invece di re-implementare
/// la logica tier→statico→cooldown. Estratto da `resolve_purpose` per evitare
/// duplicazione tra il codepath HTTP e quelli in-process.
#[derive(Debug, Clone)]
pub enum PurposeResolution {
    /// (provider, model) risolto e utilizzabile. `rationale` descrive la fonte
    /// della decisione (tier dinamico vs statico).
    Resolved {
        provider: String,
        model: String,
        rationale: String,
    },
    /// Il purpose ha un tier configurato ma il catalog non offre alcun modello
    /// disponibile per quel tier (capability mancante o tutti i provider in
    /// cooldown). Tier-only: niente fallback su un modello statico (regola H).
    NoCapableModel { tier: String },
    /// Purpose non presente in `nexus_purpose_model` o privo di tier (tier-only:
    /// un purpose senza tier non e' risolvibile).
    NotFound,
    /// La routing matrix non e' disponibile (DB down): nessun fallback hardcoded.
    MatrixUnavailable(String),
}

/// Candidato provider/model per un purpose multi-provider. Deriva dalla stessa
/// tier-rule di `nexus_purpose_model` usata da [`resolve_purpose_model_db`], ma
/// mantiene provider distinti per fan-out analitici indipendenti.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurposeProviderCandidate {
    pub provider: String,
    pub model: String,
    pub tier: Option<String>,
}

impl PurposeProviderCandidate {
    /// Identita' del GIUDICE. Due candidati con la stessa coppia (provider,
    /// model) sono lo STESSO giudice: due voti da li' non sono un quorum, sono
    /// un giudizio unico contato due volte. Definizione UNICA (regola L): la
    /// usano sia la selezione dei candidati sia i panel che li convocano, cosi'
    /// che "diversi" voglia dire la stessa cosa ai due capi.
    pub fn judge_key(&self) -> (String, String) {
        (self.provider.to_lowercase(), self.model.to_lowercase())
    }
}

/// Cosa rende due candidati DIVERSI, per chi li chiede. Sono due domande
/// distinte e vanno dette, non dedotte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateDiversity {
    /// Un candidato per PROVIDER. E' cio' che serve a failover e cascate: un
    /// secondo modello dello stesso provider cade insieme al primo, quindi come
    /// alternativa non vale nulla.
    PerProvider,
    /// Coppie (provider, model) DISTINTE, con i provider nuovi PRIMA di un
    /// secondo modello di un provider gia' preso. E' cio' che serve a un panel
    /// di GIUDICI: due modelli diversi dello stesso provider sono due pareri —
    /// meno indipendenti di due provider, molto piu' di niente — mentre due
    /// istanze dello stesso modello non sono mai due giudici.
    PerProviderAndModel,
}

/// Filtra a GIUDICI distinti (chiave [`PurposeProviderCandidate::judge_key`]),
/// al piu' `limit`, preservando l'ordine di preferenza in ingresso.
///
/// PUNTO UNICO (regola L) della nozione "questi sono N pareri, non uno ripetuto
/// N volte": la usa la selezione dei candidati per comporre il pool, e la usa
/// chi convoca un panel per non fidarsi ciecamente di quanto lunga sia la lista
/// che ha ricevuto (regola O: la garanzia sta dove viene dichiarata).
pub fn giudici_distinti(
    candidati: &[PurposeProviderCandidate],
    limit: usize,
) -> Vec<PurposeProviderCandidate> {
    let mut out: Vec<PurposeProviderCandidate> = Vec::new();
    for c in candidati {
        if out.len() >= limit {
            break;
        }
        if out.iter().any(|g| g.judge_key() == c.judge_key()) {
            continue;
        }
        out.push(c.clone());
    }
    out
}

/// Errore TIPIZZATO della risoluzione (regola M). Vive in `nexus-types` cosi'
/// che anche i crate a valle dei port (es. `nexus-wiki`) decidano sulla
/// variante — mai sul testo. Re-export per i call site interni.
pub use nexus_types::purpose::PurposeUnresolved;

impl PurposeResolution {
    /// Riduce l'esito a `(provider, model)` oppure all'errore tipizzato
    /// [`PurposeUnresolved`]. E' il punto unico del match a 4 rami (regola L):
    /// i call site che devono solo mostrare il messaggio usano [`into_model`],
    /// quelli che devono decidere sulla classe dell'esito usano questo.
    pub fn try_model(self, purpose: &str) -> Result<(String, String), PurposeUnresolved> {
        match self {
            PurposeResolution::Resolved {
                provider, model, ..
            } => Ok((provider, model)),
            PurposeResolution::NoCapableModel { tier } => Err(PurposeUnresolved::NoCapableModel {
                purpose: purpose.to_string(),
                tier,
            }),
            PurposeResolution::NotFound => Err(PurposeUnresolved::NotFound {
                purpose: purpose.to_string(),
            }),
            PurposeResolution::MatrixUnavailable(e) => Err(PurposeUnresolved::MatrixUnavailable {
                purpose: purpose.to_string(),
                message: e,
            }),
        }
    }

    /// Come [`try_model`] ma appiattito a messaggio leggibile, per i call site
    /// che mappano l'errore nel proprio tipo solo per display/log (regola M:
    /// il testo umano non decide niente).
    pub fn into_model(self, purpose: &str) -> Result<(String, String), String> {
        self.try_model(purpose).map_err(|e| e.to_string())
    }
}

/// Nome canonico (regola N) del purpose dei run di rimedio automatico
/// (service_observer_remediation, resource_violation_remediation). Mig 0626.
pub const PURPOSE_AUTO_REMEDIATION: &str = "auto_remediation";

/// Riduce un esito di risoluzione a una coppia di override `(provider, model)`
/// per un run agentico. PUNTO UNICO (regola L) del match usato dai siti di
/// remediation; regola M: la decisione e' sulla VARIANTE tipizzata (`try_model`
/// -> `PurposeUnresolved`), il testo serve solo al log. Su qualunque fallimento
/// -> `(None, None)`: il chiamante procede col routing di default (regola G:
/// niente fallback hardcoded; il rimedio non si blocca). INVARIANTE: mai lo
/// stato misto `(Some, None)` — lo slot-routing di spawn_agent_run e' attivo
/// solo con ENTRAMBI None, e un ibrido lo disabiliterebbe lasciando l'altro
/// campo al routing.
pub(crate) fn reduce_purpose_to_override(
    res: PurposeResolution,
    purpose: &str,
) -> (Option<String>, Option<String>) {
    match res.try_model(purpose) {
        Ok((provider, model)) => (Some(provider), Some(model)),
        Err(unresolved) => {
            tracing::warn!(
                purpose = %purpose,
                motivo = %unresolved,
                "purpose non risolvibile: il rimedio procede col routing di default"
            );
            (None, None)
        }
    }
}

/// Override `(provider, model)` per un run agentico dal purpose tier-aware,
/// degradando al routing di default se non risolvibile. Entry di produzione
/// dei siti di remediation che dispongono di `AppState`.
pub async fn purpose_override_or_default(
    state: &AppState,
    purpose: &str,
) -> (Option<String>, Option<String>) {
    reduce_purpose_to_override(resolve_purpose_model(state, purpose).await, purpose)
}

/// CORE decisionale (PUNTO UNICO, regola L) della risoluzione purpose→modello.
/// TIER-ONLY: il modello e' scelto ESCLUSIVAMENTE dal routing per tier
/// (`best_model_for_tier`: miglior modello del catalog per tier+capability,
/// provider in cooldown esclusi). NIENTE fallback su un (provider, model_id)
/// statico (regola H: fail-loud). Nessun nome modello hardcoded (regola G).
///
/// Riceve gia' risolta `tier_rule` cosi' da essere indipendente dalla FONTE
/// (matrix cache o query DB diretta): i due adapter pubblici la leggono dalla
/// rispettiva fonte e delegano qui. La logica decisionale vive in un solo posto.
///
/// - tier_rule assente  -> NotFound (purpose senza tier: non risolvibile)
/// - tier senza modello -> NoCapableModel (catalog/cooldown)
/// Il PAVIMENTO di un purpose: la soglia sotto cui la degradazione smette di
/// essere un ripiego e diventa un danno.
///
/// Viene da `agent.routing.agentic_min_tier` (DB, regola G) ma NON puo' mai
/// ALZARE il tier richiesto: un purpose configurato `light` lo e' per scelta —
/// i task banali (titoli, doc) usano un modello debole per costo — e promuoverlo
/// sarebbe una decisione presa alle sue spalle, per giunta piu' cara. Il
/// pavimento limita la DEGRADAZIONE, non la richiesta.
///
/// Serve percio' il MENO capace fra i due: `light` chiesto con pavimento
/// `medium` resta `light` (non degrada, ci e' gia'); `heavy` chiesto con
/// pavimento `medium` puo' scendere fino a `medium`, mai a `light`.
async fn purpose_floor(db: &PgPool, tier_richiesto: &str) -> String {
    use nexus_agent_graph::decisions::tiers::tier_rank;
    let globale = crate::orchestrator::model_routing::agentic_min_tier(db).await;
    if tier_rank(tier_richiesto) < tier_rank(&globale) {
        tier_richiesto.to_string()
    } else {
        globale
    }
}

async fn resolve_purpose_core(
    db: &PgPool,
    purpose: &str,
    tier_rule: Option<crate::routing_matrix::PurposeTierRule>,
    exclude_providers: &[String],
    only_provider: Option<&str>,
) -> PurposeResolution {
    let Some(rule) = tier_rule else {
        tracing::warn!(purpose = %purpose, "resolve_purpose: purpose privo di tier (tier-only)");
        return PurposeResolution::NotFound;
    };
    // SERVIZIO UNICO (regola L): la degradazione e' un parametro DICHIARATO, non
    // un effetto di quale strato si chiama. Col pin resta `Exact{PinnedProvider}`
    // — il pin cede il provider, mai la qualita': se il provider pinnato non ha
    // il tier, l'esito e' vuoto e il chiamante ritenta SENZA pin (I5).
    use crate::orchestrator::model_service::{
        select_model, ExactReason, ModelRequest, NoModelReason, Profile, Rank, TierPolicy,
    };
    let floor = purpose_floor(db, &rule.tier).await;
    let mut req = ModelRequest {
        tier: &rule.tier,
        // Preferenza + pavimento, non "il tier o niente": scendere di un gradino
        // resta ammesso (il 2026-07-15 il consiglio moriva con 19 modelli sani
        // un gradino sotto), ma mai sotto il pavimento.
        tier_policy: TierPolicy::Flexible,
        profile: if rule.requires_tool_use {
            Profile::Agentic
        } else {
            Profile::NonAgentic
        },
        capability: rule.capability.as_deref(),
        min_context_window: 0,
        min_tier: Some(&floor),
        exclude_providers,
        pin: None,
        rank: if rule.requires_tool_use {
            Rank::CostFirst
        } else {
            Rank::NonAgenticSafe
        },
        // Il purpose e' una risoluzione interna, non la selezione dinamica del
        // turno primario: niente riordino telemetria (vedi ModelRequest::governed).
        governed: false,
        // La risoluzione singola non dichiara un budget: i chiamanti one-shot
        // che ne hanno uno passano dal fan-out (`..._candidates_db_by`).
        latency_budget_ms: None,
    };
    if let Some(p) = only_provider {
        req.pin = Some(p);
        req.tier_policy = TierPolicy::Exact {
            why: ExactReason::PinnedProvider,
        };
    }
    match select_model(db, &req).await {
        // Il `rationale` (`tier=medium:auto` | `tier=medium:upgraded_to=high`) e
        // il log dello scostamento li produce il SERVIZIO: qui non si ri-deriva
        // nulla (regola M/I4).
        Ok(choice) => PurposeResolution::Resolved {
            provider: choice.provider,
            model: choice.model,
            rationale: choice.rationale,
        },
        Err(reason) => {
            // I6: il motivo e' TIPIZZATO. `ExactTierEmpty` col pin e' l'esito
            // ATTESO (il chiamante ritenta senza pin), non un guasto: non va
            // loggato come allarme.
            if !reason.is_expected() {
                tracing::warn!(
                    purpose = %purpose, tier = %rule.tier, motivo = ?reason,
                    "resolve_purpose: nessun modello per il tier"
                );
            }
            match reason {
                NoModelReason::CatalogUnavailable(e) => PurposeResolution::MatrixUnavailable(e),
                NoModelReason::InvalidRequest(e) => PurposeResolution::MatrixUnavailable(e),
                _ => PurposeResolution::NoCapableModel { tier: rule.tier },
            }
        }
    }
}

/// Adapter da `AppState`: legge la tier-rule dalla RoutingMatrix cache (TTL 60s)
/// e delega al core. Usare quando si dispone di `AppState`.
pub async fn resolve_purpose_model(state: &AppState, purpose: &str) -> PurposeResolution {
    let purpose = purpose.trim();
    let matrix = match state.orchestrator.routing_matrix.current() {
        Ok(m) => m,
        Err(e) => return PurposeResolution::MatrixUnavailable(e.to_string()),
    };
    resolve_purpose_core(&state.db, purpose, matrix.purpose_tier(purpose), &[], None).await
}

/// Adapter da `&PgPool`: legge la tier-rule direttamente da `nexus_purpose_model`
/// e delega al core. Per i call site che NON dispongono di `AppState` (es. i tool
/// del Nexus Builtin server come `nexus_doc_generate`, che ricevono solo
/// `&PgPool`). Stessa decisione di `resolve_purpose_model`, senza re-implementarla
/// (regola L): la fonte e' il DB invece della matrix cache.
pub async fn resolve_purpose_model_db(db: &PgPool, purpose: &str) -> PurposeResolution {
    resolve_purpose_model_db_excluding(db, purpose, &[]).await
}

/// Variante di [`resolve_purpose_model_db`] che ESCLUDE un insieme di provider
/// dalla selezione per tier (Fase C2: vincolo giudice != worker). PUNTO UNICO
/// (regola L): `resolve_purpose_model_db` vi delega con `exclude_providers = &[]`,
/// cosi' i suoi ~25 chiamanti storici restano invariati. Un sub-run di review
/// passa qui il provider del run padre da escludere.
pub async fn resolve_purpose_model_db_excluding(
    db: &PgPool,
    purpose: &str,
    exclude_providers: &[String],
) -> PurposeResolution {
    resolve_purpose_model_db_inner(db, purpose, exclude_providers, None).await
}

/// Variante di [`resolve_purpose_model_db`] che RESTRINGE la selezione per tier a
/// un SOLO provider (`only_provider`): PIN provider tier-aware. PUNTO UNICO
/// (regola L): gemella di `_excluding`, entrambe delegano a
/// [`resolve_purpose_model_db_inner`] (unica query DB della tier-rule). Usata per
/// la propagazione del PIN del provider ai sub-agenti worker: se il provider
/// pinnato non offre un modello capable del tier (o e' in cooldown) l'esito e'
/// `NoCapableModel`/`NotFound` e il chiamante decide il fallback SENZA pin
/// (preferenza forte, non hard filter). NON propaga alcun `preferred_model` (regola
/// L: il pin e' SOLO il provider; il modello si deriva sempre dal tier via catalog,
/// regola G).
pub async fn resolve_purpose_model_db_pinned(
    db: &PgPool,
    purpose: &str,
    only_provider: Option<&str>,
) -> PurposeResolution {
    resolve_purpose_model_db_inner(db, purpose, &[], only_provider).await
}

/// Risolve fino a `limit` provider DISTINTI per un purpose tier-based, usando il
/// catalog e gli stessi filtri/cooldown del routing live. E' il punto unico per
/// fan-out multi-provider: i chiamanti non interrogano `ai_price_catalog` a mano e
/// non hardcodano provider/modelli.
///
/// `limit` e' il TETTO (quanti se ne vorrebbero), `min_providers` la SOGLIA sotto
/// la quale il fan-out non ha senso. Sono due cose diverse e vanno dette
/// entrambe alla selezione: passare solo il tetto significava chiedere "fino a N
/// candidati" e scoprire soltanto a valle che venivano tutti dallo stesso
/// provider, con la tier-chain ormai abbandonata. Il 20/07 questo produceva
/// "provider distinti insufficienti (got=1 min=2)" con SEI provider abilitati e
/// sani, e i tier successivi -- gia' autorizzati dalla catena -- mai interrogati.
pub async fn resolve_purpose_provider_candidates_db(
    db: &PgPool,
    purpose: &str,
    limit: usize,
    min_providers: usize,
) -> Result<Vec<PurposeProviderCandidate>, PurposeResolution> {
    resolve_purpose_provider_candidates_db_by(
        db,
        purpose,
        limit,
        min_providers,
        CandidateDiversity::PerProvider,
        &[],
        None,
    )
    .await
}

/// Come [`resolve_purpose_provider_candidates_db`] ma col criterio di diversita'
/// ESPLICITO. Gemella dichiarata invece che semantica implicita: chi convoca un
/// panel di giudici e chi cerca un'alternativa di failover vogliono due cose
/// diverse, e prima questa differenza non era esprimibile.
///
/// Il 2026-07-26 il panel di review si e' ridotto a UN revisore — poi convocato
/// sei volte di fila, sempre `openrouter/z-ai/glm-4.7-flash` — perche' openai e
/// anthropic erano in cooldown billing e la dedup PER PROVIDER buttava via i
/// dieci modelli qualificati che openrouter offriva nello stesso tier. Un giudice
/// solo ripetuto sei volte non e' meno indipendente di due giudici: e' l'assenza
/// del quorum, con l'apparenza di averlo.
///
/// `exclude_providers` sono i fornitori che il chiamante NON puo' accettare (il
/// veto «giudice != worker»), e vanno detti QUI perche' sono ELEGGIBILITA', non
/// un filtro da applicare al risultato. La tier-chain si ferma al primo anello
/// che soddisfa `min_providers`: se quell'anello contiene solo il fornitore
/// vietato, la catena si e' fermata su un pool che il chiamante buttera' via, e
/// i tier successivi — gia' autorizzati dalla catena — non vengono mai
/// interrogati. MISURATO il 09/08/2026 sul gate duale: purpose `step_validator`
/// (tier medium, capability `reasoning`), tre soli fornitori nel tier medium
/// (anthropic, mistral, openai) di cui due in cooldown billing, esecutore
/// mistral. La selezione tornava l'unico rimasto, mistral, il chiamante lo
/// filtrava e dichiarava `unavailable_declared` — mentre deepseek, google e
/// openrouter stavano un gradino sopra, sani e mai guardati.
///
/// `latency_budget_ms` e' il budget di latenza che il chiamante DICHIARA
/// (`ModelRequest::latency_budget_ms`, mig 0725): il timeout entro cui la
/// risposta di UNA convocazione deve arrivare. Lo dichiarano i soli chiamanti
/// one-shot che quel tetto lo conoscono (il gate duale col suo timeout per
/// validatore); un budget di RUN non e' un budget di chiamata, e chi non ce
/// l'ha passa `None` (percorso bit-identico).
pub async fn resolve_purpose_provider_candidates_db_by(
    db: &PgPool,
    purpose: &str,
    limit: usize,
    min_providers: usize,
    diversity: CandidateDiversity,
    exclude_providers: &[String],
    latency_budget_ms: Option<i64>,
) -> Result<Vec<PurposeProviderCandidate>, PurposeResolution> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let purpose = purpose.trim();
    let Some(rule) = fetch_purpose_tier_rule_db(db, purpose).await? else {
        return Err(PurposeResolution::NotFound);
    };

    // SERVIZIO UNICO (regola L): niente EligibilityFilter costruito a mano, niente
    // gate ricordato dal chiamante (I2), niente tier-chain scelta qui.
    //
    // Il fan-out chiede N provider DISTINTI proprio nel tier che ne ha di meno
    // (il 'heavy' vive su pochi provider), quindi qui la tentazione di scendere
    // e' massima: piu' scendi, piu' provider trovi. Scendere di un gradino e'
    // giusto — il 2026-07-15 il panel non si convocava affatto mentre 19 modelli
    // sani stavano appena sotto — ma sotto il PAVIMENTO il compromesso cambia
    // natura: non e' "meno pareri", e' "pareri inaffidabili". Il 2026-07-16 la
    // catena arrivava a groq/gpt-oss-20b (agentic_index 3.1), che ha risposto
    // fuori tema dichiarandosi `completed`. Un consigliere che mente non e' un
    // consigliere in meno: e' un danno in piu'.
    use crate::orchestrator::model_service::{
        select_models, ModelRequest, NoModelReason, Profile, Rank, TierPolicy,
    };
    let floor = purpose_floor(db, &rule.tier).await;
    let req = ModelRequest {
        tier: &rule.tier,
        tier_policy: TierPolicy::Flexible,
        profile: if rule.requires_tool_use {
            Profile::Agentic
        } else {
            Profile::NonAgentic
        },
        capability: rule.capability.as_deref(),
        min_context_window: 0,
        min_tier: Some(&floor),
        // Il veto del chiamante entra nell'ELEGGIBILITA' (vedi doc): cosi' la
        // condizione di uscita dalla tier-chain conta i fornitori che il
        // chiamante puo' davvero usare, non quelli che scartera'.
        exclude_providers,
        pin: None,
        rank: if rule.requires_tool_use {
            Rank::CostFirst
        } else {
            Rank::NonAgenticSafe
        },
        // Il purpose e' una risoluzione interna, non la selezione dinamica del
        // turno primario: niente riordino telemetria (vedi ModelRequest::governed).
        governed: false,
        latency_budget_ms,
    };
    // Pool piu' ampio del limite per deduplicare per provider senza una query
    // dedicata fuori dal servizio.
    let pool_limit = (limit.saturating_mul(4)).max(limit) as i64;
    let rows = match select_models(db, &req, pool_limit, min_providers).await {
        Ok(v) => v,
        // Pool vuoto: per il fan-out non e' un errore da propagare (il chiamante
        // convoca chi c'e'), salvo il catalog irraggiungibile.
        Err(NoModelReason::CatalogUnavailable(e)) => {
            return Err(PurposeResolution::MatrixUnavailable(e))
        }
        Err(NoModelReason::InvalidRequest(e)) => {
            return Err(PurposeResolution::MatrixUnavailable(e))
        }
        Err(_) => Vec::new(),
    };

    let tutti: Vec<PurposeProviderCandidate> = rows
        .into_iter()
        .map(|choice| PurposeProviderCandidate {
            provider: choice.provider,
            model: choice.model,
            tier: choice.effective_tier,
        })
        .collect();

    // Primo giro, comune ai due criteri: un modello per provider. La diversita'
    // di provider e' la piu' preziosa e va spesa per prima.
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<PurposeProviderCandidate> = Vec::new();
    for c in &tutti {
        if out.len() >= limit {
            break;
        }
        let key = c.provider.to_lowercase();
        if seen.iter().any(|p| p == &key) {
            continue;
        }
        seen.push(key);
        out.push(c.clone());
    }

    if diversity == CandidateDiversity::PerProviderAndModel {
        // Restano slot: si accettano altri modelli dei provider gia' presi,
        // mai lo stesso giudice due volte. `giudici_distinti` e' il punto unico
        // che decide cosa "gia' preso" significhi.
        let mut preferenza = out;
        preferenza.extend(tutti.iter().cloned());
        out = giudici_distinti(&preferenza, limit);
    }
    Ok(out)
}

/// Esito di UN tentativo dentro [`complete_for_purpose_with_failover`].
pub enum AttemptOutcome<T> {
    /// Successo: `T` prodotto, il loop si ferma.
    Done(T),
    /// Fallimento RITENTABILE: prova il prossimo candidato del tier.
    Failover,
}

/// Errore del failover per-purpose (regola M: esiti tipizzati, non stringhe).
#[derive(Debug)]
pub enum PurposeFailoverError {
    /// Nessun candidato risolvibile per il tier (config assente / tutti in cooldown).
    NoCandidate(PurposeResolution),
    /// Tutti i candidati provati hanno restituito `Failover`.
    AllCandidatesFailed,
}

/// Numero di candidati del tier da provare con failover: override per-purpose
/// `routing.<purpose>_failover_candidates` -> generico
/// `routing.purpose_failover_candidates` -> default 3 (regola G: niente numero
/// hardcoded; `.max(1)` di guardia).
async fn purpose_failover_candidate_limit(db: &PgPool, purpose: &str) -> usize {
    let per_purpose = format!("routing.{purpose}_failover_candidates");
    for key in [per_purpose.as_str(), "routing.purpose_failover_candidates"] {
        if let Some(n) = nexus_auth::get_setting(db, key)
            .await
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            return n.max(1);
        }
    }
    3
}

/// PUNTO UNICO (regola L) del FAILOVER tier-aware per i purpose interni che fanno
/// resolve+complete inline: risolve N candidati DISTINTI del tier
/// ([`resolve_purpose_provider_candidates_db`]: health/cooldown-aware, NIENTE
/// provider hardcoded, stesso routing del resto del sistema) e prova
/// `attempt(provider, model)` in ORDINE, facendo failover al prossimo su
/// [`AttemptOutcome::Failover`]. Si arrende con [`PurposeFailoverError::
/// AllCandidatesFailed`] solo se TUTTI falliscono; il chiamante applica il suo
/// degrado (503/skip/euristica). Generalizza il loop del classificatore
/// (`intent_classifier::classify`). Il chiamante decide COSA e' un fallimento nel
/// closure (tipicamente `orchestrator::neural_value_is_failure` sul value neural).
pub async fn complete_for_purpose_with_failover<T, F, Fut>(
    db: &PgPool,
    purpose: &str,
    mut attempt: F,
) -> Result<T, PurposeFailoverError>
where
    F: FnMut(String, String) -> Fut,
    Fut: std::future::Future<Output = AttemptOutcome<T>>,
{
    let limit = purpose_failover_candidate_limit(db, purpose).await;
    // Failover in cascata: si prova un candidato alla volta finche' uno risponde,
    // quindi nessuna richiesta di diversita' (basta il primo che funziona).
    let candidates = match resolve_purpose_provider_candidates_db(db, purpose, limit, 1).await {
        Ok(c) if !c.is_empty() => c,
        Ok(_) => return Err(PurposeFailoverError::NoCandidate(PurposeResolution::NotFound)),
        Err(res) => return Err(PurposeFailoverError::NoCandidate(res)),
    };
    for cand in candidates {
        match attempt(cand.provider.clone(), cand.model.clone()).await {
            AttemptOutcome::Done(v) => return Ok(v),
            AttemptOutcome::Failover => {
                tracing::warn!(
                    purpose,
                    provider = %cand.provider,
                    model = %cand.model,
                    "purpose failover: candidato fallito, failover al prossimo del tier"
                );
            }
        }
    }
    Err(PurposeFailoverError::AllCandidatesFailed)
}

/// PUNTO UNICO (regola L) della lettura tier-rule da `nexus_purpose_model` + delega
/// al core decisionale. `_excluding` e `_pinned` sono viste su questa funzione con
/// combinazioni diverse di `exclude_providers`/`only_provider`: la query DB della
/// tier-rule vive in un solo posto.
async fn resolve_purpose_model_db_inner(
    db: &PgPool,
    purpose: &str,
    exclude_providers: &[String],
    only_provider: Option<&str>,
) -> PurposeResolution {
    let purpose = purpose.trim();
    let tier_rule = match fetch_purpose_tier_rule_db(db, purpose).await {
        Ok(rule) => rule,
        Err(e) => return e,
    };

    resolve_purpose_core(db, purpose, tier_rule, exclude_providers, only_provider).await
}

async fn fetch_purpose_tier_rule_db(
    db: &PgPool,
    purpose: &str,
) -> Result<Option<crate::routing_matrix::PurposeTierRule>, PurposeResolution> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>, bool)>(
        "SELECT tier, required_capability, requires_tool_use \
         FROM nexus_purpose_model WHERE purpose = $1 LIMIT 1",
    )
    .bind(purpose)
    .fetch_optional(db)
    .await
    .map_err(|e| PurposeResolution::MatrixUnavailable(e.to_string()))?;

    let Some((tier, capability, requires_tool_use)) = row else {
        return Ok(None);
    };
    Ok(tier.map(|t| crate::routing_matrix::PurposeTierRule {
        tier: t,
        capability,
        requires_tool_use,
    }))
}

/// Handler `GET /api/internal/routing/purpose?purpose=...`
/// Risolve (provider, model) da `nexus_purpose_model` tramite RoutingMatrixCache.
///
/// Cooldown enforcement (ADR 0020): il purpose e' una decisione AUTO (nessuna
/// forzatura utente), quindi il cooldown billing/quota e' VINCOLANTE. Se il
/// (provider, model) configurato e' in cooldown:
///   - se il purpose ha una regola tier (mig 0203), `best_model_for_tier` ha
///     gia' scelto un provider capable fuori cooldown (filtra il cooldown);
///   - altrimenti l'handler ritorna 503 con `no_capable_provider=true` invece
///     di restituire un provider morto che il brain ritenterebbe a vuoto.
pub async fn resolve_purpose(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PurposeQuery>,
) -> impl IntoResponse {
    let purpose = q.purpose.trim();
    if purpose.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing `purpose`".to_string()).into_response();
    }

    // Delega al PUNTO UNICO (regola L): la logica tier→statico→cooldown vive in
    // `resolve_purpose_model`. Qui resta solo la mappatura esito → HTTP status.
    match resolve_purpose_model(&state, purpose).await {
        PurposeResolution::Resolved {
            provider,
            model,
            rationale,
        } => (
            StatusCode::OK,
            Json(PurposeResolveResponse {
                purpose: purpose.to_string(),
                provider,
                model,
                rationale,
                no_capable_provider: false,
            }),
        )
            .into_response(),
        PurposeResolution::NoCapableModel { tier } => {
            // Tier-only: nessun modello disponibile per il tier (capability
            // mancante o tutti in cooldown). Niente fallback (regola H):
            // segnaliamo no_capable cosi' il brain salta invece di ritentare.
            tracing::warn!(
                purpose = %purpose, tier = %tier,
                "resolve_purpose: nessun modello capable per il tier -> no_capable (503)"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(PurposeResolveResponse {
                    purpose: purpose.to_string(),
                    provider: String::new(),
                    model: String::new(),
                    rationale: format!("tier={tier}:no_capable"),
                    no_capable_provider: true,
                }),
            )
                .into_response()
        }
        PurposeResolution::NotFound => (
            StatusCode::NOT_FOUND,
            format!("purpose model non trovato o privo di tier: {purpose}"),
        )
            .into_response(),
        PurposeResolution::MatrixUnavailable(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("routing_matrix non disponibile: {e}"),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /api/internal/routing/cooldown — fonte di verita' unica del cooldown
// ---------------------------------------------------------------------------

/// Risposta di `GET /api/internal/routing/cooldown`.
/// Espone lo stato di cooldown autoritativo del gate Rust (in-memory +
/// propagazione DB), cosi' il brain Python NON deve mantenere una sua logica
/// reattiva duplicata (ADR 0020, regola H): consulta questo endpoint come
/// unica fonte di verita' a runtime.
#[derive(Debug, serde::Serialize)]
pub struct CooldownEntry {
    /// SEMPRE un nome di fornitore. Fino al 13/08/2026 questo campo portava la
    /// CHIAVE del cooldown, che per una coppia era `provider\u{1}model`:
    /// misurato sul sistema vivo, `{"provider":"groq\u{1}openai/gpt-oss-20b"}` —
    /// una stringa che nessun `provider` del catalogo eguaglia.
    pub provider: String,
    /// Valorizzato quando l'esclusione riguarda UN SOLO modello di quel
    /// fornitore. `null` = tutto il fornitore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// La portata, esplicita: `provider` o `model`. Ridondante con `model` e
    /// deliberatamente: e' il campo su cui un consumatore DECIDE, e non deve
    /// dedurlo dalla presenza di un altro.
    pub scope: crate::provider_cooldown::PortataCooldown,
    pub seconds_remaining: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CooldownSnapshotResponse {
    /// Tutte le esclusioni attive (billing/quota/rate-limit), fornitori interi e
    /// coppie col modello. Chi deve saltare un FORNITORE filtra su
    /// `scope == "provider"`; chi instrada una COPPIA deve guardare entrambe.
    pub providers: Vec<CooldownEntry>,
}

/// Handler `GET /api/internal/routing/cooldown`.
/// Ritorna l'elenco dei provider in cooldown secondo il gate Rust. Il brain
/// usa questa lista per saltare i provider morti in fallback/escalation senza
/// duplicare il ragionamento sul cooldown (deprecazione di
/// `registry._is_in_billing_cooldown` come fonte primaria).
pub async fn cooldown_snapshot_handler(State(_state): State<AppState>) -> impl IntoResponse {
    let providers: Vec<CooldownEntry> = crate::provider_cooldown::cooldown_snapshot_entries()
        .into_iter()
        .map(|e| CooldownEntry {
            provider: e.chiave.provider().to_string(),
            model: e.chiave.model().map(|m| m.to_string()),
            scope: e.chiave.portata(),
            seconds_remaining: e.remaining_seconds,
            reason: e.reason,
        })
        .collect();
    (StatusCode::OK, Json(CooldownSnapshotResponse { providers })).into_response()
}

/// Body della richiesta `POST /api/internal/routing/decide`.
#[derive(Debug, Deserialize)]
pub struct RoutingDecideRequest {
    /// Testo del messaggio utente (o prompt) sul quale fare la decisione.
    pub message: String,
    /// Override esplicito del provider (utente ha forzato la scelta dal dropdown).
    /// Se presente, vince sempre sulle altre logiche.
    #[serde(default)]
    pub provider_override: Option<String>,
    /// Override esplicito del model (analogo).
    #[serde(default)]
    pub model_override: Option<String>,
    /// Numero di messaggi pregressi nella sessione corrente.
    /// Usato per stimare la complessita' (sessioni lunghe → modelli piu' capaci).
    #[serde(default)]
    pub context_message_count: usize,
    /// `project_id` della sessione (opzionale, riservato per usi futuri:
    /// override per-progetto). Oggi accettato ma non utilizzato per la
    /// decisione, mantenuto per compatibilita' API.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Identifier del profilo agente (analogo a project_id, riservato).
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Modalita' di routing scelta dall'utente per questa sessione/chat.
    /// Sovrascrive il `nexus_behavior_mode` globale DB per questa singola richiesta.
    /// Valori accettati: "veloce" | "economica" | "bilanciata" | "approfondita" | "dinamico" | "manuale".
    /// Se assente o stringa vuota, si usa il valore DB globale come fallback.
    #[serde(default)]
    pub behavior_mode: Option<String>,
    /// Intent gia' classificato dal chiamante (es. brain router_node). Se
    /// presente, il routing salta la classificazione LLM ridondante (regola L:
    /// punto unico classificazione). Evita il timeout del client su message non
    /// cachati, che costava 0.7-0.9s per la classificazione. Se assente,
    /// mcp-core classifica come prima.
    #[serde(default)]
    pub intent: Option<String>,
    /// Il TURNO corrente contiene almeno un allegato image/*. RIPRISTINO della
    /// regressione Python->Rust (CLAUDE.md sezione I, "Smart routing vision"): il
    /// brain rileva gli allegati image/* del messaggio e lo segnala qui, cosi' il
    /// routing forza un modello con supports_vision=TRUE. Default `false`: i
    /// chiamati che non popolano il campo ottengono il routing testuale invariato.
    #[serde(default)]
    pub turn_has_image: bool,
}

/// Handler `POST /api/internal/routing/decide`.
/// Restituisce JSON con `provider, model, intent, mode, risky, rationale, ...`.
///
/// Status code:
///   - 200 OK: routing risolto, provider scelto utilizzabile
///   - 503 Service Unavailable: TUTTI i provider in cooldown — il chiamante
///     (brain Python, UI) DEVE fermarsi e avvertire l'utente. Il body include
///     comunque il `RoutingResolveResult` con `no_capable_provider=true` e la
///     lista `providers_in_cooldown` per mostrare un alert dettagliato.
pub async fn decide_routing(
    State(state): State<AppState>,
    Json(body): Json<RoutingDecideRequest>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    use axum::response::IntoResponse;
    // `message` vuoto e' un errore SOLO se manca anche l'intent: senza nessuno
    // dei due non c'e' nulla da classificare. Con un intent gia' classificato
    // dal chiamante (brain router_node) il message serve solo alla
    // risky-detection (stringa vuota = non risky), quindi e' legittimo che
    // manchi (es. re-route per-turno dopo un tool_result). Il 400
    // indiscriminato faceva morire i run agentici multi-turno con la
    // sentinella __router_unavailable__ (incidente 2026-06-10).
    let has_intent = body.intent.as_deref().is_some_and(|v| !v.trim().is_empty());
    if body.message.trim().is_empty() && !has_intent {
        return Err((
            StatusCode::BAD_REQUEST,
            "campo `message` vuoto e nessun `intent` fornito".to_string(),
        ));
    }
    let result = state
        .orchestrator
        .resolve_agent_provider_detailed(
            &state.db,
            body.project_id.as_deref().unwrap_or(""),
            body.profile_id.as_deref().unwrap_or(""),
            &body.message,
            body.provider_override.as_deref(),
            body.model_override.as_deref(),
            body.context_message_count,
            body.behavior_mode
                .as_deref()
                .filter(|v| !v.trim().is_empty()),
            body.intent.as_deref().filter(|v| !v.trim().is_empty()),
            body.turn_has_image,
        )
        .await;
    // Se nessun provider e' utilizzabile, ritorna 503 ma comunque con il body
    // popolato — il client sa cosa mostrare all'utente senza dover fare una
    // chiamata aggiuntiva per i dettagli.
    let status = if result.no_capable_provider {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    Ok((status, Json(result)).into_response())
}

/// Versione GET con query params, utile per smoke test rapidi da curl/devtools.
/// Esempio:
///   curl 'http://localhost:4000/api/internal/routing/decide?message=elimina+i+file+docker'
#[derive(Debug, Deserialize)]
pub struct RoutingDecideQuery {
    pub message: String,
    #[serde(default)]
    pub mode: Option<String>,
}

pub async fn decide_routing_get(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<RoutingDecideQuery>,
) -> impl IntoResponse {
    if q.message.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "missing `message`".to_string()).into_response();
    }
    let result = state
        .orchestrator
        .resolve_agent_provider_detailed(
            &state.db,
            "",
            "",
            &q.message,
            None,
            None,
            0,
            q.mode.as_deref().filter(|v| !v.trim().is_empty()),
            None,
            // GET di smoke test: nessun allegato -> routing testuale invariato.
            false,
        )
        .await;
    (StatusCode::OK, Json(result)).into_response()
}

/// Entry esportata dal catalogo prezzi LLM. Espone solo i campi rilevanti
/// per la decisione di routing (no costi, no metadata interni — quelli
/// vengono dall'admin UI dedicata).
#[derive(Debug, serde::Serialize)]
pub struct CatalogEntry {
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub performance_tier: String,
    pub speed_tier: String,
    pub capabilities: serde_json::Value,
    pub context_window: i32,
    pub supports_tool_use: bool,
    pub is_featured: bool,
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
}

/// Filtri opzionali per il lookup catalogo via query string.
#[derive(Debug, serde::Deserialize)]
pub struct CatalogQuery {
    /// Filtra per tier (light | medium | high | heavy | frontier, scala a 5
    /// livelli mig 0528/0547). Se None ritorna tutti.
    #[serde(default)]
    pub tier: Option<String>,
    /// Filtra per provider esatto (anthropic, openai, ...). None = tutti.
    #[serde(default)]
    pub provider: Option<String>,
    /// Capability che il modello deve supportare (es. "code", "tool_use").
    /// La query controlla `capabilities @> jsonb_build_array(...)`.
    #[serde(default)]
    pub requires_capability: Option<String>,
}

/// Handler `GET /api/internal/routing/catalog` — espone il catalogo prezzi
/// LLM al brain Python e a dashboard admin. Permette al brain di "vedere"
/// quali modelli sono disponibili senza dover consultare direttamente il
/// DB (un'altra dipendenza eliminata).
///
/// Usato anche dal dashboard admin per costruire la UI di pricing review.
pub async fn list_catalog(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<CatalogQuery>,
) -> Result<Json<Vec<CatalogEntry>>, (StatusCode, String)> {
    use sqlx::Row;
    // Costruzione query dinamica (con filter opzionali). Tutti i WHERE sono
    // bind parametrizzati (no SQL injection).
    let mut sql = String::from(
        r#"SELECT provider, model, display_name, performance_tier, speed_tier,
                  capabilities, context_window, supports_tool_use, is_featured,
                  input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens,
                  output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens
           FROM ai_price_catalog
           WHERE is_enabled = TRUE
             AND (effective_to IS NULL OR effective_to > NOW())"#,
    );
    let mut binds: Vec<String> = Vec::new();
    if let Some(t) = q.tier.as_deref().filter(|s| !s.is_empty()) {
        binds.push(t.to_lowercase());
        sql.push_str(&format!(" AND performance_tier = ${}", binds.len()));
    }
    if let Some(p) = q.provider.as_deref().filter(|s| !s.is_empty()) {
        binds.push(p.to_lowercase());
        sql.push_str(&format!(" AND provider = ${}", binds.len()));
    }
    if let Some(cap) = q.requires_capability.as_deref().filter(|s| !s.is_empty()) {
        binds.push(cap.to_string());
        sql.push_str(&format!(
            " AND capabilities @> jsonb_build_array(${}::text)",
            binds.len()
        ));
    }
    sql.push_str(
        " ORDER BY is_featured DESC, performance_tier, input_cost_per_million_tokens ASC LIMIT 100",
    );

    let mut q_builder = sqlx::query(&sql);
    for bind_val in &binds {
        q_builder = q_builder.bind(bind_val);
    }
    let rows = q_builder.fetch_all(&state.db).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("query catalog: {e}"),
        )
    })?;

    let entries: Vec<CatalogEntry> = rows
        .into_iter()
        .map(|r| CatalogEntry {
            provider: r.try_get("provider").unwrap_or_default(),
            model: r.try_get("model").unwrap_or_default(),
            display_name: r.try_get("display_name").unwrap_or_default(),
            performance_tier: r.try_get("performance_tier").unwrap_or_default(),
            speed_tier: r.try_get("speed_tier").unwrap_or_default(),
            capabilities: r.try_get("capabilities").unwrap_or(serde_json::json!([])),
            context_window: r.try_get("context_window").unwrap_or(0),
            supports_tool_use: r.try_get("supports_tool_use").unwrap_or(false),
            is_featured: r.try_get("is_featured").unwrap_or(false),
            input_cost_per_million_tokens: r
                .try_get("input_cost_per_million_tokens")
                .unwrap_or(0.0),
            output_cost_per_million_tokens: r
                .try_get("output_cost_per_million_tokens")
                .unwrap_or(0.0),
        })
        .collect();
    Ok(Json(entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I due criteri di diversita' rispondono a domande diverse sullo STESSO
    /// pool, e la differenza si vede solo quando i provider sani scarseggiano:
    /// e' la situazione del 2026-07-26 (openai e anthropic in cooldown billing).
    ///
    /// `PerProvider` consegna un candidato solo — corretto per un failover, dove
    /// un secondo modello dello stesso provider cadrebbe insieme al primo, ma
    /// fatale per un panel di giudici, che si riduce a uno e lo riconvoca a ogni
    /// ciclo. `PerProviderAndModel` ne consegna due.
    ///
    /// MUTAZIONE: se il ramo `PerProviderAndModel` torna a deduplicare per solo
    /// provider, la prima asserzione fallisce mostrando il candidato unico.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_diversita_richiesta_decide_quanti_candidati(pool: sqlx::PgPool) {
        // Schema REALE (regola O): `ai_price_catalog` e `nexus_purpose_model`
        // (mig 0102) arrivano dalla migrazione. I DELETE isolano dai dati di
        // produzione. `select_models` legge il gate di qualificazione dai
        // `settings` VERI (acceso di default, mig 0595): ogni riga del catalog
        // si dichiara 'qualified'.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_purpose_model")
            .execute(&pool)
            .await
            .expect("pulizia purpose model");
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('reviewer', 'high', NULL, true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        // Un solo provider sano, tre modelli distinti nel tier richiesto.
        for (model, costo) in [("glm-4.7-flash", 0.07), ("qwen3-235b", 0.071), ("glm-5.2", 0.42)] {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                   (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
                    performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, \
                    qualification_state, qualification_expires_at, currency, last_probe_healthy_at) \
                 VALUES ('openrouter', $1, true, true, 'none', 'high', '[\"reasoning\"]'::jsonb, $2, $2, \
                         'qualified', now() + interval '30 days', 'USD', now())",
            )
            .bind(model)
            .bind(costo)
            .execute(&pool)
            .await
            .expect("catalog");
        }

        let giudici = resolve_purpose_provider_candidates_db_by(
            &pool,
            "reviewer",
            2,
            1,
            CandidateDiversity::PerProviderAndModel,
            &[],
            None,
        )
        .await
        .expect("candidati");
        assert_eq!(
            giudici.len(),
            2,
            "un panel di giudici deve poter usare due modelli dello stesso \
             provider quando gli altri sono in cooldown: {giudici:?}"
        );
        assert_eq!(
            giudici
                .iter()
                .map(|c| c.model.as_str())
                .collect::<Vec<_>>(),
            vec!["glm-4.7-flash", "qwen3-235b"],
            "ordine di preferenza (costo) preservato: {giudici:?}"
        );

        // Contro-caso: chi cerca un'ALTERNATIVA, non un parere, resta a uno.
        let alternative =
            resolve_purpose_provider_candidates_db(&pool, "reviewer", 2, 1).await.expect("candidati");
        assert_eq!(
            alternative.len(),
            1,
            "il criterio storico non cambia per gli altri chiamanti: {alternative:?}"
        );
    }

    /// Con piu' provider sani la diversita' di provider viene spesa PRIMA: due
    /// infrastrutture indipendenti battono due modelli della stessa. Solo dopo,
    /// se restano slot, si accetta un secondo modello di un provider gia' preso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn i_provider_nuovi_vengono_prima_di_un_secondo_modello(pool: sqlx::PgPool) {
        // Schema REALE (regola O): vedi nota sul test precedente.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_purpose_model")
            .execute(&pool)
            .await
            .expect("pulizia purpose model");
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('reviewer', 'high', NULL, true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        // I due piu' economici sono dello stesso provider: senza la preferenza
        // per i provider nuovi il panel li prenderebbe entrambi, scartando un
        // giudice su infrastruttura indipendente.
        for (provider, model, costo) in [
            ("openrouter", "glm-4.7-flash", 0.07),
            ("openrouter", "qwen3-235b", 0.071),
            ("google", "gemini-high", 0.30),
        ] {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                   (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
                    performance_tier, capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, \
                    qualification_state, qualification_expires_at, currency, last_probe_healthy_at) \
                 VALUES ($1, $2, true, true, 'none', 'high', '[\"reasoning\"]'::jsonb, $3, $3, \
                         'qualified', now() + interval '30 days', 'USD', now())",
            )
            .bind(provider)
            .bind(model)
            .bind(costo)
            .execute(&pool)
            .await
            .expect("catalog");
        }

        let giudici = resolve_purpose_provider_candidates_db_by(
            &pool,
            "reviewer",
            2,
            1,
            CandidateDiversity::PerProviderAndModel,
            &[],
            None,
        )
        .await
        .expect("candidati");
        assert_eq!(
            giudici.iter().map(|c| c.provider.as_str()).collect::<Vec<_>>(),
            vec!["openrouter", "google"],
            "due provider distinti battono due modelli dello stesso: {giudici:?}"
        );
    }

    /// Fan-out multi-provider: provider distinti dal catalog, dedup per provider.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn resolve_purpose_provider_candidates_deduplica_provider(pool: sqlx::PgPool) {
        // Schema REALE (regola O): vedi nota sui test precedenti.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_purpose_model")
            .execute(&pool)
            .await
            .expect("pulizia purpose model");
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('multi_provider_advisory', 'medium', 'reasoning', true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        for (provider, model, cost) in [
            ("openai", "gpt-a", 1.0),
            ("openai", "gpt-b", 0.5),
            ("anthropic", "claude-a", 2.0),
            ("deepseek", "ds-a", 0.3),
        ] {
            sqlx::query(
                "INSERT INTO ai_price_catalog \
                 (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
                  performance_tier, capabilities, qualified_capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens, \
                  qualification_state, qualification_expires_at, currency, last_probe_healthy_at) \
                 VALUES ($1, $2, true, true, 'none', 'medium', '[\"reasoning\"]'::jsonb, '[\"reasoning\"]'::jsonb, $3, $3, \
                         'qualified', now() + interval '30 days', 'USD', now())",
            )
            .bind(provider)
            .bind(model)
            .bind(cost)
            .execute(&pool)
            .await
            .expect("catalog");
        }
        let candidates =
            resolve_purpose_provider_candidates_db(&pool, "multi_provider_advisory", 3, 1)
                .await
                .expect("candidati");
        assert_eq!(candidates.len(), 3, "max 3 provider distinti");
        let providers: Vec<_> = candidates.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(providers, vec!["deepseek", "openai", "anthropic"]);
        assert_eq!(candidates[1].model, "gpt-b", "cost-first sullo stesso provider");
    }
    /// REGRESSIONE (incidente consiglio 2026-07-15) sul FAN-OUT multi-provider.
    /// Il panel chiede N provider DISTINTI, e il tier alto e' proprio quello
    /// popolato da MENO provider: 'heavy' vive solo su openai/anthropic/google
    /// (infer_tier_from_name non ammette gli altri). Con openai e anthropic
    /// insieme in cooldown billing il pool 'heavy' si e' svuotato e il panel non
    /// si convocava affatto, mentre modelli sani stavano un gradino sotto.
    /// Il fix a figura singola non copriva questo path: passava ancora
    /// `&[rule.tier]`, una tier-chain di UN elemento.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fanout_degrada_quando_il_tier_richiesto_e_esaurito(pool: sqlx::PgPool) {
        // Schema REALE (regola O): vedi nota sui test precedenti.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_purpose_model")
            .execute(&pool)
            .await
            .expect("pulizia purpose model");
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use)              VALUES ('multi_provider_advisory', 'heavy', 'reasoning', true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        // Gli unici 'heavy' sono di due provider AUTO-DISABILITATI (is_enabled
        // false = l'effetto del cooldown billing sul pool eleggibile); i modelli
        // sani vivono nel tier sotto, su provider diversi.
        for (provider, model, tier, enabled, cost) in [
            ("openai", "gpt-heavy", "heavy", false, 2.0),
            ("anthropic", "claude-heavy", "heavy", false, 3.0),
            ("deepseek", "deepseek-v4-pro", "high", true, 0.5),
            ("google", "gemini-high", "high", true, 1.0),
        ] {
            sqlx::query(
                "INSERT INTO ai_price_catalog                  (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy,                   performance_tier, capabilities, qualified_capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens,                   qualification_state, qualification_expires_at, currency, last_probe_healthy_at)                  VALUES ($1, $2, $3, true, 'none', $4, '[\"reasoning\"]'::jsonb, '[\"reasoning\"]'::jsonb, $5, $5, 'qualified', now() + interval '30 days', 'USD', now())",
            )
            .bind(provider)
            .bind(model)
            .bind(enabled)
            .bind(tier)
            .bind(cost)
            .execute(&pool)
            .await
            .expect("catalog");
        }
        let candidates = resolve_purpose_provider_candidates_db(&pool, "multi_provider_advisory", 3, 1)
            .await
            .expect("candidati");
        let providers: Vec<_> = candidates.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(
            providers,
            vec!["deepseek", "google"],
            "tier 'heavy' esaurito -> il fan-out DEGRADA a 'high' e convoca i              provider sani, invece di tornare a mani vuote (era l'incidente: il              panel non si convocava con modelli sani un gradino sotto)"
        );
        // Fascia OMOGENEA: il corto-circuito prende il primo tier con candidati,
        // mai un misto di fasce diverse.
        assert!(
            candidates.iter().all(|c| c.tier.as_deref() == Some("high")),
            "i pareri devono restare omogenei di fascia: {candidates:?}"
        );
    }

    /// REGRESSIONE (difetto osservato il 20/07): il tier richiesto NON e' vuoto,
    /// ma tutti i suoi modelli appartengono a UN SOLO provider. La tier-chain
    /// usciva al primo tier non vuoto -- la non-vuotezza come criterio -- e il
    /// panel dichiarava "provider distinti insufficienti (got=1 min=2)" mentre
    /// SEI provider erano abilitati e sani e i tier successivi, gia' autorizzati
    /// dalla catena, non erano mai stati interrogati.
    ///
    /// Il tetto (`limit`) e la soglia (`min_providers`) sono due domande diverse:
    /// qui si verifica che la seconda arrivi fino alla selezione e faccia
    /// proseguire la catena.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fanout_scende_finche_i_provider_distinti_bastano(pool: sqlx::PgPool) {
        // Schema REALE (regola O): vedi nota sui test precedenti.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_purpose_model")
            .execute(&pool)
            .await
            .expect("pulizia purpose model");
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use)
             VALUES ('multi_provider_advisory', 'medium', 'reasoning', true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");

        // Il tier richiesto e' POPOLATO ma monoprovider: due modelli, stesso
        // openrouter. Gli altri provider sani stanno un gradino sopra.
        for (provider, model, tier, cost) in [
            ("openrouter", "qwen3-235b", "medium", 0.2),
            ("openrouter", "glm-5.2", "medium", 0.3),
            ("deepseek", "deepseek-v4-pro", "high", 0.5),
            ("google", "gemini-high", "high", 1.0),
        ] {
            sqlx::query(
                "INSERT INTO ai_price_catalog
                   (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy,
                    performance_tier, capabilities, qualified_capabilities, input_cost_per_million_tokens, output_cost_per_million_tokens,
                    qualification_state, qualification_expires_at, currency, last_probe_healthy_at)
                 VALUES ($1, $2, true, true, 'none', $3, '[\"reasoning\"]'::jsonb, '[\"reasoning\"]'::jsonb, $4, $4, \
                         'qualified', now() + interval '30 days', 'USD', now())",
            )
            .bind(provider)
            .bind(model)
            .bind(tier)
            .bind(cost)
            .execute(&pool)
            .await
            .expect("catalog");
        }

        // Soglia 2 (il valore reale di `orchestrator.multi_provider_min_providers`).
        let candidates = resolve_purpose_provider_candidates_db(&pool, "multi_provider_advisory", 3, 2)
            .await
            .expect("candidati");
        let distinti: std::collections::HashSet<&str> =
            candidates.iter().map(|c| c.provider.as_str()).collect();
        assert!(
            distinti.len() >= 2,
            "con provider sani un gradino sopra il fan-out deve raggiungere la \
             soglia invece di degradare: trovati {distinti:?} da {candidates:?}"
        );
        assert!(
            distinti.contains("openrouter"),
            "il tier richiesto resta in TESTA: la diversita' si aggiunge, non \
             sostituisce la preferenza di fascia. Trovati {distinti:?}"
        );

        // MUTAZIONE / contro-caso: con soglia 1 vale la regola storica -- si esce
        // al primo tier non vuoto e si resta monoprovider. E' il comportamento
        // che produceva il degrado, e deve restare intatto per chi non chiede
        // diversita' (failover in cascata, classificatore).
        let soglia_uno = resolve_purpose_provider_candidates_db(&pool, "multi_provider_advisory", 3, 1)
            .await
            .expect("candidati");
        let distinti_uno: std::collections::HashSet<&str> =
            soglia_uno.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(
            distinti_uno.len(),
            1,
            "senza richiesta di diversita' la catena esce al primo tier non \
             vuoto: {soglia_uno:?}"
        );
    }

    /// Il pavimento LIMITA la degradazione, non ALZA la richiesta. Un purpose
    /// configurato `light` lo e' per scelta (titoli chat, doc: task banali dove
    /// un modello debole basta e costa meno): promuoverlo a `medium` perche' il
    /// pavimento agentico dice `medium` sarebbe una decisione presa alle sue
    /// spalle, e piu' cara. Sei test del routing sub-agente lo hanno colto
    /// mentre lo stavo introducendo.
    #[sqlx::test]
    async fn il_pavimento_non_alza_mai_il_tier_richiesto(pool: sqlx::PgPool) {
        crate::test_support::create_settings_table_with(
            &pool,
            "agent.routing.agentic_min_tier",
            "medium",
        )
        .await;

        // Chi chiede MENO del pavimento resta dov'e': nessuna promozione.
        assert_eq!(purpose_floor(&pool, "light").await, "light");
        // Chi chiede il pavimento o piu' e' limitato dal pavimento: puo'
        // scendere, ma non sotto.
        assert_eq!(purpose_floor(&pool, "medium").await, "medium");
        assert_eq!(purpose_floor(&pool, "heavy").await, "medium");
        assert_eq!(purpose_floor(&pool, "frontier").await, "medium");
    }

    #[test]
    fn try_model_mappa_ogni_esito_nella_variante_tipizzata() {
        let ok = PurposeResolution::Resolved {
            provider: "p".into(),
            model: "m".into(),
            rationale: "tier".into(),
        }
        .try_model("x");
        assert_eq!(ok.unwrap(), ("p".into(), "m".into()));

        assert!(matches!(
            PurposeResolution::NotFound.try_model("x"),
            Err(PurposeUnresolved::NotFound { .. })
        ));
        assert!(matches!(
            PurposeResolution::NoCapableModel { tier: "light".into() }.try_model("x"),
            Err(PurposeUnresolved::NoCapableModel { .. })
        ));
        assert!(matches!(
            PurposeResolution::MatrixUnavailable("db down".into()).try_model("x"),
            Err(PurposeUnresolved::MatrixUnavailable { .. })
        ));
    }

    #[test]
    fn try_model_resta_decidibile_lungo_la_catena_anyhow() {
        // Stessa strada dei call site (es. learned_instructions::distill_project):
        // try_model + `?` (From::from) dentro un Result anyhow. Il punto di
        // decisione a valle riconosce la variante col downcast (regola M).
        let e: anyhow::Error = PurposeResolution::NotFound
            .try_model("learned_instructions_distill")
            .map_err(anyhow::Error::from)
            .unwrap_err();
        assert!(PurposeUnresolved::in_chain(&e));
    }

    #[test]
    fn into_model_conserva_i_messaggi_storici() {
        // into_model delega a try_model (regola L): i call site che mostrano il
        // messaggio non devono vedere cambiare l'output.
        assert_eq!(
            PurposeResolution::NotFound.into_model("doc_gen").unwrap_err(),
            "purpose 'doc_gen' non configurato o privo di tier in nexus_purpose_model"
        );
        assert_eq!(
            PurposeResolution::NoCapableModel { tier: "light".into() }
                .into_model("doc_gen")
                .unwrap_err(),
            "nessun modello del tier 'light' disponibile per purpose 'doc_gen' \
             (capability mancante o provider in cooldown)"
        );
        assert_eq!(
            PurposeResolution::MatrixUnavailable("db down".into())
                .into_model("doc_gen")
                .unwrap_err(),
            "routing non disponibile per 'doc_gen': db down"
        );
    }

    // ── reduce_purpose_to_override: override dei run di rimedio (mig 0626) ──
    //
    // Regola O: i test attraversano il PRODUTTORE reale della risoluzione
    // (resolve_purpose_model_db -> resolve_purpose_core -> select_model, lo
    // STESSO core dell'adapter AppState usato in produzione; il delta non
    // coperto e' il solo adapter cache di 2 righe) composto col MEDESIMO
    // reducer dei call site. La CONSEGUENZA asserita e' la coppia Some/Some
    // (che disabilita lo slot-routing) o None/None (routing di default), mai
    // la stringa del modello.

    /// Schema REALE (regola O): `ai_price_catalog` e `nexus_purpose_model`
    /// arrivano dalla migrazione. I DELETE isolano dai dati di produzione.
    async fn crea_tabella_purpose(pool: &sqlx::PgPool) {
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(pool)
            .await
            .expect("pulizia catalog");
        sqlx::query("DELETE FROM nexus_purpose_model")
            .execute(pool)
            .await
            .expect("pulizia purpose model");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn auto_remediation_risolto_valorizza_entrambi_gli_override(pool: sqlx::PgPool) {
        crea_tabella_purpose(&pool).await;
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('auto_remediation', 'heavy', NULL, true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        // `resolve_purpose_model_db` attraversa `select_model`, che legge il gate
        // di qualificazione dai `settings` VERI (acceso di default, mig 0595).
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, is_enabled, supports_tool_use, agentic_thinking_policy, \
              performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, \
              qualification_state, qualification_expires_at, currency, last_probe_healthy_at) \
             VALUES ('deepseek', 'ds-heavy', true, true, 'none', 'heavy', 0.5, 0.5, \
                     'qualified', now() + interval '30 days', 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("catalog");
        let (p, m) = reduce_purpose_to_override(
            resolve_purpose_model_db(&pool, PURPOSE_AUTO_REMEDIATION).await,
            PURPOSE_AUTO_REMEDIATION,
        );
        // Mutazione che rende rosso: reducer che ritorna (Some, None) o
        // (None, None) su Resolved -> override monco/assente, slot-routing
        // riattivato a meta' (lo stato ibrido che l'invariante vieta).
        assert!(
            p.is_some() && m.is_some(),
            "override valorizzato su ENTRAMBI i campi: p={p:?} m={m:?}"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn auto_remediation_senza_tier_degrada_al_routing_di_default(pool: sqlx::PgPool) {
        crea_tabella_purpose(&pool).await;
        // Purpose presente ma tier NULL -> NotFound (tier-only). Il rimedio NON
        // si blocca: (None, None) = routing di default, comportamento odierno.
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('auto_remediation', NULL, NULL, true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        let (p, m) = reduce_purpose_to_override(
            resolve_purpose_model_db(&pool, PURPOSE_AUTO_REMEDIATION).await,
            PURPOSE_AUTO_REMEDIATION,
        );
        assert_eq!((p, m), (None, None), "NotFound -> degrado onesto");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn auto_remediation_catalog_vuoto_degrada_al_routing_di_default(pool: sqlx::PgPool) {
        crea_tabella_purpose(&pool).await;
        // Tier valorizzato ma NESSUN modello capace nel catalog -> NoCapableModel
        // -> (None, None). Stessa strada del produttore, ramo diverso.
        sqlx::query(
            "INSERT INTO nexus_purpose_model (purpose, tier, required_capability, requires_tool_use) \
             VALUES ('auto_remediation', 'heavy', NULL, true)",
        )
        .execute(&pool)
        .await
        .expect("purpose");
        let (p, m) = reduce_purpose_to_override(
            resolve_purpose_model_db(&pool, PURPOSE_AUTO_REMEDIATION).await,
            PURPOSE_AUTO_REMEDIATION,
        );
        assert_eq!((p, m), (None, None), "NoCapableModel -> degrado onesto");
    }
}
