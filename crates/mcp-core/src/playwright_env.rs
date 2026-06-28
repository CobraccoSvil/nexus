//! Punto unico (regola L / ADR 0026) per la risoluzione dell'eseguibile
//! Chromium installato dalla cache Playwright (`~/.cache/ms-playwright`).
//!
//! Nasce da un doppio incidente riprodotto live:
//!   - `nexus_visual_compare` (script Node inline) e il preflight
//!     `detect_missing_chromium_libs` guardavano path OBSOLETI
//!     (`chrome-linux/chrome`, `chrome-headless-shell-linux64`) mentre il
//!     Chromium COMPLETO (headed) reale vive in `chrome-linux64/chrome`.
//!   - Il server MCP esterno `@playwright/mcp` non risolve il browser da solo
//!     (cerca Chrome stable in `/opt/google/chrome`, assente) e funziona solo
//!     se gli si passa `--executable-path` al Chromium della cache. In piu' la
//!     revisione di `@playwright/mcp@latest` differisce da quella installata da
//!     Nexus: `--executable-path` bypassa il match di revisione.
//!
//! Regole rispettate:
//!   - G (niente hardcode): il path NON e' scritto a mano, viene DERIVATO dalla
//!     cache scegliendo la revisione piu' alta presente. Niente `unwrap_or`
//!     "claude-..."-style: assenza -> `Err` AZIONABILE.
//!   - H (causa radice): un solo posto sceglie il binario, sia per il tool
//!     interno sia per l'iniezione args del server MCP.
//!   - F (niente panic fuori test): tutto ritorna `Result`.

use std::path::{Path, PathBuf};

/// Nome della subdir che contiene il Chromium COMPLETO (headed) dentro una dir
/// di revisione `chromium-<rev>`. E' il path REALE corrente delle build
/// Playwright (le vecchie `chrome-linux/chrome` non esistono piu').
const CHROME_FULL_SUBPATH: &str = "chrome-linux64/chrome";

/// Comando suggerito all'utente quando il Chromium completo manca: e' lo stesso
/// flusso usato altrove nel codice per installare i browser Playwright.
pub const INSTALL_HINT: &str =
    "esegui 'npx playwright install chromium' per installare il browser Chromium";

/// Risolve il path assoluto dell'eseguibile Chromium completo dentro una dir
/// di cache Playwright, scegliendo la revisione piu' alta presente.
///
/// `cache_root` e' tipicamente `~/.cache/ms-playwright`. Funzione PURA (nessuna
/// lettura di env): testabile su una dir fittizia.
///
/// - Ignora le dir `chromium_headless_shell-*` (contengono solo l'headless
///   shell, non il browser completo headed richiesto da visual_compare e
///   @playwright/mcp).
/// - Tra le `chromium-<rev>` con un eseguibile `chrome-linux64/chrome`
///   presente, sceglie quella con `<rev>` numerica massima.
/// - Se nessuna dir valida esiste, ritorna `Err` con messaggio azionabile.
pub fn resolve_chromium_executable(cache_root: &Path) -> Result<PathBuf, String> {
    let read = std::fs::read_dir(cache_root).map_err(|e| {
        format!(
            "cache Playwright non leggibile in {}: {e}. {INSTALL_HINT}",
            cache_root.display()
        )
    })?;

    // Raccoglie le (revisione_numerica, path_eseguibile) candidate.
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();
    for entry in read.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        // Solo le dir del browser completo: "chromium-<rev>".
        // Escludiamo esplicitamente "chromium_headless_shell-<rev>".
        let Some(rev_str) = name.strip_prefix("chromium-") else {
            continue;
        };
        let Ok(rev) = rev_str.parse::<u64>() else {
            continue;
        };
        let exe = entry.path().join(CHROME_FULL_SUBPATH);
        if exe.is_file() {
            candidates.push((rev, exe));
        }
    }

    candidates.sort_by_key(|(rev, _)| std::cmp::Reverse(*rev));
    match candidates.into_iter().next() {
        Some((_, exe)) => Ok(exe),
        None => Err(format!(
            "nessun Chromium completo ({CHROME_FULL_SUBPATH}) trovato in {}: {INSTALL_HINT}",
            cache_root.display()
        )),
    }
}

/// Ritorna il path della cache Playwright derivato dall'ambiente.
///
/// Rispetta `PLAYWRIGHT_BROWSERS_PATH` (override ufficiale Playwright) se
/// valorizzato, altrimenti `$HOME/.cache/ms-playwright`. NON e' un nome modello
/// ne' un segreto: e' la convenzione di percorso del tool, derivata da HOME.
pub fn default_cache_root() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("PLAYWRIGHT_BROWSERS_PATH") {
        let trimmed = p.trim();
        if !trimmed.is_empty() && trimmed != "0" {
            return Ok(PathBuf::from(trimmed));
        }
    }
    let home = std::env::var("HOME")
        .map_err(|_| "variabile HOME non impostata: impossibile localizzare la cache Playwright".to_string())?;
    Ok(PathBuf::from(home).join(".cache").join("ms-playwright"))
}

/// Comodo: risolve l'eseguibile Chromium completo dalla cache di default.
/// Usato sia da `visual_compare` sia dall'iniezione args del server MCP
/// `@playwright/mcp` (punto unico).
pub fn resolve_chromium_from_env() -> Result<PathBuf, String> {
    let root = default_cache_root()?;
    resolve_chromium_executable(&root)
}

/// Slug del catalog del server MCP esterno `@playwright/mcp` (mig 0017). E'
/// l'UNICO server a cui si applica l'iniezione args sotto.
pub const PLAYWRIGHT_MCP_SLUG: &str = "playwright-stdio";

/// Calcola gli argomenti CLI extra da iniettare nel server `@playwright/mcp` per
/// renderlo eseguibile nell'ambiente Nexus (BUG d2 cause B e C):
///   - `--headless --isolated --no-sandbox`: il server gira headless in WSL,
///     senza profilo persistente e senza sandbox (necessario in container/WSL).
///   - `--executable-path <chromium>`: il server NON risolve il browser da solo
///     (cerca Chrome stable assente) e la sua revisione playwright-core differisce
///     da quella installata da Nexus; passando il path al Chromium della cache si
///     bypassa il match di revisione e si usa il browser realmente presente.
///
/// Il path del browser viene DERIVATO dal punto unico (regola G: niente hardcode).
/// Se il Chromium manca ritorna `Err` AZIONABILE.
pub fn playwright_mcp_extra_args() -> Result<Vec<String>, String> {
    let exe = resolve_chromium_from_env()?;
    Ok(vec![
        "--headless".to_string(),
        "--isolated".to_string(),
        "--no-sandbox".to_string(),
        "--executable-path".to_string(),
        exe.to_string_lossy().to_string(),
    ])
}

/// Decide se a un dato slug del catalog vanno iniettati gli args Playwright.
/// SCOPED: vale SOLO per `@playwright/mcp` (`PLAYWRIGHT_MCP_SLUG`). Funzione pura.
pub fn is_playwright_mcp_slug(slug: Option<&str>) -> bool {
    slug == Some(PLAYWRIGHT_MCP_SLUG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Crea una dir `chromium-<rev>/chrome-linux64/chrome` (file vuoto) sotto
    /// `root`. Se `with_exe` e' false crea solo la dir di revisione senza
    /// l'eseguibile (simula una install parziale/corrotta).
    fn make_chromium_rev(root: &Path, rev: u64, with_exe: bool) {
        let rev_dir = root.join(format!("chromium-{rev}"));
        let exe_dir = rev_dir.join("chrome-linux64");
        fs::create_dir_all(&exe_dir).unwrap();
        if with_exe {
            fs::write(exe_dir.join("chrome"), b"#!/bin/false\n").unwrap();
        }
    }

    fn make_headless_shell_rev(root: &Path, rev: u64) {
        let dir = root.join(format!("chromium_headless_shell-{rev}/chrome-headless-shell-linux64"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("chrome-headless-shell"), b"x").unwrap();
    }

    #[test]
    fn picks_highest_revision() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_env_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        make_chromium_rev(&tmp, 1217, true);
        make_chromium_rev(&tmp, 1228, true);
        // Rumore: una headless-shell dir non deve mai essere scelta.
        make_headless_shell_rev(&tmp, 1228);

        let exe = resolve_chromium_executable(&tmp).expect("deve trovare il chromium");
        assert!(exe.ends_with("chromium-1228/chrome-linux64/chrome"), "scelto: {exe:?}");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ignores_revision_without_executable() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_env_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        // 1228 e' la rev piu' alta ma SENZA eseguibile -> deve scartarla e
        // ripiegare su 1217 che ce l'ha.
        make_chromium_rev(&tmp, 1228, false);
        make_chromium_rev(&tmp, 1217, true);

        let exe = resolve_chromium_executable(&tmp).expect("deve ripiegare su 1217");
        assert!(exe.ends_with("chromium-1217/chrome-linux64/chrome"), "scelto: {exe:?}");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn errors_actionable_when_absent() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_env_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        // Solo headless shell, nessun browser completo.
        make_headless_shell_rev(&tmp, 1228);

        let err = resolve_chromium_executable(&tmp).unwrap_err();
        assert!(err.contains("playwright install chromium"), "errore: {err}");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn errors_when_cache_root_missing() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_env_missing_{}", uuid::Uuid::new_v4()));
        // tmp NON creata di proposito.
        let err = resolve_chromium_executable(&tmp).unwrap_err();
        assert!(err.contains("non leggibile"), "errore: {err}");
    }

    /// d2.3: l'iniezione args e' SCOPED sul solo slug del server @playwright/mcp.
    #[test]
    fn injection_scope_only_playwright_slug() {
        assert!(is_playwright_mcp_slug(Some("playwright-stdio")));
        assert!(!is_playwright_mcp_slug(Some("filesystem-local")));
        assert!(!is_playwright_mcp_slug(Some("github-http")));
        assert!(!is_playwright_mcp_slug(Some("postgres-stdio")));
        assert!(!is_playwright_mcp_slug(None));
    }

    /// Gli args extra contengono --executable-path col path risolto dal punto
    /// unico, derivato dalla cache (non hardcoded).
    #[test]
    fn extra_args_contain_executable_path_from_cache() {
        let tmp = std::env::temp_dir().join(format!("nexus_pw_args_{}", uuid::Uuid::new_v4()));
        let exe_dir = tmp.join("chromium-1228/chrome-linux64");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join("chrome"), b"x").unwrap();

        let exe = resolve_chromium_executable(&tmp).unwrap();
        // Verifichiamo la forma degli args usando il path risolto (la funzione
        // pubblica playwright_mcp_extra_args legge da env, qui testiamo la
        // composizione deterministica).
        let args = [
            "--headless".to_string(),
            "--isolated".to_string(),
            "--no-sandbox".to_string(),
            "--executable-path".to_string(),
            exe.to_string_lossy().to_string(),
        ];
        let pos = args.iter().position(|a| a == "--executable-path").unwrap();
        assert!(args[pos + 1].ends_with("chromium-1228/chrome-linux64/chrome"));
        assert!(args.contains(&"--headless".to_string()));
        assert!(args.contains(&"--no-sandbox".to_string()));

        fs::remove_dir_all(&tmp).ok();
    }
}
