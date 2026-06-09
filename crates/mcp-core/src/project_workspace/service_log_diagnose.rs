//! Diagnosi LLM cross-tecnologia dei log dei servizi di progetto.
//!
//! Sostituisce il pattern-matching hardcoded (`detect_crash`): invece di una
//! lista di stringhe per-linguaggio, quando il `service_observer` rileva in modo
//! STRUTTURALE che un servizio non funziona (porta non in ascolto / stato failed
//! / restart-loop), le ultime righe di log vengono classificate da un LLM. Cosi'
//! il riconoscimento funziona per qualunque tecnologia (Node, Python, .NET,
//! Java, Go, Rust, React, ...) senza chiavi fisse.
//!
//! Punto unico (regola L): provider+modello da `resolve_purpose_model`
//! (purpose `service_log_diagnosis`, tier-only DB-driven), prompt da
//! `nexus_prompt_templates` (`system.service_log_diagnosis`, configurabile a
//! caldo), inferenza via `orchestrator.neural.generate_completion`.
//!
//! Best-effort (regola H, no single point of failure): se il purpose non e'
//! configurato / il routing non e' disponibile / l'LLM fallisce / il modello
//! giudica i log sani (`is_error=false`), ritorna `None`. Il chiamante registra
//! comunque il problema strutturale con il log grezzo, cosi' resta visibile.

use crate::internal_routing::{resolve_purpose_model, PurposeResolution};
use crate::AppState;

/// Purpose (nexus_purpose_model) e template (nexus_prompt_templates), mig 0384.
const PURPOSE: &str = "service_log_diagnosis";
const TEMPLATE_KEY: &str = "system.service_log_diagnosis";
/// Coda di log inviata all'LLM (gli ultimi caratteri: la parte piu' informativa
/// di un crash). Tiene il prompt entro dimensioni ragionevoli.
const MAX_LOG_CHARS: usize = 6000;

/// Esito della diagnosi LLM di un blocco di log applicativi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LlmDiagnosis {
    /// Categoria sintetica tecnologia-agnostica (snake_case), es. "dependency_missing".
    pub error_kind: String,
    /// Linguaggio/runtime dedotto dai log (es. "node", "python", "dotnet").
    pub language: String,
    /// Una frase con la causa radice (gia' leggibile per il pannello Problemi).
    pub summary: String,
    /// "error" | "warning".
    pub severity: String,
}

/// Estrae il primo oggetto JSON (`{...}`) da un testo, tollerando code-fence o
/// testo attorno (alcuni modelli incapsulano la risposta). Niente regex/pattern
/// d'errore: e' solo delimitazione del JSON.
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

/// Parsa il JSON di diagnosi prodotto dall'LLM (funzione pura, testabile).
///
/// Ritorna `None` se: il JSON non e' interpretabile, oppure `is_error=false`
/// (il modello ritiene i log sani -> niente problema, evita falsi positivi).
pub(crate) fn parse_diagnosis(raw: &str) -> Option<LlmDiagnosis> {
    let json_str = extract_json_object(raw)?;
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;

    // Solo errori reali: is_error deve essere esplicitamente true.
    if !v.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false) {
        return None;
    }

    let get = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };

    let error_kind = {
        let k = get("error_kind");
        if k.is_empty() {
            "unknown".to_string()
        } else {
            k
        }
    };
    let language = {
        let l = get("language");
        if l.is_empty() {
            "unknown".to_string()
        } else {
            l
        }
    };
    // severity: tutto cio' che non e' esplicitamente "warning" e' "error"
    // (un servizio rilevato unhealthy e' un problema, conservativo).
    let severity = if get("severity").eq_ignore_ascii_case("warning") {
        "warning".to_string()
    } else {
        "error".to_string()
    };

    Some(LlmDiagnosis {
        error_kind,
        language,
        summary: get("summary"),
        severity,
    })
}

/// Prende la CODA del log (ultimi `MAX_LOG_CHARS` caratteri): per un crash la
/// parte finale e' la piu' rilevante.
fn tail(log_text: &str, max_chars: usize) -> String {
    let total = log_text.chars().count();
    if total <= max_chars {
        return log_text.to_string();
    }
    log_text.chars().skip(total - max_chars).collect()
}

/// Diagnostica un blocco di log via LLM. `log_text` deve essere gia' ripulito
/// dagli escape ANSI dal chiamante. Best-effort: vedi nota di modulo.
pub(crate) async fn diagnose_logs(
    state: &AppState,
    unit: &str,
    log_text: &str,
) -> Option<LlmDiagnosis> {
    if log_text.trim().is_empty() {
        return None;
    }

    let (provider, model) = match resolve_purpose_model(state, PURPOSE).await {
        PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        PurposeResolution::NoCapableModel { tier } => {
            tracing::warn!(
                unit = %unit,
                "service_log_diagnose: nessun modello tier '{tier}' per {PURPOSE}, uso log grezzo"
            );
            return None;
        }
        PurposeResolution::NotFound => {
            tracing::warn!(
                unit = %unit,
                "service_log_diagnose: purpose {PURPOSE} non configurato (mig 0384), uso log grezzo"
            );
            return None;
        }
        PurposeResolution::MatrixUnavailable(e) => {
            tracing::warn!(unit = %unit, error = %e, "service_log_diagnose: routing non disponibile, uso log grezzo");
            return None;
        }
    };

    // Prompt da template DB (configurabile a caldo, regola D/G). {logs} -> coda log.
    let template = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        TEMPLATE_KEY,
    )
    .await;
    if !template.contains("{logs}") {
        tracing::warn!(
            unit = %unit,
            "service_log_diagnose: template {TEMPLATE_KEY} assente o privo di {{logs}}, uso log grezzo"
        );
        return None;
    }
    let prompt = template.replace("{logs}", &tail(log_text, MAX_LOG_CHARS));

    // Niente log del prompt/sorgente (regola F): solo metadati.
    tracing::info!(
        unit = %unit,
        provider = %provider,
        model = %model,
        log_len = log_text.len(),
        "service_log_diagnose: invio LLM"
    );

    let resp = match state
        .orchestrator
        .neural
        .generate_completion(&provider, &model, &prompt)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(unit = %unit, error = %e, "service_log_diagnose: generate_completion fallito, uso log grezzo");
            return None;
        }
    };

    let content = resp
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    parse_diagnosis(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_error() {
        let raw = r#"{"is_error": true, "error_kind": "config_invalid", "language": "node",
            "summary": "tsconfig.json malformato: stringa non terminata alla riga 1", "severity": "error"}"#;
        let d = parse_diagnosis(raw).unwrap();
        assert_eq!(d.error_kind, "config_invalid");
        assert_eq!(d.language, "node");
        assert_eq!(d.severity, "error");
        assert!(d.summary.contains("tsconfig"));
    }

    #[test]
    fn parse_is_error_false_returns_none() {
        let raw = r#"{"is_error": false, "error_kind": "", "language": "node", "summary": "log normali", "severity": "warning"}"#;
        assert!(parse_diagnosis(raw).is_none());
    }

    #[test]
    fn parse_tolerates_code_fence_and_text() {
        // Modello che incapsula in fence + testo: estraiamo comunque l'oggetto.
        let raw = "Ecco l'analisi:\n```json\n{\"is_error\": true, \"error_kind\": \"dependency_missing\", \"summary\": \"manca express\", \"severity\": \"error\"}\n```";
        let d = parse_diagnosis(raw).unwrap();
        assert_eq!(d.error_kind, "dependency_missing");
        assert_eq!(d.language, "unknown"); // campo assente -> default
    }

    #[test]
    fn parse_non_json_returns_none() {
        assert!(parse_diagnosis("nessun json qui").is_none());
        assert!(parse_diagnosis("").is_none());
    }

    #[test]
    fn parse_warning_severity_preserved() {
        let raw = r#"{"is_error": true, "error_kind": "deprecation", "summary": "x", "severity": "warning"}"#;
        assert_eq!(parse_diagnosis(raw).unwrap().severity, "warning");
    }

    #[test]
    fn tail_keeps_last_chars() {
        let s: String = (0..100).map(|_| 'a').chain(std::iter::once('Z')).collect();
        let t = tail(&s, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('Z'));
    }
}
