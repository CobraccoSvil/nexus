//! Worker `provider_health_probe` — pinga ogni provider LLM ogni 5 minuti
//! con un prompt minimale per accertarne la salute reale (non solo presenza
//! API key).
//!
//! Motivazione: il vecchio `/api/gateway/providers` ritornava `healthy:true`
//! per tutti i provider con API key configurata, indipendentemente dal fatto
//! che il provider rispondesse o meno (es. quota esaurita, rate limit). Il
//! LED nello statusbar restava verde fino al primo errore reale fatto da un
//! utente. Con questo worker, lo stato e' sondato proattivamente:
//!
//!   - Se la risposta e' OK in <10s → provider healthy.
//!   - Se la risposta e' un errore di tipo billing/quota → cooldown lungo (6h).
//!   - Se timeout >10s → cooldown breve (60s) come "slow".
//!
//! Il risultato e' persistito in `nexus_provider_health_history` per:
//!   - Letture rapide da `gateway_providers_handler` (`last_health_check_at`).
//!   - Dashboard admin con grafici latency / error rate.
//!
//! Costo: 5 provider × ~1 token risposta × 12 check/h × 24h ≈ 1500 tokens/giorno
//! totali → trascurabile (~$0.001/giorno con i prezzi attuali).
//!
//! Configurazione via env:
//!   - `NEXUS_PROVIDER_HEALTH_PROBE_ENABLED=true` (default: true; disabilita in dev)
//!   - `NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S=300` (default: 300, min 60)

use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::time::sleep;

use crate::orchestrator::{default_model_for_provider, Orchestrator};
use crate::provider_cooldown::{is_provider_in_cooldown, put_provider_in_long_cooldown};
// `put_provider_in_cooldown` e' `pub(crate)` -> accessibile, ma la signature
// e' `(provider: &str, retry_after_seconds: Option<u64>)`. Per slow/timeout
// usiamo l'overload corto.
use crate::provider_cooldown::put_provider_in_cooldown;

/// Lista dei provider da probare. Allineata con `KNOWN_PROVIDERS` in
/// `orchestrator.rs`. Mantenuta hard-coded perche' sono note staticamente
/// (un nuovo provider richiede comunque modifiche al codice di routing).
const PROBED_PROVIDERS: &[&str] = &["anthropic", "openai", "google", "deepseek", "mistral"];

/// Prompt minimale: 1 parola, ci aspettiamo una risposta breve.
/// Il provider tipicamente risponde con "Hi!" o "Hello!" (1-2 token).
const PROBE_PROMPT: &str = "hi";

/// Timeout per ogni singola chiamata. Oltre questa soglia il provider
/// e' considerato "slow" (cooldown 60s). 30s e' un valore conservativo
/// che evita falsi positivi su latency network elevata (es. WSL Italia
/// verso provider US-East): un primo token tipicamente arriva in 1-3s ma
/// la connection setup + DNS + TLS handshake puo' aggiungere 5-15s.
const PROBE_TIMEOUT_S: u64 = 30;

/// Cooldown breve quando il provider e' lento (1 minuto). L'idea: dare un
/// piccolo respiro al provider, non escluderlo definitivamente per uno
/// spike di latency.
const SLOW_COOLDOWN_S: u64 = 60;

/// Avvia il worker in background. Restituisce subito; il loop gira per
/// l'intera vita del processo.
///
/// Chiamato da `main.rs` con `tokio::spawn`.
pub fn spawn_health_probe(orchestrator: Arc<Orchestrator>, db: PgPool) {
    let enabled = std::env::var("NEXUS_PROVIDER_HEALTH_PROBE_ENABLED")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);
    if !enabled {
        tracing::info!("provider_health_probe: DISABILITATO via env (NEXUS_PROVIDER_HEALTH_PROBE_ENABLED=false)");
        return;
    }
    let interval_s = std::env::var("NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300)
        .max(60);
    tracing::info!(
        "provider_health_probe: avvio worker (interval={}s, providers={:?})",
        interval_s, PROBED_PROVIDERS,
    );
    tokio::spawn(async move {
        // Aspetta 30s al primo avvio per dare tempo agli altri servizi
        // di stabilizzarsi (DB ready, brain pronto, ecc.).
        sleep(Duration::from_secs(30)).await;
        loop {
            run_one_round(&orchestrator, &db).await;
            sleep(Duration::from_secs(interval_s)).await;
        }
    });
}

/// Esegue un ciclo completo di probe per tutti i provider.
async fn run_one_round(orchestrator: &Orchestrator, db: &PgPool) {
    for provider in PROBED_PROVIDERS {
        // Skip se gia' in cooldown lungo (>1h): inutile spendere chiamate
        // su provider che sappiamo gia' essere giu' per quota/billing.
        if is_provider_in_cooldown(provider) {
            tracing::debug!("provider_health_probe: skip {provider} (in cooldown)");
            continue;
        }
        probe_one(orchestrator, db, provider).await;
        // Distanzia le chiamate ai provider per non saturare la rete
        // (anche se sono indipendenti, evita spike di traffico).
        sleep(Duration::from_secs(2)).await;
    }
}

/// Pinga un singolo provider. Persiste sempre il risultato (anche success).
async fn probe_one(orchestrator: &Orchestrator, db: &PgPool, provider: &str) {
    let matrix_arc = match orchestrator.routing_matrix.current_async().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("probe_one: routing_matrix non disponibile ({e}), skip {provider}");
            return;
        }
    };
    let model = default_model_for_provider(&matrix_arc, provider);
    let started = Instant::now();
    // generate_completion vive su `NeuralCoreClient`, accessibile via il
    // campo `pub(crate) neural` di `Orchestrator`.
    let result: Result<anyhow::Result<serde_json::Value>, tokio::time::error::Elapsed> =
        tokio::time::timeout(
            Duration::from_secs(PROBE_TIMEOUT_S),
            orchestrator.neural.generate_completion(provider, &model, PROBE_PROMPT),
        )
        .await;
    let latency_ms = started.elapsed().as_millis() as i32;

    let (healthy, error_kind, error_message) = match result {
        Ok(Ok(_response)) => {
            // Successo. Provider rispondente in <PROBE_TIMEOUT_S.
            tracing::debug!(
                "provider_health_probe: {provider} OK in {latency_ms}ms"
            );
            (true, None, None)
        }
        Ok(Err(e)) => {
            // Provider ha risposto con errore (HTTP error o JSON parse).
            // Classifico per decidere il tipo di cooldown.
            let msg = e.to_string();
            let kind = classify_probe_error(&msg);
            tracing::warn!(
                "provider_health_probe: {provider} ERROR ({kind}) in {latency_ms}ms: {msg}",
                msg = &msg[..msg.len().min(200)],
            );
            // Errori billing/quota → cooldown lungo (6h)
            // Tutti gli altri errori → cooldown breve (60s)
            if matches!(
                kind.as_str(),
                "quota_exceeded" | "credit_balance_too_low" | "billing_required"
            ) {
                put_provider_in_long_cooldown(provider, &kind);
            } else {
                put_provider_in_cooldown(provider, Some(SLOW_COOLDOWN_S));
            }
            (false, Some(kind), Some(truncate(&msg, 500)))
        }
        Err(_timeout_elapsed) => {
            // Timeout: provider troppo lento. Cooldown breve.
            tracing::warn!(
                "provider_health_probe: {provider} TIMEOUT (>{PROBE_TIMEOUT_S}s)"
            );
            put_provider_in_cooldown(provider, Some(SLOW_COOLDOWN_S));
            (
                false,
                Some("timeout".to_string()),
                Some(format!("nessuna risposta in {PROBE_TIMEOUT_S}s")),
            )
        }
    };

    // Persistenza fire-and-forget. Errori del DB non interrompono il loop.
    let row_result = sqlx::query(
        r#"INSERT INTO nexus_provider_health_history
           (provider, healthy, latency_ms, error_kind, error_message)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(provider)
    .bind(healthy)
    .bind(latency_ms)
    .bind(error_kind.as_deref())
    .bind(error_message.as_deref())
    .execute(db)
    .await;
    if let Err(e) = row_result {
        tracing::warn!("provider_health_probe: persistenza fallita per {provider}: {e}");
    }
}

/// Classifica un messaggio di errore in una categoria nota. Mirror della
/// logica di `brain_agent_client.rs::classify_provider_error`.
fn classify_probe_error(msg: &str) -> String {
    let lc = msg.to_lowercase();
    if lc.contains("credit balance") && lc.contains("too low") {
        return "credit_balance_too_low".to_string();
    }
    if lc.contains("insufficient_quota") || lc.contains("exceeded your current quota") {
        return "quota_exceeded".to_string();
    }
    if lc.contains("plans & billing")
        || lc.contains("upgrade or purchase credits")
        || lc.contains("billing required")
        || lc.contains("payment required")
    {
        return "billing_required".to_string();
    }
    if lc.contains("rate limit") || lc.contains("429") {
        return "rate_limit".to_string();
    }
    if lc.contains("timeout") || lc.contains("timed out") {
        return "timeout".to_string();
    }
    if lc.contains("unauthor") || lc.contains("invalid api key") || lc.contains("401") {
        return "auth_error".to_string();
    }
    if lc.contains("connection") || lc.contains("unreachable") || lc.contains("refused") {
        return "connection_error".to_string();
    }
    "unknown".to_string()
}

/// Tronca una stringa a `max` caratteri (per evitare TEXT giganti nel DB).
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
    fn test_classify_probe_error_billing() {
        assert_eq!(
            classify_probe_error("Your credit balance is too low to access the Anthropic API"),
            "credit_balance_too_low"
        );
    }

    #[test]
    fn test_classify_probe_error_quota() {
        assert_eq!(
            classify_probe_error("You exceeded your current quota, please check your plan"),
            "quota_exceeded"
        );
        assert_eq!(
            classify_probe_error("Error: insufficient_quota"),
            "quota_exceeded"
        );
    }

    #[test]
    fn test_classify_probe_error_rate_limit() {
        assert_eq!(
            classify_probe_error("HTTP 429 too many requests, rate limit exceeded"),
            "rate_limit"
        );
    }

    #[test]
    fn test_classify_probe_error_timeout() {
        assert_eq!(classify_probe_error("request timed out"), "timeout");
    }

    #[test]
    fn test_classify_probe_error_auth() {
        assert_eq!(
            classify_probe_error("HTTP 401 unauthorized: invalid api key"),
            "auth_error"
        );
    }

    #[test]
    fn test_classify_probe_error_unknown() {
        assert_eq!(
            classify_probe_error("Some weird unrelated message"),
            "unknown"
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("ciao", 10), "ciao");
        assert_eq!(truncate("ciao mondo bellissimo", 5), "ciao …");
    }
}
