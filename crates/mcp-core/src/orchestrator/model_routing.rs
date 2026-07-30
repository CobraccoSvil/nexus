//! Routing modello: stima complessita', selezione dal catalog,
//! soglie token, RoutingConfig e default per provider.

use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;

use crate::nexus_gateway::{intent_to_alias, GwMessage, GwMetadata, GwRequest};

use super::*;

#[derive(Debug)]
pub(crate) struct RoutingDecision {
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// Soglie token per intent_key (route_model_with_mode). Letti da
/// `settings.routing.token_threshold_*` via `RoutingThresholds`.
/// Usato come "view" minimale per non passare l'intera struct.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TokenThresholds {
    pub(crate) chat_breve: u32,
    pub(crate) chat_media: u32,
    pub(crate) complex_fix: u32,
}

impl TokenThresholds {
    /// Default = seed mig 0111 (allineato).
    pub(crate) fn defaults() -> Self {
        Self {
            chat_breve: 400,
            chat_media: 1_500,
            complex_fix: 3_000,
        }
    }

    pub(crate) fn from_routing_thresholds(t: &crate::routing_config::RoutingThresholds) -> Self {
        Self {
            chat_breve: t.token_threshold_chat_breve,
            chat_media: t.token_threshold_chat_media,
            complex_fix: t.token_threshold_complex_fix,
        }
    }
}

/// Risultato dettagliato di [`Orchestrator::resolve_agent_provider_detailed`].
/// Esposto come JSON tramite l'endpoint internal di routing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RoutingResolveResult {
    pub provider: String,
    pub model: String,
    pub intent: String,
    pub mode: String,
    pub risky: bool,
    pub rationale: String,
    /// Fonte della decisione di model. Permette al chiamante (brain Python,
    /// observability, dashboard admin) di capire come e' stato scelto:
    ///   - "matrix"   = decisa dalla `route_model_with_mode` (matrix statica)
    ///   - "catalog"  = decisa dal catalogo prezzi `ai_price_catalog`
    ///                  (modalita' dinamica, ottimizzazione costo/capability)
    ///   - "override" = forzata da `provider_override` utente
    ///   - "cooldown_fallback" = matrix scelta era anthropic ma in cooldown,
    ///                            sostituita con prossimo capable
    pub source: String,
    /// Behavior mode reale a livello DB (puo' differire da `mode` esposto
    /// se il routing applica un override risky o se il dinamico viene
    /// degradato a bilanciata sui task rischiosi).
    pub configured_behavior_mode: String,
    /// True se TUTTI i provider della hierarchy sono in cooldown E il
    /// `provider`/`model` ritornato e' un'ultima istanza non garantita.
    /// Il chiamante DEVE fermarsi e avvertire l'utente: nessuno dei
    /// provider configurati e' al momento utilizzabile (quote esaurite,
    /// rate limit, billing). Continuare comunque produrrebbe lo stesso
    /// errore in loop.
    pub no_capable_provider: bool,
    /// True se la decisione deriva da una FORZATURA esplicita dell'utente
    /// (`provider_override` / `model_override` dal dropdown chat, non Auto).
    /// In questo caso il cooldown billing/quota e' deliberatamente IGNORATO:
    /// l'utente ha scelto consapevolmente quel provider/modello e va usato
    /// anche se in cooldown (ADR 0020). Il chiamante puo' mostrare un avviso
    /// "stai forzando un provider in cooldown" ma NON deve bloccare il run.
    /// Quando true, `no_capable_provider` e' sempre false (la scelta utente
    /// e' per definizione "capable" dal suo punto di vista).
    pub user_override: bool,
    /// Lista provider in cooldown al momento della decisione, ordinata
    /// come la hierarchy. Permette al frontend di mostrare un alert
    /// dettagliato ("anthropic e openai sono in cooldown — solo deepseek
    /// e' disponibile").
    pub providers_in_cooldown: Vec<String>,
    /// Se valorizzato, il routing NON ha potuto decidere perche' la matrice
    /// DB e' irraggiungibile o non popolata. Il chiamante DEVE fermarsi
    /// (HTTP 503 Service Unavailable) e mostrare il messaggio all'admin.
    /// Niente fallback hardcoded: e' un errore di configurazione, non
    /// un caso da nascondere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Regola di disponibilita' provider (ADR 0020), estratta come funzione pura
/// per renderla testabile senza DB.
///
/// - Modalita' AUTO (nessuna forzatura): il cooldown e' VINCOLANTE. Se il
///   provider scelto e' in cooldown, o tutti i provider noti lo sono, la
///   decisione e' `no_capable_provider = true` (il chiamante deve fermarsi e
///   avvertire, non ritentare un provider morto).
/// - Forzatura ESPLICITA utente (`user_override`): la scelta e' consapevole e
///   non e' mai "no_capable" — il provider va usato anche se in cooldown.
pub(crate) fn compute_no_capable_provider(
    user_override: bool,
    chosen_provider_in_cooldown: bool,
    all_known_in_cooldown: bool,
) -> bool {
    !user_override && (chosen_provider_in_cooldown || all_known_in_cooldown)
}

#[derive(Debug, Clone)]
pub(crate) struct DynamicRoutingDecision {
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// Stima la complessità reale del messaggio ignorando liste/elenchi ripetitivi
/// (es. quality findings con decine di righe identiche).
/// Ritorna il numero di token "significativi" (prime 300 parole uniche).
pub(crate) fn estimate_complexity(message: &str) -> u32 {
    // Conta solo le prime 200 parole per la complessità dell'intent — il resto sono dati
    let core_words = message.split_whitespace().take(200).count() as u32;
    core_words.saturating_mul(2).max(50)
}

/// Sceglie provider e modello in base all'intent, complessità e behavior_mode.
// route_model_local rimossa: era dead_code dopo l'introduzione di route_model_from_catalog
// (refactor 0101 model-registry). Tutti i call site usano route_model_with_mode con la
// matrice DB passata esplicitamente.

/// Ricava il provider di un modello dal catalogo prezzi (ADR 0023).
/// Usato quando l'utente forza un `model_override` senza `provider_override`:
/// un modello identifica univocamente il suo provider.
/// Deterministico: se piu' provider espongono lo stesso `model`, prende quello
/// abilitato col costo input piu' basso (NULL per ultimo). Ritorna `None` se il
/// modello non e' nel catalogo (il chiamante fa fallback al routing per intent,
/// senza inventare provider — regola G).
pub(crate) async fn provider_for_model(db: &PgPool, model: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT provider FROM ai_price_catalog
         WHERE model = $1 AND is_enabled = true
         ORDER BY input_cost_per_million_tokens ASC NULLS LAST
         LIMIT 1",
    )
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Chiave settings (regola G) del PAVIMENTO di tier per i turni AGENTICI.
const AGENTIC_MIN_TIER_KEY: &str = "agent.routing.agentic_min_tier";

/// Default del pavimento agentico se il setting e' assente. NON e' un nome
/// modello (regola G non si applica): e' una soglia di policy locale, come
/// `agent.enforce_port_allocation`='true' o `agent.g1_max_nudges`. Resta
/// configurabile da DB; questo e' solo il valore quando la riga manca.
const AGENTIC_MIN_TIER_DEFAULT: &str = "medium";

/// Legge il pavimento di tier agentico dal DB (punto unico `get_setting`,
/// cache 60s di nexus-auth). Valori validi: la scala a 5 livelli del catalog
/// light/medium/high/heavy/frontier (mig 0528/0547; performance_tier di
/// ai_price_catalog). Qualunque valore non riconosciuto o assenza del setting ->
/// [`AGENTIC_MIN_TIER_DEFAULT`]. Best-effort: un errore DB non fa fallire il
/// routing, degrada al default.
pub(crate) async fn agentic_min_tier(db: &PgPool) -> String {
    let raw = crate::settings::get_setting(db, AGENTIC_MIN_TIER_KEY)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_lowercase());
    match raw.as_deref() {
        // Validazione delegata al PUNTO UNICO del vocabolario tier (regola L):
        // niente lista di tier duplicata qui. Un valore fuori scala -> default.
        Some(t) if nexus_agent_graph::decisions::tiers::is_performance_tier(t) => raw.unwrap(),
        _ => AGENTIC_MIN_TIER_DEFAULT.to_string(),
    }
}

/// Alza `required_tier` ad almeno `floor` quando il turno e' AGENTICO, usando
/// l'ordinamento tier di [`crate::routing_matrix_auto_promoter::tier_rank`], a sua
/// volta wrapper del PUNTO UNICO del vocabolario
/// ([`nexus_agent_graph::decisions::tiers`]): light < medium < high < heavy <
/// frontier (5 livelli, mig 0528/0547). Funzione PURA (testabile senza DB). Per i
/// turni NON
/// agentici e' un no-op (ritorna `required_tier` invariato): la chat semplice
/// resta libera di usare 'light'. Se `required_tier` e' gia' >= `floor` non lo
/// abbassa MAI (es. un task heavy resta heavy anche con pavimento 'medium').
fn floor_tier_for_agentic<'a>(
    is_agentic_turn: bool,
    required_tier: &'a str,
    floor: &'a str,
) -> &'a str {
    if !is_agentic_turn {
        return required_tier;
    }
    use crate::routing_matrix_auto_promoter::tier_rank;
    if tier_rank(floor) > tier_rank(required_tier) {
        floor
    } else {
        required_tier
    }
}

/// Seleziona il modello ottimale dal catalogo DB per la modalità richiesta.
/// La modalità "dinamico" sceglie il modello più adatto per capability+tier,
/// privilegiando il costo più basso a parità di tier richiesto.
///
/// `is_agentic_turn` (regola L: deciso dal chiamante che conosce l'intent;
/// convenzione del progetto `intent != "chat"`) attiva il PAVIMENTO di tier
/// agentico: per i turni multi-step a tool il tier minimo viene alzato ad
/// `agent.routing.agentic_min_tier` (default 'medium') PRIMA di costruire la
/// tier-chain, cosi' il routing non parte da un modello LIGHT debole che
/// diverge. La degradazione resta GRACEFUL: la tier-chain scende comunque verso
/// il basso (es. heavy->medium->light) se nessun candidato del tier minimo e'
/// disponibile (tutti in cooldown), senza mai fallire.
/// `ORDER BY` della selezione agentica dal catalog: COSTO prima, `is_featured`
/// come solo tie-break. PUNTO UNICO (regola L). Razionale (obiettivo primario di
/// Nexus: ottimizzare i costi restando svincolati dal provider): il `tier` gia'
/// garantisce la FASCIA di capacita' richiesta; DENTRO il tier si prende il
/// modello piu' ECONOMICO, non quello marcato `is_featured` a mano — un flag
/// curato senza semantica di costo che faceva pagare fino a ~7x (es. codestral
/// 1.0 $/M featured al posto di deepseek-v4-flash 0.14 sul tier medium). I
/// modelli economici problematici NON restano scelti: la governance
/// telemetria-aware (regola M) e gli esiti dei run li retrocedono per salute, che
/// e' il criterio corretto (un flag statico non lo era).
pub(crate) const AGENTIC_COST_FIRST_ORDER: &str =
    "input_cost_per_million_tokens ASC, is_featured DESC";

/// Ordinamento failover cross-provider: preferisce modelli NON-thinking per evitare
/// vincoli di round-trip `reasoning_content` (DeepSeek) su switch mid-run.
pub(crate) const AGENTIC_FAILOVER_ORDER: &str =
    "(uses_thinking_mode) ASC, input_cost_per_million_tokens ASC, is_featured DESC";

pub(crate) async fn route_model_from_catalog(
    db: &PgPool,
    base_tier: &str,
    capability: &str,
    mode: &str,
    is_agentic_turn: bool,
) -> Option<DynamicRoutingDecision> {
    // Promozione/declassamento del tier in base al behavior_mode.
    // "approfondita" scala in alto, "veloce"/"economica" scala in basso.
    // Il `base_tier` arriva gia' risolto dal chiamante via IntentCapabilityMap
    // (mig 0110), che applica le soglie di token threshold per l'intent.
    // Scala a 5 tier (light<medium<high<heavy<frontier): "approfondita" sale di UN
    // gradino, "veloce"/"economica" scende di UN gradino con pavimento a 'medium'
    // (non si va sotto medium per un intent di velocita'/costo).
    let mode_tier = match mode {
        "approfondita" => match base_tier {
            "light" => "medium",
            "medium" => "high",
            "high" => "heavy",
            "heavy" => "frontier",
            other => other,
        },
        "veloce" | "economica" => match base_tier {
            "frontier" => "heavy",
            "heavy" => "high",
            "high" => "medium",
            other => other,
        },
        _ => base_tier,
    };

    // PAVIMENTO AGENTICO (regola L, punto unico della selezione dinamica/catalog):
    // per i turni a tool il tier minimo e' alzato al pavimento DB-driven. La
    // tier-chain sotto degrada comunque verso il basso (graceful), quindi se il
    // tier minimo non ha candidati disponibili il run NON fallisce.
    let floor = agentic_min_tier(db).await;
    let required_tier = floor_tier_for_agentic(is_agentic_turn, mode_tier, &floor);

    // L'ordinamento per behavior_mode. `Rank` e' un enum CHIUSO: prima erano 4
    // stringhe SQL scritte qui, e da un `order_by: &str` libero e' entrata (e
    // sopravvissuta per mesi) la scala tier a 3 livelli di agent_run.rs.
    // "veloce" ordina per speed_tier; "economica"/dinamico per costo (il tier gia'
    // garantisce la capacita', regola costi); "approfondita" preferisce il piu'
    // capace/caro DENTRO il tier gia' promosso.
    let rank = match mode {
        "veloce" => crate::orchestrator::model_service::Rank::Fastest,
        "economica" => crate::orchestrator::model_service::Rank::CostFirst,
        "approfondita" => crate::orchestrator::model_service::Rank::MostCapable,
        _ => crate::orchestrator::model_service::Rank::CostFirst,
    };

    // SERVIZIO UNICO (regola L). `Degrade`: il primo tier con un candidato
    // eleggibile vince; se il tier minimo (incluso il pavimento agentico) e' tutto
    // in cooldown si scende fino a 'light' invece di fallire — e' il path che
    // sopravviveva all'incidente del consiglio, ora e' lo STESSO codice per tutti.
    //
    // `governed`: riordino telemetria-aware (ADR 0030, opt-in dal flag DB; OFF =
    // bit-identico). Vive QUI e non ovunque perche' e' la selezione DINAMICA del
    // turno primario, l'unico posto dove ha senso: il modello scelto viene poi
    // pinnato per il run.
    crate::orchestrator::model_service::select_model(
        db,
        &crate::orchestrator::model_service::ModelRequest::agentic(required_tier)
            .capability(Some(capability))
            .rank(rank)
            .governed(true),
    )
    .await
    .ok()
    .map(|c| DynamicRoutingDecision {
        provider: c.provider,
        model: c.model,
    })
}

/// Catena di degradazione graceful dei tier da `top` fino a `light` sulla scala a
/// 5 livelli (frontier>heavy>high>medium>light). PUNTO UNICO (regola L) della
/// tier-chain agentica, condiviso da `route_model_from_catalog` e
/// `select_models_tierchain`: il primo tier con un candidato sano vince; se il
/// tier alto e' tutto in cooldown si scende, invece di fallire. Un `top` fuori
/// vocabolario degrada da 'medium' (neutro), mai un fallimento silenzioso.
pub(crate) fn agentic_tier_chain(top: &str) -> Vec<&'static str> {
    match top.trim() {
        "frontier" => vec!["frontier", "heavy", "high", "medium", "light"],
        "heavy" => vec!["heavy", "high", "medium", "light"],
        "high" => vec!["high", "medium", "light"],
        "medium" => vec!["medium", "light"],
        "light" => vec!["light", "medium"],
        _ => vec!["medium", "light"],
    }
}

/// Backstop anti-runaway del pool di candidati del failover agentico: NON e' un
/// dimensionamento. Il pool deve contenere TUTTI i modelli agentici ammissibili
/// (la scelta spetta al modulo puro, non a un taglio SQL): l'ordine della query
/// e' featured+economico, quindi un limite vicino alla dimensione reale del
/// catalog taglierebbe proprio i tier alti/costosi (il catalog reale ha ~112
/// eleggibili: un limite 64 escludeva tutti i frontier/heavy, reintroducendo il
/// pavimento). 512 sta ben sopra ogni catalog realistico; la saturazione viene
/// loggata WARN (mai un cap silenzioso).
const FAILOVER_CANDIDATE_POOL: i64 = 512;

/// Pool di candidati del FAILOVER agentico cross-provider (regola L, punto unico):
/// TUTTI i modelli agentici ammissibili — OGNI tier, esclusi i provider `exclude`
/// (gia' provati / caduti in questo run) e quelli in cooldown (ADR 0020). Usato
/// dall'auto-escalation runtime quando il gateway segnala che il provider corrente
/// non e' disponibile (500 `PROVIDER_ERROR` / cooldown).
///
/// NESSUN pavimento ne' tier-chain (a differenza del routing iniziale): la SCELTA
/// del sostituto e' AGENTICA e spetta al modulo puro
/// `nexus_agent_graph::decisions::escalation::pick_failover_model` (salute ->
/// likelihood da telemetria, col tier corrente come semplice INDICAZIONE). Qui si
/// enumera solo l'eleggibilita', delegando la WHERE al punto unico
/// [`select_models_tierchain`] (supports_tool_use, `agentic_thinking_policy <>
/// 'exclude'`, provider sani). Ordine: featured + piu' economico — diventa il
/// tie-break d'ingresso della selezione pura. `Vec::new()` SOLO quando nessun
/// candidato sano resta (rete davvero esaurita) o su errore di lettura
/// (fail-open: il chiamante chiude `Error` onestamente).
///
/// FINESTRA-AWARE (regola L, coerenza con `chain_for`/`cross_provider`): `min_context_window`
/// filtra i candidati con finestra strettamente minore (`context_window >= N`). Serve al
/// failover per NON scegliere un sostituto meno capiente del modello caduto: un contesto gia'
/// grande andrebbe in overflow (HTTP 413 Request too large) sul sostituto piccolo, come
/// nell'incidente reale google/1M -> groq/128k -> 413 -> figura n/d. `0` = nessun filtro
/// (comportamento storico). Il chiamante ([`failover_provider`]) fa fail-open ritentando con
/// `0` se il vincolo svuota il pool (failover degradato > nessun failover).
pub(crate) async fn agentic_failover_candidates(
    db: &PgPool,
    exclude: &[String],
    min_context_window: i64,
) -> Vec<(String, String, Option<String>)> {
    // SERVIZIO UNICO (regola L). `AnyTier`: il tier non ORDINA qui — si ENUMERA
    // l'eleggibilita' e la SCELTA spetta al modulo puro `pick_failover_model`
    // (salute -> likelihood). Una tier-chain sottrarrebbe la decisione al modulo.
    //
    // Ma il PAVIMENTO e' un'altra cosa: non e' una preferenza fra alternative, e'
    // la soglia sotto la quale un modello NON E' un'alternativa. Misurato il
    // 16/07: con openai e anthropic senza credito, il failover — che trattava il
    // tier come "semplice indicazione" — e' sceso fino a `groq/gpt-oss-20b`
    // (agentic_index 3.1, il peggiore del parco). Il run non e' fallito: ha
    // prodotto una risposta FUORI TEMA (parlava del modello invece del task) e
    // l'ha dichiarata `completed`. Un esito bugiardo e' peggio di un fallimento,
    // perche' l'utente ci si fida.
    //
    // Prima questa scelta era difendibile: il tier era dedotto dal NOME, quindi
    // inaffidabile come vincolo. Ora il tier viene dai FATTI (mig 0599/0600), e la
    // premessa e' cambiata: il pavimento e' esprimibile e va rispettato.
    //
    // Resta GRACEFUL rispetto all'incidente del 15/07 (il consiglio che moriva):
    // li' il difetto era non scendere da `heavy` a tier SANI (high/medium); qui si
    // toglie solo l'ultimo gradino, quello sotto il pavimento agentico.
    let floor = agentic_min_tier(db).await;
    let req = crate::orchestrator::model_service::ModelRequest::agentic("")
        .tier_policy(crate::orchestrator::model_service::TierPolicy::AnyTier)
        .rank(crate::orchestrator::model_service::Rank::FailoverSafe)
        .min_context_window(min_context_window)
        .min_tier(&floor)
        .exclude(exclude);
    match crate::orchestrator::model_service::select_models(db, &req, FAILOVER_CANDIDATE_POOL, 1)
        .await
        .map(|v| {
            v.into_iter()
                .map(|c| (c.provider, c.model, c.effective_tier))
                .collect::<Vec<_>>()
        })
    {
        Ok(v) => {
            if v.len() as i64 >= FAILOVER_CANDIDATE_POOL {
                // Il backstop e' scattato: con l'ordine featured+economico il
                // taglio esclude i tier alti -> il pool NON e' piu' completo.
                // Segnale rumoroso (niente cap silenziosi): alzare il backstop.
                tracing::warn!(
                    pool = v.len(),
                    backstop = FAILOVER_CANDIDATE_POOL,
                    "agentic_failover_candidates: pool saturo, candidati troncati \
                     (i tier alti possono mancare) - alzare FAILOVER_CANDIDATE_POOL"
                );
            }
            v
        }
        Err(e) => {
            tracing::warn!("agentic_failover_candidates: {e}");
            Vec::new()
        }
    }
}









/// Numero di candidati recuperati per il riordino telemetria-aware della
/// selezione dinamica (governance). Un pool piccolo del PRIMO tier con candidati:
/// il riordino promuove/retrocede tra alternative gia' ammissibili, non allarga la
/// selezione ad altri tier (la degradazione graceful resta di `select_models_tierchain`).
const GOVERNED_CANDIDATE_POOL: i64 = 8;


/// Mapping intent classificato -> intent_key per il lookup nella routing matrix.
///
/// Punto unico (regola L): un solo posto traduce l'intent + il budget token
/// nella chiave usata da `route_model_with_mode`. Gli intent agentici hanno una
/// chiave dedicata; `code_read` (mig 0336) ha la propria per NON cadere nel ramo
/// conversazionale `chat_*` -> modello "lite" incapace di ispezionare i file;
/// ogni altro intent (incluso `chat`) degrada su chat_breve/media/lunga in base
/// alla lunghezza stimata.
pub(crate) fn intent_key_for(
    intent: &str,
    estimated_tokens: u32,
    token_thresholds: &TokenThresholds,
) -> &'static str {
    match intent {
        "debug" => "debug",
        "architecture" => "architecture",
        "refactor" => "refactor",
        "fix" => {
            if estimated_tokens > token_thresholds.complex_fix {
                "fix_complesso"
            } else {
                "fix_semplice"
            }
        }
        "test" => "test",
        "docs" => "docs",
        "file_ops" => "file_ops",
        "system_admin" => "system_admin",
        // code_read: ispezione/lettura read-only del progetto. Intent_key
        // dedicato (mig 0336) con modelli tool-robust, invece di cadere nel
        // default chat_* -> modello "lite" che non sa ispezionare i file e
        // risponde in astratto. Niente soglia token: la lettura non scala con
        // l'output ma col numero di tool call (gestite dall'iter budget).
        "code_read" => "code_read",
        // Ricerca web citata (Perplexity): intent_key dedicato -> riga
        // nexus_routing_matrix ricerca_web (fallback; il primario e' il ramo
        // non-agentico best_non_agentic_model con capability web_search).
        "ricerca_web" => "ricerca_web",
        // agentic_default: fallback neutro quando il classifier LLM non risponde.
        // Intent_key dedicato (mig 0337) con modelli tool-robust, cosi' l'agente
        // parte col _LAZY_MINIMAL_TOOLKIT e interpreta da se' invece di finire su
        // un modello "lite" conversazionale.
        "agentic_default" => "agentic_default",
        _ => {
            if estimated_tokens <= token_thresholds.chat_breve {
                "chat_breve"
            } else if estimated_tokens <= token_thresholds.chat_media {
                "chat_media"
            } else {
                "chat_lunga"
            }
        }
    }
}

/// Route (intent, behavior_mode) -> (provider, model) consultando la matrice DB
/// (cache 60s in-memory). Sostituisce la matrice hardcoded che era qui prima
/// del refactor 0101 (vedi `crates/mcp-core/src/routing_matrix.rs`).
///
/// Se la matrice non ha entry per (intent, mode), fallback in cascata:
/// 1. Prova lo stesso intent con mode 'bilanciata'
/// 2. Prova default per provider 'anthropic' (tool use solido)
/// 3. Ultima istanza: default per provider 'openai'
pub(crate) fn route_model_with_mode(
    matrix: &crate::routing_matrix::RoutingMatrix,
    intent: &str,
    estimated_tokens: u32,
    mode: &str,
    preferred_provider_for_intent: Option<&str>,
    token_thresholds: &TokenThresholds,
) -> RoutingDecision {
    // Determina intent_key composta usando le soglie da settings.routing.*
    // (mig 0111). Punto unico del mapping: vedi `intent_key_for`.
    let intent_key = intent_key_for(intent, estimated_tokens, token_thresholds);

    // Routing matrix: (intent_key, mode) → (provider, model)
    // Budget-aware lookup: usa `lookup_with_budget` che applica le regole
    // escalation (mig 0120) quando `estimated_tokens >= threshold`. Cosi'
    // task lunghi/complessi prendono automaticamente il modello escalation
    // (es. google: 2.5-pro -> 3.1-pro-preview-customtools sopra soglia).
    // Senza questo (bug 30/05/2026) i campi escalation_* del DB erano popolati
    // ma mai usati — il routing prendeva sempre il modello base.
    let est_i32: i32 = estimated_tokens.try_into().unwrap_or(i32::MAX);

    // Helper: skip provider in cooldown — chiamarli produrrebbe billing/rate-limit
    // error che farebbe fallire l'intera richiesta utente.
    let in_cooldown = |p: &str| crate::provider_cooldown::is_provider_in_cooldown(p);

    // 1. Lookup diretto (intent_key, mode) nella matrice DB con escalation
    if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, mode, est_i32) {
        if !in_cooldown(&provider) {
            // Branch: routing_matrix DB.
            return RoutingDecision { provider, model };
        }
        tracing::warn!(
            "route_model_with_mode: skip provider {} (in cooldown)",
            provider
        );
    }

    // 2. Fallback: prova lo stesso intent con mode 'bilanciata' (budget-aware)
    if mode != "bilanciata" {
        if let Some((provider, model)) =
            matrix.lookup_with_budget(intent_key, "bilanciata", est_i32)
        {
            if !in_cooldown(&provider) {
                // Branch: routing_matrix DB (mode fallback bilanciata).
                return RoutingDecision { provider, model };
            }
            tracing::warn!(
                "route_model_with_mode: skip provider {} su fallback bilanciata (in cooldown)",
                provider
            );
        }
    }

    // 2b. Fallback: cerca QUALSIASI mode per lo stesso intent_key con un provider non in cooldown
    for try_mode in &["bilanciata", "approfondita", "veloce", "economica"] {
        if let Some((provider, model)) = matrix.lookup_with_budget(intent_key, try_mode, est_i32) {
            if !in_cooldown(&provider) {
                // Branch: routing_matrix DB (cooldown bypass: any mode).
                return RoutingDecision { provider, model };
            }
        }
    }

    // 3. Fallback: usa il preferred_provider per l'intent passato dal caller
    // (letto dalla cache nexus_intent_capability, mig 0110). Se non specificato
    // o se il provider non ha default model in matrix.default_model, ritorna
    // una sentinella `__no_model__` che il chiamante a monte traduce in
    // RoutingResolveResult { no_capable_provider: true } → HTTP 503.
    if let Some(provider) = preferred_provider_for_intent {
        if let Some(model) = matrix.default_model(provider) {
            // Branch: routing_matrix default per preferred_provider intent.
            return RoutingDecision {
                provider: provider.to_string(),
                model,
            };
        }
    }

    // 4. Nessun match possibile. Niente fallback hardcoded (regola G CLAUDE.md):
    // il chiamante DEVE intercettare questa sentinella e propagare HTTP 503.
    tracing::error!(
        "route_model_with_mode: nessun match per (intent={}, mode={}) e preferred_provider mancante o non in matrix.default_models. \
         Verifica nexus_routing_matrix e nexus_intent_capability.",
        intent_key, mode
    );
    // Branch: no model available — verifica routing matrix + intent_capability.
    RoutingDecision {
        provider: "__no_model__".to_string(),
        model: "__no_model__".to_string(),
    }
}

/// True se la decisione della matrice statica NON e' direttamente servibile e
/// serve il fallback tier-aware dal catalog: o non c'e' stato alcun match
/// (sentinella `__no_model__`), o il provider scelto e' in cooldown.
///
/// Punto unico (regola L) del TRIGGER del fallback catalog. Prima il ramo
/// `__no_model__` in `resolve_agent_provider` scavalcava il catalog e cadeva su
/// un default per-provider "tier-blind" (il primo provider sano della hierarchy,
/// tipicamente google, col suo `default_model` generico `gemini-2.5-flash`, un
/// modello LIGHT che non converge sui task di coding heavy). Ora entrambi i casi
/// (`__no_model__` e cooldown) passano dallo STESSO punto unico
/// `select_agentic_model`, che rispetta tier + capability dell'intent prima di
/// qualsiasi default per-provider.
pub(crate) fn needs_catalog_fallback(decision_provider: &str) -> bool {
    decision_provider == "__no_model__"
        || crate::provider_cooldown::is_provider_in_cooldown(decision_provider)
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct SettingValueRow {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RoutingConfig {
    pub(crate) provider_hierarchy: Vec<String>,
    pub(crate) default_provider: Option<String>,
    pub(crate) token_budget: u32,
    pub(crate) max_token_budget: u32,
    pub(crate) intent_provider_hierarchy: HashMap<String, Vec<String>>,
    pub(crate) behavior_mode: String,
}

impl RoutingConfig {
    pub(crate) fn from_settings(settings: &[SettingValueRow]) -> Self {
        let mut values = HashMap::new();
        for setting in settings {
            values.insert(setting.key.as_str(), setting.value.trim().to_string());
        }

        let provider_hierarchy = [
            "provider_hierarchy",
            "provider_priority",
            "provider_order",
            "fallback_order",
        ]
        .iter()
        .find_map(|key| parse_provider_list(values.get(key).map(String::as_str)))
        .unwrap_or_else(|| {
            parse_provider_list(values.get("default_provider").map(String::as_str)).unwrap_or_else(
                || {
                    KNOWN_PROVIDERS
                        .iter()
                        .map(|provider| (*provider).to_string())
                        .collect()
                },
            )
        });

        let default_provider = values
            .get("default_provider")
            .map(|value| value.to_lowercase())
            .filter(|value| !value.is_empty());

        let token_budget = values
            .get("token_budget")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(4096);
        let max_token_budget = values
            .get("max_token_budget")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(token_budget.max(4096));

        // NB (regola G): i settings `provider_model_<provider>` / `<provider>_model`
        // e `default_model` NON vengono piu' letti. La fonte UNICA del default-per-
        // provider e' `nexus_provider_default_model` (DB, mig 0101) via
        // `default_model_for_provider`. I vecchi settings erano una seconda fonte
        // hardcoded e stale (es. provider_model_google=flash mentre il default DB e'
        // gemini-2.5-pro). Restano nel DB come dati orfani innocui (cleanup = follow-up).

        let mut intent_provider_hierarchy = HashMap::new();
        for intent in KNOWN_INTENTS {
            let keys = [
                format!("routing_{intent}_providers"),
                format!("{intent}_provider_hierarchy"),
                format!("{intent}_providers"),
            ];
            if let Some(providers) = keys
                .iter()
                .find_map(|key| parse_provider_list(values.get(key.as_str()).map(String::as_str)))
            {
                intent_provider_hierarchy.insert(intent.to_string(), providers);
            }
        }

        let behavior_mode = values
            .get("nexus_behavior_mode")
            .cloned()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "bilanciata".to_string());

        Self {
            provider_hierarchy,
            default_provider,
            token_budget,
            max_token_budget,
            intent_provider_hierarchy,
            behavior_mode,
        }
    }

    pub(crate) fn resolve_token_budget(&self, suggested_budget: Option<u32>) -> u32 {
        let requested = suggested_budget.unwrap_or(self.token_budget).max(1);
        requested.min(self.max_token_budget.max(1))
    }

    pub(crate) fn candidates(&self, intent: &str, suggested_provider: Option<&str>) -> Vec<String> {
        let mut providers = Vec::new();

        if let Some(intent_chain) = self.intent_provider_hierarchy.get(intent) {
            for provider in intent_chain {
                push_unique(&mut providers, provider.clone());
            }
        }

        for provider in &self.provider_hierarchy {
            push_unique(&mut providers, provider.clone());
        }

        if let Some(provider) = self.default_provider.as_ref() {
            push_unique(&mut providers, provider.clone());
        }

        if let Some(provider) = suggested_provider
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
        {
            push_unique(&mut providers, provider.to_lowercase());
        }

        for provider in KNOWN_PROVIDERS {
            push_unique(&mut providers, provider.to_string());
        }

        providers
    }

    pub(crate) fn resolve_model(
        &self,
        matrix: &crate::routing_matrix::RoutingMatrix,
        provider: &str,
        suggested_provider: Option<&str>,
        suggested_model: Option<&str>,
    ) -> String {
        // Override esplicito del chiamante per il provider scelto.
        if suggested_provider == Some(provider) {
            if let Some(model) = suggested_model.filter(|value| !value.is_empty()) {
                return model.to_string();
            }
        }

        // Default-per-provider: UNICA fonte = nexus_provider_default_model (DB,
        // mig 0101) via default_model_for_provider (regola G). Rimossi i branch
        // su provider_models / default_model (settings hardcoded), seconda fonte
        // stale e ridondante.
        default_model_for_provider(matrix, provider)
    }
}

pub(crate) fn parse_provider_list(value: Option<&str>) -> Option<Vec<String>> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }

    let parsed = if raw.starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw).ok()?
    } else {
        raw.split(',')
            .map(|provider| provider.trim().to_lowercase())
            .filter(|provider| !provider.is_empty())
            .collect::<Vec<_>>()
    };

    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

pub(crate) fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.iter().any(|value| value == &candidate) {
        values.push(candidate);
    }
}

/// Modello di default per un provider, letto dalla matrice DB
/// (`nexus_provider_default_model`, migrazione 0101) tramite la
/// `RoutingMatrixCache` (regola G: unica fonte nel DB, NIENTE env var ne'
/// fallback hardcoded). A DB irraggiungibile la cache mantiene l'ultima
/// matrice valida; se nessuna matrice e' disponibile gli handler ritornano
/// 503 (vedi `RoutingMatrixCache`).
///
/// Se il provider non e' presente nella matrice, ritorna il sentinel
/// `unknown-provider-<provider>` che triggera errore 400 dal layer chiamante:
/// NON c'e' alcun fallback a un modello hardcoded. (NB: `fallback_safe()`
/// esiste solo sotto `#[cfg(test)]`, non e' un meccanismo di produzione, e non
/// esistono env var `NEXUS_FALLBACK_*`.)
pub fn default_model_for_provider(
    matrix: &crate::routing_matrix::RoutingMatrix,
    provider: &str,
) -> String {
    matrix
        .default_model(provider)
        .unwrap_or_else(|| {
            tracing::warn!(
                "default_model_for_provider: provider '{}' non presente nella matrice DB ne' nei 5 standard. \
                 Aggiungilo via UPDATE/INSERT su nexus_provider_default_model.",
                provider
            );
            format!("unknown-provider-{}", provider)
        })
}

/// Cosa una richiesta di chat sta per chiedere al gateway: la richiesta gia'
/// costruita e la coppia (provider, modello) su cui il ledger la prenota.
///
/// I due pezzi nascono INSIEME, da una sola risoluzione del modello. Prima
/// vivevano in due calcoli distinti dentro `execute_via_gateway` e divergevano:
/// la prenotazione portava la coppia `(deepseek, gemini-3.1-flash-lite)`,
/// misurata nel ledger il 27/07/2026 — un modello che appartiene a google
/// prenotato su deepseek.
pub(crate) struct ChatGatewayCall {
    pub(crate) request: GwRequest,
    /// Provider su cui si prenota il credito. Col pin e' quello che eseguira'
    /// davvero; senza pin e' la STIMA (il gateway instrada per policy).
    pub(crate) ledger_provider: String,
    /// Modello su cui si prenota. Col pin coincide con `request.model`; senza
    /// pin `request.model` e' un alias logico che il gateway risolve, e questo
    /// e' il modello concreto atteso per `ledger_provider`.
    pub(crate) ledger_model: String,
}

/// Gli ingressi di [`build_chat_gateway_call`]. Struct e non 12 parametri: la
/// funzione e' il punto in cui provider, modello e pin devono restare coerenti,
/// e una lista posizionale lunga e' il modo piu' facile per scambiarne due.
pub(crate) struct ChatCallSpec<'a> {
    pub(crate) routing: &'a RoutingConfig,
    pub(crate) matrix: &'a crate::routing_matrix::RoutingMatrix,
    pub(crate) intent: &'a str,
    /// La scelta di provider dell'utente e QUANTO vincola
    /// ([`ProviderChoice`], punto unico): col pin duro la richiesta viaggia
    /// pinnata, con la sola preferenza il provider entra come suggerimento e il
    /// fallback resta. Non e' `Option<&str>` proprio perche' il solo nome del
    /// provider costringeva a dedurre la forza del vincolo.
    pub(crate) provider_choice: &'a ProviderChoice,
    pub(crate) forced_model: Option<&'a str>,
    pub(crate) suggested_provider: Option<&'a str>,
    pub(crate) suggested_model: Option<&'a str>,
    pub(crate) composed_prompt: &'a str,
    pub(crate) token_budget: u32,
    pub(crate) tenant_id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) request_id: String,
}

/// PUNTO UNICO (regola L) della richiesta che la chat manda al gateway.
///
/// Tre regole, tutte conseguenza di un difetto misurato:
///
/// 1. **Il pin viaggia come pin — e SOLO il pin.** Il provider PINNATO
///    dall'utente (dropdown + pulsante "Forza", [`ProviderChoice::Pinned`])
///    finisce in `GwRequest::pin_provider`, il campo che esiste per questo: il
///    gateway esegue ESATTAMENTE quel provider, senza `policy.decide` ne'
///    fallback cross-provider. Prima il pin non veniva valorizzato e la
///    forzatura era solo un prefisso nel nome del modello: il gateway
///    re-instradava e rispondeva con un ALTRO provider (27/07/2026: forzato
///    deepseek, ha risposto google).
///    La sola PREFERENZA ([`ProviderChoice::Preferred`]: dropdown senza
///    "Forza", o provider ricordato dalla sessione) NON pinna: entra come
///    suggerimento — decide da dove parte il routing e su cosa il ledger
///    prenota — e la richiesta conserva il fallback, esattamente cio' che i
///    tooltip del composer promettono. Senza questa distinzione ogni selezione
///    dal dropdown diventerebbe un vincolo duro e i tooltip direbbero il falso:
///    lo stesso difetto di partenza col segno invertito.
/// 2. **Il modello non si prefissa col provider.** `"deepseek/coder-large"` non
///    e' un modello: il prefisso distruggeva la risoluzione dell'alias e il
///    fornitore rifiutava il nome letterale ("you passed coder-large") con un
///    400, da cui la cascata di policy che finiva altrove. Col pin il gateway
///    non risolve alias, quindi qui si manda un modello CONCRETO; senza pin si
///    manda l'alias logico e lo risolve lui.
/// 3. **Il modello segue il provider forzato.** Il modello si risolve UNA volta
///    sola, delegando a [`RoutingConfig::resolve_model`] (lo stesso punto unico
///    del ramo `(Some(provider), None)` di `resolve_agent_provider`): il modello
///    suggerito dal routing vale solo se e' del provider su cui si sta per
///    chiedere, altrimenti si scende al default di QUEL provider. E' cio' che
///    impedisce la coppia mista che il ledger registrava.
///
/// CONSEGUENZA DA CONOSCERE: col pin il gateway va in `strict` e non ripiega su
/// un altro fornitore. Se il provider forzato fallisce, la chiamata fallisce —
/// ed e' corretto (l'utente ha chiesto QUEL provider e deve saperlo). Non e' il
/// "pin senza fallback" che mandava i run in abort: quello era il pin sui
/// TRANSITORI, chiuso in ADR 0033 (col pin il gateway ritenta lo stesso modello
/// con backoff e attende i cooldown brevi invece di fallire subito). Qui il
/// fallimento residuo e' quello vero, e la sua frase la scrive
/// [`rendered_chat_gateway_error`].
pub(crate) fn build_chat_gateway_call(spec: ChatCallSpec<'_>) -> ChatGatewayCall {
    let target = resolve_chat_target(&spec);
    ChatGatewayCall {
        request: GwRequest {
            model: target.gw_model,
            messages: vec![GwMessage {
                role: "user".to_string(),
                content: Value::String(spec.composed_prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                thinking_signature: None,
            }],
            max_tokens: Some(spec.token_budget),
            temperature: None,
            pin_provider: target.pin_provider,
            metadata: GwMetadata {
                tenant_id: spec.tenant_id.to_string(),
                user_id: spec.user_id.to_string(),
                request_id: spec.request_id.clone(),
                // Claim locale di sensibilita' (punto unico dlp, regola L).
                // L'enforcement resta nel gateway, che ri-classifica e usa
                // max(claim, classificazione) + validate_tier_claim: il claim
                // onesto evita di dichiarare 'pubblico' (0) un prompt che il
                // DLP locale sa gia' essere sensibile.
                sensitivity_tier: crate::dlp::classify_sensitivity(spec.composed_prompt) as u8,
                feature: spec.intent.to_string(),
            },
            ..Default::default()
        },
        ledger_provider: target.ledger_provider,
        ledger_model: target.ledger_model,
    }
}

/// Le decisioni di [`build_chat_gateway_call`] che riguardano il modello,
/// separate dal confezionamento della richiesta.
struct ChatTarget {
    gw_model: String,
    pin_provider: Option<String>,
    ledger_provider: String,
    ledger_model: String,
}

fn resolve_chat_target(spec: &ChatCallSpec<'_>) -> ChatTarget {
    // Provider su cui la chiamata verra' prenotata (e, se forzato, pinnata).
    // Fallback letto dalla matrice DB, non hardcoded (regola G).
    let fallback_provider = spec
        .matrix
        .lookup("chat", "bilanciata")
        .map(|(provider, _)| provider)
        .unwrap_or_else(|| "openai".to_string());
    let provider = spec
        .provider_choice
        .provider()
        .or(spec.suggested_provider)
        .map(str::to_string)
        .unwrap_or(fallback_provider);

    // UNA sola risoluzione del modello, dal punto unico. `resolve_model` onora
    // il modello suggerito solo quando appartiene al provider passato: per
    // questo il modello FORZATO dall'utente viaggia dichiarandolo di quel
    // provider (scelta esplicita, vale sempre), mentre il suggerito conserva il
    // proprio provider di provenienza e viene scartato se non combacia.
    let provider_del_suggerito = if spec.forced_model.is_some() {
        Some(provider.as_str())
    } else {
        spec.suggested_provider
    };
    let model = spec.routing.resolve_model(
        spec.matrix,
        &provider,
        provider_del_suggerito,
        spec.forced_model.or(spec.suggested_model),
    );

    // Col pin il gateway NON risolve alias: deve ricevere il modello concreto.
    // Senza pin — quindi anche con la sola preferenza — resta l'alias logico,
    // che il gateway risolve per il provider che sceglie: e' cio' che tiene in
    // vita il fallback cross-provider promesso dai tooltip del composer.
    let (gw_model, pin_provider) = match spec.provider_choice.pinned_provider() {
        Some(pinned) => (model.clone(), Some(pinned.to_string())),
        None => (
            intent_to_alias(spec.intent, &spec.routing.behavior_mode, spec.forced_model),
            None,
        ),
    };
    ChatTarget {
        gw_model,
        pin_provider,
        ledger_provider: provider,
        ledger_model: model,
    }
}

/// La frase da mostrare quando una chiamata chat al gateway fallisce.
///
/// Delega al punto unico della presentazione
/// (`nexus_types::error_presentation` via [`crate::nexus_gateway::rendered_from_error`]):
/// qui si aggiunge solo il fatto che il punto unico non puo' conoscere, cioe'
/// che il provider era FORZATO — e quindi che nessun altro fornitore e' stato
/// tentato.
///
/// Prima questo path chiudeva con `bail!("Nexus Gateway failed ...: {e}")`, che
/// APPIATTIVA la catena: `GatewayHttpError` spariva e il confine HTTP della chat
/// (`chat_messages::run`), che cerca proprio quel tipo per rendere l'errore,
/// trovava solo una stringa. L'utente leggeva il body grezzo del 400.
pub(crate) fn rendered_chat_gateway_error(
    err: &anyhow::Error,
    pin_provider: Option<&str>,
) -> nexus_types::error_presentation::RenderedError {
    let rendered = crate::nexus_gateway::rendered_from_error(err);
    match pin_provider {
        Some(provider) => rendered.con_provider_forzato(provider),
        None => rendered,
    }
}

pub(crate) fn completion_has_error(completion: &Value) -> bool {
    // metadata.error presente e non-null → errore
    if let Some(err) = completion
        .get("metadata")
        .and_then(|metadata| metadata.get("error"))
    {
        if !err.is_null() {
            return true;
        }
    }
    // campo error a root presente e non-null → errore
    if let Some(err) = completion.get("error") {
        if !err.is_null() {
            return true;
        }
    }
    completion
        .get("content")
        .and_then(Value::as_str)
        .map(|content| {
            let trimmed = content.trim_start();
            trimmed.starts_with("[Error:") || trimmed.starts_with("[error:")
        })
        .unwrap_or(false)
}

/// `true` se un value neural (completion/agent-turn) e' un FALLIMENTO ritentabile
/// via failover cross-provider per i purpose interni a GENERAZIONE DI TESTO
/// (regola M: segnale strutturato, mai prosa). Estende [`completion_has_error`]
/// col caso CONTENT VUOTO/ASSENTE (Gemini/Vertex `finish_reason=length` -> content
/// "": nessun `error`, ma turno improduttivo). PUNTO UNICO (regola L) del "questo
/// turno va ritentato su un altro provider" per
/// [`crate::internal_routing::complete_for_purpose_with_failover`] e i suoi call
/// site. NB: pensato per i purpose text-only (summary/docs/analyzer/...): un turno
/// legittimo a soli tool_call (content vuoto) verrebbe marcato fallimento, quindi
/// NON usarlo per purpose che si aspettano tool_call.
pub(crate) fn neural_value_is_failure(v: &Value) -> bool {
    if completion_has_error(v) {
        return true;
    }
    v.get("content")
        .and_then(Value::as_str)
        .map(|c| c.trim().is_empty())
        .unwrap_or(true)
}

// ────────────────────────────────────────────────────────────────────────────
// Gate di capability sul routing agentico (ADR 0018, leva 0).
// ────────────────────────────────────────────────────────────────────────────

/// Decisione del gate di capability per un run AGENTICO.
///
/// Un run agentico (intent != "chat") deve usare SOLO modelli con
/// `ai_price_catalog.supports_tool_use = true`. Questa enum separa la LOGICA
/// di decisione (pura, testabile senza DB) dall'I/O (query catalog + fallback),
/// che resta nel chiamante `resolve_agent_provider_detailed`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ToolCapabilityGate {
    /// Il modello risolto e' utilizzabile cosi' com'e'. Nessuna sostituzione.
    /// Casi: intent non agentico, gate disabilitato, oppure modello gia'
    /// tool-capable.
    KeepOriginal,
    /// Il modello risolto NON e' tool-capable: il chiamante deve cercare un
    /// fallback tool-capable (via `best_model_for_tier` o default provider).
    NeedsFallback,
}

/// Decide se applicare il gate di capability tool-use al modello risolto.
///
/// Funzione PURA: nessun accesso DB. Il chiamante fornisce i fatti gia' letti
/// (intent agentico?, flag abilitato?, il modello supporta tool_use?).
///
/// Regole:
/// - intent == "chat" (non agentico) -> `KeepOriginal` (il gate non si applica
///   ai task non-agentici: classify, title, vision, embedding, completion).
/// - gate disabilitato (`agent.require_tool_use_capability` = false) ->
///   `KeepOriginal`.
/// - intent agentico + gate abilitato + modello NON tool-capable ->
///   `NeedsFallback`.
/// - se la capability del modello e' sconosciuta (`None`, es. modello assente
///   dal catalog) il gate e' CONSERVATIVO: NON sostituisce (KeepOriginal), per
///   non degradare un modello potenzialmente valido solo perche' manca dal
///   catalog. La mancanza nel catalog e' un problema di sync separato.
/// - `agentic_thinking_policy == "exclude"` (reasoning-only SENZA function
///   calling, es. deepseek-reasoner) su run agentico -> `NeedsFallback`.
///   I dual-mode (`disable_for_tools`) e i reasoning con tool nativi (`native`)
///   restano ammessi: l'adapter del brain gestisce la modalita' (ADR 0025).
///   `None` resta conservativo (KeepOriginal).
pub(crate) fn decide_tool_capability_gate(
    intent: &str,
    gate_enabled: bool,
    model_supports_tool_use: Option<bool>,
    agentic_thinking_policy: Option<&str>,
) -> ToolCapabilityGate {
    // Il gate si applica SOLO ai run agentici. Convenzione del progetto:
    // intent == "chat" e' l'unico intent non-agentico (vedi agent_run.rs:1206
    // `intent_uses_tools = classified_intent_for_loop != "chat"`).
    if intent == "chat" {
        return ToolCapabilityGate::KeepOriginal;
    }
    if !gate_enabled {
        return ToolCapabilityGate::KeepOriginal;
    }
    // Policy 'exclude': reasoning-only senza function calling -> non puo' reggere
    // un loop agentico, serve fallback. Ha priorita' sul check tool_use.
    if agentic_thinking_policy == Some("exclude") {
        return ToolCapabilityGate::NeedsFallback;
    }
    match model_supports_tool_use {
        // Modello esplicitamente non tool-capable -> serve fallback.
        Some(false) => ToolCapabilityGate::NeedsFallback,
        // Tool-capable, oppure capability ignota (conservativo) -> tieni.
        Some(true) | None => ToolCapabilityGate::KeepOriginal,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Gate di capability VISION sul routing del TURNO (regola L).
//
// RIPRISTINO REGRESSIONE Python->Rust (CLAUDE.md sezione I, "Smart routing
// vision"): nel brain Python, se il messaggio corrente conteneva allegati
// image/*, il router forzava un override sulla routing matrix verso un modello
// con capabilities.vision=true. Dopo il cutover a Rust questo override NON era
// stato reimplementato: l'unico uso di vision era il TOOL esplicito
// nexus_describe_image_attachment (purpose vision_describe). Se l'agente non
// chiamava quel tool, il modello del turno poteva non avere vision e l'immagine
// veniva ignorata. Qui ripristiniamo l'override come PUNTO UNICO.
// ────────────────────────────────────────────────────────────────────────────

/// PUNTO UNICO (regola L) che mappa "il turno corrente ha un allegato immagine"
/// a un booleano strutturato. Deriva il segnale dai MIME-TYPE degli allegati del
/// messaggio (segnale strutturato, niente parsing del testo del prompt) tramite
/// `classify_attachment_kind` — il punto unico gia' esistente della
/// classificazione mime->kind (chat_attachments.rs). Un turno "ha un'immagine"
/// se almeno un allegato e' di kind "image".
pub(crate) fn turn_has_image_attachment(
    attachments: &[crate::orchestrator::ChatAttachment],
) -> bool {
    attachments
        .iter()
        .any(|a| crate::chat_attachments::classify_attachment_kind(&a.mime_type) == "image")
}

/// Decisione del gate di capability VISION per il routing del turno.
///
/// Separa la LOGICA (pura, testabile senza DB) dall'I/O (query catalog +
/// fallback al selettore vision), che resta nel chiamante
/// `apply_vision_capability_gate`. Specularmente a `ToolCapabilityGate`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VisionCapabilityGate {
    /// Il modello risolto va bene cosi' com'e'. Casi: il turno NON ha immagini
    /// (override condizionale: zero regressione sul routing testuale), oppure il
    /// modello risolto supporta gia' la vision.
    KeepOriginal,
    /// Il turno ha un'immagine ma il modello risolto NON supporta la vision: il
    /// chiamante deve cercare un modello vision (capability='vision' ->
    /// supports_vision=TRUE) col selettore unico, riusando le regole di
    /// degradazione di tier esistenti.
    NeedsVisionModel,
}

/// Decide se forzare un modello vision per il turno corrente.
///
/// Funzione PURA: nessun accesso DB. Il chiamante fornisce i fatti gia' letti
/// (il turno ha un'immagine?, il modello risolto supporta la vision?).
///
/// Regole (override CONDIZIONALE — nessun effetto senza immagini):
/// - turno SENZA immagini -> `KeepOriginal` (routing testuale invariato).
/// - turno CON immagine + modello esplicitamente senza vision (`Some(false)`)
///   -> `NeedsVisionModel`.
/// - turno CON immagine + modello con vision (`Some(true)`) -> `KeepOriginal`.
/// - turno CON immagine + capability ignota (`None`, modello assente dal
///   catalog): CONSERVATIVO -> `NeedsVisionModel`. A differenza del gate
///   tool-use (che resta conservativo "KeepOriginal" perche' un modello
///   tool-capable mancante dal catalog e' raro e degradarlo sarebbe peggio),
///   qui un modello senza riga catalog NON ha `supports_vision=TRUE` per
///   costruzione: ignorare l'immagine e' il fallimento concreto che questo
///   ripristino vuole evitare. Cerchiamo quindi un modello vision noto.
pub(crate) fn decide_vision_capability_gate(
    turn_has_image: bool,
    model_supports_vision: Option<bool>,
) -> VisionCapabilityGate {
    if !turn_has_image {
        return VisionCapabilityGate::KeepOriginal;
    }
    match model_supports_vision {
        Some(true) => VisionCapabilityGate::KeepOriginal,
        Some(false) | None => VisionCapabilityGate::NeedsVisionModel,
    }
}

#[cfg(test)]
mod neural_failure_tests {
    use super::neural_value_is_failure;
    use serde_json::json;

    #[test]
    fn neural_value_is_failure_golden() {
        // Successo: content non vuoto, nessun error.
        assert!(!neural_value_is_failure(&json!({ "content": "ciao" })));
        // Error strutturato (metadata.error) -> fallimento.
        assert!(neural_value_is_failure(
            &json!({ "content": "x", "metadata": { "error": "boom" } })
        ));
        // error_class a root non-null -> fallimento (via completion_has_error/error).
        assert!(neural_value_is_failure(&json!({ "error": "llm_error", "content": "" })));
        // Prosa [Error: ...] -> fallimento.
        assert!(neural_value_is_failure(&json!({ "content": "[Error: quota]" })));
        // CONTENT VUOTO senza error (caso Gemini finish_reason=length) -> fallimento.
        assert!(neural_value_is_failure(&json!({ "content": "   " })));
        assert!(neural_value_is_failure(&json!({ "content": "" })));
        // content assente -> fallimento (nessun output utile per un purpose text).
        assert!(neural_value_is_failure(&json!({ "metadata": {} })));
    }
}

#[cfg(test)]
mod vision_capability_gate_tests {
    use super::{decide_vision_capability_gate, VisionCapabilityGate};
    use crate::orchestrator::ChatAttachment;

    fn img(mime: &str) -> ChatAttachment {
        ChatAttachment {
            id: None,
            name: "x".to_string(),
            mime_type: mime.to_string(),
            size_bytes: 1,
            text_content: String::new(),
            base64_content: None,
        }
    }

    #[test]
    fn turno_con_immagine_su_modello_senza_vision_richiede_vision() {
        // Ripristino regressione: image/* nel turno + modello senza vision ->
        // si forza un modello vision.
        let d = decide_vision_capability_gate(true, Some(false));
        assert_eq!(d, VisionCapabilityGate::NeedsVisionModel);
    }

    #[test]
    fn turno_con_immagine_su_modello_vision_resta_invariato() {
        let d = decide_vision_capability_gate(true, Some(true));
        assert_eq!(d, VisionCapabilityGate::KeepOriginal);
    }

    #[test]
    fn turno_senza_immagini_non_tocca_il_routing() {
        // Override CONDIZIONALE: zero regressione sul routing testuale, qualunque
        // sia la capability del modello.
        assert_eq!(
            decide_vision_capability_gate(false, Some(false)),
            VisionCapabilityGate::KeepOriginal
        );
        assert_eq!(
            decide_vision_capability_gate(false, None),
            VisionCapabilityGate::KeepOriginal
        );
    }

    #[test]
    fn turno_con_immagine_capability_ignota_e_conservativo_verso_vision() {
        // Modello assente dal catalog: per costruzione non ha supports_vision=TRUE.
        // Ignorare l'immagine e' il fallimento da evitare -> cerca un modello vision.
        let d = decide_vision_capability_gate(true, None);
        assert_eq!(d, VisionCapabilityGate::NeedsVisionModel);
    }

    #[test]
    fn rilevamento_immagine_dai_mime_del_turno() {
        // `turn_has_image_attachment` deriva il segnale dai MIME (punto unico
        // classify_attachment_kind), non dal testo del prompt.
        use super::turn_has_image_attachment;
        assert!(turn_has_image_attachment(&[img("image/png")]));
        assert!(turn_has_image_attachment(&[img("image/JPEG")]));
        assert!(turn_has_image_attachment(&[
            img("application/pdf"),
            img("image/webp")
        ]));
        assert!(!turn_has_image_attachment(&[img("application/pdf")]));
        assert!(!turn_has_image_attachment(&[img("text/plain")]));
        assert!(!turn_has_image_attachment(&[]));
    }
}

#[cfg(test)]
mod tool_capability_gate_tests {
    use super::{compute_no_capable_provider, decide_tool_capability_gate, ToolCapabilityGate};

    #[test]
    fn gate_scarta_modello_non_tool_capable_su_intent_agentico() {
        // mistral-code-latest (supports_tool_use=false) su intent agentico -> fallback.
        let d = decide_tool_capability_gate("file_ops", true, Some(false), Some("none"));
        assert_eq!(d, ToolCapabilityGate::NeedsFallback);
    }

    #[test]
    fn gate_lascia_passare_modello_tool_capable() {
        let d = decide_tool_capability_gate("refactor", true, Some(true), Some("none"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_disattivato_lascia_passare_anche_modello_non_capable() {
        // Flag agent.require_tool_use_capability = false -> passthrough.
        let d = decide_tool_capability_gate("file_ops", false, Some(false), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_non_si_applica_a_intent_chat() {
        // Run non agentico: il gate non interviene neppure su policy 'exclude'.
        let d = decide_tool_capability_gate("chat", true, Some(false), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_conservativo_su_capability_ignota() {
        // Modello assente dal catalog (None): non degradare.
        let d = decide_tool_capability_gate("debug", true, None, None);
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    // ── ADR 0025: eleggibilita' agentica via agentic_thinking_policy ──

    #[test]
    fn gate_scarta_policy_exclude_su_intent_agentico() {
        // deepseek-reasoner: reasoning-only SENZA function calling (exclude) -> fallback.
        let d = decide_tool_capability_gate("fix", true, Some(true), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::NeedsFallback);
    }

    #[test]
    fn gate_dual_mode_passa_su_agentico() {
        // deepseek-v4 (disable_for_tools): tool-capable, NON escluso -> KeepOriginal
        // (l'adapter del brain forza il non-thinking nel loop tool).
        let d = decide_tool_capability_gate("fix", true, Some(true), Some("disable_for_tools"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_native_reasoning_passa_su_agentico() {
        // o3 (native): reasoning con tool nativi -> KeepOriginal.
        let d = decide_tool_capability_gate("debug", true, Some(true), Some("native"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    #[test]
    fn gate_exclude_lecito_su_chat() {
        // Su intent non agentico anche 'exclude' e' lecito (nessun tool-forcing).
        let d = decide_tool_capability_gate("chat", true, Some(true), Some("exclude"));
        assert_eq!(d, ToolCapabilityGate::KeepOriginal);
    }

    // ── ADR 0020: regola di disponibilita' provider (cooldown vincolante) ──

    #[test]
    fn auto_provider_in_cooldown_e_no_capable() {
        // Modalita' AUTO, provider scelto in cooldown -> no_capable.
        assert!(compute_no_capable_provider(false, true, false));
    }

    #[test]
    fn auto_tutti_in_cooldown_e_no_capable() {
        // Modalita' AUTO, tutti i provider noti in cooldown -> no_capable.
        assert!(compute_no_capable_provider(false, false, true));
    }

    #[test]
    fn auto_provider_disponibile_non_e_no_capable() {
        // Modalita' AUTO, provider scelto fuori cooldown -> capable.
        assert!(!compute_no_capable_provider(false, false, false));
    }

    #[test]
    fn user_override_bypassa_il_cooldown() {
        // Forzatura utente: anche se il provider scelto e' in cooldown e tutti
        // i provider noti lo sono, la scelta NON e' mai no_capable (l'utente
        // decide consapevolmente, ADR 0020).
        assert!(!compute_no_capable_provider(true, true, true));
        assert!(!compute_no_capable_provider(true, true, false));
        assert!(!compute_no_capable_provider(true, false, true));
    }
}

#[cfg(test)]
mod intent_key_tests {
    use super::{intent_key_for, TokenThresholds};

    fn thresholds() -> TokenThresholds {
        TokenThresholds::defaults()
    }

    #[test]
    fn code_read_ha_intent_key_dedicato_non_chat() {
        // Regressione mig 0336: `code_read` NON deve piu' cadere nel ramo
        // conversazionale chat_*. Con un budget piccolo (sotto chat_breve) il
        // vecchio default avrebbe dato "chat_breve" -> modello lite.
        let t = thresholds();
        assert_eq!(intent_key_for("code_read", 50, &t), "code_read");
        // Anche con budget ampio resta code_read (niente soglia token).
        assert_eq!(intent_key_for("code_read", 100_000, &t), "code_read");
    }

    #[test]
    fn agentic_default_ha_intent_key_dedicato_non_chat() {
        // mig 0337: il fallback neutro `agentic_default` (LLM down) deve avere
        // un intent_key dedicato -> modelli tool-robust, non chat_* lite.
        let t = thresholds();
        assert_eq!(intent_key_for("agentic_default", 50, &t), "agentic_default");
        assert_eq!(
            intent_key_for("agentic_default", 100_000, &t),
            "agentic_default"
        );
    }

    #[test]
    fn intent_sconosciuto_degrada_su_chat_per_token() {
        // Un intent non mappato (es. "chat") usa le soglie token.
        let t = thresholds(); // chat_breve=400, chat_media=1500
        assert_eq!(intent_key_for("chat", 100, &t), "chat_breve");
        assert_eq!(intent_key_for("chat", 800, &t), "chat_media");
        assert_eq!(intent_key_for("chat", 5_000, &t), "chat_lunga");
    }

    #[test]
    fn intent_agentici_conservano_la_chiave() {
        let t = thresholds();
        assert_eq!(intent_key_for("debug", 100, &t), "debug");
        assert_eq!(intent_key_for("system_admin", 100, &t), "system_admin");
        // fix si sdoppia su complex_fix (default 3000).
        assert_eq!(intent_key_for("fix", 100, &t), "fix_semplice");
        assert_eq!(intent_key_for("fix", 5_000, &t), "fix_complesso");
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Pavimento di tier per i turni AGENTICI (regola G DB-driven, regola L punto
// unico). Verifica che un turno a tool non parta da un modello LIGHT debole, che
// la chat semplice resti libera di usare LIGHT, e che la degradazione sia
// GRACEFUL (se solo LIGHT e' disponibile, l'agentico ci scende senza panic).
// ────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod agentic_tier_floor_tests {
    use super::{agentic_tier_chain, floor_tier_for_agentic, route_model_from_catalog};

    #[test]
    fn agentic_tier_chain_degrada_su_5_livelli() {
        // Ogni tier degrada verso il basso fino a 'light' (scala a 5).
        assert_eq!(
            agentic_tier_chain("frontier"),
            vec!["frontier", "heavy", "high", "medium", "light"]
        );
        assert_eq!(
            agentic_tier_chain("heavy"),
            vec!["heavy", "high", "medium", "light"]
        );
        assert_eq!(agentic_tier_chain("high"), vec!["high", "medium", "light"]);
        assert_eq!(agentic_tier_chain("medium"), vec!["medium", "light"]);
        assert_eq!(agentic_tier_chain("light"), vec!["light", "medium"]);
        // tier fuori vocabolario: degrada da 'medium' (neutro), mai fallimento.
        assert_eq!(agentic_tier_chain("boh"), vec!["medium", "light"]);
    }

    // ── Parte PURA: floor_tier_for_agentic (nessun DB) ──────────────────────

    #[test]
    fn floor_non_tocca_i_turni_non_agentici() {
        // Chat semplice: il pavimento non si applica, 'light' resta 'light'.
        assert_eq!(floor_tier_for_agentic(false, "light", "medium"), "light");
        assert_eq!(floor_tier_for_agentic(false, "light", "heavy"), "light");
    }

    #[test]
    fn floor_alza_il_tier_dei_turni_agentici() {
        // Turno agentico con tier base 'light' e pavimento 'medium' -> 'medium'.
        assert_eq!(floor_tier_for_agentic(true, "light", "medium"), "medium");
        // Pavimento 'heavy' su base 'light' -> 'heavy'.
        assert_eq!(floor_tier_for_agentic(true, "light", "heavy"), "heavy");
    }

    #[test]
    fn floor_non_abbassa_mai_un_tier_gia_alto() {
        // Un task heavy resta heavy anche col pavimento 'medium' (non declassa).
        assert_eq!(floor_tier_for_agentic(true, "heavy", "medium"), "heavy");
        // A parita' di rank resta invariato.
        assert_eq!(floor_tier_for_agentic(true, "medium", "medium"), "medium");
    }

    // ── Parte DB: route_model_from_catalog (selettore reale sul catalog) ─────

    async fn create_catalog_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE ai_price_catalog ( \
                 provider TEXT NOT NULL, \
                 model TEXT NOT NULL, \
                 is_enabled BOOLEAN NOT NULL DEFAULT true, \
                 supports_tool_use BOOLEAN NOT NULL DEFAULT true, \
                 supports_vision BOOLEAN NOT NULL DEFAULT false, \
                 supports_image_gen BOOLEAN NOT NULL DEFAULT false, \
                 supports_audio_in BOOLEAN NOT NULL DEFAULT false, \
                 supports_audio_out BOOLEAN NOT NULL DEFAULT false, \
                 supports_video_gen BOOLEAN NOT NULL DEFAULT false, \
                 agentic_thinking_policy TEXT NOT NULL DEFAULT 'none', \
                 performance_tier TEXT NOT NULL DEFAULT 'medium', \
                 capabilities JSONB NOT NULL DEFAULT '[]', \
                 context_window INTEGER NOT NULL DEFAULT 8192, \
                 input_cost_per_million_tokens DOUBLE PRECISION NOT NULL DEFAULT 0, \
                 output_cost_per_million_tokens DOUBLE PRECISION NOT NULL DEFAULT 0, \
                 is_featured BOOLEAN NOT NULL DEFAULT false, \
                 speed_tier TEXT NOT NULL DEFAULT 'medium', \
                 consecutive_failures INT NOT NULL DEFAULT 0, \
                 consecutive_tool_failures INT NOT NULL DEFAULT 0, \
                 auto_disabled_reason TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create ai_price_catalog");
    }

    async fn create_settings_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE settings ( \
                 key TEXT PRIMARY KEY, \
                 value TEXT NOT NULL \
             )",
        )
        .execute(pool)
        .await
        .expect("create settings");
    }

    #[sqlx::test]
    async fn turno_agentico_sceglie_almeno_medium(pool: sqlx::PgPool) {
        create_catalog_table(&pool).await;
        // Catalog misto light/medium/heavy, tutti tool-capable. Il piu' economico
        // in assoluto e' il LIGHT (0.1): senza pavimento il selettore lo
        // sceglierebbe. Col pavimento 'medium' (default, settings assente -> default
        // visibile) il LIGHT e' ESCLUSO e vince il piu' economico >= medium.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, capabilities) VALUES \
             ('flprov', 'light-economico', true, 'none', 'light', 0.1, '[\"code\"]'), \
             ('mdprov', 'medium-mid', true, 'none', 'medium', 1.0, '[\"code\"]'), \
             ('hvprov', 'heavy-caro', true, 'none', 'heavy', 5.0, '[\"code\"]')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // base_tier 'light' (come un intent fix_semplice a token bassi), turno
        // agentico: il pavimento default 'medium' deve scartare il LIGHT.
        let d = route_model_from_catalog(&pool, "light", "code", "dinamico", true)
            .await
            .expect("una decisione");
        assert_eq!(d.provider, "mdprov");
        assert_eq!(d.model, "medium-mid");
    }

    #[sqlx::test]
    async fn turno_non_agentico_puo_scegliere_light(pool: sqlx::PgPool) {
        create_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, capabilities) VALUES \
             ('flprov2', 'light-economico', true, 'none', 'light', 0.1, '[\"chat\"]'), \
             ('mdprov2', 'medium-mid', true, 'none', 'medium', 1.0, '[\"chat\"]')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Chat semplice (is_agentic_turn=false): il pavimento NON si applica,
        // base_tier 'light' resta 'light' -> sceglie il light economico.
        let d = route_model_from_catalog(&pool, "light", "chat", "dinamico", false)
            .await
            .expect("una decisione");
        assert_eq!(d.provider, "flprov2");
        assert_eq!(d.model, "light-economico");
    }

    #[sqlx::test]
    async fn turno_agentico_degrada_a_light_se_solo_light_disponibile(pool: sqlx::PgPool) {
        create_catalog_table(&pool).await;
        // Solo un modello LIGHT eleggibile (nessun medium/heavy): il pavimento
        // 'medium' alza la richiesta a 'medium', ma la tier-chain medium->light
        // degrada GRACEFUL al LIGHT invece di fallire (NON panic, NON None).
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, capabilities) VALUES \
             ('onlyfl', 'unico-light', true, 'none', 'light', 0.1, '[\"code\"]')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let d = route_model_from_catalog(&pool, "light", "code", "dinamico", true)
            .await
            .expect("degrado graceful al light, nessun fallimento");
        assert_eq!(d.provider, "onlyfl");
        assert_eq!(d.model, "unico-light");
    }

    #[sqlx::test]
    async fn setting_db_driven_alza_il_pavimento_a_heavy(pool: sqlx::PgPool) {
        create_catalog_table(&pool).await;
        create_settings_table(&pool).await;
        // Pavimento configurato a 'heavy' via DB (regola G): un turno agentico a
        // base_tier 'light' deve salire fino a 'heavy', scartando light e medium.
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('agent.routing.agentic_min_tier', 'heavy')",
        )
        .execute(&pool)
        .await
        .expect("insert setting");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, capabilities) VALUES \
             ('flprov3', 'light-economico', true, 'none', 'light', 0.1, '[\"code\"]'), \
             ('mdprov3', 'medium-mid', true, 'none', 'medium', 1.0, '[\"code\"]'), \
             ('hvprov3', 'heavy-caro', true, 'none', 'heavy', 5.0, '[\"code\"]')",
        )
        .execute(&pool)
        .await
        .expect("insert catalog");
        let d = route_model_from_catalog(&pool, "light", "code", "dinamico", true)
            .await
            .expect("una decisione");
        assert_eq!(d.provider, "hvprov3");
        assert_eq!(d.model, "heavy-caro");
    }
}
