//! Classificatore d'errore provider (parte pura, testo-in -> stop_reason-out).
//!
//! Punto unico Rust paritetico a ``brain/providers/error_handler.py::classify_error``
//! (regola L / ADR 0026, Wave 8b). La parte SDK-specifica (estrazione di
//! `retry-after` da `exc.response.headers`) resta lato Python perche'
//! richiede l'oggetto eccezione vero dell'SDK; qui sta solo la classificazione
//! testuale che fino ad ora era duplicata fra:
//!   - brain (regex e HTTP map autoritative)
//!   - chiamata via RPC `ClassifyError` da mcp-core
//!
//! La parita' fra le due implementazioni e' verificata da un golden test
//! cross-language (vedi ``tests/fixtures/error_classifier_golden.json``).

use regex::Regex;
use std::sync::OnceLock;

/// Esito della classificazione (subset stabile, paritetico col Python).
/// Il messaggio leggibile per l'utente NON e' qui per design: questo modulo
/// fa solo la diagnosi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedError {
    pub stop_reason: String,
    pub retriable: bool,
    pub http_status: Option<u16>,
}

/// HTTP status -> (stop_reason, retriable). Paritetico a `HTTP_ERROR_MAP`
/// nel brain Python.
fn http_status_to_reason(status: u16) -> Option<(&'static str, bool)> {
    match status {
        400 => Some(("invalid_request", false)),
        401 => Some(("auth_error", false)),
        403 => Some(("forbidden", false)),
        404 => Some(("not_found", false)),
        413 => Some(("context_too_long", false)),
        422 => Some(("unprocessable", false)),
        429 => Some(("rate_limit", true)),
        500 => Some(("provider_error", true)),
        502 => Some(("bad_gateway", true)),
        503 => Some(("service_unavailable", true)),
        529 => Some(("overloaded", true)),
        _ => None,
    }
}

fn billing_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r"credit balance|insufficient.quota|upgrade or purchase|billing|",
            r"you exceeded your current quota|account is not active|",
            r"payment required|no credits",
        ))
        .expect("BILLING regex valida")
    })
}

fn context_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r"context.?length|maximum context|token.?limit|too many tokens|",
            r"context window|max_tokens.*exceeded",
        ))
        .expect("CONTEXT regex valida")
    })
}

fn auth_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r"invalid.api.key|api key not valid|incorrect api key|",
            r"authentication|unauthorized|invalid_api_key",
        ))
        .expect("AUTH regex valida")
    })
}

fn http_status_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)(?:Error code|status)[:\s]+(\d{3})")
            .expect("HTTP status regex valida")
    })
}

/// Classifica un errore provider rappresentato come testo libero.
///
/// Stesso ordine di valutazione del brain Python:
///   1. estrazione HTTP status code
///   2. pattern billing/quota -> `billing_error`
///   3. pattern context too long -> `context_too_long`
///   4. pattern auth -> `auth_error`
///   5. HTTP status noto -> mapping
///   6. timeout / connection
///   7. fallback `error`
pub fn classify_text(raw: &str) -> ClassifiedError {
    let raw_lower = raw.to_lowercase();

    let http_status = http_status_re()
        .captures(raw)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u16>().ok());

    if billing_re().is_match(&raw_lower) {
        return ClassifiedError {
            stop_reason: "billing_error".into(),
            retriable: true,
            http_status,
        };
    }
    if context_re().is_match(&raw_lower) {
        return ClassifiedError {
            stop_reason: "context_too_long".into(),
            retriable: false,
            http_status,
        };
    }
    if auth_re().is_match(&raw_lower) {
        return ClassifiedError {
            stop_reason: "auth_error".into(),
            retriable: false,
            http_status,
        };
    }
    if let Some(status) = http_status {
        if let Some((reason, retriable)) = http_status_to_reason(status) {
            return ClassifiedError {
                stop_reason: reason.into(),
                retriable,
                http_status: Some(status),
            };
        }
    }
    if raw_lower.contains("timeout") || raw_lower.contains("timed out") {
        return ClassifiedError {
            stop_reason: "timeout".into(),
            retriable: true,
            http_status,
        };
    }
    for k in ["connection", "network", "unreachable", "refused"] {
        if raw_lower.contains(k) {
            return ClassifiedError {
                stop_reason: "connection_error".into(),
                retriable: true,
                http_status,
            };
        }
    }
    ClassifiedError {
        stop_reason: "error".into(),
        retriable: false,
        http_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn billing_pattern() {
        let c = classify_text("Your credit balance is too low to make this request");
        assert_eq!(c.stop_reason, "billing_error");
        assert!(c.retriable);
    }

    #[test]
    fn context_too_long_pattern() {
        let c = classify_text("This model's maximum context length is 8192 tokens");
        assert_eq!(c.stop_reason, "context_too_long");
        assert!(!c.retriable);
    }

    #[test]
    fn http_status_429() {
        let c = classify_text("Error code: 429 - rate limit hit");
        assert_eq!(c.stop_reason, "rate_limit");
        assert_eq!(c.http_status, Some(429));
    }

    /// Parita' cross-language con ``brain/providers/error_handler.py::classify_error``.
    // jscpd:ignore-start
    // Boilerplate caricamento fixture: duplicazione GIUSTIFICATA col gemello
    // rag::chunker::tests, il loro scopo e' essere simili (golden test).
    #[test]
    fn parita_cross_language_da_fixture_golden() {
        const FIXTURE: &str = include_str!(
            "../../../tests/fixtures/error_classifier_golden.json"
        );
        let parsed: serde_json::Value = serde_json::from_str(FIXTURE)
            .expect("fixture golden non e' JSON valido");
        let cases = parsed["cases"].as_array().expect("fixture senza array 'cases'");
        for case in cases {
            let name = case["name"].as_str().unwrap_or("<senza nome>");
            let input = case["input"].as_str().expect("input string");
            let expected = case["expected_stop_reason"].as_str().expect("expected_stop_reason");
            let actual = classify_text(input);
            assert_eq!(
                actual.stop_reason, expected,
                "caso '{name}' divergente fra Rust e Python: input={input:?}",
            );
        }
    }
    // jscpd:ignore-end
}
