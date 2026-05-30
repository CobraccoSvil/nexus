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
        "voxtral", "whisper", "embedding", "moderation", "unknown-provider",
    ];
    for bad in SUBSTRING_BLACKLIST {
        if lower.contains(bad) { return false; }
    }
    const INFIX_BLACKLIST: &[&str] = &[
        "-tts-", "-transcribe-", "-realtime-", "-instruct-", "-unknown-",
    ];
    for bad in INFIX_BLACKLIST {
        if lower.contains(bad) { return false; }
    }
    const PREFIX_BLACKLIST: &[&str] = &[
        "tts-", "dall-e", "dalle-", "imagen", "instruct-",
        "babbage", "davinci-00", "text-embedding",
    ];
    for bad in PREFIX_BLACKLIST {
        if lower.starts_with(bad) { return false; }
    }
    const SUFFIX_BLACKLIST: &[&str] = &[
        "-tts", "-transcribe", "-realtime", "-embed", "-instruct",
    ];
    for bad in SUFFIX_BLACKLIST {
        if lower.ends_with(bad) { return false; }
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
    let stats = sync_tick(db, orchestrator).await.map_err(|e| e.to_string())?;
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
        match sync_provider(db, provider, disable_missing, insert_new_disabled, orchestrator).await {
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
    let catalog_models: std::collections::HashMap<String, (bool, bool)> =
        catalog_rows.into_iter().map(|(m, e, l)| (m, (e, l))).collect();
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
                provider, api_model
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
                            audit_log(db, provider, api_model, "inserted", json!({"source":"api_discovery"})).await;
                            tracing::info!("catalog_sync[{}]: + nuovo modello rilevato '{}'", provider, api_model);

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
                                        let _ = sqlx::query(
                                            "UPDATE ai_price_catalog \
                                             SET is_enabled = true, auto_disabled_at = NULL, \
                                                 auto_disabled_reason = NULL, updated_at = NOW() \
                                             WHERE provider = $1 AND model = $2 AND is_enabled = false",
                                        )
                                        .bind(provider)
                                        .bind(api_model)
                                        .execute(db)
                                        .await;
                                        audit_log(
                                            db, provider, api_model, "probe_ok_on_insert",
                                            json!({"action":"auto_enabled"}),
                                        )
                                        .await;
                                        tracing::info!(
                                            "catalog_sync[{}]: probe OK su nuovo modello '{}' -> abilitato",
                                            provider, api_model
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
                                            db, provider, api_model, "probe_failed_on_insert",
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
                if !is_enabled && !manual_locked {
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
                            tracing::info!("catalog_sync[{}]: re-enabled '{}' (ricomparso API)", provider, api_model);
                        }
                    }
                } else if !is_enabled && manual_locked {
                    // Skip: admin lo ha disabilitato manualmente, non riabilitare anche se ricompare.
                    tracing::debug!(
                        "catalog_sync[{}]: skip re-enable '{}' (manual_locked)",
                        provider, api_model
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
        if *is_enabled && !manual_locked && !is_chat_compatible_model(catalog_model) {
            let res = sqlx::query(
                "UPDATE ai_price_catalog SET is_enabled = false, \
                 auto_disabled_at = NOW(), auto_disabled_reason = 'not_chat_compatible' \
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
                        db, provider, catalog_model, "disabled",
                        json!({"reason":"not_chat_compatible"}),
                    )
                    .await;
                    tracing::warn!(
                        "catalog_sync[{}]: disabled '{}' (non chat-compatible per blacklist aggiornata)",
                        provider, catalog_model,
                    );
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
                    if !api_m.starts_with(catalog_model.as_str()) || api_m.len() <= catalog_model.len() {
                        return false;
                    }
                    let suffix = &api_m[catalog_model.len()..];
                    suffix.starts_with('-') && suffix.len() == 9
                        && suffix[1..].chars().all(|c| c.is_ascii_digit())
                });
                if has_dated_in_api {
                    continue; // alias preservato (es. claude-haiku-4-5)
                }

                // Skip alias con suffisso data se la base name e' nell'API.
                // Es: catalog "claude-sonnet-4-6-20251201" disabilitato solo se
                // anche "claude-sonnet-4-6" non e' nell'API.
                let base_name = strip_date_suffix(catalog_model);
                if base_name.as_str() != catalog_model.as_str() && api_set.contains(base_name.as_str()) {
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
                        audit_log(db, provider, catalog_model, "disabled",
                                  json!({"reason":"missing_from_api"})).await;
                        tracing::warn!(
                            "catalog_sync[{}]: - disabled '{}' (non piu nell API)",
                            provider, catalog_model,
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
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()?;

    // Caso speciale: Google → bridge via brain REST (vedi google_provider.py).
    if provider == "google" {
        let brain_url = std::env::var("BRAIN_REST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
        let url = format!("{}/providers/google/models/live", brain_url.trim_end_matches('/'));
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
            (url, client.get(url).header("x-api-key", api_key).header("anthropic-version", "2023-06-01"))
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
        assert_eq!(strip_date_suffix("claude-sonnet-4-6-20251201"), "claude-sonnet-4-6");
        assert_eq!(strip_date_suffix("gpt-4o-mini"), "gpt-4o-mini");
        assert_eq!(strip_date_suffix("gpt-4o-mini-2024-07-18"), "gpt-4o-mini-2024-07-18");
        // (sopra ha 2 digits-2 digits-2 digits, non matcha 8 digits)
        assert_eq!(strip_date_suffix("ministral-8b-2512"), "ministral-8b-2512");
    }
}
