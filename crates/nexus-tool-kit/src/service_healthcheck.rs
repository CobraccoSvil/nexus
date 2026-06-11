//! `service_healthcheck` — probe HTTP o TCP verso un endpoint.
//!
//! Verifica che un servizio sia raggiungibile e risponda correttamente.
//! Supporta retry con backoff esponenziale. Restituisce stato, latenza
//! e dettagli sulla risposta. Utile per validare che un servizio appena
//! avviato/riavviato sia effettivamente healthy.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct ServiceHealthcheckTool;

/// Numero massimo di tentativi.
const MAX_RETRIES: u32 = 10;
/// Numero di default di tentativi.
const DEFAULT_RETRIES: u32 = 5;
/// Timeout singola richiesta (secondi).
const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 5;
/// Backoff iniziale (millisecondi).
const INITIAL_BACKOFF_MS: u64 = 500;
/// Backoff massimo (millisecondi).
const MAX_BACKOFF_MS: u64 = 10_000;

#[async_trait]
impl NexusToolHandler for ServiceHealthcheckTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'url' obbligatorio".into()))?
            .trim()
            .to_string();

        if url.is_empty() {
            return Err(NexusToolError::BadInput("URL vuoto".into()));
        }

        // Supporta TCP probe: "tcp://host:port"
        let is_tcp = url.starts_with("tcp://");

        if !is_tcp && !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(NexusToolError::BadInput(
                "L'URL deve iniziare con http://, https:// o tcp://".into(),
            ));
        }

        let retries = args
            .get("retries")
            .and_then(Value::as_u64)
            .map(|r| (r as u32).min(MAX_RETRIES))
            .unwrap_or(DEFAULT_RETRIES);

        let expected_status = args
            .get("expected_status")
            .and_then(Value::as_u64)
            .map(|s| s as u16);

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_PROBE_TIMEOUT_SECS);

        // ── Esegui probe con retry ───────────────────────────────────────
        let mut attempts: Vec<Value> = Vec::new();
        let mut backoff_ms = INITIAL_BACKOFF_MS;
        let overall_start = std::time::Instant::now();

        for attempt in 1..=retries {
            let result = if is_tcp {
                probe_tcp(&url, timeout_secs).await
            } else {
                probe_http(&url, timeout_secs).await
            };

            match result {
                Ok(probe) => {
                    let status_ok = match expected_status {
                        Some(expected) => probe.status == expected,
                        None => probe.status >= 200 && probe.status < 400,
                    };

                    attempts.push(json!({
                        "attempt": attempt,
                        "ok": status_ok,
                        "status": probe.status,
                        "latency_ms": probe.latency_ms,
                    }));

                    if status_ok {
                        return Ok(json!({
                            "ok": true,
                            "url": url,
                            "status": probe.status,
                            "latency_ms": probe.latency_ms,
                            "attempts": attempt,
                            "total_time_ms": overall_start.elapsed().as_millis() as u64,
                            "details": attempts,
                        }));
                    }
                }
                Err(err_msg) => {
                    attempts.push(json!({
                        "attempt": attempt,
                        "ok": false,
                        "error": err_msg,
                    }));
                }
            }

            // Non dormire dopo l'ultimo tentativo
            if attempt < retries {
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }

        // Tutti i tentativi falliti
        Ok(json!({
            "ok": false,
            "url": url,
            "attempts": retries,
            "total_time_ms": overall_start.elapsed().as_millis() as u64,
            "error": format!("Healthcheck fallito dopo {} tentativi", retries),
            "details": attempts,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL da verificare. Supporta http://, https://, tcp://host:port"
                },
                "retries": {
                    "type": "integer",
                    "description": "Numero di tentativi con backoff esponenziale. Default: 5, max: 10"
                },
                "expected_status": {
                    "type": "integer",
                    "description": "Status HTTP atteso. Default: qualsiasi 2xx/3xx"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout per singolo tentativo (secondi). Default: 5"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}

struct ProbeResult {
    status: u16,
    latency_ms: u64,
}

async fn probe_http(url: &str, timeout_secs: u64) -> Result<ProbeResult, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| format!("Errore creazione client: {}", e))?;

    let start = std::time::Instant::now();
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Connessione fallita: {}", e))?;

    let latency_ms = start.elapsed().as_millis() as u64;
    let status = resp.status().as_u16();

    Ok(ProbeResult { status, latency_ms })
}

async fn probe_tcp(url: &str, timeout_secs: u64) -> Result<ProbeResult, String> {
    // Formato atteso: tcp://host:port
    let addr = url
        .strip_prefix("tcp://")
        .ok_or_else(|| "Formato TCP non valido: atteso tcp://host:port".to_string())?;

    let start = std::time::Instant::now();

    tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| format!("Timeout connessione TCP a {}", addr))?
    .map_err(|e| format!("Connessione TCP fallita a {}: {}", addr, e))?;

    let latency_ms = start.elapsed().as_millis() as u64;

    // TCP non ha status code HTTP: usiamo 200 come "connesso"
    Ok(ProbeResult {
        status: 200,
        latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_readonly_network() {
        let s = ServiceHealthcheckTool.safety();
        assert!(s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_write_filesystem);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_requires_url() {
        let s = ServiceHealthcheckTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "url"));
    }

    #[test]
    fn test_input_schema_optional_retries() {
        let s = ServiceHealthcheckTool.input_schema();
        assert!(s["properties"]["retries"].is_object());
        assert!(s["properties"]["expected_status"].is_object());
        assert!(s["properties"]["timeout_secs"].is_object());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // sanity check deliberato sulle relazioni tra costanti
    fn test_constants_sane() {
        assert!(DEFAULT_RETRIES <= MAX_RETRIES);
        assert!(INITIAL_BACKOFF_MS < MAX_BACKOFF_MS);
    }
}
