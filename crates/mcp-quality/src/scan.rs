//! Scansione aggregata di qualita su un albero di sorgenti Rust.
//!
//! Punto unico (CLAUDE.md regola L) della logica "conta le violazioni di
//! qualita su un set di path": consumata sia dal gate ratchet `xtask
//! quality-scan` sia da eventuali altri call site. Delega a
//! [`crate::analyze_source`] per la singola unita di analisi; qui si occupa
//! solo di raccogliere i file e aggregare i conteggi delle metriche-chiave.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Directory escluse dalla raccolta: artefatti di build e dipendenze.
const SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "dist",
    "build",
    ".next",
];

/// Conteggi delle metriche-chiave su cui opera il gate ratchet. I valori
/// possono solo SCENDERE rispetto alla baseline (vedi `xtask quality-scan`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityCounts {
    /// Findings totali (tutte le categorie/severita).
    pub total: usize,
    /// Funzioni oltre la soglia di 50 righe (god-function).
    pub long_functions: usize,
    /// Funzioni con complessita ciclomatica > 20 (severity "high").
    pub complexity_high: usize,
    /// Findings di categoria "security".
    pub security: usize,
    /// Numero di file analizzati (diagnostica, non sottoposto a gate).
    pub files_scanned: usize,
}

/// Vero se il path appartiene a un file di test, escluso dai conteggi del gate.
fn is_test_file(path: &str) -> bool {
    path.contains("/tests/") || path.ends_with("/tests.rs") || path.ends_with("_tests.rs")
}

/// Sostituisce con righe vuote gli item annotati `#[cfg(test)]` (tipicamente
/// `mod tests { ... }` inline), preservando la numerazione delle righe.
///
/// Completa [`is_test_file`]: insieme esprimono l'unica policy di scoping del
/// gate, "i test non entrano nei conteggi" (regola L). Il solo controllo sul
/// path non basta, perche' in Rust la convenzione dominante per gli unit test
/// e' il modulo `#[cfg(test)]` inline nel file di produzione: senza questo
/// strip il gate dichiara "test esclusi" ma li conta, e finisce per penalizzare
/// chi aggiunge test — inclusi i test di regressione che CLAUDE.md (regola H)
/// pretende per ogni fix, e gli `unwrap()` che la regola F ammette proprio
/// nei soli `#[cfg(test)]`.
fn strip_cfg_test_items(src: &str) -> String {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    let mut blanked = vec![false; starts.len()];
    let mut line = 0usize;
    while line < starts.len() {
        let begin = starts[line];
        let text = src[begin..].lines().next().unwrap_or("");
        if text.trim_start().starts_with("#[cfg(test)]") {
            let end = item_end_offset(src, begin);
            // Azzera tutte le righe coperte dall'item, attributo incluso.
            let mut l = line;
            while l < starts.len() && starts[l] < end {
                blanked[l] = true;
                l += 1;
            }
            line = l.max(line + 1);
        } else {
            line += 1;
        }
    }

    let mut out = src
        .lines()
        .enumerate()
        .map(|(i, l)| if blanked[i] { "" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    // Senza il newline finale l'ultima riga azzerata sparirebbe da `lines()`,
    // disallineando i numeri di riga dei finding.
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Offset (esclusivo) di fine dell'item che inizia a `start`: la graffa di
/// chiusura bilanciata, oppure il `;` per un item senza body (`mod tests;`).
/// Graffe dentro commenti, stringhe e char literal non contano.
fn item_end_offset(src: &str, start: usize) -> usize {
    let b = src.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    let mut seen_brace = false;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_block_comment(b, i),
            b'r' | b'b' if raw_string_hashes(b, i).is_some() => i = skip_raw_string(b, i),
            b'"' => i = skip_string(b, i),
            b'\'' => i = skip_char_or_lifetime(src, i),
            b'{' => {
                depth += 1;
                seen_brace = true;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
                if depth == 0 && seen_brace {
                    return i;
                }
            }
            b';' if depth == 0 && !seen_brace => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Salta un commento a blocco, che in Rust puo' essere annidato.
fn skip_block_comment(b: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    let mut nest = 1usize;
    while i < b.len() && nest > 0 {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            nest += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            nest -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// Numero di `#` di una raw string che inizia a `start` (`r"`, `r#"`, `br##"`),
/// oppure `None` se li' non inizia una raw string.
fn raw_string_hashes(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    if b[i] == b'b' {
        i += 1;
        if b.get(i) != Some(&b'r') {
            return None;
        }
    }
    if b.get(i) != Some(&b'r') {
        return None;
    }
    // Un identificatore che finisce per `r`/`b` non apre una raw string.
    if start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        return None;
    }
    i += 1;
    let hashes = b[i..].iter().take_while(|c| **c == b'#').count();
    if b.get(i + hashes) == Some(&b'"') {
        Some(hashes)
    } else {
        None
    }
}

/// Salta una raw string: termina al primo `"` seguito da tanti `#` quanti
/// l'apertura. Nessun escape con `\` al suo interno.
fn skip_raw_string(b: &[u8], start: usize) -> usize {
    let Some(hashes) = raw_string_hashes(b, start) else {
        return start + 1;
    };
    let mut i = start;
    while i < b.len() && b[i] != b'"' {
        i += 1;
    }
    i += 1;
    while i < b.len() {
        if b[i] == b'"' && b[i + 1..].iter().take(hashes).filter(|c| **c == b'#').count() == hashes {
            return i + 1 + hashes;
        }
        i += 1;
    }
    b.len()
}

/// Salta una stringa normale, rispettando gli escape `\"`.
fn skip_string(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Distingue un char literal (`'x'`, `'\n'`) da un lifetime (`'a`), che non ha
/// apice di chiusura e non va saltato.
fn skip_char_or_lifetime(src: &str, start: usize) -> usize {
    let rest = &src[start + 1..];
    if rest.starts_with('\\') {
        // Escape: la chiusura e' il primo apice non preceduto da backslash.
        let b = rest.as_bytes();
        let mut i = 1usize;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b'\'' => return start + 1 + i + 1,
                _ => i += 1,
            }
        }
        return src.len();
    }
    match rest.chars().next() {
        Some(c) if rest.as_bytes().get(c.len_utf8()) == Some(&b'\'') => {
            start + 1 + c.len_utf8() + 1
        }
        _ => start + 1,
    }
}

/// Singolo finding in forma compatta per la work-list di refactor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingLite {
    pub category: String,
    pub severity: String,
    pub title: String,
    pub line: Option<usize>,
}

/// File con almeno un finding-target (long-fn / complexity-high / security):
/// unita di lavoro per un agente di refactor (un file = un task end-to-end).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTarget {
    pub file: String,
    pub long_functions: usize,
    pub complexity_high: usize,
    pub security: usize,
    /// Peso di priorita: security pesa piu della complessita, che pesa piu
    /// delle funzioni lunghe. Usato per ordinare la work-list (peggiori prima).
    pub priority: usize,
    pub findings: Vec<FindingLite>,
}

/// Sorgente nello scope del gate: con `include_tests` falso i moduli
/// `#[cfg(test)]` inline sono azzerati. Punto unico applicato da entrambe le
/// scansioni, cosi' gate e work-list non possono divergere (regola L).
fn scoped_source(src: &str, include_tests: bool) -> std::borrow::Cow<'_, str> {
    if include_tests {
        std::borrow::Cow::Borrowed(src)
    } else {
        std::borrow::Cow::Owned(strip_cfg_test_items(src))
    }
}

/// Vero se il finding rientra nei tre tipi-target del batch di refactor.
fn is_target(f: &crate::QualityFinding) -> bool {
    f.category == "security"
        || (f.category == "complexity" && f.severity == "high")
        || (f.category == "maintainability" && f.title.starts_with("Long function"))
}

/// Costruisce la work-list: i file con almeno un finding-target, con i loro
/// finding, ordinati per priorita decrescente. Punto unico della scansione
/// dettagliata (regola L), gemello di [`scan_counts`].
pub fn scan_targets(roots: &[PathBuf], include_tests: bool) -> Vec<FileTarget> {
    let mut targets: Vec<FileTarget> = Vec::new();
    for file in collect_rust_files(roots) {
        let path_str = file.to_string_lossy().replace('\\', "/");
        if !include_tests && is_test_file(&path_str) {
            continue;
        }
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        let src = scoped_source(&src, include_tests);
        let report = crate::analyze_source(&path_str, &src);

        let mut findings = Vec::new();
        let (mut long_functions, mut complexity_high, mut security) = (0usize, 0usize, 0usize);
        for f in &report.findings {
            if !is_target(f) {
                continue;
            }
            if f.category == "security" {
                security += 1;
            } else if f.category == "complexity" {
                complexity_high += 1;
            } else {
                long_functions += 1;
            }
            findings.push(FindingLite {
                category: f.category.clone(),
                severity: f.severity.clone(),
                title: f.title.clone(),
                line: f.line,
            });
        }
        if findings.is_empty() {
            continue;
        }
        let priority = security * 1000 + complexity_high * 100 + long_functions;
        targets.push(FileTarget {
            file: path_str,
            long_functions,
            complexity_high,
            security,
            priority,
            findings,
        });
    }
    targets.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.file.cmp(&b.file)));
    targets
}

/// Raccoglie ricorsivamente tutti i `.rs` sotto i `roots`, saltando build/deps.
pub fn collect_rust_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for r in roots {
        collect_recursive(r, &mut out);
    }
    out.sort();
    out
}

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name) {
                collect_recursive(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}


/// Esegue l'analisi su tutti i `.rs` sotto i `roots` e aggrega le metriche.
/// Se `include_tests` e' falso i file di test sono ignorati.
pub fn scan_counts(roots: &[PathBuf], include_tests: bool) -> QualityCounts {
    let mut counts = QualityCounts::default();
    for file in collect_rust_files(roots) {
        let path_str = file.to_string_lossy().replace('\\', "/");
        if !include_tests && is_test_file(&path_str) {
            continue;
        }
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        counts.files_scanned += 1;

        let src = scoped_source(&src, include_tests);
        let report = crate::analyze_source(&path_str, &src);
        for f in &report.findings {
            counts.total += 1;
            if f.category == "security" {
                counts.security += 1;
            }
            if f.category == "complexity" && f.severity == "high" {
                counts.complexity_high += 1;
            }
            if f.category == "maintainability" && f.title.starts_with("Long function") {
                counts.long_functions += 1;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_azzera_il_modulo_test_inline_e_preserva_la_produzione() {
        let src = "fn prod() {\n    let x = 1;\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert!(true);\n    }\n}\n";
        let out = strip_cfg_test_items(src);
        assert!(out.contains("fn prod()"), "il codice di produzione resta");
        assert!(!out.contains("assert!"), "il corpo del modulo test sparisce");
        assert!(!out.contains("#[cfg(test)]"), "l'attributo sparisce con l'item");
        assert_eq!(
            src.lines().count(),
            out.lines().count(),
            "la numerazione delle righe e' preservata"
        );
    }

    #[test]
    fn strip_non_e_ingannato_da_graffe_in_stringhe_commenti_e_char() {
        let src = "#[cfg(test)]\nmod tests {\n    fn t() {\n        let s = \"}\";\n        let r = r#\"}}}\"#;\n        let c = '}';\n        // }\n        /* } */\n    }\n}\nfn prod_dopo() {}\n";
        let out = strip_cfg_test_items(src);
        assert!(
            out.contains("fn prod_dopo()"),
            "le graffe dentro stringhe/commenti/char non chiudono il modulo in anticipo: {out}"
        );
        assert!(!out.contains("let s ="), "il corpo del test e' comunque azzerato");
    }

    #[test]
    fn strip_distingue_lifetime_da_char_literal() {
        let src =
            "#[cfg(test)]\nmod tests {\n    fn t<'a>(x: &'a str) -> &'a str {\n        x\n    }\n}\nfn prod_dopo() {}\n";
        let out = strip_cfg_test_items(src);
        assert!(
            out.contains("fn prod_dopo()"),
            "un lifetime non apre un char literal: {out}"
        );
        assert!(!out.contains("fn t<"), "il modulo test e' azzerato");
    }

    #[test]
    fn strip_gestisce_item_senza_body() {
        let src = "#[cfg(test)]\nmod tests;\nfn prod() {}\n";
        let out = strip_cfg_test_items(src);
        assert!(out.contains("fn prod()"), "la produzione resta: {out}");
        assert!(!out.contains("mod tests;"), "la dichiarazione sparisce: {out}");
    }

    #[test]
    fn scoped_source_rispetta_include_tests() {
        let src = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        assert!(
            scoped_source(src, true).contains("fn t()"),
            "con include_tests i test restano nello scope"
        );
        assert!(
            !scoped_source(src, false).contains("fn t()"),
            "senza include_tests i test escono dallo scope"
        );
    }

    #[test]
    fn unwrap_nei_test_inline_non_conta_come_finding() {
        // Regola F di CLAUDE.md: `unwrap()` e' ammesso proprio nei `#[cfg(test)]`.
        // Il gate non deve contarlo come debito.
        let src = "fn prod() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        let v: Option<u8> = None;\n        let _ = v.unwrap();\n    }\n}\n";
        let scoped = scoped_source(src, false);
        let report = crate::analyze_source("esempio.rs", &scoped);
        assert!(
            !report.findings.iter().any(|f| f.title.contains("unwrap")),
            "nessun finding unwrap dal modulo test: {:?}",
            report.findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }
}
