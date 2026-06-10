//! Punto unico (regola L / ADR 0026) per estrarre e parsare oggetti JSON
//! prodotti da un LLM.
//!
//! I provider, anche quando il prompt vieta esplicitamente il markdown,
//! reinseriscono spesso fence ```json ... ``` o preamboli testuali prima del
//! JSON. Prima di questo modulo la stessa logica era duplicata (in forma
//! parziale e divergente) in `wiki/triple_extractor.rs` e
//! `nexus_builtin/docs.rs`, con fallback che mascheravano l'errore (es. creare
//! una sezione fittizia col testo raw, producendo documenti malformati).
//!
//! Qui la regola e' fail-loud (regola H): se l'output non contiene un oggetto
//! JSON parsabile si ritorna `Err`, il chiamante decide come gestirlo. Nessun
//! fallback silenzioso.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// Estrae lo slice del primo oggetto JSON di primo livello (`{ ... }`) presente
/// nel testo, tollerando preamboli e fence Markdown. Non valida che sia JSON
/// ben formato: serve solo a delimitare il blocco candidato.
///
/// Strategia: trova la prima `{` e l'ultima `}`. Per gli output LLM tipici
/// (un singolo oggetto, eventualmente avvolto da prosa o fence) e' robusto.
pub fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end > start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

/// Parsa un oggetto JSON da output LLM grezzo, gestendo:
/// - fence Markdown (```json ... ``` o ``` ... ```);
/// - wrapper del gateway brain: `{"content": "<json-as-string>"}` oppure
///   `{"text": "<json-as-string>"}`;
/// - preamboli/postfazioni testuali attorno all'oggetto.
///
/// Ritorna `Err` (regola H) se non si riesce a ottenere un `Value::Object`.
/// Nessun fallback che produce strutture fittizie.
pub fn parse_llm_json(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("output LLM vuoto"));
    }

    // 1) Tentativo diretto: la risposta e' gia' JSON valido.
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        // Se e' un wrapper {content|text: "..."} scendi di un livello.
        if let Some(inner) = unwrap_string_field(&v) {
            return parse_object_slice(&inner);
        }
        if v.is_object() {
            return Ok(v);
        }
    }

    // 2) Estrai il blocco {...} tollerando fence e preamboli.
    parse_object_slice(trimmed)
}

/// Se `v` e' un oggetto con un solo campo testuale noto (`content` o `text`)
/// che a sua volta sembra contenere JSON, ritorna quella stringa.
fn unwrap_string_field(v: &Value) -> Option<String> {
    for key in ["content", "text"] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

/// Rimuove i fence Markdown, estrae il blocco `{...}` e lo parsa come oggetto.
fn parse_object_slice(raw: &str) -> Result<Value> {
    let cleaned = strip_code_fences(raw);
    let slice = extract_json_object(&cleaned)
        .ok_or_else(|| anyhow!("output LLM non contiene un oggetto JSON ({{...}})"))?;
    let parsed: Value = serde_json::from_str(slice)
        .with_context(|| format!("JSON parse error sul payload LLM (len={})", slice.len()))?;
    if !parsed.is_object() {
        return Err(anyhow!("il JSON estratto non e' un oggetto"));
    }
    Ok(parsed)
}

/// Rimuove i delimitatori di code fence (```json / ```) mantenendo il corpo.
fn strip_code_fences(s: &str) -> String {
    s.replace("```json", "")
        .replace("```JSON", "")
        .replace("```", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_diretto() {
        let v = parse_llm_json(r#"{"a": 1}"#).unwrap();
        assert_eq!(v.get("a").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn json_con_fence() {
        let v = parse_llm_json("```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(v.get("a").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn json_con_preambolo() {
        let v = parse_llm_json("Ecco il risultato:\n{\"a\": 1}\nFine.").unwrap();
        assert_eq!(v.get("a").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn wrapper_content_con_json_stringa() {
        let v = parse_llm_json(r#"{"content": "```json\n{\"sections\": []}\n```"}"#).unwrap();
        assert!(v.get("sections").is_some());
    }

    #[test]
    fn errore_se_non_json() {
        assert!(parse_llm_json("nessun oggetto qui").is_err());
    }

    #[test]
    fn errore_se_vuoto() {
        assert!(parse_llm_json("   ").is_err());
    }
}
