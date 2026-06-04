//! `http_request` — chiamata HTTP strutturata verso un URL arbitrario.
//!
//! GET/POST/PUT/PATCH/DELETE con body JSON opzionale, headers custom,
//! timeout configurabile. Pensato per testare endpoint del progetto
//! durante lo sviluppo iterativo senza passare per `run_command "curl ..."`.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

pub struct HttpRequestTool;

/// Metodi HTTP ammessi.
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Timeout massimo consentito (secondi).
const MAX_TIMEOUT_SECS: u64 = 120;
/// Timeout di default (secondi).
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Dimensione massima corpo risposta (bytes) — 2 MB.
const MAX_RESPONSE_BODY: usize = 2 * 1024 * 1024;

#[async_trait]
impl NexusToolHandler for HttpRequestTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        // ── Parametri obbligatori ─────────────────────────────────────────
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'url' obbligatorio".into()))?
            .trim()
            .to_string();

        if url.is_empty() {
            return Err(NexusToolError::BadInput("URL vuoto".into()));
        }

        // Validazione base: deve iniziare con http:// o https://
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(NexusToolError::BadInput(
                "L'URL deve iniziare con http:// o https://".into(),
            ));
        }

        // ── Metodo HTTP ──────────────────────────────────────────────────
        let method = args
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_uppercase();

        if !ALLOWED_METHODS.contains(&method.as_str()) {
            return Err(NexusToolError::BadInput(format!(
                "Metodo HTTP non valido: '{}'. Ammessi: {}",
                method,
                ALLOWED_METHODS.join(", ")
            )));
        }

        // ── Timeout ──────────────────────────────────────────────────────
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        // ── Headers custom ───────────────────────────────────────────────
        let headers: HashMap<String, String> = if let Some(h) = args.get("headers") {
            if let Some(obj) = h.as_object() {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // ── Body (per POST/PUT/PATCH) ────────────────────────────────────
        let body = args.get("body");

        // ── Costruisci richiesta ─────────────────────────────────────────
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .danger_accept_invalid_certs(true) // ambienti dev con cert self-signed
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| {
                NexusToolError::BadInput(format!("Errore creazione client HTTP: {}", e))
            })?;

        let mut req = match method.as_str() {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            "HEAD" => client.head(&url),
            "OPTIONS" => client.request(reqwest::Method::OPTIONS, &url),
            _ => unreachable!(),
        };

        // Aggiungi headers custom
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        // Aggiungi body se presente (per metodi che lo supportano)
        if let Some(b) = body {
            match method.as_str() {
                "POST" | "PUT" | "PATCH" => {
                    req = req.header("Content-Type", "application/json");
                    req = req.json(b);
                }
                _ => {
                    // Body ignorato per GET/HEAD/OPTIONS/DELETE
                }
            }
        }

        // ── Esegui richiesta ─────────────────────────────────────────────
        let start = std::time::Instant::now();
        let response = req
            .send()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("Richiesta HTTP fallita: {}", e)))?;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        let status = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_string();

        // Raccolta headers risposta
        let resp_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or("<non-utf8>").to_string(),
                )
            })
            .collect();

        let content_type = resp_headers
            .get("content-type")
            .cloned()
            .unwrap_or_default();

        // ── Lettura body risposta con limite ──────────────────────────────
        let raw_bytes = response.bytes().await.map_err(|e| {
            NexusToolError::BadInput(format!("Errore lettura corpo risposta: {}", e))
        })?;

        let truncated = raw_bytes.len() > MAX_RESPONSE_BODY;
        let limited_bytes = if truncated {
            &raw_bytes[..MAX_RESPONSE_BODY]
        } else {
            &raw_bytes[..]
        };

        // Tenta parsing JSON, altrimenti testo
        let body_value = if content_type.contains("application/json") {
            match serde_json::from_slice::<Value>(limited_bytes) {
                Ok(v) => json!({"type": "json", "data": v}),
                Err(_) => {
                    let text = String::from_utf8_lossy(limited_bytes).to_string();
                    json!({"type": "text", "data": text})
                }
            }
        } else {
            let text = String::from_utf8_lossy(limited_bytes).to_string();
            json!({"type": "text", "data": text})
        };

        Ok(json!({
            "ok": status >= 200 && status < 400,
            "status": status,
            "status_text": status_text,
            "latency_ms": elapsed_ms,
            "headers": resp_headers,
            "body": body_value,
            "body_size_bytes": raw_bytes.len(),
            "truncated": truncated,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL completo (deve iniziare con http:// o https://)"
                },
                "method": {
                    "type": "string",
                    "description": "Metodo HTTP. Default: GET",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
                },
                "headers": {
                    "type": "object",
                    "description": "Headers HTTP custom (chiave-valore stringa)",
                    "additionalProperties": { "type": "string" }
                },
                "body": {
                    "description": "Corpo della richiesta (JSON). Usato solo per POST/PUT/PATCH."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in secondi. Default: 30, max: 120"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_network_egress() {
        let s = HttpRequestTool.safety();
        assert!(!s.read_only);
        assert!(s.network_egress);
        assert!(!s.can_write_filesystem);
        assert!(!s.can_execute_subproc);
    }

    #[test]
    fn test_input_schema_requires_url() {
        let s = HttpRequestTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "url"));
    }

    #[test]
    fn test_input_schema_method_enum() {
        let s = HttpRequestTool.input_schema();
        let method_enum = s["properties"]["method"]["enum"].as_array().unwrap();
        assert!(method_enum.len() == 7);
        assert!(method_enum.iter().any(|v| v == "GET"));
        assert!(method_enum.iter().any(|v| v == "POST"));
    }

    #[test]
    fn test_allowed_methods_list() {
        assert!(ALLOWED_METHODS.contains(&"GET"));
        assert!(ALLOWED_METHODS.contains(&"POST"));
        assert!(ALLOWED_METHODS.contains(&"DELETE"));
        assert!(!ALLOWED_METHODS.contains(&"CONNECT"));
    }
}
