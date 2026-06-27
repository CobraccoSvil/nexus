//! `xtask quality-scan` — gate ratchet sulle metriche di qualita del codice
//! Rust del workspace, basato su [`mcp_quality::scan`] (punto unico, regola L).
//!
//! Metriche sottoposte a gate (possono solo SCENDERE rispetto alla baseline):
//!   - `total`            findings totali
//!   - `long_functions`   funzioni > 50 righe
//!   - `complexity_high`  funzioni con complessita ciclomatica > 20
//!   - `security`         findings di categoria security
//!
//! Modalita:
//!   --gate     (default) confronta con la baseline; esce non-zero se una
//!              metrica e' PEGGIORATA. Se e' migliorata, invita a --update.
//!   --update   ricalcola e riscrive la baseline (riallineamento al ribasso).
//!
//! Flag:
//!   --baseline <PATH>   file baseline (default scripts/quality-baseline.json)
//!   --root <PATH>       radice da scansionare, ripetibile (default crates)
//!   --include-tests     includi i file di test nei conteggi

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use mcp_quality::scan::{QualityCounts, scan_counts, scan_targets};

const DEFAULT_BASELINE: &str = "scripts/quality-baseline.json";

struct Opts {
    update: bool,
    baseline: String,
    roots: Vec<PathBuf>,
    include_tests: bool,
    export: Option<String>,
}

fn parse(args: &[String]) -> Opts {
    let mut update = false;
    let mut include_tests = false;
    let mut baseline = DEFAULT_BASELINE.to_string();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut export: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--update" => update = true,
            "--gate" => update = false,
            "--include-tests" => include_tests = true,
            "--baseline" => {
                if let Some(v) = it.next() {
                    baseline = v.clone();
                }
            }
            "--root" => {
                if let Some(v) = it.next() {
                    roots.push(PathBuf::from(v));
                }
            }
            "--export" => {
                if let Some(v) = it.next() {
                    export = Some(v.clone());
                }
            }
            _ => {}
        }
    }
    if roots.is_empty() {
        roots.push(PathBuf::from("crates"));
    }
    Opts {
        update,
        baseline,
        roots,
        include_tests,
        export,
    }
}

fn load_baseline(path: &str) -> Result<QualityCounts> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("baseline non trovata: {path} (esegui `xtask quality-scan --update`)"))?;
    let counts: QualityCounts =
        serde_json::from_str(&raw).with_context(|| format!("baseline non valida: {path}"))?;
    Ok(counts)
}

fn write_baseline(path: &str, counts: &QualityCounts) -> Result<()> {
    let json = serde_json::to_string_pretty(counts)?;
    std::fs::write(path, format!("{json}\n")).with_context(|| format!("scrittura baseline {path}"))?;
    Ok(())
}

/// Restituisce l'exit code (0 = ok, 1 = regressione).
pub fn run(args: &[String]) -> Result<i32> {
    let opts = parse(args);

    // Export work-list: i file con finding-target, ordinati per priorita.
    // Indipendente da gate/update — serve al batch di refactor notturno.
    if let Some(path) = &opts.export {
        let targets = scan_targets(&opts.roots, opts.include_tests);
        let (mut tot_lf, mut tot_cx, mut tot_sec) = (0usize, 0usize, 0usize);
        for t in &targets {
            tot_lf += t.long_functions;
            tot_cx += t.complexity_high;
            tot_sec += t.security;
        }
        let json = serde_json::to_string_pretty(&targets)?;
        std::fs::write(path, format!("{json}\n"))
            .with_context(|| format!("scrittura export {path}"))?;
        println!(
            "xtask quality-scan: work-list esportata in {path}\n  file target: {}\n  long-fn: {} | complexity-high: {} | security: {}",
            targets.len(),
            tot_lf,
            tot_cx,
            tot_sec
        );
        return Ok(0);
    }

    let current = scan_counts(&opts.roots, opts.include_tests);

    println!(
        "xtask quality-scan: {} file analizzati ({})",
        current.files_scanned,
        if opts.include_tests { "test inclusi" } else { "test esclusi" }
    );

    if opts.update {
        write_baseline(&opts.baseline, &current)?;
        println!("Baseline aggiornata in {}:", opts.baseline);
        print_counts("  ", &current);
        return Ok(0);
    }

    let base = load_baseline(&opts.baseline)?;
    let metrics: [(&str, usize, usize); 4] = [
        ("findings totali", current.total, base.total),
        ("funzioni >50 righe", current.long_functions, base.long_functions),
        ("complessita >20", current.complexity_high, base.complexity_high),
        ("security", current.security, base.security),
    ];

    println!("  {:<22} {:>8} {:>8} {:>8}", "metrica", "attuale", "baseline", "delta");
    let mut regressions: Vec<String> = Vec::new();
    let mut improvements = 0usize;
    for (name, cur, b) in metrics {
        let delta = cur as i64 - b as i64;
        println!("  {name:<22} {cur:>8} {b:>8} {delta:>+8}");
        if cur > b {
            regressions.push(format!("{name}: {b} -> {cur} (+{})", cur - b));
        } else if cur < b {
            improvements += 1;
        }
    }

    if !regressions.is_empty() {
        eprintln!("\nxtask quality-scan: REGRESSIONE qualita (le metriche possono solo scendere):");
        for r in &regressions {
            eprintln!("  - {r}");
        }
        eprintln!("Riduci le violazioni introdotte, oppure se sono giustificate aggiorna la baseline con `xtask quality-scan --update`.");
        bail!("quality-scan gate fallito");
    }

    if improvements > 0 {
        println!(
            "\nQualita migliorata su {improvements} metrica/e: riallinea la baseline al ribasso con `xtask quality-scan --update`."
        );
    } else {
        println!("\nxtask quality-scan: nessuna regressione.");
    }
    Ok(0)
}

fn print_counts(prefix: &str, c: &QualityCounts) {
    println!("{prefix}findings totali   : {}", c.total);
    println!("{prefix}funzioni >50 righe: {}", c.long_functions);
    println!("{prefix}complessita >20   : {}", c.complexity_high);
    println!("{prefix}security          : {}", c.security);
}
