//! Anonimizzazione di codice sorgente nei prompt.
//!
//! Porting di `packages/llm-gateway/src/redaction/code-anonymizer.ts` (fase 3,
//! regex-based). Sostituisce con placeholder reidratabili:
//!  1. identificatori annotati con `@confidential` (e tutti i loro usi);
//!  2. valori inline assegnati a campi sensibili (`password=`, `token:`, ...);
//!  3. string literal ad alta entropia (probabili token/hash).
//!
//! Tutti i valori sostituiti finiscono nella `RedactionMap` per la
//! reidratazione post-flight.
//!
//! Regola F: nessun log dei valori; il modulo ritorna solo conteggi e tipi.

use std::sync::LazyLock;

use regex::Regex;

use super::redaction_map::RedactionMap;

/// Identificatore dopo annotazione `@confidential` (cattura il nome).
static CONFIDENTIAL_ANNOTATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"@confidential\s*\n\s*(?:const|let|var|function|class|def|private|public|protected)\s+(\w+)",
    )
    .expect("regex confidential_annotation valida")
});

/// String literal (tra apici, doppi apici o backtick) di >=20 char base64-like.
static SECRET_STRING_LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(["'`])[A-Za-z0-9+/=_\-]{20,}(["'`])"#).expect("regex literal valida"));

/// Assegnazione inline `campo = "valore"` (>=8 char), per tre tipi di apice.
static INLINE_SECRET_DOUBLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:password|secret|token|key|api_?key|access_?key)\s*[:=]\s*"([^"]{8,})""#)
        .expect("regex inline double valida")
});
static INLINE_SECRET_SINGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:password|secret|token|key|api_?key|access_?key)\s*[:=]\s*'([^']{8,})'")
        .expect("regex inline single valida")
});
static INLINE_SECRET_BACKTICK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:password|secret|token|key|api_?key|access_?key)\s*[:=]\s*`([^`]{8,})`")
        .expect("regex inline backtick valida")
});

/// Esito dell'anonimizzazione.
#[derive(Debug, Clone, Default)]
pub struct AnonymizationResult {
    pub text: String,
    /// Numero di sostituzioni effettuate.
    pub count: usize,
    /// Tipi di valore sostituiti (`identifier`, `secret_value`, `high_entropy_string`).
    pub types: Vec<String>,
}

/// Anonimizzatore di codice. Stateless: la `RedactionMap` e' passata per
/// raccogliere i placeholder.
#[derive(Debug, Default, Clone, Copy)]
pub struct CodeAnonymizer;

impl CodeAnonymizer {
    /// Anonimizza `text` usando `map` per la reidratazione. Ritorna il testo
    /// modificato, il conteggio e i tipi toccati.
    pub fn anonymize(&self, text: &str, map: &mut RedactionMap) -> AnonymizationResult {
        let mut result = text.to_string();
        let mut count = 0usize;
        let mut types: Vec<String> = Vec::new();

        // 1. Identificatori @confidential: sostituisce il nome con un placeholder
        //    in TUTTI i suoi usi.
        let conf_names: Vec<String> = CONFIDENTIAL_ANNOTATION
            .captures_iter(text)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        for name in conf_names {
            let placeholder = map.store(&name, "identifier");
            // Sostituisce ogni occorrenza dell'identificatore come parola intera.
            let word_re = Regex::new(&format!(r"\b{}\b", regex::escape(&name)))
                .expect("regex word-boundary su identificatore valida");
            result = word_re.replace_all(&result, placeholder.as_str()).into_owned();
            count += 1;
            push_unique(&mut types, "identifier");
        }

        // 2. Assegnazioni inline di segreti (3 tipi di apice). La cattura 1 e' il
        //    valore tra apici; sostituiamo solo il valore, preservando il resto.
        for re in [
            &*INLINE_SECRET_DOUBLE,
            &*INLINE_SECRET_SINGLE,
            &*INLINE_SECRET_BACKTICK,
        ] {
            let (next, n) = replace_inner_capture(re, &result, map, "secret_value");
            result = next;
            if n > 0 {
                count += n;
                push_unique(&mut types, "secret_value");
            }
        }

        // 3. String literal ad alta entropia.
        let (next, n) = replace_high_entropy(&result, map);
        result = next;
        if n > 0 {
            count += n;
            push_unique(&mut types, "high_entropy_string");
        }

        AnonymizationResult {
            text: result,
            count,
            types,
        }
    }
}

/// Aggiunge un tipo alla lista se non gia' presente.
fn push_unique(types: &mut Vec<String>, t: &str) {
    if !types.iter().any(|x| x == t) {
        types.push(t.to_string());
    }
}

/// Sostituisce la cattura 1 (valore tra apici) di ogni match con un placeholder,
/// preservando il contesto. Ritorna il testo e il numero di sostituzioni.
fn replace_inner_capture(
    re: &Regex,
    text: &str,
    map: &mut RedactionMap,
    kind: &str,
) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let mut count = 0usize;

    for caps in re.captures_iter(text) {
        let whole = caps.get(0).expect("match 0 sempre presente");
        let inner = match caps.get(1) {
            Some(g) => g,
            None => continue,
        };
        // Testo prima del match invariato.
        out.push_str(&text[last..whole.start()]);
        // Ricostruisce il match sostituendo solo il valore catturato.
        let placeholder = map.store(inner.as_str(), kind);
        let before_inner = &text[whole.start()..inner.start()];
        let after_inner = &text[inner.end()..whole.end()];
        out.push_str(before_inner);
        out.push_str(&placeholder);
        out.push_str(after_inner);
        last = whole.end();
        count += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// Sostituisce i literal ad alta entropia (>=20 char, entropia di Shannon > 4.0).
fn replace_high_entropy(text: &str, map: &mut RedactionMap) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let mut count = 0usize;

    for caps in SECRET_STRING_LITERAL.captures_iter(text) {
        let whole = caps.get(0).expect("match 0 sempre presente");
        let matched = whole.as_str();
        // Inner = senza i due apici esterni.
        let inner = &matched[1..matched.len() - 1];
        out.push_str(&text[last..whole.start()]);
        if looks_like_secret(inner) {
            let quote = &matched[0..1];
            let placeholder = map.store(inner, "high_entropy_string");
            out.push_str(quote);
            out.push_str(&placeholder);
            out.push_str(quote);
            count += 1;
        } else {
            out.push_str(matched);
        }
        last = whole.end();
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// Euristica entropia di Shannon: stringa >=20 char con entropia > 4.0 bit/char
/// e' un probabile token/hash. Parita' con `looksLikeSecret` del TS.
fn looks_like_secret(s: &str) -> bool {
    if s.chars().count() < 20 {
        return false;
    }
    let mut freq: std::collections::HashMap<char, usize> = std::collections::HashMap::new();
    let total = s.chars().count() as f64;
    for c in s.chars() {
        *freq.entry(c).or_insert(0) += 1;
    }
    let mut entropy = 0.0f64;
    for &n in freq.values() {
        let p = n as f64 / total;
        entropy -= p * p.log2();
    }
    entropy > 4.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonimizza_identificatore_confidential() {
        let mut map = RedactionMap::new("req");
        let code = "// @confidential\nconst apiToken = compute();\nuse(apiToken);";
        let r = CodeAnonymizer.anonymize(code, &mut map);
        assert!(r.types.contains(&"identifier".to_string()));
        // Il nome originale non deve piu' comparire.
        assert!(!r.text.contains("apiToken"));
        // Entrambi gli usi sostituiti con lo stesso placeholder.
        assert!(r.text.contains("__NEXUS_IDENTIFIER_1__"));
        // Round-trip.
        let back = map.rehydrate(&r.text);
        assert!(back.contains("apiToken"));
    }

    #[test]
    fn anonimizza_assegnazione_inline() {
        let mut map = RedactionMap::new("req");
        let code = r#"password = "superSegreta123""#;
        let r = CodeAnonymizer.anonymize(code, &mut map);
        assert!(r.count >= 1);
        assert!(r.types.contains(&"secret_value".to_string()));
        assert!(!r.text.contains("superSegreta123"));
        // Il campo e gli apici restano.
        assert!(r.text.contains("password ="));
        assert!(r.text.contains('"'));
        let back = map.rehydrate(&r.text);
        assert!(back.contains("superSegreta123"));
    }

    #[test]
    fn anonimizza_high_entropy_literal() {
        let mut map = RedactionMap::new("req");
        // Stringa lunga ad alta entropia.
        let code = r#"let x = "aZ9bX2cV5wQ8rT1yU4pL7kM0nB3dF6gH""#;
        let r = CodeAnonymizer.anonymize(code, &mut map);
        assert!(r.types.contains(&"high_entropy_string".to_string()));
        assert!(!r.text.contains("aZ9bX2cV5wQ8rT1yU4pL7kM0nB3dF6gH"));
    }

    #[test]
    fn testo_normale_non_modificato() {
        let mut map = RedactionMap::new("req");
        let code = "let total = a + b; // somma normale";
        let r = CodeAnonymizer.anonymize(code, &mut map);
        assert_eq!(r.count, 0);
        assert_eq!(r.text, code);
    }

    #[test]
    fn entropia_stringa_breve_e_no() {
        // Sotto i 20 char -> mai segreto.
        assert!(!looks_like_secret("breve"));
    }
}
