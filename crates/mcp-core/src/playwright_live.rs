//! Live monitoring di run Playwright.
//!
//! Mantiene un broadcast channel per job (UUID della riga `jobs`) cui SSE
//! consumer si agganciano per ricevere eventi in tempo reale durante un run.
//!
//! Eventi emessi:
//! - `line`: una riga di stdout/stderr del processo `npx playwright test`
//! - `progress`: contatori aggiornati (passed/failed/skipped, total, current_spec)
//! - `final`: status terminale ("passed"/"failed"/"timeout") + progress finale

use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaywrightProgress {
    /// Numero totale di test attesi (parsed da "[N/M]"). None all'inizio.
    pub total: Option<u32>,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub flaky: u32,
    /// Path:line del test attualmente in esecuzione (se identificabile).
    pub current_spec: Option<String>,
    /// Lista compatta degli ultimi N test falliti (max 20).
    #[serde(default)]
    pub failed_specs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum PlaywrightEvent {
    /// Una riga di output dal processo (gia' troncata a 2 KB).
    #[serde(rename = "line")]
    Line { job_id: Uuid, line: String },
    /// Progress incrementale (counter aggiornati).
    #[serde(rename = "progress")]
    Progress {
        job_id: Uuid,
        progress: PlaywrightProgress,
    },
    /// Evento terminale: status + progress finale.
    #[serde(rename = "final")]
    Final {
        job_id: Uuid,
        status: String,
        exit_code: i32,
        progress: PlaywrightProgress,
    },
}

impl PlaywrightEvent {
    pub fn job_id(&self) -> Uuid {
        match self {
            Self::Line { job_id, .. } => *job_id,
            Self::Progress { job_id, .. } => *job_id,
            Self::Final { job_id, .. } => *job_id,
        }
    }
}

/// Mappa job_id → broadcast::Sender. Buffer 256 messaggi per consumer lento.
pub type PlaywrightChannels = Arc<DashMap<Uuid, broadcast::Sender<PlaywrightEvent>>>;

pub fn new_channels() -> PlaywrightChannels {
    Arc::new(DashMap::new())
}

/// Crea (o restituisce esistente) il channel per un job. Buffer 256 eventi.
pub fn register(channels: &PlaywrightChannels, job_id: Uuid) -> broadcast::Sender<PlaywrightEvent> {
    channels
        .entry(job_id)
        .or_insert_with(|| broadcast::channel(256).0)
        .clone()
}

/// Emette un evento. No-op se il channel non esiste (job non in corso o nessun listener).
pub fn emit(channels: &PlaywrightChannels, ev: PlaywrightEvent) {
    if let Some(tx) = channels.get(&ev.job_id()) {
        let _ = tx.send(ev);
    }
}

/// Rimuove il channel quando il job termina (evita memory leak).
/// Va chiamato dopo aver emesso `PlaywrightEvent::Final`.
pub fn unregister(channels: &PlaywrightChannels, job_id: Uuid) {
    channels.remove(&job_id);
}

// ---------------------------------------------------------------------------
// Parser incrementale Playwright reporter=list
// ---------------------------------------------------------------------------

/// Aggiorna `progress` parsando una singola riga dell'output di Playwright.
///
/// Pattern riconosciuti (reporter=list):
/// - `[N/M] e2e/foo.spec.ts:5:1 › test name`     → progress (in corso)
/// - `  ✓  N e2e/foo.spec.ts:5:1 › test (123ms)` → passed++
/// - `  ✘  N e2e/foo.spec.ts:5:1 › test (123ms)` → failed++ + record path
/// - `  -  N e2e/foo.spec.ts:5:1 › test`         → skipped++
/// - `  ⊘  N e2e/foo.spec.ts:5:1 › test`         → skipped++
/// - `Running N tests using 1 worker`            → total
/// - `N passed, M failed`                        → final summary (override)
pub fn parse_line(line: &str, progress: &mut PlaywrightProgress) {
    // Strip ANSI escape sequences semplici (es. "\x1b[32m...\x1b[39m") per matching
    let clean = strip_ansi(line);
    let trimmed = clean.trim_start();

    // Totale dichiarato all'inizio: "Running 19 tests using 1 worker"
    if let Some(rest) = trimmed.strip_prefix("Running ") {
        if let Some((num_str, _)) = rest.split_once(' ') {
            if let Ok(n) = num_str.parse::<u32>() {
                progress.total = Some(n);
            }
        }
        return;
    }

    // Progress in corso: "  [3/19] e2e/foo.spec.ts:5:1 › test"
    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some((counter, rest)) = rest.split_once(']') {
            if let Some((_done, tot)) = counter.split_once('/') {
                if let Ok(t) = tot.parse::<u32>() {
                    progress.total = Some(t);
                }
            }
            let spec = rest.trim().to_string();
            if !spec.is_empty() {
                progress.current_spec = Some(spec.chars().take(120).collect());
            }
            return;
        }
    }

    // Esiti singoli test (primi caratteri non-space)
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return;
    }
    // Caratteri marker (UTF-8 multibyte): ✓ = E2 9C 93, ✘ = E2 9C 98, ✗ = E2 9C 97
    // - = ASCII, ⊘ = E2 8A 98
    if trimmed.starts_with("✓") || trimmed.starts_with("ok") {
        progress.passed = progress.passed.saturating_add(1);
    } else if trimmed.starts_with("✘") || trimmed.starts_with("✗") {
        progress.failed = progress.failed.saturating_add(1);
        // Estrai path:line per il record
        if let Some(spec) = extract_spec_path(trimmed) {
            if progress.failed_specs.len() < 20 {
                progress.failed_specs.push(spec);
            }
        }
    } else if trimmed.starts_with("⊘") || (bytes[0] == b'-' && trimmed.len() > 1) {
        progress.skipped = progress.skipped.saturating_add(1);
    }
    // Riga finale di summary: "  5 passed, 2 failed, 1 flaky (12s)"
    // Override dei contatori (gestisce double-count da rerun)
    if let Some(summary) = parse_summary_line(trimmed) {
        progress.passed = summary.0;
        progress.failed = summary.1;
        progress.flaky = summary.2;
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // skip fino a 'm' o termina (CSI sequences)
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn extract_spec_path(line: &str) -> Option<String> {
    // Cerca "e2e/...spec.ts:N:M" — pattern comune Playwright
    for part in line.split_whitespace() {
        if (part.contains(".spec.ts") || part.contains(".spec.js")) && part.contains(':') {
            return Some(part.to_string());
        }
    }
    None
}

/// Parsa una riga tipo "5 passed, 2 failed, 1 flaky (12s)". Ritorna (passed, failed, flaky).
fn parse_summary_line(line: &str) -> Option<(u32, u32, u32)> {
    if !line.contains("passed") && !line.contains("failed") {
        return None;
    }
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut flaky = 0u32;
    let mut found = false;
    for part in line.split(',') {
        let p = part.trim();
        let tokens: Vec<&str> = p.split_whitespace().collect();
        if tokens.len() >= 2 {
            if let Ok(n) = tokens[0].parse::<u32>() {
                match tokens[1] {
                    "passed" => {
                        passed = n;
                        found = true;
                    }
                    "failed" => {
                        failed = n;
                        found = true;
                    }
                    "flaky" => {
                        flaky = n;
                        found = true;
                    }
                    _ => {}
                }
            }
        }
    }
    if found {
        Some((passed, failed, flaky))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_running_header() {
        let mut p = PlaywrightProgress::default();
        parse_line("Running 19 tests using 1 worker", &mut p);
        assert_eq!(p.total, Some(19));
    }

    #[test]
    fn parses_passed() {
        let mut p = PlaywrightProgress::default();
        parse_line("  ✓  1 e2e/foo.spec.ts:5:1 › my test (123ms)", &mut p);
        assert_eq!(p.passed, 1);
        assert_eq!(p.failed, 0);
    }

    #[test]
    fn parses_failed_and_records_path() {
        let mut p = PlaywrightProgress::default();
        parse_line("  ✘  2 e2e/bar.spec.ts:10:3 › broken test (50ms)", &mut p);
        assert_eq!(p.failed, 1);
        assert!(p
            .failed_specs
            .iter()
            .any(|s| s.contains("bar.spec.ts:10:3")));
    }

    #[test]
    fn parses_summary() {
        let mut p = PlaywrightProgress::default();
        parse_line("  5 passed, 2 failed, 1 flaky (12s)", &mut p);
        assert_eq!(p.passed, 5);
        assert_eq!(p.failed, 2);
        assert_eq!(p.flaky, 1);
    }

    #[test]
    fn parses_progress_counter() {
        let mut p = PlaywrightProgress::default();
        parse_line("  [3/19] e2e/foo.spec.ts:5:1 › test name", &mut p);
        assert_eq!(p.total, Some(19));
        assert!(p.current_spec.as_deref().unwrap().contains("foo.spec.ts"));
    }

    #[test]
    fn strips_ansi_escapes() {
        let s = "\x1b[32m  ✓  1 test \x1b[39m";
        let mut p = PlaywrightProgress::default();
        parse_line(s, &mut p);
        assert_eq!(p.passed, 1);
    }
}
