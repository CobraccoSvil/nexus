//! Parsing strutturato degli errori di build/compilazione (capacita' 2 del
//! layer di osservabilita').
//!
//! Trasforma l'output grezzo di un comando (stdout+stderr) in una lista di
//! `Diagnostic { file, line, column, severity, message, source }`, cosi' che
//! sia l'agente AI sia l'editor possano consumarli in forma strutturata invece
//! di leggere testo libero.
//!
//! Modulo PURO (nessun IO): tutta la logica e' in `parse_diagnostics`, testata
//! in-file. Riusato sia dall'endpoint `execute_command` (errori di build su
//! richiesta) sia dal `service_observer` (stack-trace runtime).
//!
//! Parser supportati: TypeScript (`tsc`), Rust (`cargo`), ESLint (formato
//! stylish), Python (traceback). I parser sono applicati TUTTI sull'output
//! combinato (un `npm run build` puo' invocare `tsc`): le regex sono
//! abbastanza specifiche da non produrre falsi positivi incrociati.

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

/// Cap difensivo: oltre questo numero di diagnostics smettiamo di accumulare.
const MAX_DIAGNOSTICS: usize = 500;

/// Una singola diagnostica strutturata estratta dall'output di build.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: u32,
    pub column: Option<u32>,
    /// "error" | "warning"
    pub severity: String,
    pub message: String,
    /// Strumento di provenienza: "tsc" | "cargo" | "eslint" | "python"
    pub source: String,
}

// TypeScript classico:  src/foo.ts(12,5): error TS2322: Type '...'
static RE_TSC_PAREN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^(?P<file>[^\s(][^(\n]*?)\((?P<line>\d+),(?P<col>\d+)\):\s+(?P<sev>error|warning)\s+TS\d+:\s+(?P<msg>.+)$")
        .expect("regex tsc paren valida")
});

// TypeScript pretty:  src/foo.ts:12:5 - error TS2322: Type '...'
static RE_TSC_COLON: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^(?P<file>[^\s][^:\n]*?):(?P<line>\d+):(?P<col>\d+)\s+-\s+(?P<sev>error|warning)\s+TS\d+:\s+(?P<msg>.+)$")
        .expect("regex tsc colon valida")
});

// Rust/cargo:  error[E0308]: mismatched types \n   --> src/main.rs:10:5
static RE_CARGO: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^(?P<sev>error|warning)(?:\[[A-Z]\d+\])?:\s+(?P<msg>[^\n]*)\n\s*-->\s+(?P<file>[^:\n]+):(?P<line>\d+):(?P<col>\d+)")
        .expect("regex cargo valida")
});

// Python traceback frame:  File "app.py", line 42, in <module>
static RE_PY_FRAME: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?m)^\s*File "(?P<file>[^"]+)", line (?P<line>\d+)"#)
        .expect("regex python frame valida")
});

// Python riga finale errore:  ValueError: invalid literal
static RE_PY_ERROR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^(?P<exc>[A-Za-z_][A-Za-z0-9_.]*(?:Error|Exception|Warning)):\s*(?P<msg>.*)$")
        .expect("regex python error valida")
});

// ESLint stylish: riga path del file (assoluto/relativo, niente :line:col).
static RE_ESLINT_FILE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?P<file>(?:/|\./|[A-Za-z]:\\|[\w.-]).*\.(?:ts|tsx|js|jsx|mjs|cjs|vue))$")
        .expect("regex eslint file valida")
});

// ESLint stylish: riga problema  "  12:5  error  message  rule-name"
static RE_ESLINT_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s+(?P<line>\d+):(?P<col>\d+)\s+(?P<sev>error|warning)\s+(?P<msg>.+?)(?:\s{2,}[\w./-]+)?\s*$")
        .expect("regex eslint line valida")
});

fn norm_sev(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "warning" | "warn" => "warning".to_string(),
        _ => "error".to_string(),
    }
}

/// Estrae diagnostics strutturati dall'output di un comando di build.
///
/// `command` e' accettato per coerenza/futuro hinting ma i parser sono
/// applicati tutti: un comando wrapper (es. `npm run build`) puo' delegare a
/// piu' strumenti. Output deduplicato e limitato a `MAX_DIAGNOSTICS`.
pub fn parse_diagnostics(_command: &str, stdout: &str, stderr: &str) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    let mut seen: HashSet<(String, u32, Option<u32>, String)> = HashSet::new();
    // tsc/cargo possono scrivere su stdout o stderr a seconda del wrapper.
    let combined = format!("{stdout}\n{stderr}");

    let mut push = |d: Diagnostic, out: &mut Vec<Diagnostic>| {
        if out.len() >= MAX_DIAGNOSTICS {
            return;
        }
        let key = (d.file.clone(), d.line, d.column, d.message.clone());
        if seen.insert(key) {
            out.push(d);
        }
    };

    for re in [&*RE_TSC_PAREN, &*RE_TSC_COLON] {
        for c in re.captures_iter(&combined) {
            push(
                Diagnostic {
                    file: c["file"].trim().to_string(),
                    line: c["line"].parse().unwrap_or(0),
                    column: c["col"].parse().ok(),
                    severity: norm_sev(&c["sev"]),
                    message: c["msg"].trim().to_string(),
                    source: "tsc".to_string(),
                },
                &mut out,
            );
        }
    }

    for c in RE_CARGO.captures_iter(&combined) {
        push(
            Diagnostic {
                file: c["file"].trim().to_string(),
                line: c["line"].parse().unwrap_or(0),
                column: c["col"].parse().ok(),
                severity: norm_sev(&c["sev"]),
                message: c["msg"].trim().to_string(),
                source: "cargo".to_string(),
            },
            &mut out,
        );
    }

    parse_eslint(&combined, &mut |d| push(d, &mut out));
    parse_python(&combined, &mut |d| push(d, &mut out));

    out
}

/// ESLint stylish: parsing riga-per-riga con file "corrente" come stato.
fn parse_eslint(text: &str, push: &mut dyn FnMut(Diagnostic)) {
    let mut current_file: Option<String> = None;
    for raw_line in text.lines() {
        if let Some(c) = RE_ESLINT_FILE.captures(raw_line) {
            current_file = Some(c["file"].trim().to_string());
            continue;
        }
        if let Some(file) = &current_file {
            if let Some(c) = RE_ESLINT_LINE.captures(raw_line) {
                push(Diagnostic {
                    file: file.clone(),
                    line: c["line"].parse().unwrap_or(0),
                    column: c["col"].parse().ok(),
                    severity: norm_sev(&c["sev"]),
                    message: c["msg"].trim().to_string(),
                    source: "eslint".to_string(),
                });
            } else if raw_line.trim().is_empty() {
                // riga vuota = fine blocco file
                current_file = None;
            }
        }
    }
}

/// Python traceback: l'ultimo frame eredita il messaggio della riga finale
/// di eccezione (se presente).
fn parse_python(text: &str, push: &mut dyn FnMut(Diagnostic)) {
    let frames: Vec<(String, u32)> = RE_PY_FRAME
        .captures_iter(text)
        .map(|c| (c["file"].trim().to_string(), c["line"].parse().unwrap_or(0)))
        .collect();
    if frames.is_empty() {
        return;
    }
    let exc_msg: Option<String> = RE_PY_ERROR
        .captures_iter(text)
        .last()
        .map(|c| format!("{}: {}", c["exc"].trim(), c["msg"].trim()));

    let last_idx = frames.len() - 1;
    for (i, (file, line)) in frames.into_iter().enumerate() {
        let message = if i == last_idx {
            exc_msg
                .clone()
                .unwrap_or_else(|| "Traceback (vedi log completo)".to_string())
        } else {
            "Traceback frame".to_string()
        };
        push(Diagnostic {
            file,
            line,
            column: None,
            severity: "error".to_string(),
            message,
            source: "python".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tsc_paren_format() {
        let stderr = "src/cart.ts(47,12): error TS2532: Object is possibly 'undefined'.";
        let d = parse_diagnostics("npm run build", "", stderr);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].file, "src/cart.ts");
        assert_eq!(d[0].line, 47);
        assert_eq!(d[0].column, Some(12));
        assert_eq!(d[0].severity, "error");
        assert_eq!(d[0].source, "tsc");
    }

    #[test]
    fn parse_tsc_pretty_format() {
        let out = "src/app.tsx:10:3 - error TS2304: Cannot find name 'foo'.";
        let d = parse_diagnostics("tsc", out, "");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].file, "src/app.tsx");
        assert_eq!(d[0].line, 10);
        assert_eq!(d[0].column, Some(3));
    }

    #[test]
    fn parse_cargo_format() {
        let stderr = "error[E0308]: mismatched types\n   --> src/main.rs:10:20\n    |\n10  |     let x: u8 = \"s\";";
        let d = parse_diagnostics("cargo build", "", stderr);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].file, "src/main.rs");
        assert_eq!(d[0].line, 10);
        assert_eq!(d[0].column, Some(20));
        assert_eq!(d[0].source, "cargo");
        assert!(d[0].message.contains("mismatched types"));
    }

    #[test]
    fn parse_eslint_stylish() {
        let out = "/proj/src/index.js\n  3:10  error  'x' is assigned but never used  no-unused-vars\n  5:1   warning  Unexpected console  no-console\n";
        let d = parse_diagnostics("eslint .", out, "");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].file, "/proj/src/index.js");
        assert_eq!(d[0].line, 3);
        assert_eq!(d[0].severity, "error");
        assert_eq!(d[1].severity, "warning");
        assert_eq!(d[0].source, "eslint");
    }

    #[test]
    fn parse_python_traceback() {
        let stderr = "Traceback (most recent call last):\n  File \"app.py\", line 42, in <module>\n    main()\n  File \"lib.py\", line 7, in main\n    raise ValueError(\"bad\")\nValueError: bad input";
        let d = parse_diagnostics("python app.py", "", stderr);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].file, "app.py");
        assert_eq!(d[1].file, "lib.py");
        // l'ultimo frame eredita il messaggio dell'eccezione
        assert!(d[1].message.contains("ValueError: bad input"));
        assert_eq!(d[1].source, "python");
    }

    #[test]
    fn no_false_positives_on_clean_output() {
        let out = "Compiled successfully.\n  Build done in 3.2s\n";
        let d = parse_diagnostics("npm run build", out, "");
        assert!(d.is_empty());
    }

    #[test]
    fn dedup_identical() {
        let stderr = "src/a.ts(1,1): error TS1: dup\nsrc/a.ts(1,1): error TS1: dup";
        let d = parse_diagnostics("tsc", "", stderr);
        assert_eq!(d.len(), 1);
    }
}
