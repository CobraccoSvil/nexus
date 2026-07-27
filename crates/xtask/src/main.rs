//! xtask — task runner interno al workspace per linting editoriale e integrità.
//!
//! Subcommand supportati:
//!   - `lint-commits <base> <head>`: scansiona i commit nell'intervallo e
//!     verifica che rispettino le regole redazionali Nexus:
//!       * nessun emoji nel messaggio di commit
//!       * diff > 500 righe richiede label `big-refactor` nel messaggio
//!       * nessun `tracing::*!` con campo `payload`, `prompt` o `response`
//!         non-hashato nei file modificati.
//!   - `audit-settings [--report|--gate|--json FILE|--no-db]`: censimento
//!     configurazioni `settings` (DB live + migrazioni vs lettori vs UI).
//!     Punto unico (regola L), gate ratchet vs audit-settings-baseline.json.
//!   - `battery-explain [modello]`: chi e' eleggibile ADESSO per la batteria di
//!     qualificazione e PERCHE'. Compone la domanda con `nexus-model-eligibility`,
//!     lo stesso crate da cui mcp-core compone il claim: la diagnosi non puo'
//!     rispondere su una regola diversa da quella che gira (regola O).
//!
//! Uscita non-zero se trova violazioni. Progettato per essere eseguito in CI
//! senza dipendenze native pesanti.

mod audit_settings;
mod battery_explain;
mod migrate;
mod premessa;
mod quality_scan;
mod service_manifests;

use std::process::Command;

use anyhow::{Context, Result, bail};
use regex::Regex;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    match cmd {
        "lint-commits" => {
            let base = args.get(2).cloned().unwrap_or_else(|| "main".into());
            let head = args.get(3).cloned().unwrap_or_else(|| "HEAD".into());
            lint_commits(&base, &head)
        }
        "audit-settings" => {
            let code = audit_settings::run(&args[2..])?;
            std::process::exit(code);
        }
        "quality-scan" => {
            let code = quality_scan::run(&args[2..])?;
            std::process::exit(code);
        }
        "battery-explain" => {
            let code = battery_explain::run(&args[2..])?;
            std::process::exit(code);
        }
        "service-manifests" => {
            let code = service_manifests::run(&args[2..])?;
            std::process::exit(code);
        }
        "migrate" => {
            let code = migrate::run(&args[2..])?;
            std::process::exit(code);
        }
        _ => {
            eprintln!("xtask — task runner interno");
            eprintln!("  lint-commits <base> <head>    Controlli redazionali sui commit");
            eprintln!("  audit-settings [flags]        Censimento settings DB/codice/UI");
            eprintln!("  quality-scan [--gate|--update] Gate ratchet qualita codice Rust");
            eprintln!(
                "  battery-explain [modello]     Eleggibilita' batteria: chi e perche' (DB live)"
            );
            eprintln!(
                "  service-manifests [flags]     Manifest di servizio derivati dal catalogo (DB live)"
            );
            eprintln!(
                "  migrate --set S [flags]       Applica un set di migrazioni (DB live)"
            );
            Ok(())
        }
    }
}

fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("esecuzione git {args:?}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {:?} fallito: {}", args, stderr);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn lint_commits(base: &str, head: &str) -> Result<()> {
    let range = format!("{base}..{head}");
    let log = git(&["log", "--format=%H%n%B%n---END---", &range])?;

    // emoji broad (BMP + supplementary symbols + dingbats)
    let emoji_re = Regex::new(
        r"[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}\u{1F000}-\u{1F2FF}\u{2700}-\u{27BF}]",
    )?;
    let tracing_leak_re =
        Regex::new(r"tracing::(?:info|debug|warn|error|trace)!\s*\([^)]*\b(?:payload|prompt|response)\s*=\s*[^%?h]")?;

    let mut violations: Vec<String> = Vec::new();

    let mut current_hash: Option<String> = None;
    let mut current_msg: Vec<String> = Vec::new();
    for line in log.lines() {
        if line == "---END---" {
            if let Some(hash) = current_hash.take() {
                let msg = current_msg.join("\n");
                check_commit(&hash, &msg, &emoji_re, &tracing_leak_re, &mut violations)?;
            }
            current_msg.clear();
        } else if current_hash.is_none() {
            if !line.is_empty() {
                current_hash = Some(line.to_string());
            }
        } else {
            current_msg.push(line.to_string());
        }
    }

    if violations.is_empty() {
        println!("xtask lint-commits: nessuna violazione nell'intervallo {range}");
        Ok(())
    } else {
        eprintln!("xtask lint-commits: {} violazioni", violations.len());
        for v in &violations {
            eprintln!("  - {v}");
        }
        bail!("lint-commits fallito");
    }
}

fn check_commit(
    hash: &str,
    msg: &str,
    emoji_re: &Regex,
    tracing_leak_re: &Regex,
    violations: &mut Vec<String>,
) -> Result<()> {
    if emoji_re.is_match(msg) {
        violations.push(format!("{hash}: emoji nel messaggio di commit"));
    }

    // diff size
    let numstat = git(&["show", "--numstat", "--format=", hash]).unwrap_or_default();
    let mut total: u64 = 0;
    for l in numstat.lines() {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.len() >= 2 {
            let added = cols[0].parse::<u64>().unwrap_or(0);
            let removed = cols[1].parse::<u64>().unwrap_or(0);
            total += added + removed;
        }
    }
    if total > 500 && !msg.to_lowercase().contains("big-refactor") {
        violations.push(format!(
            "{hash}: diff di {total} righe senza label `big-refactor`"
        ));
    }

    // scan diff per tracing leak
    let diff = git(&["show", "--format=", hash]).unwrap_or_default();
    for (i, line) in diff.lines().enumerate() {
        if let Some(stripped) = line.strip_prefix('+') {
            if tracing_leak_re.is_match(stripped) {
                violations.push(format!(
                    "{hash}: sospetto leak PII in tracing ({}): {}",
                    i,
                    stripped.trim()
                ));
            }
        }
    }

    Ok(())
}
