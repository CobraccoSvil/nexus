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
