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

/// La classe "la richiesta non entra in questo modello". E' un identificatore del
/// vocabolario `error_class` (regola N), non una frase: sta scritto una volta e i
/// call site lo nominano, cosi' non puo' divergere per un refuso.
pub const CONTEXT_TOO_LONG: &str = "context_too_long";

/// HTTP status -> (stop_reason, retriable).
fn http_status_to_reason(status: u16) -> Option<(&'static str, bool)> {
    match status {
        400 => Some(("invalid_request", false)),
        401 => Some(("auth_error", false)),
        403 => Some(("forbidden", false)),
        404 => Some(("not_found", false)),
        413 => Some((CONTEXT_TOO_LONG, false)),
        422 => Some(("unprocessable", false)),
        429 => Some(("rate_limit", true)),
        500 => Some(("provider_error", true)),
        502 => Some(("bad_gateway", true)),
        503 => Some(("service_unavailable", true)),
        529 => Some(("overloaded", true)),
        _ => None,
    }
}

/// Mappa il `primary_cause` STRUTTURATO del gateway (`details.primary_cause` di
/// `GatewayHttpError`) sullo `stop_reason` canonico di questo modulo.
///
/// Regola M: il gateway CLASSIFICA gia' il fallimento alla fonte (`CallFailure`);
/// appiattirlo con `to_string()` e poi ri-dedurlo con una regex sul Display e' una
/// perdita di informazione, non una classificazione. Questa funzione e' il ponte che
/// preserva il segnale.
///
/// Ritorna `Some` per OGNI causa che il gateway dichiara: quella e' la
/// classificazione, e il testo non la rivede. L'unica eccezione e' `client_error`
/// (vedi sotto), dove lo status del failure aggiunge informazione che solo il
/// chiamante possiede.
///
/// `empty_completion` e' il caso che il testo NON puo' ricostruire: il gateway ha
/// ricevuto un `200` con zero output utile (`is_degenerate_completion`) e lo riporta
/// come HTTP 500 `PROVIDER_ERROR`; sul Display resta solo "Nexus Gateway 500 Internal
/// Server Error: {body}", che la regex HTTP non intercetta -> l'errore degradava a
/// `error` -> `Transient` -> nessun contatore, nessun degrado, mai (incidente
/// z-ai/glm-4.7-flash: 3 figure del consiglio in timeout, zero apprendimento).
///
/// Le altre cause tornavano `None` sulla premessa — scritta qui — che fossero
/// "gia' ricostruibili dal testo". Non lo sono: il 2026-07-16 groq ha rifiutato una
/// richiesta della batteria per tetto token/minuto (413, `code=rate_limit_exceeded`,
/// "TPM: Limit 8000, Requested 20083") e il messaggio invitava ad alzare il piano
/// "at https://console.groq.com/settings/billing". La causa strutturata
/// (`context_too_long`) veniva scartata qui, il testo finiva alla regex billing, e
/// quella trovava la parola `billing` DENTRO L'URL DELLA DOCUMENTAZIONE ->
/// `billing_error` -> `credit_balance_too_low` -> groq spento 6h per credito
/// esaurito mentre rispondeva 200 alle chiamate normali, e ogni giro della batteria
/// chiuso `inconclusive` (zero tier `measured` su 116 modelli).
///
/// Una regex sul testo di un errore non e' una classificazione: e' un indovinello su
/// prosa che il provider puo' riscrivere quando vuole. Il gateway ha gia' la
/// risposta (status + codice macchina): questo e' il ponte che la preserva.
pub fn error_class_from_primary_cause(cause: &str) -> Option<&'static str> {
    match cause.trim() {
        "empty_completion" => Some("empty_completion"),
        // Credito/fatturazione dichiarati dal gateway: 402, oppure un CODICE
        // strutturato del provider (`insufficient_quota`, `billing_*`). E' l'unica
        // provenienza legittima di `billing_error`: mai la prosa del messaggio.
        "billing" | "cooldown_billing" => Some("billing_error"),
        // Il modello non regge QUESTA richiesta (413 senza codice di rate-limit):
        // model-specific, nessun cooldown al provider (e' sano).
        CONTEXT_TOO_LONG => Some(CONTEXT_TOO_LONG),
        // Transitorio dichiarato (429/5xx/timeout/cap per-tentativo) o provider
        // saltato perche' gia' in cooldown: il modello non e' stato misurato.
        // `Transient` = stato invariato, si ritenta al giro dopo. Nessuno dei due
        // e' un difetto del modello.
        "transient" | "cooldown" => Some("transient"),
        // Budget della richiesta esaurito da NOI, non dal provider: non e' un
        // difetto del modello ne' della sua salute.
        "request_budget_exceeded" => Some("request_budget_exceeded"),
        // `client_error` copre 400/401/403/404/422: classi con conseguenze OPPOSTE
        // (404 = modello inesistente -> disable; 400 = richiesta malformata ->
        // niente). Le distingue lo status del primo failure, che sta nei `details`
        // e non in questa stringa: decide il chiamante (vedi
        // `model_qualification::error_class_from_gateway`).
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
            stop_reason: CONTEXT_TOO_LONG.into(),
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
    fn primary_cause_empty_completion_sopravvive_come_segnale() {
        // Il caso che il TESTO non puo' ricostruire: il gateway riporta la risposta
        // degenere come HTTP 500 e sul Display resta solo "Nexus Gateway 500 Internal
        // Server Error: {body}". Prima degradava a `error` -> Transient -> nessun
        // contatore (incidente z-ai/glm-4.7-flash). Il primary_cause strutturato lo
        // preserva.
        assert_eq!(
            error_class_from_primary_cause("empty_completion"),
            Some("empty_completion")
        );
        // Prova che il testo NON basta: la regex HTTP non intercetta il Display del
        // gateway, quindi il fallback testuale non produce "empty_completion".
        let dal_testo = classify_text("Nexus Gateway 500 Internal Server Error: {\"error\":\"...\"}");
        assert_ne!(dal_testo.stop_reason, "empty_completion");
    }

    /// Il corpo VERBATIM con cui groq ha rifiutato una richiesta della batteria il
    /// 2026-07-16 (413, tetto token/minuto del piano). Nessun credito esaurito:
    /// alla stessa chiave, nello stesso minuto, una richiesta piccola tornava 200.
    const GROQ_413_TPM: &str = concat!(
        r#"{"error":{"message":"Request too large for model `openai/gpt-oss-120b` "#,
        r#"in organization `org_01kxde0tfkefr82z5htyktj0g5` service tier `on_demand` "#,
        r#"on tokens per minute (TPM): Limit 8000, Requested 20083, please reduce "#,
        r#"your message size and try again. Need more tokens? Upgrade to Dev Tier "#,
        r#"today at https://console.groq.com/settings/billing","type":"tokens","#,
        r#""code":"rate_limit_exceeded"}}"#,
    );

    /// LA REGRESSIONE: il testo di groq contiene la parola `billing`, ma solo
    /// dentro l'URL della documentazione. Il classificatore testuale ci casca — ed
    /// e' per questo che non deve piu' essere consultato quando il gateway ha gia'
    /// detto la causa.
    #[test]
    fn il_testo_di_un_rate_limit_mente_dicendo_billing() {
        assert_eq!(classify_text(GROQ_413_TPM).stop_reason, "billing_error");
        assert!(
            GROQ_413_TPM.contains("console.groq.com/settings/billing"),
            "la sola occorrenza di 'billing' e' l'URL: e' li' che la regex abbocca"
        );
    }

    /// Con la causa strutturata del gateway, lo stesso identico errore NON e' piu'
    /// un credito esaurito: groq resta acceso.
    #[test]
    fn la_causa_del_gateway_vince_sul_testo_bugiardo() {
        // Il gateway vede code=rate_limit_exceeded e dichiara `transient`.
        assert_eq!(error_class_from_primary_cause("transient"), Some("transient"));
        // Anche se lo status 413 fosse letto come contesto (nessun codice di
        // rate-limit), resta un difetto della richiesta: mai del credito.
        assert_eq!(
            error_class_from_primary_cause("context_too_long"),
            Some("context_too_long")
        );
        for cause in ["transient", "context_too_long", "cooldown", "request_budget_exceeded"] {
            assert_ne!(
                error_class_from_primary_cause(cause),
                Some("billing_error"),
                "{cause}: nessuna causa non-billing puo' spegnere un provider per credito"
            );
        }
    }

    /// `billing_error` ha UNA sola provenienza: il gateway che l'ha stabilito dallo
    /// status 402 o da un codice macchina del provider.
    #[test]
    fn solo_il_gateway_dichiara_il_credito_esaurito() {
        assert_eq!(error_class_from_primary_cause("billing"), Some("billing_error"));
        assert_eq!(
            error_class_from_primary_cause("cooldown_billing"),
            Some("billing_error")
        );
    }

    /// `client_error` resta l'unica causa senza mappa: 404 (modello inesistente) e
    /// 400 (richiesta malformata) hanno conseguenze opposte e le distingue lo
    /// status, che il chiamante legge dai `details`.
    #[test]
    fn client_error_lo_decide_il_chiamante_che_ha_lo_status() {
        assert_eq!(error_class_from_primary_cause("client_error"), None);
        assert_eq!(error_class_from_primary_cause(""), None);
    }

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
