//! `model_service`: il SERVIZIO UNICO di selezione di un modello dal catalog.
//!
//! # Perche' esiste
//!
//! `select_models_tierchain` e' gia' il punto unico della WHERE di eleggibilita',
//! ed e' gia' chiamato da tutta la famiglia `best_model_for_tier*` /
//! `select_agentic_model*`. Eppure, nello stesso giorno (2026-07-15), sono stati
//! misurati tre difetti della STESSA domanda ("quale modello per questo tier?"):
//!
//! - `best_model_for_tier_pinned` passava `&[tier]` mentre `route_model_from_catalog`
//!   usava `agentic_tier_chain`: la chat degradava e sopravviveva al cooldown, il
//!   consiglio moriva con `NoCapableModel` in 6.9s con 19 modelli sani un gradino
//!   sotto;
//! - `resolve_purpose_provider_candidates_db` (il fan-out del consiglio) aveva lo
//!   stesso difetto, e il fix del primo non lo copriva;
//! - `best_non_agentic_model` pure, e il fix dei primi due non copriva nemmeno lui
//!   (viveva dentro `if requires_tool_use { .. }`).
//!
//! La causa non e' la disattenzione: e' che **il tipo del parametro `tier` cambia
//! strato per strato**. In basso e' `&[&str]` (una catena esplicita: il chiamante
//! decide se degradare), in alto e' `&str` (uno scalare: la catena la sceglie un
//! `if` interno che il chiamante non vede). La degradazione diventa cosi' un
//! EFFETTO COLLATERALE di quale strato chiami. E `&[tier]` letto nel codice e'
//! ambiguo fra "bersaglio esatto, voluto" (l'upscale: corretto) e "non ho pensato
//! alla degradazione" (il difetto): un `grep` non li distingue.
//!
//! **Una funzione condivisa con parametri liberi non e' un punto unico: e' una
//! convenzione, e le convenzioni non hanno test.**
//!
//! # Cosa garantisce (invarianti, non buone intenzioni)
//!
//! - **I2** — il gate di qualificazione segue il PROFILO, non la diligenza del
//!   chiamante: `Profile::Agentic` implica `qualification_gate(db)` applicato,
//!   sempre. I costruttori a mano di `EligibilityFilter` spariscono.
//! - **I3** — degradazione MONOTONA: `rank(effective) <= rank(requested)`, e con
//!   `Exact` vale `effective == requested`.
//! - **I4** — `degraded` e' un DATO calcolato dal servizio (regola M), non una
//!   deduzione del lettore: oggi un solo call site su ~20 lo ricava a mano.
//! - **I5** — il pin cede il PROVIDER, mai la qualita': `pin` implica nessuna
//!   degradazione, e il tipo lo rende dicibile (`ExactReason::PinnedProvider`).
//! - **I6** — esaurimento TIPIZZATO: mai un `None` muto. `NoModelReason` distingue
//!   "la catena si e' esaurita" da "il bersaglio esatto era vuoto" da "il gate ha
//!   svuotato il pool" (worker di qualificazione fermo).
//! - **I7** — l'ordinamento ha UN vocabolario: `Rank` e' un enum chiuso. L'
//!   `order_by: &str` interpolato verbatim sparisce, e con esso la possibilita' di
//!   scrivere una scala tier dentro una stringa SQL che il compilatore non guarda
//!   — che e' ESATTAMENTE come la scala a 3 livelli di `agent_run.rs` e' sopravvissuta
//!   alla migrazione a 5 livelli (mig 0528), invisibile, per mesi.
//!
//! # Stato
//!
//! FASE 5 del piano di convergenza: la facciata delega a `select_models_tierchain`
//! e NESSUN call site e' ancora migrato. La migrazione dei ~20 call site (fase 6)
//! e' l'operazione a rischio piu' alto e viene dopo la rete di caratterizzazione.

use sqlx::PgPool;

use nexus_agent_graph::decisions::tiers::{tier_rank, tier_rank_sql};

use super::model_routing::{agentic_tier_chain, AGENTIC_COST_FIRST_ORDER, AGENTIC_FAILOVER_ORDER};
use super::{qualification_gate, select_models_tierchain, EligibilityFilter};

/// Come si tratta il tier RICHIESTO quando non ha candidati eleggibili.
/// E' un PARAMETRO ESPLICITO: non e' piu' la conseguenza di quale strato chiami.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierPolicy {
    /// Il tier e' un REQUISITO da soddisfare al meglio: se e' vuoto si scende
    /// lungo [`agentic_tier_chain`]. E' il default di ogni routing che deve
    /// servire una richiesta: l'alternativa e' "nessun modello", che era
    /// l'incidente del 2026-07-15.
    Degrade,
    /// Il tier e' il BERSAGLIO della richiesta: nessun altro tier e' accettabile,
    /// e l'esito e' GARANTITO di quel tier. Il `why` e' obbligatorio: e' cio' che
    /// rende dicibile la differenza fra "voluto" e "non ci ho pensato" — la
    /// differenza che il `grep` su `&[tier]` non vede.
    Exact { why: ExactReason },
    /// Il tier non e' un vincolo: enumera su TUTTI i tier e lascia decidere
    /// l'ordinamento (o un modulo puro a valle).
    AnyTier,
}

/// Perche' un tier e' un bersaglio esatto invece che un requisito degradabile.
/// Ogni variante corrisponde a un sito REALE verificato: non e' una tassonomia
/// teorica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactReason {
    /// Il tier e' il target di uno scale-move gia' deciso a monte dal modulo puro
    /// dello scale-controller: questa selezione lo ESEGUE, non lo negozia.
    /// Degradare qui significherebbe fare l'opposto di un upscale.
    ScaleTarget,
    /// Il pin cede il provider, mai la qualita': se il provider pinnato non ha il
    /// tier richiesto l'esito e' vuoto, e il chiamante ritenta SENZA pin
    /// prendendo il tier giusto altrove. Degradare qui aggancerebbe il run a un
    /// modello piu' debole pur di onorare il pin.
    PinnedProvider,
    /// Si sta riparando/riconciliando una riga di configurazione che deve
    /// PRESERVARE il tier deciso dall'admin (heal di un pin morto, default di
    /// provider): il tier non e' una preferenza di questa richiesta.
    PreserveConfig,
}

/// Ordinamenti AMMESSI. Enum chiuso: chiude la porta all'`ORDER BY` libero.
///
/// Non e' pedanteria di tipi. L'`order_by: &str` interpolato verbatim ha lasciato
/// passare, per mesi e invisibile al compilatore, un
/// `CASE performance_tier WHEN 'heavy' THEN 2 WHEN 'medium' THEN 1 ELSE 0 END`
/// — una scala a 3 livelli in un vocabolario a 5, che faceva collassare `frontier`
/// e `high` su `light` e faceva scegliere all'escalation "sali al piu' capace" un
/// modello MENO capace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rank {
    /// Il piu' economico, featured a parita' (`AGENTIC_COST_FIRST_ORDER`).
    CostFirst,
    /// Non-thinking prima (evita i vincoli di round-trip su switch mid-run),
    /// poi costo (`AGENTIC_FAILOVER_ORDER`).
    FailoverSafe,
    /// Anti "reasoner puro": retrocede i thinking-senza-tool, che nelle chiamate
    /// TESTUALI bruciano il budget in reasoning e tornano vuoti (incidente
    /// 2026-06-10). Poi costo e featured.
    NonAgenticSafe,
    /// Finestra piu' ampia prima (upscale per contesto), poi costo.
    WidestWindow,
    /// Capacita' DECRESCENTE per tier — l'UNICO modo di ordinare per tier.
    /// L'espressione viene generata da [`tier_rank_sql`], cioe' dallo stesso
    /// vocabolario di [`tier_rank`]: una scala sola, verificata contro Postgres.
    HighestTierFirst,
    /// Piu' veloce prima (behavior_mode "veloce"), poi costo. `speed_tier` e' un
    /// vocabolario SUO (fast/medium/slow), distinto dal performance_tier.
    Fastest,
    /// Il piu' capace/caro DENTRO il tier gia' promosso (behavior_mode
    /// "approfondita"): featured prima, poi costo DECRESCENTE. Non e'
    /// `HighestTierFirst`: li' il tier e' gia' fissato dal chiamante, qui si
    /// sceglie il migliore al suo interno.
    MostCapable,
}

impl Rank {
    /// La clausola SQL. Nessun `CASE` scritto a mano: quello per tier lo genera
    /// il punto unico del vocabolario.
    fn to_sql(self) -> String {
        match self {
            Rank::CostFirst => AGENTIC_COST_FIRST_ORDER.to_string(),
            Rank::FailoverSafe => AGENTIC_FAILOVER_ORDER.to_string(),
            Rank::NonAgenticSafe => "(uses_thinking_mode AND NOT supports_tool_use) ASC, \
                 input_cost_per_million_tokens ASC, is_featured DESC"
                .to_string(),
            Rank::WidestWindow => {
                "context_window DESC, input_cost_per_million_tokens ASC NULLS LAST".to_string()
            }
            Rank::HighestTierFirst => format!(
                "{} DESC, input_cost_per_million_tokens DESC NULLS LAST, \
                 output_cost_per_million_tokens DESC NULLS LAST",
                tier_rank_sql("performance_tier")
            ),
            Rank::Fastest => "CASE speed_tier WHEN 'fast' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, \
                 input_cost_per_million_tokens ASC"
                .to_string(),
            Rank::MostCapable => {
                "is_featured DESC, input_cost_per_million_tokens DESC".to_string()
            }
        }
    }
}

/// Profilo d'uso della richiesta: decide i filtri e il gate in UN posto, invece
/// che call site per call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Tool-loop: `supports_tool_use`, thinking-policy, e il GATE di
    /// qualificazione (`require_qualified` + `exclude_preview`) letto dal DB.
    /// I5/I2: il gate non dipende piu' dal fatto che il chiamante si ricordi.
    Agentic,
    /// vision / chat / embedding / web_search: niente tool_use ne' thinking-policy.
    /// Il gate misura il profilo AGENTICO, quindi qui NON si applica: e' una
    /// scelta dichiarata, non una dimenticanza.
    NonAgentic,
}

/// La richiesta. Sostituisce la famiglia di ~8 funzioni a strati con firme diverse.
#[derive(Debug, Clone)]
pub struct ModelRequest<'a> {
    pub tier: &'a str,
    pub tier_policy: TierPolicy,
    pub profile: Profile,
    pub capability: Option<&'a str>,
    pub min_context_window: i64,
    /// PAVIMENTO di capacita': nessun modello sotto questo tier e' ammissibile.
    /// E' eleggibilita', non preferenza — vedi [`EligibilityFilter::min_tier`].
    /// `None` = nessun pavimento (il default storico).
    pub min_tier: Option<&'a str>,
    pub exclude_providers: &'a [String],
    /// `Some(p)` RESTRINGE al provider `p`. Con un pin la `tier_policy` DEVE
    /// essere `Exact { why: PinnedProvider }` (invariante I5, verificata).
    pub pin: Option<&'a str>,
    pub rank: Rank,
    /// Riordino telemetria-aware dei candidati ammissibili (ADR 0030, opt-in dal
    /// flag DB `agent.governance.telemetry_aware`): un modello con error-rate o
    /// timeout alti negli ultimi check viene retrocesso, l'alternativa sana
    /// promossa (regola M: decide la telemetria strutturata, non un flag statico).
    ///
    /// E' un PARAMETRO e non un comportamento implicito del servizio: si applica
    /// alla selezione DINAMICA del turno primario (la chat), dove il modello
    /// scelto viene poi pinnato/sticky per il run. Accenderlo ovunque cambierebbe
    /// in silenzio la scelta di siti che oggi non lo usano — es. l'upscale, che
    /// deve eseguire il bersaglio dello scale-controller, non negoziarlo.
    ///
    /// A flag OFF (default) e' bit-identico alla selezione normale: il riordino
    /// e' STABILE, con telemetria uniforme il top-1 resta quello del `rank`.
    pub governed: bool,
}

impl<'a> ModelRequest<'a> {
    /// Richiesta agentica standard: il tier e' un requisito, si degrada.
    pub fn agentic(tier: &'a str) -> Self {
        Self {
            tier,
            tier_policy: TierPolicy::Degrade,
            profile: Profile::Agentic,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            pin: None,
            rank: Rank::CostFirst,
            governed: false,
        }
    }

    /// Richiesta non-agentica (vision/chat/embedding): il tier e' un requisito.
    pub fn non_agentic(tier: &'a str) -> Self {
        Self {
            profile: Profile::NonAgentic,
            rank: Rank::NonAgenticSafe,
            ..Self::agentic(tier)
        }
    }

    pub fn capability(mut self, c: Option<&'a str>) -> Self {
        self.capability = c;
        self
    }

    pub fn rank(mut self, r: Rank) -> Self {
        self.rank = r;
        self
    }

    pub fn exclude(mut self, providers: &'a [String]) -> Self {
        self.exclude_providers = providers;
        self
    }

    pub fn min_context_window(mut self, n: i64) -> Self {
        self.min_context_window = n;
        self
    }

    /// Pavimento di capacita' (eleggibilita'): scarta i modelli sotto `tier`.
    /// Da usare quando un modello troppo debole non e' un ripiego ma un danno —
    /// es. il failover di un run agentico (vedi `EligibilityFilter::min_tier`).
    pub fn min_tier(mut self, tier: &'a str) -> Self {
        self.min_tier = Some(tier);
        self
    }

    /// Pin del provider: forza `Exact { PinnedProvider }` (I5). Non e' possibile
    /// costruire "pin + degradazione" per distrazione: lo impedisce questo
    /// costruttore, e `validate` lo verifica comunque per chi costruisce a mano.
    pub fn pinned(mut self, provider: &'a str) -> Self {
        self.pin = Some(provider);
        self.tier_policy = TierPolicy::Exact {
            why: ExactReason::PinnedProvider,
        };
        self
    }

    pub fn tier_policy(mut self, p: TierPolicy) -> Self {
        self.tier_policy = p;
        self
    }

    /// Attiva il riordino telemetria-aware (ADR 0030). Vedi [`ModelRequest::governed`]:
    /// e' per la selezione DINAMICA del turno primario, non per tutti.
    pub fn governed(mut self, on: bool) -> Self {
        self.governed = on;
        self
    }
}

/// L'esito DICE cosa e' successo (regola M): `degraded` e' calcolato dal servizio
/// confrontando richiesto ed effettivo, mai lasciato dedurre a chi legge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    pub requested_tier: String,
    /// Il `performance_tier` della riga scelta. `None` se la colonna e' NULL
    /// (il tier sta per diventare nullable) o se la policy era `AnyTier`.
    pub effective_tier: Option<String>,
    /// I4: un DATO, non una deduzione.
    pub degraded: bool,
    /// Per log e rationale: `tier=heavy:auto` | `tier=heavy:degraded_to=high`.
    pub rationale: String,
}

/// Perche' non c'e' un modello. I6: mai un `None` muto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoModelReason {
    /// `Degrade`: la catena intera (dal tier richiesto giu' fino a `light`) non
    /// ha prodotto candidati. E' il caso grave: il parco e' fermo.
    ChainExhausted { requested_tier: String },
    /// `Exact`: il bersaglio era vuoto. Spesso e' l'esito ATTESO (il pin non ha
    /// quel tier -> il chiamante ritenta senza pin; l'upscale non ha un target ->
    /// niente upscale). Non e' un errore.
    ExactTierEmpty {
        requested_tier: String,
        why: ExactReason,
    },
    /// Il gate di qualificazione ha svuotato il pool: candidati ci sarebbero, ma
    /// nessuno e' qualificato. Distinto dagli altri perche' la causa NON e' il
    /// parco: e' il worker di qualificazione fermo o la batteria che boccia tutti
    /// (successo il 2026-07-15: un difetto del probe squalificava modelli sani).
    GateEmpty { requested_tier: String },
    /// Errore di lettura del catalog (regola H: propagato, non silenziato come
    /// "nessun modello").
    CatalogUnavailable(String),
    /// La richiesta viola un'invariante del servizio (I5): pin + degradazione.
    /// E' un bug del chiamante, non uno stato del parco.
    InvalidRequest(String),
}

impl NoModelReason {
    /// `true` se e' un esito atteso e non un guasto: il chiamante ha un piano B
    /// (ritentare senza pin, saltare l'upscale).
    pub fn is_expected(&self) -> bool {
        matches!(self, NoModelReason::ExactTierEmpty { .. })
    }
}

/// Testo per log e messaggi. Il TIPO resta il segnale su cui si decide (regola M):
/// questo e' solo per chi legge.
impl std::fmt::Display for NoModelReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoModelReason::ChainExhausted { requested_tier } => write!(
                f,
                "nessun modello eleggibile in tutta la catena da '{requested_tier}' in giu' \
                 (il parco e' fermo: provider in cooldown o catalog vuoto)"
            ),
            NoModelReason::ExactTierEmpty { requested_tier, why } => write!(
                f,
                "il tier '{requested_tier}' e' vuoto e non e' degradabile ({why:?})"
            ),
            NoModelReason::GateEmpty { requested_tier } => write!(
                f,
                "il gate di qualificazione ha svuotato il pool per '{requested_tier}': \
                 candidati ci sarebbero, ma nessuno e' qualificato (worker di \
                 qualificazione fermo o batteria che boccia tutti)"
            ),
            NoModelReason::CatalogUnavailable(e) => write!(f, "catalog illeggibile: {e}"),
            NoModelReason::InvalidRequest(e) => write!(f, "richiesta non valida: {e}"),
        }
    }
}

/// I5: pin e degradazione sono incompatibili. Pura, testabile.
fn validate(req: &ModelRequest<'_>) -> Result<(), NoModelReason> {
    if req.pin.is_some() && !matches!(req.tier_policy, TierPolicy::Exact { .. }) {
        return Err(NoModelReason::InvalidRequest(format!(
            "pin={} con tier_policy={:?}: il pin cede il PROVIDER, mai la qualita' \
             (I5). Usa ModelRequest::pinned(), che forza Exact{{PinnedProvider}}.",
            req.pin.unwrap_or("?"),
            req.tier_policy
        )));
    }
    Ok(())
}

/// La catena di tier che la policy implica. Pura, testabile: e' il cuore di I3.
fn chain_for<'a>(req: &ModelRequest<'a>) -> Vec<&'a str> {
    match req.tier_policy {
        // Il punto unico della degradazione (regola L): non si costruisce una
        // catena a mano qui.
        TierPolicy::Degrade => agentic_tier_chain(req.tier),
        TierPolicy::Exact { .. } => vec![req.tier],
        // Catena vuota = nessun filtro tier: decide l'ordinamento.
        TierPolicy::AnyTier => vec![],
    }
}

/// I filtri che il PROFILO implica. PURA: il gate arriva come parametro, cosi'
/// l'invariante I2 e' verificabile senza dipendere dalla cache statica di
/// `qualification_gate` (60s, in-process e condivisa fra i test: renderebbe i
/// test dipendenti dall'ordine di esecuzione — regola F).
///
/// L'iniezione NON indebolisce I2: l'unico ingresso pubblico ([`select_model`])
/// legge il gate dal DB e lo passa qui. Nessun call site puo' scegliere di
/// saltarlo, perche' non gli viene chiesto.
fn filter_for<'a>(
    req: &'a ModelRequest<'a>,
    gate: super::QualificationGate,
) -> EligibilityFilter<'a> {
    let agentic = req.profile == Profile::Agentic;
    // Il profilo NON-agentico non e' coperto dal gate (che misura l'uso
    // agentico): scelta dichiarata, non una dimenticanza.
    let gate = if agentic {
        gate
    } else {
        super::QualificationGate::default()
    };
    EligibilityFilter {
        require_tool_use: agentic,
        require_thinking_non_exclude: agentic,
        capability: req.capability,
        min_context_window: req.min_context_window,
        min_tier: req.min_tier,
        exclude_providers: req.exclude_providers,
        apply_cooldown: true,
        only_provider: req.pin,
        require_qualified: gate.require_qualified,
        exclude_preview: gate.exclude_preview,
    }
}

/// Compone l'esito e CALCOLA `degraded` (I4). Pura.
fn choice_from(
    req: &ModelRequest<'_>,
    provider: String,
    model: String,
    effective_tier: Option<String>,
) -> ModelChoice {
    let degraded = match req.tier_policy {
        // AnyTier non ha un tier richiesto da confrontare: non e' degradazione.
        TierPolicy::AnyTier => false,
        _ => effective_tier
            .as_deref()
            .is_some_and(|t| !t.eq_ignore_ascii_case(req.tier.trim())),
    };
    let rationale = match (&req.tier_policy, degraded) {
        (TierPolicy::AnyTier, _) => "tier=any".to_string(),
        (_, true) => format!(
            "tier={}:degraded_to={}",
            req.tier,
            effective_tier.as_deref().unwrap_or("?")
        ),
        (_, false) => format!("tier={}:auto", req.tier),
    };
    ModelChoice {
        provider,
        model,
        requested_tier: req.tier.to_string(),
        effective_tier,
        degraded,
        rationale,
    }
}

/// Distingue "il parco e' fermo" da "il gate ha svuotato il pool" (I6): la
/// stessa query SENZA gate. Se col gate spento un candidato c'era, la causa e' il
/// gate — informazione che oggi si perde in un `tracing::warn!`.
async fn diagnose_empty(
    db: &PgPool,
    req: &ModelRequest<'_>,
    chain: &[&str],
    gate: super::QualificationGate,
) -> NoModelReason {
    if req.profile == Profile::Agentic && (gate.require_qualified || gate.exclude_preview) {
        let mut senza_gate = filter_for(req, gate);
        senza_gate.require_qualified = false;
        senza_gate.exclude_preview = false;
        if let Ok(v) = select_models_tierchain(db, &senza_gate, chain, &req.rank.to_sql(), 1).await {
            if !v.is_empty() {
                return NoModelReason::GateEmpty {
                    requested_tier: req.tier.to_string(),
                };
            }
        }
    }
    match req.tier_policy {
        TierPolicy::Exact { why } => NoModelReason::ExactTierEmpty {
            requested_tier: req.tier.to_string(),
            why,
        },
        _ => NoModelReason::ChainExhausted {
            requested_tier: req.tier.to_string(),
        },
    }
}

/// IL PUNTO DI INGRESSO. Sceglie UN modello, o dice PERCHE' non c'e'.
///
/// I2: il gate di qualificazione si legge QUI dal DB, una volta, per tutti. Un
/// call site non puo' saltarlo perche' non gli viene chiesto di fornirlo.
pub async fn select_model(
    db: &PgPool,
    req: &ModelRequest<'_>,
) -> Result<ModelChoice, NoModelReason> {
    let gate = qualification_gate(db).await;
    if req.governed && crate::governance_telemetry::governance_enabled(db).await {
        return select_model_governed(db, req, gate).await;
    }
    select_model_with_gate(db, req, gate).await
}

/// Quanti candidati recuperare per il riordino telemetria-aware. Un pool piccolo
/// del PRIMO tier con candidati: il riordino promuove/retrocede fra alternative
/// GIA' ammissibili, non allarga la selezione ad altri tier (la degradazione
/// resta di `TierPolicy`).
const GOVERNED_CANDIDATE_POOL: i64 = 8;

/// [`select_model`] col riordino telemetria-aware (ADR 0030). Stessa
/// eleggibilita' e stessa tier-chain: cambia solo QUALE dei candidati ammissibili
/// vince, in base alla telemetria strutturata (regola M) invece che al solo
/// `rank`. Il riordino e' STABILE: con telemetria uniforme il top-1 resta quello
/// del `rank`, quindi a flag OFF il comportamento e' bit-identico.
async fn select_model_governed(
    db: &PgPool,
    req: &ModelRequest<'_>,
    gate: super::QualificationGate,
) -> Result<ModelChoice, NoModelReason> {
    validate(req)?;
    let chain = chain_for(req);
    let filter = filter_for(req, gate);
    let rows =
        select_models_tierchain(db, &filter, &chain, &req.rank.to_sql(), GOVERNED_CANDIDATE_POOL)
            .await
            .map_err(NoModelReason::CatalogUnavailable)?;
    if rows.is_empty() {
        return Err(diagnose_empty(db, req, &chain, gate).await);
    }
    // 0/1 candidati: nulla da riordinare (evita I/O telemetria inutile).
    if rows.len() < 2 {
        let (p, m, t) = rows.into_iter().next().expect("len >= 1");
        return Ok(choice_from(req, p, m, t));
    }
    // Il tier effettivo viaggia con la riga: lo si ritrova dopo il riordino, che
    // lavora su (provider, model). Senza questa mappa `degraded` andrebbe
    // ri-dedotto — cioe' esattamente cio' che I4 vieta.
    let tier_by_pair: std::collections::HashMap<(String, String), Option<String>> = rows
        .iter()
        .map(|(p, m, t)| ((p.clone(), m.clone()), t.clone()))
        .collect();
    let candidates: Vec<(String, String)> =
        rows.into_iter().map(|(p, m, _)| (p, m)).collect();
    let telemetry = crate::governance_telemetry::load_model_telemetry(db, &candidates).await;
    let policy = crate::governance_telemetry::load_governance_policy(db).await;
    let ranked = nexus_agent_graph::decisions::governance::rank_candidates(
        &candidates,
        &telemetry,
        &[],
        &policy,
    );
    let (provider, model) = ranked
        .into_iter()
        .next()
        .ok_or_else(|| NoModelReason::ChainExhausted {
            requested_tier: req.tier.to_string(),
        })?;
    let effective_tier = tier_by_pair
        .get(&(provider.clone(), model.clone()))
        .cloned()
        .flatten();
    Ok(choice_from(req, provider, model, effective_tier))
}

/// Come [`select_model`] ma ritorna fino a `limit` candidati, dello STESSO tier
/// (il corto-circuito della tier-chain sceglie il primo tier con candidati:
/// i risultati restano omogenei di fascia, mai un misto heavy+light).
///
/// Serve ai fan-out multi-provider: il consiglio chiede N provider DISTINTI, e
/// deduplica a valle. E' il caso che ha PIU' bisogno della degradazione, non
/// meno: chiede piu' provider proprio nel tier che ne ha di meno.
pub async fn select_models(
    db: &PgPool,
    req: &ModelRequest<'_>,
    limit: i64,
) -> Result<Vec<ModelChoice>, NoModelReason> {
    let gate = qualification_gate(db).await;
    validate(req)?;
    let chain = chain_for(req);
    let filter = filter_for(req, gate);
    let rows = select_models_tierchain(db, &filter, &chain, &req.rank.to_sql(), limit)
        .await
        .map_err(NoModelReason::CatalogUnavailable)?;
    if rows.is_empty() {
        return Err(diagnose_empty(db, req, &chain, gate).await);
    }
    Ok(rows
        .into_iter()
        .map(|(p, m, t)| choice_from(req, p, m, t))
        .collect())
}

/// Come [`select_model`] ma col gate ESPLICITO. Privata: esiste perche' la cache
/// del gate (60s, statica e in-process) renderebbe i test dipendenti dall'ordine
/// di esecuzione. Non e' una porta di servizio per i call site — non e' `pub`.
async fn select_model_with_gate(
    db: &PgPool,
    req: &ModelRequest<'_>,
    gate: super::QualificationGate,
) -> Result<ModelChoice, NoModelReason> {
    validate(req)?;
    let chain = chain_for(req);
    let filter = filter_for(req, gate);
    let rows = select_models_tierchain(db, &filter, &chain, &req.rank.to_sql(), 1)
        .await
        .map_err(NoModelReason::CatalogUnavailable)?;
    let Some((provider, model, effective_tier)) = rows.into_iter().next() else {
        return Err(diagnose_empty(db, req, &chain, gate).await);
    };
    let choice = choice_from(req, provider, model, effective_tier);
    // I3: la degradazione e' MONOTONA. Non e' un commento: e' un controllo.
    debug_assert!(
        !choice.degraded || matches!(req.tier_policy, TierPolicy::Degrade),
        "I3 violata: degradazione con tier_policy={:?}",
        req.tier_policy
    );
    debug_assert!(
        choice
            .effective_tier
            .as_deref()
            .is_none_or(|t| tier_rank(t) <= tier_rank(req.tier))
            || req.tier_policy == TierPolicy::AnyTier,
        "I3 violata: il tier effettivo ({:?}) e' PIU' capace del richiesto ({})",
        choice.effective_tier,
        req.tier
    );
    if choice.degraded {
        tracing::warn!(
            tier_richiesto = %choice.requested_tier,
            tier_effettivo = %choice.effective_tier.as_deref().unwrap_or("?"),
            provider = %choice.provider, model = %choice.model,
            "model_service: DEGRADAZIONE di tier — nessun modello eleggibile nel \
             tier richiesto (cooldown o gate), scelto il migliore del primo tier \
             disponibile scendendo"
        );
    }
    Ok(choice)
}

#[cfg(test)]
mod tests;
