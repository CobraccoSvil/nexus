// safety: tutte le `Regex::new("...").unwrap()` in questo modulo sono
// applicate a pattern literal hardcoded, compilati UNA volta in static
// `LazyLock<Regex>` (C3, docs/tech-debt-rust.md): `analyze_source` e' chiamata
// in loop per-file dagli scan di progetto e ricompilare le regex ad ogni file
// era il collo di bottiglia. Se un pattern fosse malformato il panic
// avverrebbe al primo accesso allo static, mai a runtime su dati utente.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub mod injection;
pub mod scan;

// --- Regex compilate una sola volta (pattern literal; safety: literal valido) ---

// Definizione funzione con cattura del nome (Rust/TS/JS/Python).
static RE_FN_DEF_CAPTURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:pub\s+)?(?:async\s+)?(?:fn|function|def)\s+(\w+)").unwrap()
});
// Definizione funzione senza cattura (solo match).
static RE_FN_DEF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:pub\s+)?(?:async\s+)?(?:fn|function|def)\s+\w+").unwrap()
});
// Branch keyword per la complessita' ciclomatica (variante completa con ??).
// JS/TS don't have `match` as a control-flow keyword; omitting it avoids false
// positives from variable names like `const match of` or `match.index`.
static RE_BRANCH_FULL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(if|else if|elif|while|for|switch|case|catch|\?\?|&&|\|\|)\b").unwrap()
});
// Branch keyword per JS/TS (senza `match`, vedi check_complexity).
static RE_BRANCH_JS_TS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(if|else\s+if|elif|while|for|switch|case|catch|&&|\|\|)\b").unwrap()
});
// Branch keyword per Rust/Python (con `match`).
static RE_BRANCH_RUST_PY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(if|else\s+if|elif|while|for|match|case|catch|&&|\|\|)\b").unwrap()
});
// Funzione Rust con nome PascalCase (violazione naming).
static RE_RS_FN_UPPERCASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:pub\s+)?fn\s+([A-Z]\w*)").unwrap());
// Classe JS/TS con nome lowercase (violazione naming).
static RE_JS_CLASS_LOWERCASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"class\s+([a-z]\w*)").unwrap());
// Marker TODO/FIXME/HACK/XXX/WORKAROUND con messaggio.
static RE_TODO_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(TODO|FIXME|HACK|XXX|WORKAROUND)\b:?\s*(.*)").unwrap());
// Soppressioni di warning che possono indicare dead code.
static RE_SUPPRESSED_WARNING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#\[allow\(dead_code\)\]|// @ts-ignore|# type: ignore|#\[cfg\(dead_code\)\]")
        .unwrap()
});
// `var` JS legacy.
static RE_JS_VAR_DECL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*var\s+\w+").unwrap());
// def Python con cattura della lista parametri.
static RE_PY_FN_PARAMS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"def\s+\w+\s*\(([^)]*)\)").unwrap());
// import { X, Y } from '...' (TS/JS).
static RE_TS_NAMED_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"import\s+\{([^}]+)\}\s+from"#).unwrap());
// import Python (con eventuale from).
static RE_PY_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:from\s+\S+\s+)?import\s+(\S+)").unwrap());
// Numeri "magici" (>= 200 a 3+ cifre, o qualunque 4+ cifre).
static RE_MAGIC_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([2-9]\d{2,}|\d{4,})\b").unwrap());
// Righe da saltare nel check dei magic number (dichiarazioni/commenti).
static RE_MAGIC_SKIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:const|let|var|val|=\s*)\s*\w+\s*[=:]|//|#").unwrap());
// Branch keyword semplice per i blocchi complessi senza commenti.
static RE_BRANCH_KEYWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(if|for|while|match|switch)\b").unwrap());
// Dichiarazione const/let TS/JS con cattura del nome.
static RE_TSJS_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:const|let)\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*[=:]").unwrap());
// Dichiarazione let Rust con cattura del nome.
static RE_RS_LET_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\blet\s+(?:mut\s+)?([a-zA-Z][a-zA-Z0-9_]*)\s*[=:]").unwrap());
// Firma funzione con lista parametri lunga (>= 80 char) per il check parametri.
static RE_FN_LONG_PARAMS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:pub\s+)?(?:async\s+)?(?:fn|function|def)\s+(\w+)\s*\(([^)]{80,})").unwrap()
});
// Stringhe letterali candidate a estrazione come costante.
static RE_STRING_LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']([A-Za-z0-9_/.-]{6,40})["']"#).unwrap());
// Inizio loop (for/while con parentesi, o metodi iterativi).
static RE_LOOP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(for|while)\s*\(|\b(forEach|flatMap|reduce|each|loop)\b").unwrap()
});
// Query DB in file con JSX: pattern restrittivo (solo ORM/client espliciti).
static RE_QUERY_JSX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\.query\(|\.execute\(|\.findOne\(|\.findAll\(|prisma\.\w+\.\w+\(|knex\(|db\.\w+\(|await\s+\w+\.(query|execute|findOne|findAll|select|load)\b"
    ).unwrap()
});
// Query DB in file backend: pattern esteso (SQL inline + driver/ORM).
static RE_QUERY_BACKEND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(select|insert|update|delete)\b.*\b(from|into|set)\b|\.query\(|\.execute\(|\.findOne\(|\.findAll\(|await\s+\w+\.(query|execute|findOne|findAll|select|load)\b|prisma\.\w+\.\w+\(|knex\(|db\.\w+\("
    ).unwrap()
});
// Ordinamento/filtro applicato in codice (candidato a clausola SQL).
static RE_SORT_IN_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(sort|filter|find|reduce|slice|splice)\s*\(").unwrap());
// Chiamata DB generica nelle righe precedenti (lookback post-query).
static RE_DB_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\.query|\.execute|prisma\.|knex\(|db\.|\.from\(|SqlCommand|ExecuteReader|\.fetch\(|await fetch)"
    ).unwrap()
});

pub struct RuleOverrides {
    pub disabled_rules: std::collections::HashSet<String>,
}

impl RuleOverrides {
    pub fn empty() -> Self { Self { disabled_rules: std::collections::HashSet::new() } }
    pub fn is_disabled(&self, key: &str) -> bool { self.disabled_rules.contains(key) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityFinding {
    pub category: String,
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub line: Option<usize>,
    pub suggested_comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub file_path: String,
    pub findings: Vec<QualityFinding>,
    pub metrics: QualityMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityMetrics {
    pub total_lines: usize,
    pub code_lines: usize,
    pub comment_lines: usize,
    pub blank_lines: usize,
    pub max_complexity: usize,
    pub avg_function_length: f64,
    pub duplicate_blocks: usize,
}

pub struct FunctionBody {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub body: String,
}

pub fn extract_context_snippet(source: &str, line: usize, context: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() || line == 0 { return String::new(); }
    let idx = line.saturating_sub(1);
    let start = idx.saturating_sub(context);
    let end = (idx + context + 1).min(lines.len());
    lines[start..end].join("\n")
}

pub fn extract_function_bodies(source: &str, max_fns: usize) -> Vec<FunctionBody> {
    let lines: Vec<&str> = source.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < lines.len() && result.len() < max_fns {
        if let Some(caps) = RE_FN_DEF_CAPTURE.captures(lines[i]) {
            let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let start = i;
            let mut depth = 0i32;
            let mut found_open = false;
            let mut end = i;
            for (j, line) in lines.iter().enumerate().skip(i) {
                for ch in line.chars() {
                    match ch {
                        '{' => { found_open = true; depth += 1; }
                        '}' if found_open => {
                            depth -= 1;
                            if depth == 0 {
                                end = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if found_open && depth == 0 && j > start {
                    break;
                }
                end = j;
            }
            let body = lines[start..=end.min(lines.len() - 1)].join("\n");
            result.push(FunctionBody {
                name,
                start_line: start + 1,
                end_line: end + 1,
                body,
            });
            i = end;
        }
        i += 1;
    }
    result
}

// Esegue tutti gli analizzatori per-riga/per-funzione e appende i loro finding,
// incluso il detector SQL injection (ADR 0021).
fn run_all_analyzers(lines: &[&str], file_path: &str, source: &str, findings: &mut Vec<QualityFinding>) {
    findings.extend(check_complexity(lines, file_path));
    findings.extend(check_long_functions(lines));
    findings.extend(check_naming_conventions(lines, file_path));
    findings.extend(check_code_smells(lines));
    findings.extend(check_todos_fixmes(lines));
    findings.extend(check_dead_code_hints(lines));
    findings.extend(check_untyped_variables(lines, file_path));
    findings.extend(check_unused_imports(lines, file_path));
    findings.extend(check_missing_docs(lines, file_path));
    findings.extend(check_comment_quality(lines, file_path));
    findings.extend(check_unused_variables(lines, file_path));
    findings.extend(check_too_many_params(lines, file_path));
    findings.extend(check_repeated_literals(lines, file_path));
    findings.extend(check_db_queries_in_loops(lines, file_path, &RuleOverrides::empty()));
    findings.extend(check_duplicate_blocks_detailed(lines));

    // SQL injection nel codice applicativo (ADR 0021): detector unico condiviso
    // con il tool MCP. Gira solo su file di codice (.rs/.py/.ts/.js); per i .sql
    // ritorna sempre vuoto.
    for inj in injection::detect_sql_injection(file_path, source) {
        findings.push(QualityFinding {
            category: "security".into(),
            severity: inj.severity,
            title: "Potential SQL injection".into(),
            detail: inj.detail,
            line: Some(inj.line),
            suggested_comment: Some(
                "// REFACTOR: usare query parametrizzata (placeholder $1/?/:name + bind)".into(),
            ),
        });
    }
}

// Calcola le metriche aggregate (righe, complessita max, lunghezza media).
fn compute_metrics(lines: &[&str]) -> QualityMetrics {
    // Line metrics. Usiamo saturating_sub perche' count_comment_lines
    // puo' contare lo stesso line piu' volte (es. line in mezzo a commento
    // multilinea conta sia per il blocco che per la riga marker), portando
    // a comment_lines > total_lines - blank_lines e quindi underflow su usize.
    let total_lines = lines.len();
    let blank_lines = lines.iter().filter(|l| l.trim().is_empty()).count();
    let comment_lines = count_comment_lines(lines);
    let code_lines = total_lines
        .saturating_sub(blank_lines)
        .saturating_sub(comment_lines);

    let duplicate_blocks = find_duplicate_blocks(lines);

    let functions = extract_functions(lines);
    let max_complexity = functions.iter().map(|f| f.complexity).max().unwrap_or(0);
    let avg_function_length = if functions.is_empty() {
        0.0
    } else {
        functions.iter().map(|f| f.length as f64).sum::<f64>() / functions.len() as f64
    };

    QualityMetrics {
        total_lines,
        code_lines,
        comment_lines,
        blank_lines,
        max_complexity,
        avg_function_length,
        duplicate_blocks,
    }
}

pub fn analyze_source(file_path: &str, source: &str) -> QualityReport {
    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();

    run_all_analyzers(&lines, file_path, source, &mut findings);
    let metrics = compute_metrics(&lines);

    QualityReport {
        file_path: file_path.to_string(),
        findings,
        metrics,
    }
}

struct FunctionInfo {
    complexity: usize,
    length: usize,
}

fn count_comment_lines(lines: &[&str]) -> usize {
    let mut count = 0;
    let mut in_block = false;
    for line in lines {
        let t = line.trim();
        if t.starts_with("/*") || t.starts_with("/**") {
            in_block = true;
            count += 1;
        } else if in_block {
            count += 1;
            if t.contains("*/") {
                in_block = false;
            }
        } else if t.starts_with("//") || t.starts_with('#') {
            count += 1;
        }
    }
    count
}

fn extract_functions(lines: &[&str]) -> Vec<FunctionInfo> {
    let mut functions = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if RE_FN_DEF.is_match(lines[i]) {
            let start = i;
            let mut depth = 0;
            let mut complexity = 1;
            let mut found_open = false;

            for (j, line_j135) in lines.iter().enumerate().skip(i) {
                for ch in line_j135.chars() {
                    match ch {
                        '{' | '(' if !found_open && ch == '{' => {
                            found_open = true;
                            depth += 1;
                        }
                        '{' if found_open => depth += 1,
                        '}' if found_open => {
                            depth -= 1;
                            if depth == 0 {
                                complexity += RE_BRANCH_FULL.find_iter(line_j135).count();
                                functions.push(FunctionInfo {
                                    complexity,
                                    length: j - start + 1,
                                });
                                i = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if found_open && depth == 0 && j > start {
                    break;
                }
                if found_open {
                    complexity += RE_BRANCH_FULL.find_iter(line_j135).count();
                }
            }
        }
        i += 1;
    }
    functions
}

// Costruisce il finding "alta complessita" se la soglia (>10) e' superata.
// severity high oltre 20. Estratto per tenere `check_complexity` sotto soglia
// senza alterare la logica di scansione delle graffe (comportamento invariato).
fn complexity_finding(name: &str, complexity: usize, fn_start: usize) -> Option<QualityFinding> {
    if complexity <= 10 {
        return None;
    }
    Some(QualityFinding {
        category: "complexity".into(),
        severity: if complexity > 20 { "high" } else { "medium" }.into(),
        title: format!("High cyclomatic complexity in `{}`", name),
        detail: format!("Complexity: {} (threshold: 10)", complexity),
        line: Some(fn_start + 1),
        suggested_comment: None,
    })
}

fn check_complexity(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    // For JS/TS files, `match` is not a control-flow keyword — it's commonly used as a
    // variable name (e.g. `const match of`, `match.index`). Including `\bmatch\b` would
    // inflate complexity counts with false positives. Use `match` only for Rust/Python.
    let is_js_ts = matches!(
        std::path::Path::new(file_path).extension().and_then(|e| e.to_str()).unwrap_or(""),
        "ts" | "tsx" | "js" | "jsx"
    );
    let branch_re: &Regex = if is_js_ts { &RE_BRANCH_JS_TS } else { &RE_BRANCH_RUST_PY };
    let mut findings = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if let Some(cap) = RE_FN_DEF_CAPTURE.captures(lines[i]) {
            let name = cap[1].to_string();
            let fn_start = i;
            let mut depth = 0;
            let mut complexity = 1;
            let mut found_open = false;

            for (j, line_j196) in lines.iter().enumerate().skip(i) {
                for ch in line_j196.chars() {
                    if ch == '{' {
                        if !found_open {
                            found_open = true;
                        }
                        depth += 1;
                    } else if ch == '}' && found_open {
                        depth -= 1;
                        if depth == 0 {
                            findings.extend(complexity_finding(&name, complexity, fn_start));
                            i = j;
                            break;
                        }
                    }
                }
                if found_open && depth == 0 && j > fn_start {
                    break;
                }
                if found_open {
                    complexity += branch_re.find_iter(lines[j]).count();
                }
            }
        }
        i += 1;
    }
    findings
}

fn check_long_functions(lines: &[&str]) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if let Some(cap) = RE_FN_DEF_CAPTURE.captures(lines[i]) {
            let name = cap[1].to_string();
            let start = i;
            let mut depth = 0;
            let mut found = false;

            for (j, line_j251) in lines.iter().enumerate().skip(i) {
                for ch in line_j251.chars() {
                    if ch == '{' {
                        found = true;
                        depth += 1;
                    } else if ch == '}' && found {
                        depth -= 1;
                        if depth == 0 {
                            let length = j - start + 1;
                            if length > 50 {
                                findings.push(QualityFinding {
                                    category: "maintainability".into(),
                                    severity: if length > 100 { "high" } else { "medium" }.into(),
                                    title: format!("Long function `{}`", name),
                                    detail: format!("{} lines (threshold: 50)", length),
                                    line: Some(start + 1),
                                    suggested_comment: None,
                                });
                            }
                            i = j;
                            break;
                        }
                    }
                }
                if found && depth == 0 && j > start {
                    break;
                }
            }
        }
        i += 1;
    }
    findings
}

fn check_naming_conventions(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_rust = file_path.ends_with(".rs");
    let is_js_ts = file_path.ends_with(".ts")
        || file_path.ends_with(".js")
        || file_path.ends_with(".tsx")
        || file_path.ends_with(".jsx");

    if is_rust {
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = RE_RS_FN_UPPERCASE.captures(line) {
                findings.push(QualityFinding {
                    category: "naming".into(),
                    severity: "low".into(),
                    title: format!("Function `{}` should be snake_case", &cap[1]),
                    detail: "Rust convention: functions use snake_case".into(),
                    line: Some(i + 1),
                    suggested_comment: None,
                });
            }
        }
    }

    if is_js_ts {
        for (i, line) in lines.iter().enumerate() {
            if let Some(cap) = RE_JS_CLASS_LOWERCASE.captures(line) {
                findings.push(QualityFinding {
                    category: "naming".into(),
                    severity: "low".into(),
                    title: format!("Class `{}` should be PascalCase", &cap[1]),
                    detail: "Convention: classes use PascalCase".into(),
                    line: Some(i + 1),
                    suggested_comment: None,
                });
            }
        }
    }
    findings
}

// Smell per-riga: righe troppo lunghe (> 120 char, code) e `.unwrap()`.
fn code_smells_per_line(lines: &[&str], findings: &mut Vec<QualityFinding>) {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();

        // Long lines
        if line.len() > 120
            && !t.starts_with("//")
            && !t.starts_with('#')
            && !t.starts_with("use ")
            && !t.starts_with("import ")
        {
            findings.push(QualityFinding {
                category: "style".into(),
                severity: "low".into(),
                title: "Line too long".into(),
                detail: format!("{} chars (threshold: 120)", line.len()),
                line: Some(i + 1),
                suggested_comment: None,
            });
        }

        // Unwrap usage in non-test code
        if t.contains(".unwrap()") && !t.starts_with("//") {
            findings.push(QualityFinding {
                category: "reliability".into(),
                severity: "medium".into(),
                title: "Potential panic: `.unwrap()`".into(),
                detail: "Consider using `?` or `.expect()` with a message".into(),
                line: Some(i + 1),
                suggested_comment: None,
            });
        }
    }
}

// Emette il finding di regione fortemente indentata.
fn emit_deep_nesting(start: usize, max_indent: usize, count: usize, findings: &mut Vec<QualityFinding>) {
    findings.push(QualityFinding {
        category: "complexity".into(),
        severity: "medium".into(),
        title: "Deeply nested code".into(),
        detail: format!(
            "Region of {} line(s) starting here — max indentation level ~{} (threshold: 6)",
            count,
            max_indent / 4
        ),
        line: Some(start + 1),
        suggested_comment: None,
    });
}

// Deep nesting — grouped by contiguous region to avoid one finding per line.
// A region is a run of non-blank, non-comment lines with indent >= 24 (6 levels).
// We emit one finding per region pointing to the first line of that region.
fn code_smells_deep_nesting(lines: &[&str], findings: &mut Vec<QualityFinding>) {
    let mut region_start: Option<(usize, usize)> = None; // (line_idx, max_indent)
    let mut region_lines: usize = 0;

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") {
            // Blank/comment: close any open region but don't start new one
            if let Some((start, max_indent)) = region_start.take() {
                emit_deep_nesting(start, max_indent, region_lines, findings);
            }
            region_lines = 0;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent >= 24 {
            match region_start.as_mut() {
                None => {
                    region_start = Some((i, indent));
                    region_lines = 1;
                }
                Some((_, max_indent)) => {
                    if indent > *max_indent { *max_indent = indent; }
                    region_lines += 1;
                }
            }
        } else {
            // Indent dropped — close region if open
            if let Some((start, max_indent)) = region_start.take() {
                emit_deep_nesting(start, max_indent, region_lines, findings);
            }
            region_lines = 0;
        }
    }
    // Close any trailing region
    if let Some((start, max_indent)) = region_start.take() {
        emit_deep_nesting(start, max_indent, region_lines, findings);
    }
}

fn check_code_smells(lines: &[&str]) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    code_smells_per_line(lines, &mut findings);
    code_smells_deep_nesting(lines, &mut findings);
    findings
}

fn check_todos_fixmes(lines: &[&str]) -> Vec<QualityFinding> {
    let mut findings = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = RE_TODO_MARKER.captures(line) {
            let kind = cap[1].to_uppercase();
            let msg = cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            findings.push(QualityFinding {
                category: "maintainability".into(),
                severity: if kind == "FIXME" || kind == "HACK" {
                    "high"
                } else {
                    "medium"
                }
                .into(),
                title: format!("{} marker", kind),
                detail: if msg.is_empty() {
                    "No description provided".into()
                } else {
                    msg.to_string()
                },
                line: Some(i + 1),
                suggested_comment: None,
            });
        }
    }
    findings
}

fn check_dead_code_hints(lines: &[&str]) -> Vec<QualityFinding> {
    let mut findings = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if RE_SUPPRESSED_WARNING.is_match(line.trim()) {
            findings.push(QualityFinding {
                category: "dead_code".into(),
                severity: "low".into(),
                title: "Suppressed warning hint".into(),
                detail: "Code suppresses a linter/compiler warning — may indicate dead code".into(),
                line: Some(i + 1),
                suggested_comment: None,
            });
        }
    }
    findings
}

fn find_duplicate_blocks(lines: &[&str]) -> usize {
    // Simple duplicate detection: find groups of 4+ consecutive non-blank lines
    // that appear more than once
    let min_block = 4;
    if lines.len() < min_block * 2 {
        return 0;
    }

    let mut blocks: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for i in 0..lines.len().saturating_sub(min_block) {
        let block: Vec<&str> = lines[i..i + min_block]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if block.len() >= min_block {
            let key = block.join("\n");
            *blocks.entry(key).or_insert(0) += 1;
        }
    }

    blocks.values().filter(|&&c| c > 1).count()
}

// Segnala l'uso di `any` in TS su una singola riga (gia' trimmata in `t`).
fn untyped_ts_any(t: &str, i: usize) -> Option<QualityFinding> {
    if t.contains(": any") || t.contains("as any") || t.contains("<any>") {
        return Some(QualityFinding {
            category: "typing".into(),
            severity: "medium".into(),
            title: "TypeScript `any` type used".into(),
            detail: "Replace `any` with a specific type to improve type safety".into(),
            line: Some(i + 1),
            suggested_comment: Some("// TODO: replace `any` with a specific type".into()),
        });
    }
    None
}

// Segnala l'uso di `var` legacy in JS su una singola riga.
fn untyped_js_var(line: &str, i: usize) -> Option<QualityFinding> {
    if RE_JS_VAR_DECL.is_match(line) {
        return Some(QualityFinding {
            category: "typing".into(),
            severity: "low".into(),
            title: "Use of `var` instead of `const`/`let`".into(),
            detail: "Prefer `const` or `let` for block scoping".into(),
            line: Some(i + 1),
            suggested_comment: None,
        });
    }
    None
}

// Segnala una funzione Python i cui parametri (esclusi self/virgole) non hanno
// annotazioni di tipo.
fn untyped_py_params(line: &str, i: usize) -> Option<QualityFinding> {
    let cap = RE_PY_FN_PARAMS.captures(line)?;
    let params = &cap[1];
    if !params.trim().is_empty()
        && !params.contains(':')
        && !params.replace("self", "").replace(',', "").trim().is_empty()
    {
        return Some(QualityFinding {
            category: "typing".into(),
            severity: "low".into(),
            title: "Python function missing type annotations".into(),
            detail: "Add type hints to improve code clarity and tooling support".into(),
            line: Some(i + 1),
            suggested_comment: Some("# TODO: add type annotations".into()),
        });
    }
    None
}

fn check_untyped_variables(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_ts = file_path.ends_with(".ts") || file_path.ends_with(".tsx");
    let is_js = file_path.ends_with(".js") || file_path.ends_with(".jsx");
    let is_py = file_path.ends_with(".py");

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with('#') { continue; }

        if is_ts { findings.extend(untyped_ts_any(t, i)); }
        if is_js { findings.extend(untyped_js_var(line, i)); }
        if is_py { findings.extend(untyped_py_params(line, i)); }
    }
    findings
}

// Costruisce un finding "import possibilmente non usato" con dettaglio dato.
fn unused_import_finding(name: &str, i: usize, detail: &str) -> QualityFinding {
    QualityFinding {
        category: "dead_code".into(),
        severity: "low".into(),
        title: format!("Possibly unused import `{}`", name),
        detail: detail.into(),
        line: Some(i + 1),
        suggested_comment: None,
    }
}

// Import TS/JS `import { X, Y } from '...'` non referenziati altrove nel file.
fn unused_imports_ts_js(lines: &[&str], full_text: &str, findings: &mut Vec<QualityFinding>) {
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = RE_TS_NAMED_IMPORT.captures(line) {
            for name in cap[1].split(',') {
                let trimmed = name.trim().split(" as ").last().unwrap_or("").trim();
                if trimmed.is_empty() { continue; }
                // Conta le occorrenze fuori dalla riga di import.
                if full_text.matches(trimmed).count() <= 1 {
                    findings.push(unused_import_finding(
                        trimmed, i, "This import may not be used anywhere in the file",
                    ));
                }
            }
        }
    }
}

// Import Python non referenziati altrove nel file.
fn unused_imports_py(lines: &[&str], full_text: &str, findings: &mut Vec<QualityFinding>) {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if let Some(cap) = RE_PY_IMPORT.captures(t) {
            let name = cap[1].split(" as ").last().unwrap_or("").trim();
            let name = name.split(',').next().unwrap_or("").trim();
            if name.is_empty() { continue; }
            if full_text.matches(name).count() <= 1 {
                findings.push(unused_import_finding(
                    name, i, "This import may not be used in the file",
                ));
            }
        }
    }
}

fn check_unused_imports(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_ts_js = file_path.ends_with(".ts") || file_path.ends_with(".tsx")
        || file_path.ends_with(".js") || file_path.ends_with(".jsx");
    let is_py = file_path.ends_with(".py");

    let full_text = lines.join("\n");

    if is_ts_js { unused_imports_ts_js(lines, &full_text, &mut findings); }
    if is_py { unused_imports_py(lines, &full_text, &mut findings); }
    findings
}

// `pub fn`/`pub async fn` Rust non preceduta da doc-comment o attributo.
fn missing_docs_rust(lines: &[&str], t: &str, i: usize) -> Option<QualityFinding> {
    if !(t.starts_with("pub fn") || t.starts_with("pub async fn")) {
        return None;
    }
    let prev = if i > 0 { lines[i - 1].trim() } else { "" };
    if !prev.starts_with("///") && !prev.starts_with("#[") {
        return Some(QualityFinding {
            category: "docs".into(),
            severity: "low".into(),
            title: "Public function without documentation".into(),
            detail: "Add `/// ` doc comment to describe this function".into(),
            line: Some(i + 1),
            suggested_comment: Some("/// TODO: document this function".into()),
        });
    }
    None
}

// `export function`/`export class` TS non preceduta da blocco JSDoc.
fn missing_docs_ts(lines: &[&str], t: &str, i: usize) -> Option<QualityFinding> {
    let is_export = t.starts_with("export function") || t.starts_with("export async function")
        || t.starts_with("export class") || t.starts_with("export default function");
    if !is_export {
        return None;
    }
    let prev = if i > 0 { lines[i - 1].trim() } else { "" };
    if !prev.starts_with("*/") && !prev.starts_with("*") && !prev.starts_with("/**") {
        return Some(QualityFinding {
            category: "docs".into(),
            severity: "low".into(),
            title: "Exported function/class without JSDoc".into(),
            detail: "Add `/** */` JSDoc comment to describe this export".into(),
            line: Some(i + 1),
            suggested_comment: Some("/** TODO: document this export */".into()),
        });
    }
    None
}

// `def`/`async def` Python la cui prima riga di corpo non e' una docstring.
fn missing_docs_py(lines: &[&str], t: &str, i: usize) -> Option<QualityFinding> {
    if !(t.starts_with("def ") || t.starts_with("async def ")) {
        return None;
    }
    let next_code = lines.iter().skip(i + 1)
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .unwrap_or("");
    if !next_code.starts_with("\"\"\"") && !next_code.starts_with("'''") {
        return Some(QualityFinding {
            category: "docs".into(),
            severity: "low".into(),
            title: "Python function without docstring".into(),
            detail: "Add a docstring to describe the function".into(),
            line: Some(i + 1),
            suggested_comment: Some("# TODO: add docstring".into()),
        });
    }
    None
}

fn check_missing_docs(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_rust = file_path.ends_with(".rs");
    let is_ts = file_path.ends_with(".ts") || file_path.ends_with(".tsx");
    let is_py = file_path.ends_with(".py");

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if is_rust { findings.extend(missing_docs_rust(lines, t, i)); }
        if is_ts { findings.extend(missing_docs_ts(lines, t, i)); }
        if is_py { findings.extend(missing_docs_py(lines, t, i)); }
    }
    findings
}

// Segnala i "magic number" (non nei .sql), uno per riga al massimo.
fn comment_magic_numbers(lines: &[&str], file_path: &str, findings: &mut Vec<QualityFinding>) {
    if file_path.ends_with(".sql") { return; }
    for (i, line) in lines.iter().enumerate() {
        if RE_MAGIC_SKIP.is_match(line) { continue; }
        if let Some(m) = RE_MAGIC_NUMBER.find(line) {
            let num: u64 = m.as_str().parse().unwrap_or(0);
            // skip common non-magic: port numbers like 3000, 4000, 8080 are OK
            if num > 99 && num != 100 && num != 1000 && num != 1024 {
                findings.push(QualityFinding {
                    category: "comments".into(),
                    severity: "low".into(),
                    title: format!("Magic number `{}`", m.as_str()),
                    detail: "Consider extracting as a named constant with a comment explaining its purpose".into(),
                    line: Some(i + 1),
                    suggested_comment: Some(format!("// TODO: extract {} as a named constant", m.as_str())),
                });
                break; // only first per line
            }
        }
    }
}

// Segnala i blocchi complessi (> 15 righe, >= 3 branch) privi di commento.
fn comment_complex_blocks(lines: &[&str], findings: &mut Vec<QualityFinding>) {
    let mut block_start = 0;
    let mut branch_count = 0;
    let mut has_comment = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with("*") || t.starts_with("#") {
            has_comment = true;
        }
        if RE_BRANCH_KEYWORD.is_match(t) {
            branch_count += 1;
        }
        if t.is_empty() || i == lines.len() - 1 {
            let block_len = i - block_start;
            if block_len > 15 && branch_count >= 3 && !has_comment {
                findings.push(QualityFinding {
                    category: "comments".into(),
                    severity: "medium".into(),
                    title: "Complex block without explanatory comment".into(),
                    detail: format!("Block of {} lines with {} branches has no comments", block_len, branch_count),
                    line: Some(block_start + 1),
                    suggested_comment: Some("// TODO: add comment explaining the logic of this block".into()),
                });
            }
            block_start = i + 1;
            branch_count = 0;
            has_comment = false;
        }
    }
}

fn check_comment_quality(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let total = lines.len();
    if total == 0 { return findings; }

    let comment_count = count_comment_lines(lines);

    // File > 100 lines with 0 comments
    if total > 100 && comment_count == 0 {
        findings.push(QualityFinding {
            category: "comments".into(),
            severity: "medium".into(),
            title: "No comments in large file".into(),
            detail: format!("File has {} lines but no comments — add comments to aid understanding", total),
            line: None,
            suggested_comment: Some("// Add comments explaining the purpose and logic of key sections".into()),
        });
    }

    comment_magic_numbers(lines, file_path, &mut findings);
    comment_complex_blocks(lines, &mut findings);
    findings
}

// Costruisce un finding "variabile non usata" con dettaglio dato.
fn unused_var_finding(name: &str, i: usize, detail: String) -> QualityFinding {
    QualityFinding {
        category: "dead_code".into(),
        severity: "low".into(),
        title: format!("Unused variable `{}`", name),
        detail,
        line: Some(i + 1),
        suggested_comment: None,
    }
}

// const/let TS/JS dichiarate ma non referenziate altrove nel file.
fn unused_vars_ts_js(lines: &[&str], source: &str, findings: &mut Vec<QualityFinding>) {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with("export ") { continue; }
        for cap in RE_TSJS_DECL.captures_iter(line) {
            let name = &cap[1];
            if name == "_" || name.starts_with('_') { continue; }
            // Conta le occorrenze nel file (esclusa la dichiarazione stessa).
            if source.matches(name).count() <= 1 {
                findings.push(unused_var_finding(
                    name, i, format!("`{}` è dichiarata ma non usata nel file", name),
                ));
            }
        }
    }
}

// let Rust (non prefissate da `_`) dichiarate ma non referenziate.
fn unused_vars_rust(lines: &[&str], source: &str, findings: &mut Vec<QualityFinding>) {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("//") { continue; }
        for cap in RE_RS_LET_DECL.captures_iter(line) {
            let name = &cap[1];
            if name.starts_with('_') { continue; }
            if source.matches(name).count() <= 1 {
                findings.push(unused_var_finding(
                    name, i, format!("`{}` è dichiarata ma non usata", name),
                ));
            }
        }
    }
}

/// Variabili dichiarate ma mai usate nel file (oltre agli import già coperti da check_unused_imports).
fn check_unused_variables(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_ts = file_path.ends_with(".ts") || file_path.ends_with(".tsx");
    let is_js = file_path.ends_with(".js") || file_path.ends_with(".jsx");
    let is_rs = file_path.ends_with(".rs");
    if !is_ts && !is_js && !is_rs { return findings; }

    let source = lines.join("\n");

    if is_ts || is_js { unused_vars_ts_js(lines, &source, &mut findings); }
    if is_rs { unused_vars_rust(lines, &source, &mut findings); }
    findings
}

/// Funzioni con troppi parametri (> 5) — segnale di God Function o necessità di struct.
fn check_too_many_params(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_code = file_path.ends_with(".ts") || file_path.ends_with(".tsx")
        || file_path.ends_with(".js") || file_path.ends_with(".rs")
        || file_path.ends_with(".py") || file_path.ends_with(".cs");
    if !is_code { return findings; }

    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = RE_FN_LONG_PARAMS.captures(line) {
            let params = &cap[2];
            // Conta le virgole come approssimazione del numero di parametri
            let param_count = params.chars().filter(|&c| c == ',').count() + 1;
            if param_count > 5 {
                findings.push(QualityFinding {
                    category: "maintainability".into(),
                    severity: "medium".into(),
                    title: format!("Too many parameters in `{}`", &cap[1]),
                    // FALSO POSITIVO del detector SQL injection: la parola inglese
                    // "into" (grouping into...) combinata col placeholder {} del
                    // format! attiva SQL_KEYWORD_RE + RS_FORMAT_RE. Non e' una query:
                    // e' testo di un messaggio diagnostico. Lasciato invariato.
                    detail: format!("{} parameters (threshold: 5) — consider grouping into a struct/object", param_count),
                    line: Some(i + 1),
                    suggested_comment: None,
                });
            }
        }
    }
    findings
}

/// Costanti stringa/numero ripetute identiche in più punti senza essere estratte.
fn check_repeated_literals(lines: &[&str], file_path: &str) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    let is_code = file_path.ends_with(".ts") || file_path.ends_with(".tsx")
        || file_path.ends_with(".js") || file_path.ends_with(".rs")
        || file_path.ends_with(".py") || file_path.ends_with(".cs");
    if !is_code { return findings; }

    // Conta stringhe letterali ripetute (escluse quelle brevi o comuni)
    let mut counts: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with('#') || t.starts_with("import ") || t.starts_with("use ") { continue; }
        for cap in RE_STRING_LITERAL.captures_iter(line) {
            let val = cap[1].to_string();
            // Salta valori comuni non meritevoli di costante
            if matches!(val.as_str(), "utf-8" | "UTF-8" | "application/json" | "text/plain" | "GET" | "POST" | "PUT" | "DELETE") { continue; }
            counts.entry(val).or_default().push(i + 1);
        }
    }

    for (literal, occurrences) in counts.iter() {
        if occurrences.len() >= 3 {
            findings.push(QualityFinding {
                category: "maintainability".into(),
                severity: "low".into(),
                title: format!("Repeated string literal `\"{}\"`", literal),
                detail: format!("Appare {} volte (righe: {}). Estrarre come costante named.", occurrences.len(),
                    occurrences.iter().take(5).map(|n| n.to_string()).collect::<Vec<_>>().join(", ")),
                line: Some(occurrences[0]),
                suggested_comment: None,
            });
        }
    }
    findings
}

/// Bilancio netto di parentesi tonde IGNORANDO quelle dentro stringhe letterali
/// (`'...'`, `"..."`, `` `...` ``). Serve a capire se una chiamata `db.query(...)`
/// resta aperta su piu' righe: le parentesi dentro la stringa SQL argomento (es. la
/// lista colonne di un `INSERT`) non devono contare come apertura/chiusura di chiamata.
fn net_parens_outside_strings(s: &str) -> i32 {
    let mut depth = 0i32;
    let mut string_quote: Option<char> = None;
    let mut escaped = false;
    for ch in s.chars() {
        match string_quote {
            Some(quote) => {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == quote {
                    string_quote = None;
                }
            }
            None => match ch {
                '\'' | '"' | '`' => string_quote = Some(ch),
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            },
        }
    }
    depth
}

/// Rileva query SQL eseguite dentro loop — pattern N+1 esteso a tutti i linguaggi.
/// Regola: il DB deve fornire il dato già filtrato, ordinato e limitato. Non elaborare in codice.
fn check_db_queries_in_loops(lines: &[&str], file_path: &str, overrides: &RuleOverrides) -> Vec<QualityFinding> {
    if overrides.is_disabled("quality.n_plus_one") {
        return vec![];
    }
    let mut findings = Vec::new();
    let is_code = file_path.ends_with(".ts") || file_path.ends_with(".tsx")
        || file_path.ends_with(".js") || file_path.ends_with(".rs")
        || file_path.ends_with(".py") || file_path.ends_with(".cs")
        || file_path.ends_with(".go");
    if !is_code { return findings; }

    // Esclude file che sono chiaramente componenti UI React/JSX:
    // se il file contiene return (<...) o JSX significativo non è backend.
    // Euristiche: nome file contiene "Table", "Form", "Modal", "Panel", "View",
    // "Component", "Widget", "Page", "Screen", "Card", "Item", "Row", "Cell".
    let ui_name_keywords = [
        "Table", "Form", "Modal", "Panel", "View", "Component", "Widget",
        "Page", "Screen", "Card", "Item", "Row", "Cell", "List", "Grid",
        "Dialog", "Drawer", "Sidebar", "Header", "Footer", "Nav", "Menu",
        "Button", "Input", "Field", "Select", "Picker", "Dropdown",
    ];
    let file_name = file_path.split('/').next_back().unwrap_or(file_path);
    let is_ui_component = (file_path.ends_with(".tsx") || file_path.ends_with(".jsx"))
        && ui_name_keywords.iter().any(|kw| file_name.contains(kw));
    if is_ui_component { return findings; }

    // Per file .tsx/.jsx generici: verifica se il file contiene JSX (return con <)
    // Se sì, usa un pattern query_re molto più restrittivo (solo veri client DB/ORM).
    let is_tsx_jsx = file_path.ends_with(".tsx") || file_path.ends_with(".jsx");
    let has_jsx_return = is_tsx_jsx && lines.iter().any(|l| {
        let t = l.trim();
        t.starts_with("return (") && t.contains('<')
            || t == "return ("
            || (t.starts_with("return") && t.contains("</"))
    });

    // Pattern che indicano un loop (RE_LOOP).
    // `for` e `while` richiedono esplicitamente `(` dopo (con eventuale spazio) per evitare
    // false match su parole come "for user" dentro template literal o commenti.
    // `forEach`, `map`, `flatMap`, `reduce`, `each`, `loop` vengono lasciati come word boundary
    // perché in pratica appaiono sempre come metodi (`.forEach(`, `.map(`, ecc.).
    //
    // Pattern che indicano una query DB dentro il loop.
    // Per file con JSX: pattern molto restrittivo (solo ORM/client espliciti, no fetch generico).
    // Per file backend: pattern esteso MA limitato a veri driver/ORM DB — `await fetch(` e metodi
    // HTTP generici (.get/.post/.fetch/.read) sono ESCLUSI: sono chiamate HTTP, non query DB, e
    // causavano falsi positivi su webhook handler e API route (es. Stripe, PayPal, ecc.).
    let query_re: &Regex = if has_jsx_return { &RE_QUERY_JSX } else { &RE_QUERY_BACKEND };

    let mut loop_depth = 0usize;
    let mut loop_start_line = 0usize;
    let mut brace_depth = 0i32;
    let mut loop_brace_depth = 0i32;
    // Parentesi ancora aperte della chiamata query gia' contata: finche' > 0 siamo
    // sulle righe di continuazione della STESSA query e non dobbiamo ri-emettere.
    let mut open_query_parens = 0i32;

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with('#') { continue; }

        // Traccia depth parentesi graffe
        for ch in t.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => {
                    brace_depth -= 1;
                    if loop_depth > 0 && brace_depth <= loop_brace_depth {
                        loop_depth = loop_depth.saturating_sub(1);
                    }
                }
                _ => {}
            }
        }

        // Rileva inizio loop
        if RE_LOOP.is_match(t) && (t.contains('{') || t.ends_with(')') || t.ends_with("=>")) {
            loop_depth += 1;
            loop_start_line = i + 1;
            loop_brace_depth = brace_depth - 1;
        }

        // Dentro un loop: cerca query DB. Una singola chiamata `db.query(...)` puo'
        // estendersi su piu' righe (metodo su una riga, stringa SQL argomento sulla
        // successiva) e `RE_QUERY_BACKEND` matcha entrambe: senza deduplica la stessa
        // query genererebbe due finding su righe adiacenti. La query va contata UNA
        // volta sopprimendo i match che cadono dentro le parentesi ancora aperte della
        // chiamata gia' registrata; due query DISTINTE restano invece due finding.
        let paren_delta = net_parens_outside_strings(t);
        if open_query_parens > 0 {
            // Riga di continuazione della chiamata gia' contata: non ri-emettere.
            open_query_parens = (open_query_parens + paren_delta).max(0);
        } else if loop_depth > 0 && query_re.is_match(t) {
            findings.push(QualityFinding {
                category: "reliability".into(),
                severity: "high".into(),
                title: "DB query inside loop (N+1 pattern)".into(),
                detail: format!(
                    "Query eseguita dentro un loop (iniziato alla riga {}). \
                     Il DB deve restituire il dataset completo con JOIN/WHERE/ORDER BY/LIMIT. \
                     Non filtrare, ordinare o paginare in codice.",
                    loop_start_line
                ),
                line: Some(i + 1),
                suggested_comment: Some(
                    "// REFACTOR: spostare la query fuori dal loop, usare JOIN o WHERE IN per caricare tutti i dati in una sola chiamata".into()
                ),
            });
            // Se la chiamata resta aperta (parentesi non chiuse su questa riga),
            // sopprimi i match sulle righe di continuazione della stessa query.
            open_query_parens = paren_delta.max(0);
        }
        // Fuori da ogni loop lo stato di continuazione non ha piu' senso: azzera per
        // non trascinare parentesi non bilanciate su codice successivo.
        if loop_depth == 0 {
            open_query_parens = 0;
        }

        // Cerca sort/filter su dati che potrebbero venire dal DB
        if RE_SORT_IN_CODE.is_match(t) && !t.starts_with("//") {
            // Controlla se nelle righe precedenti (fino a 15) c'è una chiamata DB
            let lookback = lines[i.saturating_sub(15)..i].iter().any(|l| RE_DB_CALL.is_match(l));
            if lookback {
                let op = if t.contains(".sort") { "sort" }
                    else if t.contains(".filter") { "filter" }
                    else if t.contains(".find(") { "find" }
                    else { "reduce/slice" };
                findings.push(QualityFinding {
                    category: "reliability".into(),
                    severity: "medium".into(),
                    title: format!("Post-query `{}` in application code", op),
                    detail: format!(
                        "`.{}()` applicato su dati DB in codice. \
                         Preferire ORDER BY / WHERE / LIMIT / aggregazioni nel DB: \
                         più efficiente, scalabile e corretto su dataset grandi.",
                        op
                    ),
                    line: Some(i + 1),
                    suggested_comment: Some(format!(
                        "// REFACTOR: sostituire .{}() con clausola SQL (ORDER BY / WHERE / LIMIT / GROUP BY)", op
                    )),
                });
            }
        }
    }

    findings
}

/// Rileva duplicazione di codice tra blocchi dello stesso file (blocchi >= 6 righe identici).
fn check_duplicate_blocks_detailed(lines: &[&str]) -> Vec<QualityFinding> {
    let mut findings = Vec::new();
    const MIN_BLOCK: usize = 6;
    if lines.len() < MIN_BLOCK * 2 { return findings; }

    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for i in 0..lines.len().saturating_sub(MIN_BLOCK) {
        let block: Vec<&str> = lines[i..i + MIN_BLOCK]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
            .collect();
        if block.len() < MIN_BLOCK { continue; }
        let key = block.join("\n");
        if let Some(first_line) = seen.get(&key) {
            findings.push(QualityFinding {
                category: "duplication".into(),
                severity: "medium".into(),
                title: "Duplicate code block".into(),
                detail: format!(
                    "Blocco di {} righe identico a quello di riga {}. Estrarre in funzione condivisa.",
                    MIN_BLOCK, first_line
                ),
                line: Some(i + 1),
                suggested_comment: Some("// REFACTOR: estrarre questo blocco in una funzione riutilizzabile".into()),
            });
        } else {
            seen.insert(key, i + 1);
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_analysis() {
        let source = r#"
fn main() {
    let x = 1;
    // TODO: refactor this
    println!("{}", x);
}
"#;
        let report = analyze_source("main.rs", source);
        assert!(report.findings.iter().any(|f| f.title.contains("TODO")));
        assert!(report.metrics.code_lines > 0);
    }

    #[test]
    fn test_complexity_detection() {
        let source = r#"
fn complex(x: i32) {
    if x > 0 {
        if x > 10 {
            if x > 20 {
                for i in 0..x {
                    if i > 5 {
                        while i > 0 {
                            if i % 2 == 0 {
                                if i % 3 == 0 {
                                    if i % 5 == 0 {
                                        if i % 7 == 0 {
                                            println!("done");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
"#;
        let report = analyze_source("test.rs", source);
        assert!(report.findings.iter().any(|f| f.category == "complexity"));
    }

    #[test]
    fn test_naming_check() {
        let source = "pub fn BadName() { }\n";
        let report = analyze_source("lib.rs", source);
        assert!(report.findings.iter().any(|f| f.category == "naming"));
    }

    #[test]
    fn test_unwrap_detection() {
        let source = r#"
fn foo() {
    let x = some_option.unwrap();
}
"#;
        let report = analyze_source("test.rs", source);
        assert!(report.findings.iter().any(|f| f.title.contains("unwrap")));
    }

    #[test]
    fn test_n_plus_one_query_multilinea_conta_una_volta() {
        // Regressione: una singola `pool.query(...)` distribuita su piu' righe dentro
        // un loop deve produrre UN solo finding N+1, non uno per riga che matcha il
        // pattern (riga del metodo `.query(` + riga della stringa SQL `INSERT ...`).
        // Riproduce il caso reale Beaty-Book/backend/server.js:374-379.
        let source = r#"
async function seed() {
    for (let d = 1; d <= 6; d++) {
        await pool.query(
            'INSERT INTO opening_hours (location_id, day_of_week) VALUES ($1, $2) ON CONFLICT DO NOTHING',
            [lid, d]
        );
    }
}
"#;
        let report = analyze_source("server.js", source);
        let n_plus_one = report
            .findings
            .iter()
            .filter(|f| f.title.contains("N+1"))
            .count();
        assert_eq!(
            n_plus_one, 1,
            "una query multilinea deve dare un solo finding N+1, trovati {n_plus_one}"
        );
    }

    #[test]
    fn test_n_plus_one_query_distinte_restano_separate() {
        // Due chiamate DB DISTINTE nello stesso loop restano due finding: la deduplica
        // sopprime solo le righe di continuazione della stessa chiamata, non query diverse.
        let source = r#"
async function run(users) {
    for (const u of users) {
        await db.query('SELECT * FROM orders WHERE user_id = $1', [u.id]);
        await db.query('UPDATE users SET seen = true WHERE id = $1', [u.id]);
    }
}
"#;
        let report = analyze_source("service.js", source);
        let n_plus_one = report
            .findings
            .iter()
            .filter(|f| f.title.contains("N+1"))
            .count();
        assert_eq!(
            n_plus_one, 2,
            "due query distinte devono dare due finding N+1, trovati {n_plus_one}"
        );
    }

    #[test]
    fn test_net_parens_outside_strings_ignora_stringhe() {
        // Le parentesi dentro la stringa SQL non contano; solo quella della call.
        assert_eq!(net_parens_outside_strings("await pool.query("), 1);
        assert_eq!(
            net_parens_outside_strings("'INSERT INTO t (a, b) VALUES ($1, $2)',"),
            0
        );
        assert_eq!(net_parens_outside_strings(");"), -1);
        assert_eq!(
            net_parens_outside_strings("await db.query('SELECT (1)', [x]);"),
            0
        );
    }

    #[test]
    fn bench_analyze_source_200_iterazioni() {
        // MISURA C3 (docs/tech-debt-rust.md): conferma empirica che le regex
        // static LazyLock vengono compilate una sola volta e non per-file.
        // Lanciare con --nocapture per leggere il tempo:
        //   cargo test -p mcp-quality --message-format=short -- --nocapture bench_analyze_source
        // Sorgente sintetico di ~100 righe con funzioni, branch, loop e marker.
        let mut source = String::new();
        for i in 0..10 {
            source.push_str(&format!(
                "fn funzione_{i}(a: i32, b: i32) -> i32 {{\n\
                 \x20   // TODO: rifinire il blocco {i}\n\
                 \x20   let totale = a + b + {i}21;\n\
                 \x20   if a > b && b > 0 {{\n\
                 \x20       for k in 0..a {{\n\
                 \x20           println!(\"valore {{}}\", k);\n\
                 \x20       }}\n\
                 \x20   }}\n\
                 \x20   totale\n\
                 }}\n\n"
            ));
        }
        assert!(source.lines().count() >= 100, "il sorgente sintetico deve avere ~100 righe");

        let start = std::time::Instant::now();
        for _ in 0..200 {
            let report = analyze_source("bench.rs", &source);
            assert!(report.metrics.total_lines >= 100);
        }
        let elapsed = start.elapsed();
        println!(
            "bench analyze_source: 200 iterazioni su ~{} righe in {:?} ({:?} per iterazione)",
            source.lines().count(),
            elapsed,
            elapsed / 200
        );
    }
}
