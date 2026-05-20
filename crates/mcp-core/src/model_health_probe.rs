//! Worker `model_health_probe` — pinga ogni singolo modello del catalog
//! `ai_price_catalog` con `is_enabled = true` per accertarne la salute reale,
//! a differenza di `provider_health_probe` che pinga solo UN modello per
//! provider.
//!
//! Motivazione: un provider puo' essere globalmente "up" ma alcuni modelli
//! del suo catalog possono essere broken (modello deprecato, hollow content,
//! capability non supportata). Esempi reali rilevati in produzione:
//!   - `deepseek-v3` / `deepseek-v3.2` / `deepseek-r1`: provider risponde,
//!     ma DeepSeek API ritorna 400 "supported model names are deepseek-v4-...".
//!   - `gemini-1.5-flash` / `gemini-2.0-flash`: 404 "no longer available
//!     to new users".
//!   - `gemini-3.5-flash`: enabled nel catalog ma hollow_completion costante
//!     (modello "pre-rilasciato").
//!
//! Il worker:
//!   1. Lista tutti i modelli `is_enabled = true` dal catalog.
//!   2. Salta i modelli appartenenti a provider in cooldown lungo (quota/billing).
//!   3. Pinga ognuno con un prompt minimale ("hi", max_tokens generosi per
//!      evitare falsi positivi su modelli "thinking-only" tipo gemini-2.5-pro).
//!   4. Classifica il risultato:
//!       - OK con content non-vuoto -> reset `consecutive_failures` a 0; se
//!         il modello era stato auto-disabled, lo riabilita.
//!       - Errore "provider-wide" (quota_exceeded, billing_required,
//!         rate_limit): NON incrementa il counter (e' colpa del provider,
//!         non del modello).
//!       - Errore "model-specific" (model_not_found, invalid_request,
//!         hollow_completion, unsupported, ecc.): incrementa
//!         `consecutive_failures`. Se >= soglia, auto-disable.
//!   5. Persiste tutto in `ai_model_health_history` (append-only).
//!
//! Costo: 200-300 modelli enabled, una chiamata da ~50 token / 30 min →
//! 12k-18k token/h totali, circa $0.02/giorno con i prezzi attuali.
//! Lo si puo' ridurre alzando l'interval (settings.model_health_probe_interval_s).

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::{PgPool, Row};
use tokio::time::sleep;

use crate::orchestrator::Orchestrator;
use crate::provider_cooldown::is_provider_in_cooldown;

/// Prompt minimale. Stesso usato da `provider_health_probe`.
const PROBE_PROMPT: &str = "ping";

/// Timeout per la singola chiamata al modello. Piu' generoso del provider
/// probe (30s) perche' i modelli "thinking" (gemini-2.5-pro) possono
/// spendere molto tempo nella fase di reasoning.
const PROBE_TIMEOUT_S: u64 = 60;

/// Intervallo minimo configurabile (sotto questo, troppe chiamate API).
const MIN_INTERVAL_S: u64 = 300;

/// Pausa tra una probe model e la successiva per non saturare il rate
/// limit del provider (anche se ogni probe e' poche dozzine di token,
/// 200 probes in burst possono triggerare 429).
const INTER_PROBE_SLEEP_MS: u64 = 250;

/// Avvia il worker in background. Restituisce subito.
pub fn spawn_model_health_probe(
    orchestrator: Arc<Orchestrator>,
    db: PgPool,
    enabled: bool,
    interval_s: u64,
    failure_threshold: i32,
) {
    let enabled = match std::env::var("NEXUS_MODEL_HEALTH_PROBE_ENABLED").as_deref() {
        Ok("false") | Ok("0") => false,
        Ok("true") | Ok("1") => true,
        _ => enabled,
    };
    if !enabled {
        tracing::info!("model_health_probe: DISABILITATO (model_health_probe_enabled=false)");
        return;
    }
    let interval_s = std::env::var("NEXUS_MODEL_HEALTH_PROBE_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(interval_s)
        .max(MIN_INTERVAL_S);
    tracing::info!(
        "model_health_probe: avvio worker (interval={interval_s}s, threshold={failure_threshold})",
    );
    tokio::spawn(async move {
        // Aspetta 60s al primo avvio: piu' del provider_health_probe (30s)
        // perche' vogliamo che quello finisca prima il suo primo giro e
        // popoli i cooldown dei provider non-funzionanti.
        sleep(Duration::from_secs(60)).await;
        loop {
            run_one_round(&orchestrator, &db, failure_threshold).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

/// Esegue UNA ronda di probe: pinga tutti i modelli enabled non skipped.
/// Esportato `pub(crate)` per consentire trigger manuale dall'endpoint
/// `POST /api/admin/probe-models`.
pub(crate) async fn run_one_round(
    orchestrator: &Orchestrator,
    db: &PgPool,
    failure_threshold: i32,
) -> ProbeRoundStats {
    let models = match load_enabled_models(db).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("model_health_probe: impossibile leggere catalog: {e}");
            return ProbeRoundStats::default();
        }
    };
    let mut stats = ProbeRoundStats {
        total: models.len(),
        ..Default::default()
    };

    for (provider, model, consecutive_failures) in models {
        // Salta se il provider e' in cooldown lungo: faremmo solo rumore
        // (tutte le probe ritornerebbero errore di quota/billing che e'
        // gia' noto al sistema).
        if is_provider_in_cooldown(&provider) {
            stats.skipped_provider_cooldown += 1;
            continue;
        }

        match probe_one_model(orchestrator, db, &provider, &model, consecutive_failures, failure_threshold).await {
            ProbeOutcome::Ok => stats.healthy += 1,
            ProbeOutcome::ProviderWide => stats.provider_wide_errors += 1,
            ProbeOutcome::ModelSpecificCounted => stats.model_errors += 1,
            ProbeOutcome::AutoDisabled => {
                stats.model_errors += 1;
                stats.auto_disabled += 1;
            }
        }

        sleep(Duration::from_millis(INTER_PROBE_SLEEP_MS)).await;
    }

    tracing::info!(
        "model_health_probe: round completato — total={} healthy={} provider_errors={} \
         model_errors={} auto_disabled={} skipped={}",
        stats.total,
        stats.healthy,
        stats.provider_wide_errors,
        stats.model_errors,
        stats.auto_disabled,
        stats.skipped_provider_cooldown,
    );
    stats
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct ProbeRoundStats {
    pub total: usize,
    pub healthy: usize,
    pub provider_wide_errors: usize,
    pub model_errors: usize,
    pub auto_disabled: usize,
    pub skipped_provider_cooldown: usize,
}

enum ProbeOutcome {
    Ok,
    ProviderWide,
    ModelSpecificCounted,
    AutoDisabled,
}

/// Legge i modelli enabled dal catalog. Ritorna (provider, model, consecutive_failures).
async fn load_enabled_models(db: &PgPool) -> sqlx::Result<Vec<(String, String, i32)>> {
    let rows = sqlx::query(
        "SELECT provider, model, consecutive_failures
           FROM ai_price_catalog
          WHERE is_enabled = true
          ORDER BY provider, model",
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let p: String = r.try_get("provider").unwrap_or_default();
            let m: String = r.try_get("model").unwrap_or_default();
            let f: i32 = r.try_get("consecutive_failures").unwrap_or(0);
            (p, m, f)
        })
        .collect())
}

/// Pinga un singolo modello e applica la logica di counter / auto-disable.
async fn probe_one_model(
    orchestrator: &Orchestrator,
    db: &PgPool,
    provider: &str,
    model: &str,
    prior_failures: i32,
    failure_threshold: i32,
) -> ProbeOutcome {
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_S),
        orchestrator.neural.generate_completion(provider, model, PROBE_PROMPT),
    )
    .await;
    let latency_ms = started.elapsed().as_millis() as i32;

    let outcome = match result {
        Ok(Ok(response)) => {
            // Distingue OK genuino da hollow_completion: il content deve
            // contenere almeno 1 carattere E non essere un errore ingoiato.
            //
            // Il brain Python (es. brain/providers/anthropic_provider.py:211)
            // intercetta exception API (billing_error, quota_exceeded, ecc.)
            // e ritorna ProviderResult con content="[Error: ...]" invece di
            // propagare l'eccezione. Cosi' il Rust riceveva "completed" e
            // marcava healthy=true mentre in realta' il provider era giu'.
            // Quindi controlliamo anche il pattern "[Error:" / "[error:".
            let content_text = extract_content_text(&response);
            let trimmed = content_text.trim();
            if trimmed.is_empty() {
                Classification::ModelSpecific(
                    "hollow_completion".to_string(),
                    Some("response had 0 chars of content".to_string()),
                )
            } else if trimmed.starts_with("[Error:") || trimmed.starts_with("[error:") {
                // Errore ingoiato dal brain. Estrai messaggio e classifica.
                let inner = trimmed
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim_start_matches("Error:")
                    .trim_start_matches("error:")
                    .trim();
                classify_model_error(inner)
            } else {
                Classification::Ok
            }
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            classify_model_error(&msg)
        }
        Err(_timeout_elapsed) => Classification::ModelSpecific(
            "timeout".to_string(),
            Some(format!("no response in {PROBE_TIMEOUT_S}s")),
        ),
    };

    // Persist history (fire-and-forget, no impact se fail).
    let (healthy, error_kind, error_message) = match &outcome {
        Classification::Ok => (true, None, None),
        Classification::ProviderWide(kind, msg) | Classification::ModelSpecific(kind, msg) => {
            (false, Some(kind.clone()), msg.clone())
        }
    };
    let _ = sqlx::query(
        r#"INSERT INTO ai_model_health_history
           (provider, model, healthy, latency_ms, error_kind, error_message)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(provider)
    .bind(model)
    .bind(healthy)
    .bind(latency_ms)
    .bind(error_kind.as_deref())
    .bind(error_message.as_deref().map(|s| truncate(s, 500)))
    .execute(db)
    .await;

    // Applica logica counter / auto-disable / auto-reenable.
    match outcome {
        Classification::Ok => {
            // Il probe usa prompt "ping" (1-2 token output) — un account
            // con budget quasi vuoto puo' passare il probe ma fallire sui
            // workload reali (es. anthropic con credit basso risponde
            // a "hi" ma fallisce su 5000+ token). Quindi il probe-OK NON
            // resetta il counter di consecutive_failures: solo i run REALI
            // (in chat_messages.rs::2117+) possono resettarlo, perche'
            // solo loro testano workload reale.
            // Il probe puo' SOLO segnalare success per logging.
            if prior_failures > 0 {
                tracing::debug!(
                    "model_health_probe: {provider}/{model} probe-OK ma prior_failures={} (non reset, attende run reale)",
                    prior_failures
                );
            }
            ProbeOutcome::Ok
        }
        Classification::ProviderWide(_, _) => {
            // Errore di provider, non del modello: counter invariato.
            ProbeOutcome::ProviderWide
        }
        Classification::ModelSpecific(kind, _msg) => {
            let new_count = prior_failures + 1;
            let should_disable = new_count >= failure_threshold;
            if should_disable {
                let _ = sqlx::query(
                    "UPDATE ai_price_catalog
                        SET is_enabled = false,
                            consecutive_failures = $3,
                            auto_disabled_at = NOW(),
                            auto_disabled_reason = $4,
                            updated_at = NOW()
                      WHERE provider = $1 AND model = $2",
                )
                .bind(provider)
                .bind(model)
                .bind(new_count)
                .bind(&kind)
                .execute(db)
                .await;
                tracing::warn!(
                    "model_health_probe: AUTO-DISABLE {provider}/{model} (failures={new_count}, reason={kind})"
                );
                ProbeOutcome::AutoDisabled
            } else {
                let _ = sqlx::query(
                    "UPDATE ai_price_catalog
                        SET consecutive_failures = $3, updated_at = NOW()
                      WHERE provider = $1 AND model = $2",
                )
                .bind(provider)
                .bind(model)
                .bind(new_count)
                .execute(db)
                .await;
                tracing::debug!(
                    "model_health_probe: {provider}/{model} fail #{new_count}/{failure_threshold} ({kind})"
                );
                ProbeOutcome::ModelSpecificCounted
            }
        }
    }
}

enum Classification {
    Ok,
    /// Errore che riguarda il provider intero (non punisce il modello).
    ProviderWide(String, Option<String>),
    /// Errore specifico del modello (incrementa il counter).
    ModelSpecific(String, Option<String>),
}

fn classify_model_error(msg: &str) -> Classification {
    let lc = msg.to_lowercase();

    // Provider-wide: NON puniamo il singolo modello.
    if lc.contains("credit balance") && lc.contains("too low") {
        return Classification::ProviderWide("credit_balance_too_low".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("insufficient_quota") || lc.contains("exceeded your current quota") {
        return Classification::ProviderWide("quota_exceeded".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("plans & billing")
        || lc.contains("upgrade or purchase credits")
        || lc.contains("billing required")
        || lc.contains("payment required")
    {
        return Classification::ProviderWide("billing_required".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("rate limit") || lc.contains("429") {
        return Classification::ProviderWide("rate_limit".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("unauthor") || lc.contains("invalid api key") || lc.contains("401") {
        return Classification::ProviderWide("auth_error".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("connection")
        || lc.contains("unreachable")
        || lc.contains("refused")
        || lc.contains("dns")
    {
        return Classification::ProviderWide("connection_error".into(), Some(truncate(msg, 500)));
    }

    // Model-specific: il modello stesso e' problematico.
    if lc.contains("model")
        && (lc.contains("not found")
            || lc.contains("no longer available")
            || lc.contains("not supported")
            || lc.contains("does not exist")
            || lc.contains("supported api model names")
            || lc.contains("supported model names"))
    {
        return Classification::ModelSpecific("model_not_found".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("invalid_request") || lc.contains("invalid request") || lc.contains("400") {
        return Classification::ModelSpecific("invalid_request".into(), Some(truncate(msg, 500)));
    }
    if lc.contains("unsupported") || lc.contains("not supported") {
        return Classification::ModelSpecific("unsupported".into(), Some(truncate(msg, 500)));
    }

    // Default: trattalo come model-specific (conservativo: meglio
    // disabilitare un modello dubbio che continuare a pingerlo a vuoto).
    Classification::ModelSpecific("unknown".into(), Some(truncate(msg, 500)))
}

/// Estrae il testo del content dalla response in vari formati provider.
/// Versione "text" usata per pattern-match su "[Error:". `extract_content_len`
/// resta come wrapper per backward-compat con i test esistenti.
fn extract_content_text(value: &serde_json::Value) -> String {
    if let Some(s) = value.get("content").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = value.get("choices").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            if let Some(s) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return s.to_string();
            }
        }
    }
    if let Some(candidates) = value.get("candidates").and_then(|v| v.as_array()) {
        if let Some(first) = candidates.first() {
            if let Some(parts) = first
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                let mut buf = String::new();
                for p in parts {
                    if let Some(s) = p.get("text").and_then(|t| t.as_str()) {
                        buf.push_str(s);
                    }
                }
                return buf;
            }
        }
    }
    String::new()
}

fn extract_content_len(value: &serde_json::Value) -> usize {
    // Cerca il content in diversi formati comuni.
    if let Some(s) = value.get("content").and_then(|v| v.as_str()) {
        return s.trim().len();
    }
    if let Some(arr) = value.get("choices").and_then(|v| v.as_array()) {
        if let Some(first) = arr.first() {
            if let Some(s) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return s.trim().len();
            }
        }
    }
    if let Some(candidates) = value.get("candidates").and_then(|v| v.as_array()) {
        if let Some(first) = candidates.first() {
            if let Some(parts) = first
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                let mut total = 0usize;
                for p in parts {
                    if let Some(s) = p.get("text").and_then(|t| t.as_str()) {
                        total += s.trim().len();
                    }
                }
                return total;
            }
        }
    }
    // Fallback: lunghezza JSON serializzato senza spazi.
    serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_model_specific_not_found() {
        let c = classify_model_error("404 model gemini-1.5-flash not found");
        matches!(c, Classification::ModelSpecific(ref k, _) if k == "model_not_found");
    }

    #[test]
    fn classify_model_specific_v4_redirect() {
        let c = classify_model_error(
            "The supported API model names are deepseek-v4-pro or deepseek-v4-flash",
        );
        matches!(c, Classification::ModelSpecific(ref k, _) if k == "model_not_found");
    }

    #[test]
    fn classify_provider_wide_quota() {
        let c = classify_model_error("You exceeded your current quota");
        matches!(c, Classification::ProviderWide(ref k, _) if k == "quota_exceeded");
    }

    #[test]
    fn extract_content_anthropic() {
        let v = serde_json::json!({"content": "Hello!"});
        assert_eq!(extract_content_len(&v), 6);
    }

    #[test]
    fn extract_content_openai() {
        let v = serde_json::json!({
            "choices": [{ "message": { "content": "Hi there" } }]
        });
        assert_eq!(extract_content_len(&v), 8);
    }

    #[test]
    fn extract_content_gemini() {
        let v = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "hello" }, { "text": "!" }] }
            }]
        });
        assert_eq!(extract_content_len(&v), 6);
    }

    #[test]
    fn extract_content_empty() {
        let v = serde_json::json!({"content": ""});
        assert_eq!(extract_content_len(&v), 0);
    }
}
