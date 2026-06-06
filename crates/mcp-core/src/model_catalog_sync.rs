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
//!   2. Per ogni provider con `{provider}_api_key` nelle settings:
//!      - GET https://api.{provider}.com/v1/models (formato OpenAI-compatible
//!        per openai/mistral/deepseek; Anthropic ha header speciale)
//!      - Confronta con catalog: INSERT nuovi (is_enabled=false, prezzi 0
//!        da raffinare manualmente), DISABLE modelli non piu' esposti
//!   3. Audit ogni delta in `ai_price_catalog_audit`
//!   4. Emit notification dispatcher 'CatalogModelChanged' per admin
//!
//! Provider Google: sync via brain REST (`/providers/google/models/live`) che
//! gira il SDK Python google-genai con Service Account dal DB (Vertex) o API
//! key (Gemini direct). Il worker Rust non puo' parlare Vertex direct perche'
//! l'auth Google richiede google-auth con private_key RSA — meglio centralizzare
//! il client nel brain dove e' gia' implementato (vedi google_provider.py).
//!
//! Provider locali (ollama/vllm): skipped, catalog manuale per setup custom.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::model_health_probe::{probe_model_on_insert, ProbeOnInsertResult};
use crate::orchestrator::Orchestrator;
use crate::settings::get_setting;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
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
) -> ClassifiedCaps {
    let p = provider.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();

    // ── tool_use: metadata esplicito, altrimenti default true (la stragrande
    //    maggioranza dei modelli chat moderni supporta function calling). ──
    let supports_tool_use = meta_tool_use.unwrap_or(true);

    // ── vision: metadata esplicito, altrimenti euristica per famiglia. ──
    let supports_vision = meta_vision.unwrap_or_else(|| match p.as_str() {
        "openai" => {
            m.starts_with("gpt-4o")
                || m.starts_with("gpt-4.1")
                || m.starts_with("gpt-4-turbo")
                || m.starts_with("o1")
                || m.starts_with("o3")
        }
        "anthropic" => m.contains("opus") || m.contains("sonnet"),
        "google" => m.contains("gemini"),
        _ => false,
    });

    // ── Detection reasoning model (guida sia A sia B). ──
    // Famiglie reasoning-only note (incompatibili col tool-forcing o col loop
    // multi-turno nell'integrazione attuale).
    let reasoning_only_family = match p.as_str() {
        // OpenAI o-series: o1/o3/o4 (prefisso "o" + cifra).
        "openai" => {
            m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
        }
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
    let gemini_25_thinking =
        p == "google" && m.contains("gemini-2.5") && !m.contains("flash-lite");
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
    let is_thinking = reasoning_only_family
        || (meta_reasoning.unwrap_or(false) && !is_hybrid_agentic(&p, &m));

    // Concetto B (non forzare tool_choice): tutti i reasoning + gli ibridi con
    // extended thinking (Claude opus/sonnet) + i Gemini 2.5 thinking.
    let uses_thinking_mode =
        is_reasoning_signal || is_hybrid_agentic(&p, &m) || gemini_25_thinking;

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
pub(crate) async fn model_passes_selection_policy(db: &PgPool, provider: &str, model: &str) -> bool {
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
    // Caso Google: il sync passa per il brain (Vertex SDK con Service Account
    // dal DB). Non serve api_key qui — il brain risolve autonomamente backend
    // gemini/vertex e auth via google_provider_backend setting.
    let api_key = if provider == "google" {
        String::new()
    } else {
        get_setting(db, &format!("{}_api_key", provider))
            .await?
            .ok_or_else(|| anyhow::anyhow!("api_key non configurata"))?
    };

    // Fetch modelli dall'API provider.
    let api_models = fetch_provider_models(provider, &api_key).await?;
    if api_models.is_empty() {
        anyhow::bail!("API ha ritornato lista vuota (sospetto, skip per safety)");
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
                    let res = sqlx::query(
                        "INSERT INTO ai_price_catalog \
                         (provider, model, display_name, input_cost_per_million_tokens, \
                          output_cost_per_million_tokens, currency, capabilities, is_enabled, effective_from) \
                         VALUES ($1, $2, $3, 0, 0, 'USD', '[]'::jsonb, false, NOW()) \
                         ON CONFLICT (provider, model) DO NOTHING",
                    )
                    .bind(provider)
                    .bind(api_model)
                    .bind(api_model)
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
                                        let cc = classify_capabilities(
                                            provider, api_model, None, None, None,
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
                // Re-enable SOLO se il modello e' ammesso dalla policy (ADR 0025):
                // un legacy ricomparso nell'API non deve rientrare.
                let policy_ok = model_passes_selection_policy(db, provider, api_model).await;
                if !is_enabled && !manual_locked && policy_ok {
                    // Modello disabilitato dal worker (missing_from_api) ma ricomparso: re-enable.
                    let res = sqlx::query(
                        "UPDATE ai_price_catalog SET is_enabled = true, effective_from = NOW(), \
                         auto_disabled_at = NULL, auto_disabled_reason = NULL \
                         WHERE provider = $1 AND model = $2",
                    )
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
                            provider, catalog_model, reason,
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

/// Chiama l'endpoint /v1/models del provider e ritorna la lista di model id.
///
/// Per Google passa per il brain REST (`/providers/google/models/live`) che
/// usa il SDK Python google-genai con Service Account dal DB (Vertex) o API
/// key (Gemini direct). Per gli altri provider chiama direttamente l'endpoint
/// OpenAI-compatible del provider con la api_key dal DB.
async fn fetch_provider_models(provider: &str, api_key: &str) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;

    // Caso speciale: Google → bridge via brain REST (vedi google_provider.py).
    if provider == "google" {
        let brain_url =
            std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        let url = format!(
            "{}/providers/google/models/live",
            brain_url.trim_end_matches('/')
        );
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("brain {url} status={status}: {body_text}");
        }
        #[derive(Debug, Deserialize)]
        struct BrainModelsResponse {
            #[allow(dead_code)]
            provider: String,
            models: Vec<String>,
        }
        let body: BrainModelsResponse = resp.json().await?;
        return Ok(body.models);
    }

    let (url, builder) = match provider {
        "anthropic" => {
            let url = "https://api.anthropic.com/v1/models";
            (
                url,
                client
                    .get(url)
                    .header("x-api-key", api_key)
                    .header("anthropic-version", "2023-06-01"),
            )
        }
        "openai" => {
            let url = "https://api.openai.com/v1/models";
            (url, client.get(url).bearer_auth(api_key))
        }
        "mistral" => {
            let url = "https://api.mistral.ai/v1/models";
            (url, client.get(url).bearer_auth(api_key))
        }
        "deepseek" => {
            let url = "https://api.deepseek.com/v1/models";
            (url, client.get(url).bearer_auth(api_key))
        }
        _ => anyhow::bail!("provider non supportato per autodiscovery: {}", provider),
    };

    let resp = builder.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("API {} ha risposto status {}", url, resp.status());
    }
    let body: ModelsListResponse = resp.json().await?;
    let models: Vec<String> = body.data.into_iter().map(|m| m.id).collect();
    Ok(models)
}

#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn classify_metadata_litellm_ha_priorita() {
        // function_calling/vision espliciti vincono sull'euristica.
        let c = classify_capabilities("openai", "gpt-4o", Some(true), Some(true), Some(false));
        assert!(c.supports_tool_use);
        assert!(c.supports_vision);
        assert!(!c.is_thinking);
        assert!(!c.uses_thinking_mode);
    }

    #[test]
    fn classify_o_series_e_reasoning_only_escluso_da_agentico() {
        // o-series: reasoning-only -> A (escludi) e B (non forzare) entrambi true.
        let c = classify_capabilities("openai", "o3-mini", None, None, None);
        assert!(c.is_thinking, "o-series deve essere escluso da agentico");
        assert!(c.uses_thinking_mode);
    }

    #[test]
    fn classify_deepseek_v4_reasoning_only() {
        // deepseek-v4-pro: reasoning-only (no reasoning_content passback) -> A+B.
        let c = classify_capabilities("deepseek", "deepseek-v4-pro", None, None, None);
        assert!(c.is_thinking);
        assert!(c.uses_thinking_mode);
    }

    #[test]
    fn classify_claude_ibrido_agentico_ma_thinking_mode() {
        // Claude opus/sonnet: NON escluso da agentico (A=false) ma extended
        // thinking -> non forzare tool_choice (B=true). Caso che il merge naïf
        // avrebbe rotto.
        let c = classify_capabilities("anthropic", "claude-sonnet-4-6", None, None, None);
        assert!(!c.is_thinking, "Claude deve restare agentic-eligibile");
        assert!(c.uses_thinking_mode, "Claude usa extended thinking -> non forzare");
        assert!(c.supports_vision);
    }

    #[test]
    fn classify_modello_chat_standard_non_thinking() {
        // mistral-large: tool-capable, non-thinking -> candidato agentico ideale.
        let c = classify_capabilities("mistral", "mistral-large-2411", None, None, None);
        assert!(c.supports_tool_use);
        assert!(!c.is_thinking);
        assert!(!c.uses_thinking_mode);
    }

    #[test]
    fn classify_gemini_25_thinking_dual_mode() {
        // Gemini 2.5 (pro E flash) ha il thinking attivo di default: e' dual-mode,
        // NON reasoning-only. Quindi is_thinking=false (eleggibile all'agentico),
        // uses_thinking_mode=true e policy='disable_for_tools' (non-thinking nei
        // tool-loop -> niente MALFORMED_FUNCTION_CALL). flash-lite NON ha thinking.
        let flash = classify_capabilities("google", "gemini-2.5-flash", None, None, None);
        assert!(!flash.is_thinking, "gemini-2.5-flash NON va escluso dall'agentico");
        assert!(flash.uses_thinking_mode, "gemini-2.5-flash e' thinking");
        assert_eq!(flash.agentic_thinking_policy, "disable_for_tools");
        let pro = classify_capabilities("google", "gemini-2.5-pro", None, None, None);
        assert!(!pro.is_thinking, "gemini-2.5-pro e' dual-mode, non reasoning-only");
        assert_eq!(pro.agentic_thinking_policy, "disable_for_tools");
        let lite = classify_capabilities("google", "gemini-2.5-flash-lite", None, None, None);
        assert!(!lite.uses_thinking_mode, "gemini-2.5-flash-lite NON e' thinking");
        assert_eq!(lite.agentic_thinking_policy, "none");
    }

    // ── ADR 0025: agentic_thinking_policy per famiglia ──

    #[test]
    fn classify_agentic_thinking_policy_per_famiglia() {
        let p = |prov, model| classify_capabilities(prov, model, None, None, None).agentic_thinking_policy;
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
}
