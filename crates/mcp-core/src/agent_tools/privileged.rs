//! Instradamento dei comandi privilegiati (sudo) al Sudo Manager (ADR 0017).
//!
//! PUNTO UNICO (regola L): tutti i comandi che l'agente esegue via `run_command`
//! passano da [`try_route_privileged_command`]. Se il comando e' un'operazione
//! privilegiata NOTA e sicura (apt install/update, dipendenze Playwright) viene
//! instradato al binary `nexus-sudo-runner` whitelistato (sudo_manager); se e'
//! `sudo` arbitrario viene rifiutato con un messaggio guida.
//!
//! Razionale: il NOPASSWD e' concesso SOLO a nexus-sudo-runner, mai a `sudo`
//! diretto. Quindi `sudo apt install X` nella shell isolata di run_command
//! fallirebbe sempre. Qui lo intercettiamo e lo eseguiamo in modo controllato:
//! l'agente scrive il comando in modo naturale, Nexus lo instrada in sicurezza.
//!
//! Sicurezza: niente shell (il runner usa Command::args diretti); i nomi
//! pacchetto passano [`crate::sudo_manager::is_valid_package_name`] (stesso
//! pattern del runner); i comandi compositi (metacaratteri shell) NON vengono
//! instradati come install sicuro.

use super::AgentToolContext;
use crate::sudo_manager;

const SUDO_SETUP_HINT: &str = "Se il Sudo Manager non e' configurato su questo host, un \
amministratore deve eseguire una volta: bash deploy/install-sudo-manager.sh";

/// Se `command` e' un comando privilegiato instradabile, lo esegue via
/// sudo_manager e ritorna `Some(output)`. Altrimenti `None` (esecuzione
/// normale in shell). Per `sudo <altro>` non instradabile ritorna comunque
/// `Some(messaggio_guida)` (evita il fallimento muto nella shell).
pub async fn try_route_privileged_command(
    ctx: &AgentToolContext,
    command: &str,
) -> Option<String> {
    let (had_sudo, rest) = strip_sudo_prefix(command);

    // Dipendenze di sistema Playwright (`playwright install --with-deps` o
    // `playwright install-deps`): purpose a lista fissa gia' esistente.
    if is_playwright_with_deps(&rest) {
        return Some(run_playwright_deps(ctx).await);
    }

    if let Some((subcommand, rest_tokens)) = apt_tokens(&rest) {
        match subcommand.as_str() {
            "update" => return Some(run_apt_update(ctx).await),
            "install" => {
                if let Some(pkgs) = extract_packages(&rest_tokens) {
                    return Some(run_apt_install(ctx, &pkgs).await);
                }
                // install senza pacchetti validi (o token sospetto): se l'utente
                // ha scritto sudo, spiega; altrimenti lascia eseguire normalmente.
                if had_sudo {
                    return Some(unsupported_sudo_message(&rest));
                }
                return None;
            }
            _ => {
                if had_sudo {
                    return Some(unsupported_sudo_message(&rest));
                }
                return None;
            }
        }
    }

    // `sudo <qualcos'altro>`: non instradabile -> messaggio guida.
    if had_sudo {
        return Some(unsupported_sudo_message(&rest));
    }
    None
}

/// Rimuove un eventuale prefisso `sudo` (e i suoi flag `-E`/`-n`/`-H`/`--`).
/// Ritorna `(c_era_sudo, comando_senza_sudo)`.
fn strip_sudo_prefix(cmd: &str) -> (bool, String) {
    let trimmed = cmd.trim();
    let mut tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.first().copied() == Some("sudo") {
        tokens.remove(0);
        while let Some(t) = tokens.first() {
            if t.starts_with('-') {
                tokens.remove(0);
            } else {
                break;
            }
        }
        (true, tokens.join(" "))
    } else {
        (false, trimmed.to_string())
    }
}

/// True se il comando contiene metacaratteri shell (comando composito): in tal
/// caso NON e' instradabile in sicurezza come singola operazione apt.
fn has_shell_metachars(s: &str) -> bool {
    s.contains("&&")
        || s.contains("||")
        || s.contains(';')
        || s.contains('|')
        || s.contains('>')
        || s.contains('<')
        || s.contains('`')
        || s.contains("$(")
        || s.contains('\n')
        || s.contains('\r')
}

/// Se `rest` e' un comando `apt`/`apt-get` semplice (no metacaratteri), ritorna
/// `(subcommand, token_rimanenti)` dove subcommand e' install/update/upgrade/...
fn apt_tokens(rest: &str) -> Option<(String, Vec<String>)> {
    if has_shell_metachars(rest) {
        return None;
    }
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let prog = *tokens.first()?;
    if prog != "apt" && prog != "apt-get" {
        return None;
    }
    // Salta i flag globali (es. -q, -o ...) fino al subcommand.
    let mut idx = 1;
    while idx < tokens.len() && tokens[idx].starts_with('-') {
        idx += 1;
    }
    let subcommand = (*tokens.get(idx)?).to_string();
    let rest_tokens = tokens[idx + 1..].iter().map(|s| s.to_string()).collect();
    Some((subcommand, rest_tokens))
}

/// Estrae i nomi pacchetto dai token di `apt install ...` (salta i flag come
/// `-y`/`--no-install-recommends`). Ritorna `None` se non c'e' nessun pacchetto
/// valido o se un token non-flag non passa il pattern nome-pacchetto (sospetto:
/// non instradare, per sicurezza).
fn extract_packages(rest_tokens: &[String]) -> Option<Vec<String>> {
    let mut pkgs = Vec::new();
    for t in rest_tokens {
        if t.starts_with('-') {
            continue;
        }
        if !sudo_manager::is_valid_package_name(t) {
            return None;
        }
        pkgs.push(t.clone());
    }
    if pkgs.is_empty() {
        None
    } else {
        Some(pkgs)
    }
}

fn is_playwright_with_deps(rest: &str) -> bool {
    if has_shell_metachars(rest) {
        return false;
    }
    let lower = rest.to_lowercase();
    lower.contains("playwright") && (lower.contains("--with-deps") || lower.contains("install-deps"))
}

async fn run_apt_install(ctx: &AgentToolContext, pkgs: &[String]) -> String {
    match sudo_manager::install_system_packages(&ctx.db, pkgs).await {
        Ok(out) => format!(
            "[sudo-runner] apt-get install -y {}\n{}",
            pkgs.join(" "),
            format_sudo_outcome(&out)
        ),
        Err(e) => format!(
            "\u{274C} [sudo] Installazione pacchetti ({}) fallita: {e}\n{SUDO_SETUP_HINT}",
            pkgs.join(" ")
        ),
    }
}

async fn run_apt_update(ctx: &AgentToolContext) -> String {
    match sudo_manager::apt_update(&ctx.db).await {
        Ok(out) => format!("[sudo-runner] apt-get update\n{}", format_sudo_outcome(&out)),
        Err(e) => format!("\u{274C} [sudo] apt-get update fallito: {e}\n{SUDO_SETUP_HINT}"),
    }
}

async fn run_playwright_deps(ctx: &AgentToolContext) -> String {
    match sudo_manager::execute(&ctx.db, "playwright-install-deps").await {
        Ok(out) => format!(
            "[sudo-runner] Dipendenze di sistema Playwright installate (purpose \
             'playwright-install-deps').\n{}\n\nNOTA: i BROWSER (chromium, ecc.) si \
             installano SENZA sudo con `npx playwright install <browser>` (cache utente). \
             Esegui quel comando separatamente se non l'hai gia' fatto.",
            format_sudo_outcome(&out)
        ),
        Err(e) => format!(
            "\u{274C} [sudo] Installazione deps Playwright fallita: {e}\n{SUDO_SETUP_HINT}"
        ),
    }
}

fn format_sudo_outcome(out: &sudo_manager::SudoOutcome) -> String {
    let status = if out.success { "OK" } else { "FALLITO" };
    let mut body = format!(
        "Esito: {status} (exit {}, {} ms).",
        out.exit_code, out.duration_ms
    );
    let combined = format!("{}\n{}", out.stdout, out.stderr);
    let trimmed = combined.trim();
    if !trimmed.is_empty() {
        let excerpt: String = trimmed.chars().take(4000).collect();
        body.push_str("\n--- output ---\n");
        body.push_str(&excerpt);
        if trimmed.chars().count() > 4000 {
            body.push_str("\n[...troncato...]");
        }
    }
    body
}

fn unsupported_sudo_message(rest: &str) -> String {
    format!(
        "\u{274C} [sudo non instradabile] Il comando privilegiato `sudo {rest}` non e' tra \
         quelli consentiti. Nexus instrada al gestore privilegiato controllato (ADR 0017) SOLO:\n\
         - installazione pacchetti di sistema: `sudo apt-get install -y <pkg> [<pkg> ...]`\n\
         - aggiornamento indice apt: `sudo apt-get update`\n\
         - dipendenze Playwright: `npx playwright install --with-deps`\n\
         Per gestire i servizi del progetto usa i tool dedicati (run_service, service_restart, \
         stop_service). Il sudo arbitrario (rm/chmod/chown/editing fuori progetto) non e' \
         consentito per ragioni di sicurezza e isolamento progetto."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_sudo_riconosce_prefisso_e_flag() {
        assert_eq!(
            strip_sudo_prefix("sudo apt-get install -y libnss3"),
            (true, "apt-get install -y libnss3".to_string())
        );
        assert_eq!(
            strip_sudo_prefix("sudo -E -- apt-get update"),
            (true, "apt-get update".to_string())
        );
        assert_eq!(
            strip_sudo_prefix("apt-get update"),
            (false, "apt-get update".to_string())
        );
    }

    #[test]
    fn apt_install_estrae_pacchetti_saltando_flag() {
        let (sub, rest) = apt_tokens("apt-get install -y libnss3 libasound2t64").unwrap();
        assert_eq!(sub, "install");
        let pkgs = extract_packages(&rest).unwrap();
        assert_eq!(pkgs, vec!["libnss3", "libasound2t64"]);
    }

    #[test]
    fn apt_update_riconosciuto() {
        let (sub, _) = apt_tokens("apt update").unwrap();
        assert_eq!(sub, "update");
    }

    #[test]
    fn comando_composito_non_instradato() {
        // `apt install x && rm -rf /` non deve passare il parsing apt sicuro.
        assert!(apt_tokens("apt-get install -y libnss3 && rm -rf /").is_none());
        assert!(!is_playwright_with_deps("playwright install --with-deps; rm -rf /"));
    }

    #[test]
    fn token_pacchetto_sospetto_rifiutato() {
        // Un flag travestito o un path non e' un nome pacchetto valido -> None.
        let pkgs = extract_packages(&["/etc/passwd".to_string()]);
        assert!(pkgs.is_none());
        let pkgs2 = extract_packages(&["--allow-unauthenticated".to_string()]);
        // i flag vengono saltati: lista vuota -> None
        assert!(pkgs2.is_none());
    }

    #[test]
    fn playwright_with_deps_riconosciuto() {
        assert!(is_playwright_with_deps("npx playwright install --with-deps"));
        assert!(is_playwright_with_deps("playwright install-deps chromium"));
        assert!(!is_playwright_with_deps("npx playwright install chromium"));
    }

    #[test]
    fn comando_non_privilegiato_non_intercettato() {
        // apt_tokens su comando non-apt -> None; strip senza sudo -> had_sudo false
        assert!(apt_tokens("npm install").is_none());
        let (had, _) = strip_sudo_prefix("npm install");
        assert!(!had);
    }
}
