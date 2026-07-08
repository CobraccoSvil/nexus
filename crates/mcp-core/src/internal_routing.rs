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

// ---------------------------------------------------------------------------
// POST /api/internal/provider-error — bridge cooldown brain Python → Rust
// ---------------------------------------------------------------------------

/// Body della richiesta `POST /api/internal/provider-error`.
/// Chiamato dal brain Python (cooldown_bridge.py) quando rileva un errore
/// provider in punti non osservati direttamente da Rust (es. catena
/// classificatore, fallback chain in registry.py).
///
/// Due percorsi (retrocompatibili):
///   - legacy: `error_class` gia' calcolata dal chiamante (es. registry.py,
///     dove la classe e' una decisione locale del brain, non una
///     ri-classificazione del testo);
///   - raw: `error_class` assente, il chiamante invia `error_text` (+ `status`
///     HTTP se noto) e mcp-core classifica con `provider_error_classifier`
///     (punto unico regola L: niente doppia classificazione keyword in Python).
#[derive(Debug, Deserialize)]
pub struct ProviderErrorPayload {
    pub provider: String,
    /// Classe pre-calcolata (percorso legacy). Se assente o vuota, mcp-core
    /// la deriva da `error_text`/`status`.
    #[serde(default)]
    pub error_class: Option<String>,
    /// Testo raw dell'errore provider (percorso raw: il brain NON classifica).
    #[serde(default)]
    pub error_text: Option<String>,
    /// HTTP status strutturato, se noto al chiamante (es. `exc.response.status_code`).
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub retry_after_seconds: Option<u64>,
}

/// Deriva la classe d'errore effettiva (e il retry-after) dal payload.
/// PUNTO UNICO della classificazione: il ramo raw delega a
/// `provider_error_classifier::classify_with_status`; il ramo legacy passa
/// la classe cosi' com'e' (retrocompatibilita' con i payload vecchi).
/// `Err` se il payload non contiene ne' classe ne' materiale raw.
fn derive_error_class(body: &ProviderErrorPayload) -> Result<(String, Option<u64>), String> {
    let explicit = body
        .error_class
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(class) = explicit {
        return Ok((class.to_string(), body.retry_after_seconds));
    }
    let raw = body.error_text.as_deref().map(str::trim).unwrap_or("");
    if raw.is_empty() && body.status.is_none() {
        return Err(
            "payload senza `error_class` e senza `error_text`/`status`: nulla da classificare"
                .to_string(),
        );
    }
    let classified = crate::provider_error_classifier::classify_with_status(raw, body.status);
    // retry-after: esplicito del payload > estratto dal testo raw (l'estrazione
    // che il bridge brain faceva con regex locale ora vive nel classifier Rust).
    let retry_after = body.retry_after_seconds.or_else(|| {
        crate::provider_error_classifier::extract_retry_after_seconds(raw)
    });
    Ok((classified.stop_reason, retry_after))
}

/// Handler `POST /api/internal/provider-error`.
/// Applica il cooldown appropriato in base alla classe dell'errore:
///   - `billing_error` → cooldown lungo 6h (in-memory + Redis + TTL su
///                       `nexus_provider_health.billing_cooldown_until`, la fonte
///                       persistente GIUSTA perche' scade). NON disabilita piu' il
///                       catalog/matrix: il routing salta i provider in cooldown
///                       via `is_provider_in_cooldown`/`cooldown_snapshot`. La
///                       `billing_cooldown_recovery_loop` (probe-then-reenable)
///                       azzera il TTL quando il provider torna 200.
///   - `rate_limit`    → cooldown breve con retry_after o default 60s
///   - `overloaded` / `provider_error` → cooldown breve 60s
pub async fn provider_error_handler(
    State(_state): State<AppState>,
    Json(body): Json<ProviderErrorPayload>,
) -> impl IntoResponse {
    let provider = body.provider.trim().to_lowercase();
    if provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "campo `provider` vuoto".to_string(),
        )
            .into_response();
    }
    let (error_class, retry_after_seconds) = match derive_error_class(&body) {
        Ok(derived) => derived,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    match error_class.as_str() {
        "billing_error" => {
            crate::provider_cooldown::put_provider_in_long_cooldown(
                &provider,
                &format!("brain bridge: {error_class}"),
            );
            tracing::warn!(
                "provider-error bridge: '{}' → COOLDOWN LUNGO + TTL health (billing_error)",
                provider
            );
        }
        "rate_limit" => {
            // Durata DB-driven (regola G): retry_after del provider se presente,
            // altrimenti la soglia short configurata (slow_cooldown_s), non un 60 letterale.
            let secs = retry_after_seconds.unwrap_or_else(|| {
                crate::provider_cooldown::provider_health_timings().slow_cooldown_s
            });
            crate::provider_cooldown::put_provider_in_cooldown(&provider, Some(secs));
            tracing::warn!(
                "provider-error bridge: '{}' → cooldown breve {}s (rate_limit)",
                provider,
                secs
            );
        }
        "overloaded" | "provider_error" => {
            let secs = crate::provider_cooldown::provider_health_timings().slow_cooldown_s;
            crate::provider_cooldown::put_provider_in_cooldown(&provider, Some(secs));
            tracing::warn!(
                "provider-error bridge: '{}' → cooldown breve {}s ({})",
                provider,
                secs,
                error_class
            );
        }
        other => {
            tracing::debug!(
                "provider-error bridge: '{}' classe non riconosciuta '{}', ignorata",
                provider,
                other
            );
        }
    }
    (StatusCode::OK, "ok".to_string()).into_response()
}

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

impl PurposeResolution {
    /// Riduce l'esito a `(provider, model)` oppure a un messaggio d'errore
    /// leggibile (tier-only: nessun fallback). Helper per i call site che vogliono
    /// solo il modello risolto e mappano l'errore nel proprio tipo. Evita di
    /// duplicare il match a 4 rami in ogni chiamante (regola L).
    pub fn into_model(self, purpose: &str) -> Result<(String, String), String> {
        match self {
            PurposeResolution::Resolved {
                provider, model, ..
            } => Ok((provider, model)),
            PurposeResolution::NoCapableModel { tier } => Err(format!(
                "nessun modello del tier '{tier}' disponibile per purpose '{purpose}' \
                 (capability mancante o provider in cooldown)"
            )),
            PurposeResolution::NotFound => Err(format!(
                "purpose '{purpose}' non configurato o privo di tier in nexus_purpose_model"
            )),
            PurposeResolution::MatrixUnavailable(e) => {
                Err(format!("routing non disponibile per '{purpose}': {e}"))
            }
        }
    }
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
    match crate::orchestrator::best_model_for_tier_pinned(
        db,
        &rule.tier,
        rule.capability.as_deref(),
        rule.requires_tool_use,
        exclude_providers,
        only_provider,
    )
    .await
    {
        Some((provider, model)) => PurposeResolution::Resolved {
            provider,
            model,
            rationale: format!("tier={}:auto", rule.tier),
        },
        None => {
            tracing::warn!(
                purpose = %purpose, tier = %rule.tier,
                "resolve_purpose: nessun modello catalog per il tier (capability mancante o cooldown)"
            );
            PurposeResolution::NoCapableModel { tier: rule.tier }
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
    let row = match sqlx::query_as::<_, (Option<String>, Option<String>, bool)>(
        "SELECT tier, required_capability, requires_tool_use \
         FROM nexus_purpose_model WHERE purpose = $1 LIMIT 1",
    )
    .bind(purpose)
    .fetch_optional(db)
    .await
    {
        Ok(r) => r,
        Err(e) => return PurposeResolution::MatrixUnavailable(e.to_string()),
    };

    let Some((tier, capability, requires_tool_use)) = row else {
        return PurposeResolution::NotFound;
    };

    let tier_rule = tier.map(|t| crate::routing_matrix::PurposeTierRule {
        tier: t,
        capability,
        requires_tool_use,
    });

    resolve_purpose_core(db, purpose, tier_rule, exclude_providers, only_provider).await
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
    pub provider: String,
    pub seconds_remaining: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CooldownSnapshotResponse {
    /// Lista dei provider attualmente in cooldown (billing/quota/rate-limit).
    pub providers: Vec<CooldownEntry>,
}

/// Handler `GET /api/internal/routing/cooldown`.
/// Ritorna l'elenco dei provider in cooldown secondo il gate Rust. Il brain
/// usa questa lista per saltare i provider morti in fallback/escalation senza
/// duplicare il ragionamento sul cooldown (deprecazione di
/// `registry._is_in_billing_cooldown` come fonte primaria).
pub async fn cooldown_snapshot_handler(State(_state): State<AppState>) -> impl IntoResponse {
    let providers: Vec<CooldownEntry> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(provider, seconds_remaining, reason)| CooldownEntry {
            provider,
            seconds_remaining,
            reason,
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
    let has_intent = body
        .intent
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty());
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

    fn payload(
        error_class: Option<&str>,
        error_text: Option<&str>,
        status: Option<u16>,
        retry_after_seconds: Option<u64>,
    ) -> ProviderErrorPayload {
        ProviderErrorPayload {
            provider: "test_provider".to_string(),
            error_class: error_class.map(str::to_string),
            error_text: error_text.map(str::to_string),
            status,
            retry_after_seconds,
        }
    }

    #[test]
    fn legacy_classe_esplicita_passa_invariata() {
        // Retrocompatibilita': il payload vecchio (classe pre-calcolata dal
        // brain, es. registry.py billing) NON viene ri-classificato.
        let (class, retry) =
            derive_error_class(&payload(Some("billing_error"), None, None, Some(42))).unwrap();
        assert_eq!(class, "billing_error");
        assert_eq!(retry, Some(42));
    }

    #[test]
    fn raw_testo_billing_classificato_da_rust() {
        let (class, retry) = derive_error_class(&payload(
            None,
            Some("Your credit balance is too low to make this request"),
            None,
            None,
        ))
        .unwrap();
        assert_eq!(class, "billing_error");
        assert_eq!(retry, None);
    }

    #[test]
    fn raw_status_429_con_retry_after_dal_testo() {
        // Lo status strutturato del chiamante mappa rate_limit; il retry-after
        // assente nel payload viene estratto dal testo raw (punto unico Rust).
        let (class, retry) = derive_error_class(&payload(
            None,
            Some("Too many requests, please retry after 30 seconds"),
            Some(429),
            None,
        ))
        .unwrap();
        assert_eq!(class, "rate_limit");
        assert_eq!(retry, Some(30));
    }

    #[test]
    fn retry_after_esplicito_vince_sul_testo() {
        let (class, retry) = derive_error_class(&payload(
            None,
            Some("rate limit, retry after 30"),
            None,
            Some(7),
        ))
        .unwrap();
        assert_eq!(class, "rate_limit");
        assert_eq!(retry, Some(7));
    }

    #[test]
    fn classe_vuota_equivale_ad_assente() {
        // error_class="" (stringa vuota) deve attivare il ramo raw, non
        // passare una classe vuota al match del cooldown.
        let (class, _) = derive_error_class(&payload(
            Some("  "),
            Some("Error code: 529 - overloaded"),
            None,
            None,
        ))
        .unwrap();
        assert_eq!(class, "overloaded");
    }

    #[test]
    fn payload_senza_classe_ne_materiale_raw_e_errore() {
        assert!(derive_error_class(&payload(None, None, None, None)).is_err());
        assert!(derive_error_class(&payload(None, Some("   "), None, None)).is_err());
        // Solo status, senza testo: classificabile (HTTP map).
        let (class, _) = derive_error_class(&payload(None, None, Some(500), None)).unwrap();
        assert_eq!(class, "provider_error");
    }
}
