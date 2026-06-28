//! Worker `model_catalog_sync` — fix Bug 7 (audit 26/05/2026).
//!
//! Mantiene `ai_price_catalog` allineato con i modelli realmente esposti
//! dalle API dei provider. Senza questo, quando un provider deprecava un
//! modello (es. DeepSeek v3 -> v4) il catalog restava stale per settimane e
//! gli agent_run fallivano con "hollow completion" perche' chiamavano modelli
//! inesistenti lato provider, sprecando token e degradando l'UX.
//!
//! Flusso ogni N ore (settings `catalog_sync.interval_hours`, default 6):
//!   1. Lista provider da `catalog_sync.providers` (CSV)
//!   2. Per ogni provider: GET {gateway}/v1/models/{provider} (via UNICA per
//!      la discovery — il gateway incapsula l'auth di ogni provider, Vertex
//!      Service Account incluso). Poi confronta con catalog: INSERT nuovi
//!      (is_enabled=false, prezzi 0 da raffinare manualmente), DISABLE modelli
//!      non piu' esposti.
//!   3. Audit ogni delta in `ai_price_catalog_audit`
//!   4. Emit notification dispatcher 'CatalogModelChanged' per admin
//!
//! Sorgente UNICA dei modelli live: il Nexus Gateway (regola L). Il worker NON
//! chiama piu' direttamente gli endpoint `api.{provider}.com/v1/models` ne'
//! delega al brain per Google: tutta l'auth (API key cloud, Vertex Service
//! Account) e' gia' nel gateway, che espone `GET /v1/models/{provider}` per
//! tutti i provider. Vedi `NexusGatewayClient::list_models`.
//!
//! Provider locali (ollama/vllm): skipped, catalog manuale per setup custom.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::PgPool;

use crate::model_health_probe::{probe_model_on_insert, ProbeOnInsertResult};
use crate::orchestrator::Orchestrator;
use crate::settings::get_setting;

const DEFAULT_INTERVAL_HOURS: u64 = 6;

/// Filtro chat-compatibilita': i provider espongono nelle loro `/v1/models` API
/// anche modelli specializzati (voice, TTS, transcribe, embedding, instruct
/// legacy, image generation, modelli "preview" hollow) che NON sono usabili
/// dalla chat agentic di Nexus. Senza filtro, il catalog viene inquinato e
/// il routing puo' selezionarli, generando errori in cascata.
///
/// Ritorna `true` se il modello e' un valido modello di chat completion.
fn is_chat_compatible_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    const SUBSTRING_BLACKLIST: &[&str] = &[
        "voxtral",
        "whisper",
        "embedding",
        "moderation",
        "unknown-provider",
    ];
    for bad in SUBSTRING_BLACKLIST {
        if lower.contains(bad) {
            return false;
        }
    }
    const INFIX_BLACKLIST: &[&str] = &[
        "-tts-",
        "-transcribe-",
        "-realtime-",
        "-instruct-",
        "-unknown-",
    ];
    for bad in INFIX_BLACKLIST {
        if lower.contains(bad) {
            return false;
        }
    }
    const PREFIX_BLACKLIST: &[&str] = &[
        "tts-",
        "dall-e",
        "dalle-",
        "imagen",
        "instruct-",
        "babbage",
        "davinci-00",
        "text-embedding",
    ];
    for bad in PREFIX_BLACKLIST {
        if lower.starts_with(bad) {
            return false;
        }
    }
    const SUFFIX_BLACKLIST: &[&str] = &["-tts", "-transcribe", "-realtime", "-embed", "-instruct"];
    for bad in SUFFIX_BLACKLIST {
        if lower.ends_with(bad) {
            return false;
        }
    }
    // Nota: NESSUNA blacklist per nome modello/famiglia (es. ex-blacklist
    // hardcoded di gemini-3.x). I modelli "fantasma" (esposti dall'API
    // discovery ma rispondono 404/hollow in inference) sono ora rilevati
    // dinamicamente dal `probe_model_on_insert` chiamato qui sotto al primo
    // discovery. Self-healing: quando un modello placeholder viene poi davvero
    // rilasciato dal provider, il probe successivo passa e il modello viene
    // riabilitato automaticamente dal worker `model_health_probe`. La blacklist
    // strutturale qui sopra (TTS/embedding/realtime/instruct/imagen) resta:
    // sono modelli che per design NON sono chat completion, il probe sarebbe
    // solo rumore.
    true
}

/// Inferisce le capabilities di un modello dal nome quando l'API discovery
/// non le ritorna esplicite (caso comune per Google Vertex). Restituisce una
/// lista pronta per `capabilities` JSONB di ai_price_catalog.
///
/// Bug osservato 30/05/2026: `gemini-3.1-pro-preview-customtools` passa il
/// probe-on-insert (HTTP 200) ma resta con capabilities=[]. Il routing filtra
/// per capability matching e quel modello (potenzialmente il piu' capable di
/// Vertex per tool use) non viene mai scelto. Soluzione: dopo il probe OK,
/// se capabilities e' vuoto, popolare con una stima euristica basata sul nome.
///
/// Regole (in ordine di specificita'):
///   - *-pro / *-pro-preview / *-pro-* (es. customtools)  -> reasoning/code/long-context
///   - *-flash-lite                                        -> chat/simple
///   - *-flash / *-flash-preview                           -> code/chat/fix
///   - *-opus / *-sonnet                                   -> reasoning/code/long-context
///   - *-haiku                                             -> chat/simple
///   - *-codex / *-codestral / *-devstral                  -> code
///   - default                                             -> chat (sempre safe)
///
/// Restituisce sempre almeno `["chat"]`. Non infersce vision/image/audio
/// (queste richiedono detection esplicita lato provider).
pub(crate) fn infer_capabilities_from_name(provider: &str, model: &str) -> Vec<&'static str> {
    let m = model.to_ascii_lowercase();
    // Aggiungi marker tool_use_optimized per modelli con suffisso "customtools".
    let mut caps: Vec<&'static str> = Vec::new();
    if m.contains("customtools") {
        caps.push("tool_use_optimized");
    }
    let base: &[&str] = match provider {
        "google" => {
            if m.contains("pro") {
                &["reasoning", "code", "long-context", "chat"]
            } else if m.contains("flash-lite") {
                &["chat", "simple"]
            } else if m.contains("flash") {
                &["code", "chat", "fix"]
            } else {
                &["chat"]
            }
        }
        "anthropic" => {
            if m.contains("opus") || m.contains("sonnet") {
                &["reasoning", "code", "long-context", "chat"]
            } else if m.contains("haiku") {
                &["chat", "simple"]
            } else {
                &["chat"]
            }
        }
        "openai" => {
            if m.contains("codex") {
                &["code", "chat"]
            } else if m.contains("nano") || m.contains("mini") {
                &["chat", "simple"]
            } else {
                &["reasoning", "code", "chat"]
            }
        }
        "mistral" => {
            if m.contains("codestral") || m.contains("devstral") {
                &["code", "chat"]
            } else if m.contains("large") || m.contains("magistral") {
                &["reasoning", "code", "chat"]
            } else if m.contains("medium") {
                &["code", "chat"]
            } else {
                &["chat"]
            }
        }
        "deepseek" => {
            if m.contains("pro") || m.contains("reasoner") {
                &["reasoning", "code", "chat"]
            } else {
                &["code", "chat"]
            }
        }
        _ => &["chat"],
    };
    for c in base {
        if !caps.contains(c) {
            caps.push(c);
        }
    }
    caps
}

/// Inferisce il `performance_tier` (light|medium|heavy) dal nome del modello.
///
/// PERCHE': lo schema `ai_price_catalog.performance_tier` ha default 'medium',
/// e il catalog_sync inserisce i nuovi modelli scoperti via API senza tier.
/// Risultato: ogni modello nuovo (anche piccolo come ministral-3b/8b) diventava
/// 'medium' ed entrava nel pool dei candidati per gli intent agentici medium,
/// degradando la qualita'. Qui classifichiamo dal nome (euristica gemella di
/// `infer_capabilities_from_name`), applicata SOLO ai nuovi insert (non
/// sovrascrive le righe esistenti / override admin).
///
/// Regole per famiglia (allineate alla migrazione 0354 che riclassifica gli
/// esistenti): i modelli "piccoli" (mini/nano/lite/haiku/ministral/small/nemo)
/// sono light; i flagship (opus/o3/o1/*-pro) heavy; il resto medium.
/// Estrae il major version number da un nome modello OpenAI della famiglia
/// `gpt-N` / `gpt-N.M` (es. "gpt-5.5" -> 5, "gpt-4.1-nano" -> 4). Ritorna None
/// se il nome non e' un modello `gpt-` numerato (es. o-series, chatgpt-*).
/// Usato dall'euristica tier per distinguere i flagship recenti (gpt-5+) dai
/// modelli precedenti senza hardcodare i nomi esatti (regola G).
fn openai_gpt_major(model_lower: &str) -> Option<u32> {
    let rest = model_lower.strip_prefix("gpt-")?;
    // Prende le cifre iniziali del token di versione (fino a '.', '-' o fine).
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub(crate) fn infer_tier_from_name(provider: &str, model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    match provider {
        "google" => {
            if m.contains("pro") {
                "heavy"
            } else {
                // flash, flash-lite
                "light"
            }
        }
        "anthropic" => {
            if m.contains("opus") {
                "heavy"
            } else if m.contains("haiku") {
                "light"
            } else {
                // sonnet e altri
                "medium"
            }
        }
        "openai" => {
            // I "piccoli" hanno la precedenza assoluta: qualunque variante
            // mini/nano (anche di un flagship, es. gpt-5.4-mini) e' light.
            if m.contains("nano") || m.contains("mini") {
                "light"
            } else if m.contains("o3") || m.contains("o1") || m.contains("pro") {
                // Reasoning o-series (o1/o3) e varianti *-pro = flagship heavy.
                "heavy"
            } else if openai_gpt_major(&m).is_some_and(|major| major >= 5) {
                // Flagship GPT di punta: gpt-5, gpt-5.x e successivi senza
                // suffisso mini/nano e che non siano chat-only sono heavy.
                // Parsare il major number (anziche' confrontare nomi esatti)
                // mantiene l'euristica robusta alle versioni future della
                // famiglia (regola G: niente nome modello hardcoded).
                // I "chat-latest" sono varianti ottimizzate per chat veloce,
                // non i flagship reasoning: restano medium.
                if m.contains("chat") {
                    "medium"
                } else {
                    "heavy"
                }
            } else {
                // gpt-4o, gpt-4.1, ecc. restano medium.
                "medium"
            }
        }
        "mistral" => {
            // Mistral non ha un tier "heavy" reale: large e' il loro massimo.
            // Piccoli (ministral 3b/8b/14b, small, nemo) -> light.
            if m.contains("ministral") || m.contains("small") || m.contains("nemo") {
                "light"
            } else {
                // large, medium, codestral, devstral, magistral
                "medium"
            }
        }
        "deepseek" => "medium",
        _ => "medium",
    }
}

/// Flag di capability canonici di un modello (colonne reali di ai_price_catalog).
/// Vedi ADR 0024 e migrazione 0318: il catalog e' l'UNICA fonte; il brain li
/// legge derivati via vista `v_model_capabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClassifiedCaps {
    /// Il modello sa invocare tool (function calling).
    pub supports_tool_use: bool,
    /// Il modello accetta input immagine (vision).
    pub supports_vision: bool,
    /// Concetto A: ESCLUDI dal routing agentico. Modelli reasoning-only che
    /// non reggono il loop a tool forzati (o-series, deepseek reasoner/v4,
    /// magistral, gemini *-pro). NON include i modelli ibridi che fanno bene
    /// l'agentico pur avendo una modalita' thinking (Claude, gpt non-o).
    pub is_thinking: bool,
    /// Concetto B: gira in thinking/extended-thinking mode -> l'adapter NON
    /// deve forzare tool_choice e abilita il budget di ragionamento. Superset
    /// di `is_thinking` (ogni reasoning-only e' anche thinking-mode, ma anche
    /// gli ibridi come Claude opus/sonnet lo sono).
    pub uses_thinking_mode: bool,
    /// Policy d'uso nei run agentici (ADR 0025), driver canonico dell'eleggibilita'
    /// e del toggle modalita': none | disable_for_tools | native | exclude.
    pub agentic_thinking_policy: &'static str,
}

/// Classificatore UNICO delle capability di un modello (ADR 0024).
///
/// Invocato da OGNI path di aggiornamento del catalog (sync LiteLLM in
/// `models::run_catalog_sync` e discovery provider in `sync_provider`), cosi'
/// quando i modelli si aggiornano la classificazione si aggiorna con loro.
/// Usa i metadata espliciti quando disponibili (LiteLLM: function_calling,
/// vision, reasoning); altrimenti euristica sul nome. Niente nome modello
/// hardcoded nella logica di business: qui classifichiamo per FAMIGLIA.
///
/// Le righe `capability_source='manual'` NON vengono toccate dai chiamanti
/// (guard SQL nell'UPSERT): questa funzione produce solo il default 'auto'.
pub(crate) fn classify_capabilities(
    provider: &str,
    model: &str,
    meta_tool_use: Option<bool>,
    meta_vision: Option<bool>,
    meta_reasoning: Option<bool>,
    // Provider instradabili da /vision/describe (punto unico DB-driven, setting
    // `vision.routable_providers`, mig 0373). Letto dal chiamante e passato qui:
    // la funzione resta pura/sincrona/testabile. supports_vision=true SOLO se il
    // provider e' in questo set (regola G/L: niente lista hardcoded duplicata).
    vision_routable: &std::collections::HashSet<String>,
) -> ClassifiedCaps {
    let p = provider.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();

    // ── tool_use: metadata esplicito, altrimenti default true (la stragrande
    //    maggioranza dei modelli chat moderni supporta function calling). ──
    //    ECCEZIONE per FAMIGLIA (regola L): i magistral sono la linea reasoning
    //    di Mistral e SUPPORTANO function calling (docs.mistral.ai/capabilities/
    //    function_calling elenca Magistral Small/Medium 1.2 tra i modelli
    //    function-calling), ma il metadata LiteLLM e' INCOERENTE tra varianti
    //    (alcune ritornano tool_use=false). Classifichiamo per famiglia con un
    //    valore unico (tool_use=true), ignorando il metadata per-modello
    //    inaffidabile. Verificato live: i magistral chiamano i tool col
    //    tool_choice forzato MANTENENDO il reasoning attivo (non serve, ne' e'
    //    possibile, spegnerlo: reasoning_effort non e' supportato dai magistral).
    //    Stessa eccezione per FAMIGLIA per DeepSeek V4 (v4-pro, v4-flash):
    //    verificato live (2026-06-10, probe API diretto) che ENTRAMBI eseguono
    //    function calling correttamente (finish_reason=tool_calls, tool call ben
    //    formata, reasoning attivo insieme ai tool; nel tool-loop l'adapter li fa
    //    girare non-thinking via extra_body.thinking=disabled, policy
    //    'disable_for_tools'). Il FALSE storico nel catalog era un degrado
    //    runtime (malformed_tool_calls da run hollow) scritto senza guard
    //    capability_source, non una verita' del provider.
    let supports_tool_use = if (p == "mistral" && m.contains("magistral"))
        || (p == "deepseek" && m.contains("v4"))
    {
        true
    } else {
        meta_tool_use.unwrap_or(true)
    };

    // ── vision: riflette l'instradabilita' REALE da brain/grpc_server/routes/
    //    vision.py. La lista dei provider instradabili NON e' piu' hardcoded ma
    //    vive nel setting DB `vision.routable_providers` (mig 0373), passato qui
    //    in `vision_routable` (punto unico DB-driven, regola G/L). Il metadata
    //    `meta_vision` di LiteLLM e' lasco ("accetta immagini in input") e marca
    //    falsi positivi (es. mistral-small) che il routing per tier sceglierebbe
    //    per vision_describe -> /vision/describe 501. Quindi: vision=true SOLO se
    //    il provider e' routable; l'euristica/metadata per-modello distingue poi
    //    QUALI modelli del provider hanno vision.
    let supports_vision = if vision_routable.contains(p.as_str()) {
        match p.as_str() {
            "openai" => meta_vision.unwrap_or_else(|| {
                m.starts_with("gpt-4o")
                    || m.starts_with("gpt-4.1")
                    || m.starts_with("gpt-4-turbo")
                    || m.starts_with("o1")
                    || m.starts_with("o3")
            }),
            "anthropic" => meta_vision.unwrap_or_else(|| {
                m.contains("opus") || m.contains("sonnet") || m.contains("haiku")
            }),
            "google" => meta_vision.unwrap_or_else(|| m.contains("gemini")),
            // Provider routable senza euristica per-modello nota: usa il metadata.
            _ => meta_vision.unwrap_or(false),
        }
    } else {
        // Provider senza ramo in vision.py: non instradabile -> niente vision.
        false
    };

    // ── Detection reasoning model (guida sia A sia B). ──
    // Famiglie reasoning-only note (incompatibili col tool-forcing o col loop
    // multi-turno nell'integrazione attuale).
    let reasoning_only_family = match p.as_str() {
        // OpenAI o-series: o1/o3/o4 (prefisso "o" + cifra).
        "openai" => m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4"),
        // DeepSeek reasoner e V4 (richiede reasoning_content passback non
        // implementato -> non regge il loop agentico, vedi mig 0317).
        "deepseek" => m.contains("reasoner") || m.contains("v4"),
        // Mistral magistral: linea reasoning.
        "mistral" => m.contains("magistral"),
        // Google: i Gemini 2.5 NON sono reasoning-only da escludere. Sono
        // dual-mode tool-capable e si gestiscono via agentic_thinking_policy
        // ='disable_for_tools' (gestito sotto da `gemini_25_thinking`): girano
        // non-thinking nei tool-loop, evitando il MALFORMED_FUNCTION_CALL.
        "google" => false,
        _ => false,
    };
    // Gemini 2.5 (pro E flash, ESCLUSO flash-lite) ha il thinking attivo di
    // default: col function calling in modalita' thinking produce
    // MALFORMED_FUNCTION_CALL (mig 0274). Va trattato come dual-mode -> tool-capable
    // + policy 'disable_for_tools' (non-thinking nei tool-loop), NON escluso
    // dall'agentico. Bug storico: l'euristica precedente assumeva che solo i
    // *-pro fossero thinking ("flash NON lo sono"), lasciando gemini-2.5-flash
    // con policy 'none' -> thinking attivo coi tool -> MALFORMED ad ogni run.
    // flash-lite NON ha thinking di default: resta policy 'none'.
    let gemini_25_thinking = p == "google" && m.contains("gemini-2.5") && !m.contains("flash-lite");
    // Marker generici nel nome, cross-provider.
    let name_reasoning = m.contains("reasoner")
        || m.contains("reasoning")
        || m.contains("thinking")
        || reasoning_only_family;
    // Metadata esplicito reasoning (LiteLLM) ha priorita' come segnale positivo.
    let is_reasoning_signal = meta_reasoning.unwrap_or(false) || name_reasoning;

    // Concetto A (escludi da agentico): solo le famiglie reasoning-only. Gli
    // ibridi (Claude opus/sonnet, gpt non-o) restano agentic-eligibili anche
    // se hanno una modalita' thinking.
    let is_thinking =
        reasoning_only_family || (meta_reasoning.unwrap_or(false) && !is_hybrid_agentic(&p, &m));

    // Concetto B (non forzare tool_choice): tutti i reasoning + gli ibridi con
    // extended thinking (Claude opus/sonnet) + i Gemini 2.5 thinking.
    let uses_thinking_mode = is_reasoning_signal || is_hybrid_agentic(&p, &m) || gemini_25_thinking;

    // Policy agentica canonica (ADR 0025):
    //   - exclude: reasoning-only SENZA function calling (deepseek-reasoner).
    //   - native:  reasoning con tool nativi (OpenAI o-series).
    //   - disable_for_tools: dual-mode (deepseek-v4, magistral, gemini-pro, claude
    //     opus/sonnet, qualunque thinking tool-capable) -> non-thinking nei tool-loop.
    //   - none: modello non-thinking standard.
    let agentic_thinking_policy: &'static str = if p == "deepseek" && m.contains("reasoner") {
        "exclude"
    } else if p == "openai" && reasoning_only_family {
        "native"
    } else if is_reasoning_signal || is_hybrid_agentic(&p, &m) || gemini_25_thinking {
        "disable_for_tools"
    } else {
        "none"
    };

    ClassifiedCaps {
        supports_tool_use,
        supports_vision,
        is_thinking,
        uses_thinking_mode,
        agentic_thinking_policy,
    }
}

/// Modelli ibridi: ottimi per l'agentico (NON escludere, A=false) ma con
/// extended thinking che impone tool_choice non forzato (B=true).
fn is_hybrid_agentic(provider: &str, model_lc: &str) -> bool {
    match provider {
        "anthropic" => model_lc.contains("opus") || model_lc.contains("sonnet"),
        _ => false,
    }
}

/// PUNTO UNICO della regola di ammissione di un modello al catalog ABILITATO
/// (ADR 0025, regola L). True se (provider, model) e' ammesso dalla policy
/// `nexus_model_selection_policy`: matcha un `allowed_pattern` (o la lista e'
/// vuota) e NESSUN `denied_pattern`. Se non esiste una riga policy per il
/// provider, ammette (true) per non bloccare provider non ancora configurati.
/// La discovery la consulta prima di abilitare un modello, cosi' i modelli
/// legacy (pruned dalla mig 0320) non rientrano via probe-on-insert.
pub(crate) async fn model_passes_selection_policy(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> bool {
    let row: Option<(bool,)> = sqlx::query_as::<_, (bool,)>(
        "SELECT (
             ( $2 ~ ANY(allowed_patterns) OR cardinality(allowed_patterns) = 0 )
             AND NOT ( $2 ~ ANY(denied_patterns) )
         ) AS ok
         FROM nexus_model_selection_policy WHERE provider = $1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    row.map(|(ok,)| ok).unwrap_or(true)
}

/// Loop principale: chiamato da `main.rs` startup via `tokio::spawn(...)`.
/// `orchestrator` (Option) e' usato per il probe-on-insert dei nuovi modelli
/// scoperti dall'API discovery — se None, i nuovi modelli vengono inseriti
/// con `is_enabled=false` (comportamento legacy, sicuro ma richiede admin
/// per abilitare). In produzione e' sempre Some.
pub async fn catalog_sync_loop(db: PgPool, orchestrator: Option<Arc<Orchestrator>>) {
    // Boot: aspetta 30s per dare priorita' agli altri worker.
    tokio::time::sleep(Duration::from_secs(30)).await;
    tracing::info!("catalog_sync worker avviato");

    loop {
        match sync_tick(&db, orchestrator.as_deref()).await {
            Ok(stats) => {
                if stats.inserted > 0 || stats.disabled > 0 || stats.reenabled > 0 {
                    tracing::info!(
                        "catalog_sync: tick completato (provider_ok={} provider_skipped={} inserted={} disabled={} reenabled={})",
                        stats.providers_ok, stats.providers_skipped,
                        stats.inserted, stats.disabled, stats.reenabled,
                    );
                } else {
                    tracing::debug!(
                        "catalog_sync: tick completato (no changes, providers_ok={})",
                        stats.providers_ok,
                    );
                }
            }
            Err(e) => {
                tracing::warn!("catalog_sync: tick fallito: {}", e);
            }
        }

        // Dopo OGNI tick (anche se non ha trovato nuovi modelli), esegui
        // auto-upgrade della routing matrix + auto-populate escalation.
        // Senza questo i campi escalation_* della routing matrix restavano
        // NULL per i nuovi intent/modelli, e `lookup_with_budget` non
        // escalava mai a un modello piu' capable per task lunghi.
        if let Err(e) = crate::models::auto_upgrade_models_and_routing(&db).await {
            tracing::warn!("catalog_sync: auto_upgrade_models_and_routing fallito: {e}");
        }

        // Sleep fino al prossimo tick (interval dinamico dalle settings).
        let interval = load_interval_hours(&db).await;
        tokio::time::sleep(Duration::from_secs(interval * 3600)).await;
    }
}

#[derive(Debug, Default)]
struct SyncStats {
    providers_ok: u32,
    providers_skipped: u32,
    inserted: u32,
    disabled: u32,
    reenabled: u32,
}

async fn load_interval_hours(db: &PgPool) -> u64 {
    get_setting(db, "catalog_sync.interval_hours")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_HOURS)
        .max(1) // mai meno di 1h per non hammerare le API
}

/// Provider instradabili dall'endpoint /vision/describe (setting
/// `vision.routable_providers`, mig 0373). Punto unico DB-driven della lista
/// (regola G/L): `classify_capabilities` lo usa per marcare supports_vision e
/// `vision.py` lo legge per il routing/messaggio. Default vuoto se il setting
/// manca: fail-safe VISIBILE (nessun modello vision -> il purpose vision_describe
/// segnala no_capable), niente magic fallback hardcoded (regola G).
pub(crate) async fn load_vision_routable(db: &PgPool) -> std::collections::HashSet<String> {
    get_setting(db, "vision.routable_providers")
        .await
        .ok()
        .flatten()
        .map(|csv| {
            csv.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Trigger manuale sync (endpoint admin / test E2E).
pub async fn trigger_sync_now(
    db: &PgPool,
    orchestrator: Option<&Orchestrator>,
) -> Result<SyncSummary, String> {
    let stats = sync_tick(db, orchestrator)
        .await
        .map_err(|e| e.to_string())?;
    Ok(SyncSummary {
        providers_ok: stats.providers_ok,
        providers_skipped: stats.providers_skipped,
        inserted: stats.inserted,
        disabled: stats.disabled,
        reenabled: stats.reenabled,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct SyncSummary {
    pub providers_ok: u32,
    pub providers_skipped: u32,
    pub inserted: u32,
    pub disabled: u32,
    pub reenabled: u32,
}

async fn sync_tick(db: &PgPool, orchestrator: Option<&Orchestrator>) -> anyhow::Result<SyncStats> {
    // Honor enable flag.
    let enabled = get_setting(db, "catalog_sync.enabled")
        .await?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);
    if !enabled {
        tracing::debug!("catalog_sync: disabilitato via settings, skip tick");
        return Ok(SyncStats::default());
    }

    let providers_csv = get_setting(db, "catalog_sync.providers")
        .await?
        .unwrap_or_else(|| "anthropic,openai,mistral,deepseek,google".to_string());
    let disable_missing = get_setting(db, "catalog_sync.disable_missing")
        .await?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);
    let insert_new_disabled = get_setting(db, "catalog_sync.insert_new_disabled")
        .await?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true);

    let mut stats = SyncStats::default();
    for provider_raw in providers_csv.split(',') {
        let provider = provider_raw.trim();
        if provider.is_empty() {
            continue;
        }
        match sync_provider(
            db,
            provider,
            disable_missing,
            insert_new_disabled,
            orchestrator,
        )
        .await
        {
            Ok((ins, dis, re)) => {
                stats.providers_ok += 1;
                stats.inserted += ins;
                stats.disabled += dis;
                stats.reenabled += re;
            }
            Err(e) => {
                stats.providers_skipped += 1;
                tracing::warn!("catalog_sync[{}] skip: {}", provider, e);
            }
        }
    }
    Ok(stats)
}

async fn sync_provider(
    db: &PgPool,
    provider: &str,
    disable_missing: bool,
    insert_new_disabled: bool,
    orchestrator: Option<&Orchestrator>,
) -> anyhow::Result<(u32, u32, u32)> {
    // Fetch modelli live dal gateway (via UNICA per la discovery: il gateway
    // incapsula l'auth di ogni provider, Vertex Service Account incluso, quindi
    // non servono api_key qui ne' la delega al brain per Google — regola L).
    let api_models = fetch_provider_models(orchestrator, provider).await?;
    if api_models.is_empty() {
        anyhow::bail!("gateway ha ritornato lista vuota (sospetto, skip per safety)");
    }

    // Carica modelli del catalog locale per questo provider.
    // Il flag `manual_locked` viene letto da `auto_disabled_reason LIKE 'manual:%'`
    // — convenzione introdotta col fix Bug A (audit 27/05/2026) per evitare che
    // il worker riabiliti modelli che l'admin ha disabilitato manualmente
    // (es. modelli "preview" che l'API espone ma non accetta in inference).
    let catalog_rows = sqlx::query_as::<_, (String, bool, bool)>(
        "SELECT model, is_enabled, \
         (auto_disabled_reason IS NOT NULL AND auto_disabled_reason LIKE 'manual:%') AS manual_locked \
         FROM ai_price_catalog WHERE provider = $1",
    )
    .bind(provider)
    .fetch_all(db)
    .await?;

    // (model_name -> (is_enabled, manual_locked))
    let catalog_models: std::collections::HashMap<String, (bool, bool)> = catalog_rows
        .into_iter()
        .map(|(m, e, l)| (m, (e, l)))
        .collect();
    let api_set: std::collections::HashSet<&str> = api_models.iter().map(|s| s.as_str()).collect();

    let mut inserted = 0u32;
    let mut disabled = 0u32;
    let mut reenabled = 0u32;
    // Quante righe 'auto' di questo provider hanno avuto il performance_tier
    // riallineato all'euristica corrente in questa passata (vedi blocco Some).
    let mut tier_realigned = 0u32;

    // 1. Nuovi modelli dall'API non presenti nel catalog -> INSERT
    for api_model in &api_models {
        // Skip modelli non chat-compatibili (TTS, embedding, instruct legacy,
        // hollow placeholder) — vedi `is_chat_compatible_model` per la
        // blacklist consolidata da incidenti reali.
        if !is_chat_compatible_model(api_model) {
            tracing::debug!(
                "catalog_sync[{}]: skip '{}' (non chat-compatible)",
                provider,
                api_model
            );
            continue;
        }
        match catalog_models.get(api_model) {
            None => {
                if insert_new_disabled {
                    // performance_tier inferito dal nome (non il default 'medium'
                    // dello schema): evita che i modelli piccoli scoperti via API
                    // entrino come 'medium' nel pool agentico. Vedi
                    // infer_tier_from_name + migrazione 0354.
                    let inferred_tier = infer_tier_from_name(provider, api_model);
                    // pricing_state='unknown': costo 0 qui e' un PLACEHOLDER non
                    // raffinato, NON un modello gratuito (regola H, mig 0477). Niente
                    // promozione automatica a 'free': la distingue solo l'admin/seed.
                    let res = sqlx::query(
                        "INSERT INTO ai_price_catalog \
                         (provider, model, display_name, input_cost_per_million_tokens, \
                          output_cost_per_million_tokens, currency, capabilities, performance_tier, is_enabled, pricing_state, effective_from) \
                         VALUES ($1, $2, $3, 0, 0, 'USD', '[]'::jsonb, $4, false, 'unknown', NOW()) \
                         ON CONFLICT (provider, model) DO NOTHING",
                    )
                    .bind(provider)
                    .bind(api_model)
                    .bind(api_model)
                    .bind(inferred_tier)
                    .execute(db)
                    .await;
                    if let Ok(r) = res {
                        if r.rows_affected() > 0 {
                            inserted += 1;
                            audit_log(
                                db,
                                provider,
                                api_model,
                                "inserted",
                                json!({"source":"api_discovery"}),
                            )
                            .await;
                            tracing::info!(
                                "catalog_sync[{}]: + nuovo modello rilevato '{}'",
                                provider,
                                api_model
                            );

                            // Probe-on-insert: subito dopo l'INSERT (modello e'
                            // is_enabled=false di default), prova una chiamata
                            // di test al provider. Se passa, abilita; se fallisce
                            // con model_not_found/hollow, marca esplicitamente
                            // il motivo cosi' l'admin sa che NON va abilitato
                            // manualmente. Cosi' i modelli "fantasma" (es. la
                            // famiglia gemini-3.x al 05/2026) non possono mai
                            // entrare enabled via auto-discovery.
                            if let Some(orch) = orchestrator {
                                match probe_model_on_insert(orch, provider, api_model).await {
                                    ProbeOnInsertResult::Healthy => {
                                        // Infer capabilities dal nome: il modello e'
                                        // ora utilizzabile, ma il routing filtra per
                                        // capability matching e capabilities=[] lo
                                        // renderebbe invisibile. Popoliamo SOLO se
                                        // attualmente vuoto (rispetta override admin).
                                        let inferred_caps =
                                            infer_capabilities_from_name(provider, api_model);
                                        let caps_json = json!(inferred_caps);
                                        // Classificazione flag canonici (ADR 0024):
                                        // discovery -> solo euristica nome (niente
                                        // metadata LiteLLM). Scritti SOLO su righe
                                        // 'auto' (le 'manual' restano intatte).
                                        let vision_routable = load_vision_routable(db).await;
                                        let cc = classify_capabilities(
                                            provider,
                                            api_model,
                                            None,
                                            None,
                                            None,
                                            &vision_routable,
                                        );
                                        // Gate allowlist (ADR 0025): abilita SOLO se il
                                        // modello e' ammesso dalla model_selection_policy.
                                        // Cosi' i modelli legacy (pruned dalla 0320) non
                                        // rientrano via probe-on-insert.
                                        let allowed =
                                            model_passes_selection_policy(db, provider, api_model)
                                                .await;
                                        let _ = sqlx::query(
                                            "UPDATE ai_price_catalog \
                                             SET is_enabled = $8, \
                                                 auto_disabled_at = CASE WHEN $8 THEN NULL ELSE NOW() END, \
                                                 auto_disabled_reason = CASE WHEN $8 THEN NULL ELSE 'fuori model_selection_policy (mig 0320)' END, \
                                                 updated_at = NOW(), \
                                                 capabilities = CASE \
                                                     WHEN capabilities IS NULL OR capabilities = '[]'::jsonb \
                                                         THEN $3::jsonb \
                                                     ELSE capabilities \
                                                 END, \
                                                 supports_tool_use = CASE WHEN capability_source='auto' THEN $4 ELSE supports_tool_use END, \
                                                 supports_vision = CASE WHEN capability_source='auto' THEN $5 ELSE supports_vision END, \
                                                 is_thinking = CASE WHEN capability_source='auto' THEN $6 ELSE is_thinking END, \
                                                 uses_thinking_mode = CASE WHEN capability_source='auto' THEN $7 ELSE uses_thinking_mode END, \
                                                 agentic_thinking_policy = CASE WHEN capability_source='auto' THEN $9 ELSE agentic_thinking_policy END \
                                             WHERE provider = $1 AND model = $2 AND is_enabled = false",
                                        )
                                        .bind(provider)
                                        .bind(api_model)
                                        .bind(&caps_json)
                                        .bind(cc.supports_tool_use)
                                        .bind(cc.supports_vision)
                                        .bind(cc.is_thinking)
                                        .bind(cc.uses_thinking_mode)
                                        .bind(allowed)
                                        .bind(cc.agentic_thinking_policy)
                                        .execute(db)
                                        .await;
                                        audit_log(
                                            db, provider, api_model,
                                            if allowed { "probe_ok_on_insert" } else { "probe_ok_but_outside_policy" },
                                            json!({"action": if allowed {"auto_enabled"} else {"kept_disabled_outside_allowlist"}, "inferred_capabilities": inferred_caps}),
                                        )
                                        .await;
                                        tracing::info!(
                                            "catalog_sync[{}]: probe OK su '{}' -> {} (policy allowlist)",
                                            provider, api_model, if allowed {"abilitato"} else {"lasciato disabilitato (fuori allowlist)"}
                                        );
                                    }
                                    ProbeOnInsertResult::ModelBroken(kind) => {
                                        let reason = format!("failed_initial_probe:{}", kind);
                                        let _ = sqlx::query(
                                            "UPDATE ai_price_catalog \
                                             SET auto_disabled_at = NOW(), \
                                                 auto_disabled_reason = $3, updated_at = NOW() \
                                             WHERE provider = $1 AND model = $2",
                                        )
                                        .bind(provider)
                                        .bind(api_model)
                                        .bind(&reason)
                                        .execute(db)
                                        .await;
                                        audit_log(
                                            db,
                                            provider,
                                            api_model,
                                            "probe_failed_on_insert",
                                            json!({"reason": reason}),
                                        )
                                        .await;
                                        tracing::warn!(
                                            "catalog_sync[{}]: probe FAIL su nuovo modello '{}' (reason={}) -> resta disabled",
                                            provider, api_model, kind
                                        );
                                    }
                                    ProbeOnInsertResult::ProviderDown(kind) => {
                                        // Provider giu' (quota/billing/auth): non possiamo
                                        // sapere se il modello e' valido. Lasciamo disabled
                                        // con motivazione esplicita: il model_health_probe
                                        // worker (run periodico) lo riabilitera' quando il
                                        // provider torna up E il probe passa.
                                        let reason = format!("provider_down_on_insert:{}", kind);
                                        let _ = sqlx::query(
                                            "UPDATE ai_price_catalog SET auto_disabled_reason = $3, updated_at = NOW() \
                                             WHERE provider = $1 AND model = $2",
                                        )
                                        .bind(provider)
                                        .bind(api_model)
                                        .bind(&reason)
                                        .execute(db)
                                        .await;
                                        tracing::info!(
                                            "catalog_sync[{}]: probe inconclusive (provider down: {}) su '{}' -> resta disabled, model_health_probe lo rivedra'",
                                            provider, kind, api_model
                                        );
                                    }
                                    ProbeOnInsertResult::Inconclusive(reason) => {
                                        tracing::debug!(
                                            "catalog_sync[{}]: probe inconclusive su '{}': {} -> resta disabled (default)",
                                            provider, api_model, reason
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some(&(is_enabled, manual_locked)) => {
                // FIX A (regola H): riallinea supports_vision dei modelli GIA'
                // presenti (capability_source='auto') da classify_capabilities a
                // ogni passata. Prima il sync toccava SOLO is_enabled: una
                // correzione dell'euristica vision (o la rimozione di un falso
                // positivo come mistral) non si propagava ai modelli esistenti ->
                // serviva una migrazione-dati manuale (es. 0372). Ora si propaga
                // da sola. Le righe 'manual' (curate dall'admin) restano intatte.
                {
                    let vision_routable = load_vision_routable(db).await;
                    let cc = classify_capabilities(
                        provider,
                        api_model,
                        None,
                        None,
                        None,
                        &vision_routable,
                    );
                    let _ = sqlx::query(
                        "UPDATE ai_price_catalog SET supports_vision = $3, updated_at = NOW() \
                         WHERE provider = $1 AND model = $2 AND capability_source = 'auto' \
                           AND supports_vision <> $3",
                    )
                    .bind(provider)
                    .bind(api_model)
                    .bind(cc.supports_vision)
                    .execute(db)
                    .await;

                    // Stesso principio (regola H + L) per performance_tier: la
                    // correzione dell'euristica infer_tier_from_name (es. i
                    // flagship con naming recente) deve propagarsi alle righe
                    // 'auto' GIA' presenti, non solo ai nuovi insert. Prima il
                    // tier era scritto SOLO all'INSERT (ON CONFLICT DO NOTHING),
                    // quindi i modelli flagship restavano 'medium' e l'auto-
                    // promoter non li selezionava come 'heavy'. UPDATE mirato
                    // (solo dove il tier differisce); le righe 'manual' restano
                    // intatte (guard capability_source='auto').
                    let inferred_tier = infer_tier_from_name(provider, api_model);
                    let tier_res = sqlx::query(
                        "UPDATE ai_price_catalog SET performance_tier = $3, updated_at = NOW() \
                         WHERE provider = $1 AND model = $2 AND capability_source = 'auto' \
                           AND performance_tier IS DISTINCT FROM $3",
                    )
                    .bind(provider)
                    .bind(api_model)
                    .bind(inferred_tier)
                    .execute(db)
                    .await;
                    if let Ok(r) = tier_res {
                        if r.rows_affected() > 0 {
                            tier_realigned += 1;
                            tracing::info!(
                                "catalog_sync[{}]: tier riallineato '{}' -> {}",
                                provider,
                                api_model,
                                inferred_tier
                            );
                        }
                    }
                }
                // Re-enable SOLO se il modello e' ammesso dalla policy (ADR 0025):
                // un legacy ricomparso nell'API non deve rientrare.
                let policy_ok = model_passes_selection_policy(db, provider, api_model).await;
                if !is_enabled && !manual_locked && policy_ok {
                    // Modello disabilitato dal worker (missing_from_api) ma ricomparso: re-enable.
                    // Il reason si azzera SOLO se appartiene al ciclo is_enabled: i reason del
                    // ciclo tool-capability ('malformed_tool_calls', 'tool_probe_failed:%')
                    // vanno PRESERVATI — azzerarli lasciava supports_tool_use=false orfano
                    // (reason NULL), irraggiungibile dal ri-test del probe (incidente
                    // magistral-small-2509, 2026-06-10).
                    let sql = format!(
                        "UPDATE ai_price_catalog SET is_enabled = true, effective_from = NOW(), \
                         auto_disabled_at = NULL, \
                         auto_disabled_reason = CASE WHEN {tool_reason} \
                                                     THEN auto_disabled_reason \
                                                     ELSE NULL END, \
                         updated_at = NOW() \
                         WHERE provider = $1 AND model = $2",
                        tool_reason = crate::tool_capability::TOOL_REASON_PREDICATE_SQL
                    );
                    let res = sqlx::query(&sql)
                    .bind(provider)
                    .bind(api_model)
                    .execute(db)
                    .await;
                    if let Ok(r) = res {
                        if r.rows_affected() > 0 {
                            reenabled += 1;
                            audit_log(db, provider, api_model, "reenabled", json!({})).await;
                            tracing::info!(
                                "catalog_sync[{}]: re-enabled '{}' (ricomparso API)",
                                provider,
                                api_model
                            );
                        }
                    }
                } else if !is_enabled && manual_locked {
                    // Skip: admin lo ha disabilitato manualmente, non riabilitare anche se ricompare.
                    tracing::debug!(
                        "catalog_sync[{}]: skip re-enable '{}' (manual_locked)",
                        provider,
                        api_model
                    );
                }
            }
        }
    }

    // 1bis. Modelli gia' nel catalog (is_enabled=true) che NON passano la
    // blacklist `is_chat_compatible_model`: vanno auto-disabilitati. Senza
    // questo passo, l'unico controllo di compatibility e' all'INSERT (riga
    // sopra), quindi un modello entrato in catalog prima dell'aggiornamento
    // della blacklist (es. gemini-3.5-flash, regola H CLAUDE.md) resta ON
    // per sempre. `manual_locked` viene rispettato: se l'admin ha deciso di
    // tenerlo abilitato a mano, non lo tocchiamo.
    for (catalog_model, (is_enabled, manual_locked)) in &catalog_models {
        if *is_enabled && !manual_locked {
            // Prune self-healing (ADR 0025): un modello enabled va disabilitato se
            // non e' chat-compatibile (blacklist) OPPURE non passa la
            // model_selection_policy (famiglia legacy). Cosi' i modelli vecchi non
            // restano enabled neanche se entrati prima dell'aggiornamento policy.
            let chat_ok = is_chat_compatible_model(catalog_model);
            let policy_ok = model_passes_selection_policy(db, provider, catalog_model).await;
            if !chat_ok || !policy_ok {
                let reason = if !chat_ok {
                    "not_chat_compatible"
                } else {
                    "fuori model_selection_policy (legacy)"
                };
                let res = sqlx::query(
                    "UPDATE ai_price_catalog SET is_enabled = false, \
                     auto_disabled_at = NOW(), auto_disabled_reason = $3 \
                     WHERE provider = $1 AND model = $2",
                )
                .bind(provider)
                .bind(catalog_model)
                .bind(reason)
                .execute(db)
                .await;
                if let Ok(r) = res {
                    if r.rows_affected() > 0 {
                        disabled += 1;
                        audit_log(
                            db,
                            provider,
                            catalog_model,
                            "disabled",
                            json!({ "reason": reason }),
                        )
                        .await;
                        tracing::warn!(
                            "catalog_sync[{}]: disabled '{}' ({})",
                            provider,
                            catalog_model,
                            reason,
                        );
                    }
                }
            }
        }
    }

    // 2. Modelli del catalog enabled non piu' nell'API -> disable
    if disable_missing {
        for (catalog_model, (is_enabled, _manual_locked)) in &catalog_models {
            if *is_enabled && !api_set.contains(catalog_model.as_str()) {
                // Skip alias "senza data" (es. claude-haiku-4-5) se l'API ritorna lo
                // stesso modello con suffisso data (es. claude-haiku-4-5-20251001).
                // Anthropic ritorna solo dated, ma il catalog/routing usa l'alias
                // perche' e' piu' stabile (l'alias punta sempre alla versione corrente).
                let has_dated_in_api = api_models.iter().any(|api_m| {
                    if !api_m.starts_with(catalog_model.as_str())
                        || api_m.len() <= catalog_model.len()
                    {
                        return false;
                    }
                    let suffix = &api_m[catalog_model.len()..];
                    suffix.starts_with('-')
                        && suffix.len() == 9
                        && suffix[1..].chars().all(|c| c.is_ascii_digit())
                });
                if has_dated_in_api {
                    continue; // alias preservato (es. claude-haiku-4-5)
                }

                // Skip alias con suffisso data se la base name e' nell'API.
                // Es: catalog "claude-sonnet-4-6-20251201" disabilitato solo se
                // anche "claude-sonnet-4-6" non e' nell'API.
                let base_name = strip_date_suffix(catalog_model);
                if base_name.as_str() != catalog_model.as_str()
                    && api_set.contains(base_name.as_str())
                {
                    continue;
                }

                // FIX 2 (catalog_sync probe-aware): la lista upstream LiteLLM/
                // provider e' un INDIZIO, non la verita'. La verita' e' l'account:
                // se il probe (model_health_probe) trova ancora il modello SANO,
                // non spegnerlo solo perche' "datato" / non piu' in lista. Cosi'
                // evitiamo l'inversione diagnosticata (modello funzionante per
                // l'account disabilitato perche' rimosso da upstream).
                // Disabilitiamo solo se anche l'health reale lo conferma rotto.
                if model_recently_healthy(db, provider, catalog_model).await {
                    // Annotazione diagnostica idempotente: il modello resta
                    // is_enabled=true (NON tocchiamo auto_disabled_*), ma
                    // l'audit_log registra la decisione cosi' e' rintracciabile
                    // perche' un modello "datato" e' rimasto attivo.
                    tracing::info!(
                        "catalog_sync[{}]: '{}' assente da upstream MA probe recente healthy -> NON disabilito (legacy, lascio is_enabled=true)",
                        provider, catalog_model,
                    );
                    audit_log(
                        db,
                        provider,
                        catalog_model,
                        "kept_enabled_legacy",
                        json!({"reason":"missing_from_api_but_recently_healthy"}),
                    )
                    .await;
                    continue;
                }

                let res = sqlx::query(
                    "UPDATE ai_price_catalog SET is_enabled = false, \
                     auto_disabled_at = NOW(), auto_disabled_reason = 'missing_from_api' \
                     WHERE provider = $1 AND model = $2",
                )
                .bind(provider)
                .bind(catalog_model)
                .execute(db)
                .await;
                if let Ok(r) = res {
                    if r.rows_affected() > 0 {
                        disabled += 1;
                        audit_log(
                            db,
                            provider,
                            catalog_model,
                            "disabled",
                            json!({"reason":"missing_from_api"}),
                        )
                        .await;
                        tracing::warn!(
                            "catalog_sync[{}]: - disabled '{}' (non piu nell API)",
                            provider,
                            catalog_model,
                        );
                    }
                }
            }
        }
    }

    if tier_realigned > 0 {
        tracing::info!(
            "catalog_sync[{}]: performance_tier riallineato su {} righe 'auto'",
            provider,
            tier_realigned
        );
    }

    Ok((inserted, disabled, reenabled))
}

/// Rimuove suffisso data ISO (es. -20251201) dal model name per gestire alias.
fn strip_date_suffix(model: &str) -> String {
    // Pattern: trailing -YYYYMMDD (8 digits dopo dash)
    if let Some(idx) = model.rfind('-') {
        let suffix = &model[idx + 1..];
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return model[..idx].to_string();
        }
    }
    model.to_string()
}

/// FIX 2: verifica se un modello e' "recentemente sano" secondo l'account,
/// non secondo la lista upstream. Usato dal catalog_sync per NON disabilitare
/// modelli assenti da upstream ma ancora funzionanti per l'account.
///
/// Sano = (a) esiste un health check recente con healthy=true entro la finestra
///         `agent.catalog_sync_health_window_hours` (default 24h), OPPURE
///        (b) il catalog riporta consecutive_failures=0 per il modello (mai
///         fallito un probe model-specific).
///
/// Conservativo: se la query fallisce, ritorna `false` (cioe' lascia procedere
/// la disabilitazione come prima — niente nuovi falsi-positivi su DB down).
async fn model_recently_healthy(db: &PgPool, provider: &str, model: &str) -> bool {
    // Finestra di freschezza DB-driven (regola G: niente hardcode magico).
    let window_hours: i64 = get_setting(db, "agent.catalog_sync_health_window_hours")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|h| *h > 0)
        .unwrap_or(24);

    // (a) Ultimo health check recente healthy=true.
    let recent_healthy: Option<bool> = sqlx::query_scalar(
        "SELECT healthy
           FROM ai_model_health_history
          WHERE provider = $1 AND model = $2
            AND checked_at >= NOW() - make_interval(hours => $3)
          ORDER BY checked_at DESC
          LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .bind(window_hours as i32)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if matches!(recent_healthy, Some(true)) {
        return true;
    }

    // (b) Fallback: nessun probe recente, ma il catalog non registra fallimenti
    // model-specific consecutivi -> consideriamo il modello ancora valido.
    // (Se un probe l'avesse trovato rotto, consecutive_failures sarebbe > 0.)
    if recent_healthy.is_none() {
        let cf: Option<i32> = sqlx::query_scalar(
            "SELECT consecutive_failures FROM ai_price_catalog
              WHERE provider = $1 AND model = $2",
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
        if matches!(cf, Some(0)) {
            return true;
        }
    }

    false
}

async fn audit_log(db: &PgPool, provider: &str, model: &str, action: &str, details: Value) {
    let _ = sqlx::query(
        "INSERT INTO ai_price_catalog_audit (provider, model, action, details) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(provider)
    .bind(model)
    .bind(action)
    .bind(details)
    .execute(db)
    .await;
}

/// Discovery dei modelli live di un provider tramite il Nexus Gateway
/// (`GET /v1/models/{provider}`), via UNICA per l'autodiscovery (regola L).
///
/// Il gateway incapsula l'auth di OGNI provider — incluso Vertex con Service
/// Account — quindi qui non servono api_key ne' la vecchia delega al brain per
/// Google (`/providers/google/models/live`): un solo punto di accesso HTTP
/// (`NexusGatewayClient::list_models`) sostituisce le chiamate dirette agli
/// endpoint `api.{provider}.com/v1/models`.
///
/// La logica a valle (filtro chat-compat, infer capabilities/tier, classify,
/// upsert) resta invariata: cambia solo la SORGENTE della lista nomi modello.
async fn fetch_provider_models(
    orchestrator: Option<&Orchestrator>,
    provider: &str,
) -> anyhow::Result<Vec<String>> {
    let gateway = orchestrator
        .and_then(|orch| orch.nexus_gateway.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!("Nexus Gateway non disponibile: autodiscovery modelli impossibile")
        })?;
    gateway.list_models(provider).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_tier_mistral_small_families_are_light() {
        // Caso del bug: ministral/small/nemo NON devono essere medium.
        assert_eq!(
            infer_tier_from_name("mistral", "ministral-8b-2512"),
            "light"
        );
        assert_eq!(
            infer_tier_from_name("mistral", "ministral-3b-latest"),
            "light"
        );
        assert_eq!(
            infer_tier_from_name("mistral", "mistral-small-2506"),
            "light"
        );
        assert_eq!(
            infer_tier_from_name("mistral", "magistral-small-latest"),
            "light"
        );
        assert_eq!(
            infer_tier_from_name("mistral", "open-mistral-nemo-2407"),
            "light"
        );
        // I capaci restano medium (Mistral non ha heavy reale).
        assert_eq!(
            infer_tier_from_name("mistral", "mistral-large-latest"),
            "medium"
        );
        assert_eq!(
            infer_tier_from_name("mistral", "mistral-medium-3"),
            "medium"
        );
        assert_eq!(
            infer_tier_from_name("mistral", "codestral-latest"),
            "medium"
        );
        assert_eq!(infer_tier_from_name("mistral", "devstral-2512"), "medium");
    }

    #[test]
    fn test_infer_tier_other_providers() {
        // Google: pro=heavy, flash*=light.
        assert_eq!(infer_tier_from_name("google", "gemini-2.5-pro"), "heavy");
        assert_eq!(infer_tier_from_name("google", "gemini-2.5-flash"), "light");
        assert_eq!(
            infer_tier_from_name("google", "gemini-2.5-flash-lite"),
            "light"
        );
        // Anthropic: opus=heavy, sonnet=medium, haiku=light.
        assert_eq!(
            infer_tier_from_name("anthropic", "claude-opus-4-6"),
            "heavy"
        );
        assert_eq!(
            infer_tier_from_name("anthropic", "claude-sonnet-4-6"),
            "medium"
        );
        assert_eq!(
            infer_tier_from_name("anthropic", "claude-haiku-4-5-20251001"),
            "light"
        );
        // OpenAI: o3/o1/pro=heavy, nano/mini=light, resto medium.
        assert_eq!(infer_tier_from_name("openai", "o3"), "heavy");
        assert_eq!(
            infer_tier_from_name("openai", "gpt-5.4-pro-2026-03-05"),
            "heavy"
        );
        assert_eq!(infer_tier_from_name("openai", "gpt-4.1-nano"), "light");
        assert_eq!(infer_tier_from_name("openai", "o4-mini"), "light");
        assert_eq!(infer_tier_from_name("openai", "gpt-4.1"), "medium");
    }

    #[test]
    fn test_infer_tier_flagship_naming_recente() {
        // OpenAI: i flagship gpt-5+ senza suffisso mini/nano sono heavy
        // (prima erano erroneamente medium perche' l'euristica marcava heavy
        // solo con 'pro' nel nome). Robusto alle versioni future via major.
        assert_eq!(infer_tier_from_name("openai", "gpt-5"), "heavy");
        assert_eq!(infer_tier_from_name("openai", "gpt-5.5"), "heavy");
        assert_eq!(infer_tier_from_name("openai", "gpt-5.4"), "heavy");
        assert_eq!(infer_tier_from_name("openai", "gpt-6"), "heavy");
        // Le varianti piccole di un flagship restano light.
        assert_eq!(infer_tier_from_name("openai", "gpt-5.4-mini"), "light");
        assert_eq!(infer_tier_from_name("openai", "gpt-5-nano"), "light");
        // chat-latest = variante chat veloce, non flagship reasoning -> medium.
        assert_eq!(
            infer_tier_from_name("openai", "gpt-5-chat-latest"),
            "medium"
        );
        // I gpt precedenti (4.x, 4o) restano medium.
        assert_eq!(infer_tier_from_name("openai", "gpt-4o"), "medium");
        assert_eq!(infer_tier_from_name("openai", "gpt-4.1"), "medium");
        // Anthropic: naming nuovo opus-4-8 deve restare heavy (caso del bug).
        assert_eq!(
            infer_tier_from_name("anthropic", "claude-opus-4-8"),
            "heavy"
        );
    }

    #[test]
    fn test_is_chat_compatible_no_per_name_blacklist() {
        // La blacklist hardcoded per famiglia gemini-3.x e' stata RIMOSSA:
        // il filtraggio dei modelli fantasma e' ora fatto dal probe attivo
        // (probe_model_on_insert) al primo discovery. La blacklist semantica
        // qui sotto resta solo per i modelli che per DESIGN non sono chat
        // completion (TTS, embedding, realtime, instruct legacy).
        // Quindi questi devono passare la blacklist (e poi il probe deciderà):
        assert!(is_chat_compatible_model("gemini-3.5-flash"));
        assert!(is_chat_compatible_model("gemini-3-pro-preview"));
        assert!(is_chat_compatible_model("gemini-1.0-pro"));
        assert!(is_chat_compatible_model("gemini-2.5-pro"));
        assert!(is_chat_compatible_model("gpt-5-future"));
        assert!(is_chat_compatible_model("claude-5-sonnet"));
    }

    #[test]
    fn test_is_chat_compatible_blacklist_other_providers() {
        // Smoke test: la blacklist storica continua a funzionare.
        assert!(!is_chat_compatible_model("tts-1"));
        assert!(!is_chat_compatible_model("text-embedding-3-small"));
        assert!(!is_chat_compatible_model("gpt-4o-realtime-preview"));
        assert!(!is_chat_compatible_model("whisper-1"));
        // Modelli chat veri restano OK.
        assert!(is_chat_compatible_model("claude-sonnet-4-6"));
        assert!(is_chat_compatible_model("gpt-4o"));
        assert!(is_chat_compatible_model("mistral-large-latest"));
    }

    #[test]
    fn test_strip_date_suffix() {
        assert_eq!(strip_date_suffix("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!(
            strip_date_suffix("claude-sonnet-4-6-20251201"),
            "claude-sonnet-4-6"
        );
        assert_eq!(strip_date_suffix("gpt-4o-mini"), "gpt-4o-mini");
        assert_eq!(
            strip_date_suffix("gpt-4o-mini-2024-07-18"),
            "gpt-4o-mini-2024-07-18"
        );
        // (sopra ha 2 digits-2 digits-2 digits, non matcha 8 digits)
        assert_eq!(strip_date_suffix("ministral-8b-2512"), "ministral-8b-2512");
    }

    // ── ADR 0024: classificatore unico delle capability ──

    /// Routable di test: i provider con ramo vision.py (setting reale: mig 0373).
    fn rt() -> std::collections::HashSet<String> {
        ["google", "anthropic", "openai"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn classify_metadata_litellm_ha_priorita() {
        // function_calling/vision espliciti vincono sull'euristica.
        let c = classify_capabilities(
            "openai",
            "gpt-4o",
            Some(true),
            Some(true),
            Some(false),
            &rt(),
        );
        assert!(c.supports_tool_use);
        assert!(c.supports_vision);
        assert!(!c.is_thinking);
        assert!(!c.uses_thinking_mode);
    }

    #[test]
    fn classify_o_series_e_reasoning_only_escluso_da_agentico() {
        // o-series: reasoning-only -> A (escludi) e B (non forzare) entrambi true.
        let c = classify_capabilities("openai", "o3-mini", None, None, None, &rt());
        assert!(c.is_thinking, "o-series deve essere escluso da agentico");
        assert!(c.uses_thinking_mode);
    }

    #[test]
    fn classify_deepseek_v4_reasoning_only() {
        // deepseek-v4-pro: reasoning-only (no reasoning_content passback) -> A+B.
        let c = classify_capabilities("deepseek", "deepseek-v4-pro", None, None, None, &rt());
        assert!(c.is_thinking);
        assert!(c.uses_thinking_mode);
    }

    #[test]
    fn classify_deepseek_v4_famiglia_tool_capable() {
        // Verita' di famiglia (verificata live 2026-06-10): i deepseek-v4 fanno
        // function calling. tool_use=true ANCHE con metadata assente o false
        // (il FALSE storico era un degrado runtime, non una verita' provider).
        // Policy dual-mode: 'disable_for_tools' (non-thinking nei tool-loop).
        for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
            for meta in [None, Some(false)] {
                let c = classify_capabilities("deepseek", model, meta, None, None, &rt());
                assert!(
                    c.supports_tool_use,
                    "{model} (meta_tool_use={meta:?}) deve restare tool-capable"
                );
                assert_eq!(c.agentic_thinking_policy, "disable_for_tools");
            }
        }
    }

    #[test]
    fn classify_claude_ibrido_agentico_ma_thinking_mode() {
        // Claude opus/sonnet: NON escluso da agentico (A=false) ma extended
        // thinking -> non forzare tool_choice (B=true). Caso che il merge naïf
        // avrebbe rotto.
        let c = classify_capabilities("anthropic", "claude-sonnet-4-6", None, None, None, &rt());
        assert!(!c.is_thinking, "Claude deve restare agentic-eligibile");
        assert!(
            c.uses_thinking_mode,
            "Claude usa extended thinking -> non forzare"
        );
        assert!(c.supports_vision);
    }

    #[test]
    fn classify_modello_chat_standard_non_thinking() {
        // mistral-large: tool-capable, non-thinking -> candidato agentico ideale.
        let c = classify_capabilities("mistral", "mistral-large-2411", None, None, None, &rt());
        assert!(c.supports_tool_use);
        assert!(!c.is_thinking);
        assert!(!c.uses_thinking_mode);
    }

    #[test]
    fn classify_vision_solo_provider_instradabili() {
        // La capability vision riflette l'instradabilita' da vision.py (google,
        // anthropic, openai). I provider SENZA ramo vision (mistral, deepseek)
        // devono avere supports_vision=false ANCHE se LiteLLM passa meta_vision=true
        // (falso positivo): altrimenti best_model_for_tier li sceglierebbe per il
        // purpose vision_describe -> /vision/describe 501.
        let mistral = classify_capabilities(
            "mistral",
            "mistral-small-latest",
            None,
            Some(true),
            None,
            &rt(),
        );
        assert!(
            !mistral.supports_vision,
            "mistral non e' instradabile da vision.py: vision=false"
        );
        let pixtral = classify_capabilities(
            "mistral",
            "pixtral-large-latest",
            None,
            Some(true),
            None,
            &rt(),
        );
        assert!(
            !pixtral.supports_vision,
            "pixtral non e' instradabile finche' vision.py non supporta mistral"
        );
        let deepseek =
            classify_capabilities("deepseek", "deepseek-chat", None, Some(true), None, &rt());
        assert!(
            !deepseek.supports_vision,
            "deepseek non e' instradabile: vision=false"
        );
        // Provider instradabili: vision corretta.
        assert!(
            classify_capabilities("google", "gemini-2.5-flash-lite", None, None, None, &rt())
                .supports_vision
        );
        assert!(
            classify_capabilities("anthropic", "claude-haiku-4-5", None, None, None, &rt())
                .supports_vision
        );
        assert!(classify_capabilities("openai", "gpt-4o", None, None, None, &rt()).supports_vision);
        // Per i provider instradabili, meta_vision esplicito e' rispettato.
        assert!(
            classify_capabilities("openai", "gpt-4o-mini", None, Some(true), None, &rt())
                .supports_vision
        );
    }

    #[test]
    fn classify_gemini_25_thinking_dual_mode() {
        // Gemini 2.5 (pro E flash) ha il thinking attivo di default: e' dual-mode,
        // NON reasoning-only. Quindi is_thinking=false (eleggibile all'agentico),
        // uses_thinking_mode=true e policy='disable_for_tools' (non-thinking nei
        // tool-loop -> niente MALFORMED_FUNCTION_CALL). flash-lite NON ha thinking.
        let flash = classify_capabilities("google", "gemini-2.5-flash", None, None, None, &rt());
        assert!(
            !flash.is_thinking,
            "gemini-2.5-flash NON va escluso dall'agentico"
        );
        assert!(flash.uses_thinking_mode, "gemini-2.5-flash e' thinking");
        assert_eq!(flash.agentic_thinking_policy, "disable_for_tools");
        let pro = classify_capabilities("google", "gemini-2.5-pro", None, None, None, &rt());
        assert!(
            !pro.is_thinking,
            "gemini-2.5-pro e' dual-mode, non reasoning-only"
        );
        assert_eq!(pro.agentic_thinking_policy, "disable_for_tools");
        let lite =
            classify_capabilities("google", "gemini-2.5-flash-lite", None, None, None, &rt());
        assert!(
            !lite.uses_thinking_mode,
            "gemini-2.5-flash-lite NON e' thinking"
        );
        assert_eq!(lite.agentic_thinking_policy, "none");
    }

    // ── ADR 0025: agentic_thinking_policy per famiglia ──

    #[test]
    fn classify_agentic_thinking_policy_per_famiglia() {
        let p = |prov, model| {
            classify_capabilities(prov, model, None, None, None, &rt()).agentic_thinking_policy
        };
        // Reasoning-only senza function calling -> exclude.
        assert_eq!(p("deepseek", "deepseek-reasoner"), "exclude");
        // OpenAI o-series: tool nativi -> native.
        assert_eq!(p("openai", "o3-mini"), "native");
        // Dual-mode (thinking + tool-capable) -> disable_for_tools.
        assert_eq!(p("deepseek", "deepseek-v4-pro"), "disable_for_tools");
        assert_eq!(p("anthropic", "claude-sonnet-4-6"), "disable_for_tools");
        assert_eq!(p("google", "gemini-2.5-pro"), "disable_for_tools");
        assert_eq!(p("mistral", "magistral-medium-latest"), "disable_for_tools");
        // Modelli non-thinking standard -> none.
        assert_eq!(p("openai", "gpt-4o"), "none");
        assert_eq!(p("mistral", "mistral-large-2411"), "none");
        // Gemini 2.5 flash/pro: thinking di default -> disable_for_tools.
        // flash-lite NON e' thinking -> none.
        assert_eq!(p("google", "gemini-2.5-flash"), "disable_for_tools");
        assert_eq!(p("google", "gemini-2.5-flash-lite"), "none");
    }

    #[test]
    fn classify_magistral_tool_use_uniforme_per_famiglia() {
        // I magistral (linea reasoning Mistral) supportano function calling
        // (doc Mistral). La classificazione DEVE essere uniforme per l'intera
        // famiglia, indipendentemente dalla variante e dal metadata LiteLLM
        // (che e' incoerente: alcune varianti ritornano tool_use=false). Tutte:
        //   - supports_tool_use=true (verita' Mistral, per famiglia)
        //   - is_thinking=true + uses_thinking_mode=true (linea reasoning)
        //   - agentic_thinking_policy='disable_for_tools' (non-thinking nei tool)
        for model in [
            "magistral-small-latest",
            "magistral-medium-latest",
            "magistral-small-2509",
            "magistral-medium-2509",
            "magistral-medium-1-2-2509",
        ] {
            // Metadata assente.
            let c = classify_capabilities("mistral", model, None, None, None, &rt());
            assert!(
                c.supports_tool_use,
                "{model}: tool_use deve essere true (doc Mistral)"
            );
            assert!(c.is_thinking, "{model}: e' reasoning-only -> is_thinking");
            assert!(
                c.uses_thinking_mode,
                "{model}: e' reasoning -> uses_thinking_mode"
            );
            assert_eq!(
                c.agentic_thinking_policy, "disable_for_tools",
                "{model}: reasoning dual-mode -> disable_for_tools"
            );
            // REGRESSIONE: anche col metadata LiteLLM incoerente (tool_use=false),
            // la classificazione per famiglia forza true. E' il bug che causava il
            // degrado a supports_tool_use=false di alcune varianti magistral.
            let c_bad_meta =
                classify_capabilities("mistral", model, Some(false), None, None, &rt());
            assert!(
                c_bad_meta.supports_tool_use,
                "{model}: il metadata tool_use=false di LiteLLM va ignorato per la famiglia magistral"
            );
        }
    }
}
