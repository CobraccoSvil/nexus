//! Classificatore d'errore provider (parte pura, testo-in -> stop_reason-out).
//!
//! Classificatore TESTUALE Rust (regex + HTTP map). E' un fallback: la fonte
//! primaria e' il segnale STRUTTURATO alla sorgente (status + codice d'errore
//! del provider) via `classify_provider_error` / `ProviderHttpError` nel gateway
//! (regola M, ADR 0033). Questo modulo interviene solo quando resta disponibile
//! il solo messaggio testuale gia' appiattito.
//!
//! Regressione fissata dal golden `tests/fixtures/error_classifier_golden.json`.
//! Nota storica: il file golden nasceva per la parita' col brain Python
//! (`brain/providers/error_handler.py`), ora rimosso; resta come golden Rust.

use regex::Regex;
use std::sync::OnceLock;

/// Esito della classificazione (subset stabile).
/// Il messaggio leggibile per l'utente NON e' qui per design: questo modulo
/// fa solo la diagnosi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedError {
    pub stop_reason: String,
    pub retriable: bool,
    pub http_status: Option<u16>,
}

/// HTTP status -> (stop_reason, retriable).
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
        Regex::new(r"(?i)(?:Error code|status)[:\s]+(\d{3})").expect("HTTP status regex valida")
    })
}

/// Rate-limit testuale SENZA status HTTP affidabile (es. errori inline o
/// eccezioni che non espongono `status_code`). Valutato DOPO la HTTP map:
/// uno status esplicito noto vince sempre sul testo.
fn rate_limit_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"rate.?limit|too many requests|throttl|\b429\b")
            .expect("RATE_LIMIT regex valida")
    })
}

fn retry_after_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)retry[-_ ]?(?:in|after)[:=\s]+(\d+)").expect("RETRY_AFTER regex valida")
    })
}

/// Estrae i secondi di retry suggeriti dal testo raw dell'errore
/// (es. "Please retry after 30 seconds"). Fallback testuale: la fonte primaria
/// e' l'header `Retry-After` letto in modo strutturato dall'adapter provider.
/// Punto unico dell'estrazione dal testo (regola L).
pub fn extract_retry_after_seconds(raw: &str) -> Option<u64> {
    retry_after_re()
        .captures(raw)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
}

/// Classifica un errore provider rappresentato come testo libero.
///
/// Ordine di valutazione:
///   1. estrazione HTTP status code
///   2. pattern billing/quota -> `billing_error`
///   3. pattern context too long -> `context_too_long`
///   4. pattern auth -> `auth_error`
///   5. HTTP status noto -> mapping
///   5bis. rate-limit testuale (status assente o non mappato)
///   6. timeout / connection
///   7. fallback `error`
pub fn classify_text(raw: &str) -> ClassifiedError {
    classify_with_status(raw, None)
}

/// Variante di [`classify_text`] con HTTP status gia' noto al chiamante
/// (es. estratto in modo strutturato dalla risposta HTTP del provider). Lo
/// status esplicito vince sull'estrazione regex dal testo; entra allo stesso
/// passo della HTTP map (coerente con la regola M: il segnale strutturato
/// prevale sul testo).
pub fn classify_with_status(raw: &str, known_status: Option<u16>) -> ClassifiedError {
    let raw_lower = raw.to_lowercase();

    let http_status = known_status.or_else(|| {
        http_status_re()
            .captures(raw)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<u16>().ok())
    });

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
    // 5bis. Rate-limit testuale: copre i casi senza status affidabile
    // (errore inline, eccezione rilanciata come stringa "429 Resource has
    // been exhausted", "throttled", ...). Dopo la HTTP map per design: uno
    // status esplicito mappato vince sempre sul testo.
    if rate_limit_re().is_match(&raw_lower) {
        return ClassifiedError {
            stop_reason: "rate_limit".into(),
            retriable: true,
            http_status,
        };
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

    #[test]
    fn rate_limit_testuale_senza_status() {
        // Copre l'eccezione Google rilanciata come stringa (niente "Error code:").
        let c = classify_text("429 Resource has been exhausted (e.g. check quota).");
        assert_eq!(c.stop_reason, "rate_limit");
        assert!(c.retriable);
        assert_eq!(c.http_status, None);

        let c = classify_text("Request was throttled by the upstream provider");
        assert_eq!(c.stop_reason, "rate_limit");
    }

    #[test]
    fn status_esplicito_vince_sul_testo() {
        // known_status passa dalla HTTP map PRIMA del pattern testuale:
        // un 503 esplicito non diventa rate_limit anche se il testo cita 429.
        let c = classify_with_status("please slow down, 429 in upstream", Some(503));
        assert_eq!(c.stop_reason, "service_unavailable");
        assert_eq!(c.http_status, Some(503));

        // known_status vince sull'estrazione regex dal testo.
        let c = classify_with_status("Error code: 500 - whatever", Some(429));
        assert_eq!(c.stop_reason, "rate_limit");
        assert_eq!(c.http_status, Some(429));
    }

    #[test]
    fn estrazione_retry_after_dal_testo() {
        assert_eq!(
            extract_retry_after_seconds("Rate limited. Please retry after 30 seconds"),
            Some(30)
        );
        assert_eq!(extract_retry_after_seconds("retry in 5"), Some(5));
        assert_eq!(extract_retry_after_seconds("Retry-After: 120"), Some(120));
        assert_eq!(extract_retry_after_seconds("nessun suggerimento"), None);
    }

    /// Golden test del classificatore testuale (regressione su stop_reason).
    // jscpd:ignore-start
    // Boilerplate caricamento fixture: duplicazione GIUSTIFICATA col gemello
    // rag::chunker::tests, il loro scopo e' essere simili (golden test).
    #[test]
    fn classify_text_da_fixture_golden() {
        const FIXTURE: &str = include_str!("../../../tests/fixtures/error_classifier_golden.json");
        let parsed: serde_json::Value =
            serde_json::from_str(FIXTURE).expect("fixture golden non e' JSON valido");
        let cases = parsed["cases"]
            .as_array()
            .expect("fixture senza array 'cases'");
        for case in cases {
            let name = case["name"].as_str().unwrap_or("<senza nome>");
            let input = case["input"].as_str().expect("input string");
            let expected = case["expected_stop_reason"]
                .as_str()
                .expect("expected_stop_reason");
            let actual = classify_text(input);
            assert_eq!(
                actual.stop_reason, expected,
                "caso '{name}' divergente dal golden: input={input:?}",
            );
        }
    }
    // jscpd:ignore-end
}
