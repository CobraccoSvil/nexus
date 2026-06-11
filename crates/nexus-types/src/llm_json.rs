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
//! L'algoritmo di estrazione e' PARITETICO a
//! `brain/utils/json_extract.py::extract_json_block` (ADR 0032: logica
//! bilingue per localita', golden fixture obbligatoria). Tre strategie, dalla
//! piu' alla meno robusta:
//!   1. strip dei code fence markdown ai bordi, poi parse diretto;
//!   2. brace-matching counter dal primo `{` alla `}` bilanciata a qualunque
//!      profondita' (rispetta stringhe ed escape);
//!   3. fallback regex single-level (legacy).
//! La parita' e' verificata dalla golden fixture condivisa
//! `tests/fixtures/json_extract_golden.json` (letta anche da
//! `brain/tests/test_json_extract_parity.py`). Drift = bug.
//!
//! Qui la regola e' fail-loud (regola H): se l'output non contiene un oggetto
//! JSON parsabile, `parse_llm_json` ritorna `Err`, il chiamante decide come
//! gestirlo. Nessun fallback silenzioso.

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// Estrae il primo oggetto JSON valido da `text` (paritetico al Python:
/// `brain/utils/json_extract.py::extract_json_block`).
///
/// Ritorna `None` se non trova un oggetto JSON valido. I valori non-oggetto
/// (es. una lista JSON top-level) ritornano `None`: i call site si aspettano
/// un oggetto.
pub fn extract_json_block(text: &str) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    let content = strip_edge_fences(text);

    // 1. Parse diretto del contenuto pulito.
    if let Ok(parsed) = serde_json::from_str::<Value>(content) {
        return if parsed.is_object() { Some(parsed) } else { None };
    }

    // 2. Brace-matching counter (gestisce N livelli annidati, rispetta
    //    stringhe ed escape — stessa macchina a stati del Python).
    if let Some(start) = content.find('{') {
        let mut depth: i64 = 0;
        let mut in_string = false;
        let mut escape = false;
        for (i, ch) in content[start..].char_indices() {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' => escape = true,
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        let candidate = &content[start..start + i + ch.len_utf8()];
                        return match serde_json::from_str::<Value>(candidate) {
                            Ok(parsed) if parsed.is_object() => Some(parsed),
                            Ok(_) => None,
                            // Candidato bilanciato ma malformato: cade nel
                            // fallback regex (come il `break` del Python).
                            Err(_) => regex_fallback(content),
                        };
                    }
                }
                _ => {}
            }
        }
    }

    // 3. Fallback legacy regex (annidamento single-level).
    regex_fallback(content)
}

/// Strategia 3 del Python: regex single-level `\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}`.
fn regex_fallback(content: &str) -> Option<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\{[^{}]*(?:\{[^{}]*\}[^{}]*)*\}").expect("regex fallback json valida")
    });
    let m = re.find(content)?;
    match serde_json::from_str::<Value>(m.as_str()) {
        Ok(parsed) if parsed.is_object() => Some(parsed),
        _ => None,
    }
}

/// Rimuove i code fence markdown SOLO ai bordi del testo (paritetico ai due
/// `re.sub` del Python: `^```(?:json)?\s*` e `\s*```\s*$`). Un fence dentro
/// una stringa JSON non viene toccato — il vecchio `replace` globale
/// corrompeva payload contenenti "```" nei valori.
fn strip_edge_fences(s: &str) -> &str {
    let mut t = s.trim();
    if let Some(rest) = t.strip_prefix("```json") {
        t = rest.trim_start();
    } else if let Some(rest) = t.strip_prefix("```") {
        t = rest.trim_start();
    }
    if let Some(rest) = t.strip_suffix("```") {
        t = rest.trim_end();
    }
    t
}

/// Parsa un oggetto JSON da output LLM grezzo, gestendo:
/// - fence Markdown (```json ... ``` o ``` ... ```);
/// - wrapper del gateway brain: `{"content": "<json-as-string>"}` oppure
///   `{"text": "<json-as-string>"}`;
/// - preamboli/postfazioni testuali attorno all'oggetto (brace-matching).
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
            return extract_json_block(&inner).ok_or_else(|| {
                anyhow!("il wrapper content/text non contiene un oggetto JSON ({{...}})")
            });
        }
        if v.is_object() {
            return Ok(v);
        }
    }

    // 2) Estrazione paritetica (fence ai bordi, brace-matching, fallback).
    extract_json_block(trimmed)
        .ok_or_else(|| anyhow!("output LLM non contiene un oggetto JSON ({{...}})"))
}

/// VECCHIO percorso (first-`{`/last-`}`): NON gestisce testo dopo l'oggetto
/// ne' graffe nelle stringhe. Mantenuto solo finche' `wiki/triple_extractor.rs`
/// non converge su `parse_llm_json` (sequenza: nuovo -> valida -> rimuovi).
pub fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end > start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

/// Se `v` e' un oggetto con un campo testuale noto (`content` o `text`),
/// ritorna quella stringa (wrapper del gateway brain).
fn unwrap_string_field(v: &Value) -> Option<String> {
    for key in ["content", "text"] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
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

    #[test]
    fn graffe_in_stringa_non_confondono_il_matching() {
        // Il vecchio first-{/last-} avrebbe incluso " post" nel candidato;
        // il brace-matching si ferma alla graffa bilanciata vera.
        let v = parse_llm_json(r#"pre {"msg": "apre { e basta"} post {"x": 1}"#).unwrap();
        assert_eq!(
            v.get("msg").and_then(Value::as_str),
            Some("apre { e basta")
        );
    }

    /// Parita' cross-language con ``brain/utils/json_extract.py``.
    ///
    /// La fixture e' la stessa letta da ``brain/tests/test_json_extract_parity.py``:
    /// se questo test e quello pytest passano entrambi, l'estrazione e'
    /// identica fra Rust e Python (regola L / ADR 0026 / ADR 0032, Wave 6).
    // jscpd:ignore-start
    // Boilerplate caricamento fixture: duplicazione GIUSTIFICATA coi gemelli
    // rag::chunker::tests / provider_error_classifier::tests (golden test).
    #[test]
    fn parita_cross_language_da_fixture_golden() {
        const FIXTURE: &str = include_str!("../../../tests/fixtures/json_extract_golden.json");
        let parsed: serde_json::Value =
            serde_json::from_str(FIXTURE).expect("fixture golden non e' JSON valido");
        let cases = parsed["cases"]
            .as_array()
            .expect("fixture senza array 'cases'");
        for case in cases {
            let name = case["name"].as_str().unwrap_or("<senza nome>");
            let input = case["input"].as_str().expect("input string");
            let expected = &case["expected"];
            let actual = extract_json_block(input);
            match (actual, expected.is_null()) {
                (None, true) => {}
                (Some(v), false) => assert_eq!(
                    &v, expected,
                    "caso '{name}' divergente fra Rust e Python: input={input:?}",
                ),
                (got, _) => panic!(
                    "caso '{name}' divergente fra Rust e Python: \
                     atteso {expected}, ottenuto {got:?} (input={input:?})"
                ),
            }
        }
    }
    // jscpd:ignore-end
}
