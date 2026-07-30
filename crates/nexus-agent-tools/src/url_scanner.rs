//! Scanner URL hardcoded nei sorgenti (governance risorse, classe `network`).
//!
//! Rileva URL verso host INTERNI (localhost, 127.0.0.1, 0.0.0.0,
//! host.docker.internal) scritti come letterali nei sorgenti del progetto:
//! sono la causa ricorrente di "il frontend chiama la porta sbagliata" quando
//! le porte vengono riallocate (incidente login Beauty-Book: URL hardcoded
//! verso una porta che il backend non usava piu'). Gli URL esterni NON sono
//! oggetto di questo scanner (integrazioni legittime).
//!
//! Eccezioni (riga NON segnalata):
//!   - lettura da env/config sulla stessa riga (process.env.*, import.meta.env,
//!     os.environ, ${VAR}) — l'URL e' un default governabile;
//!   - commenti (//, #, *) e file di documentazione (.md, .txt);
//!   - file `.env*` (posto canonico della configurazione).
//!
//! Punto unico di parsing per: enforcement in scrittura (via
//! `security::resource_governance::enforce_on_write`) e linter periodico dei
//! sorgenti. Regola L.

use once_cell::sync::Lazy;
use regex::Regex;

/// URL interni: scheme http/https/ws/wss verso host loopback o docker-interni,
/// con o senza porta. Cattura l'intero URL per il report.
static INTERNAL_URL_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)\b(https?|wss?)://(localhost|127\.0\.0\.1|0\.0\.0\.0|host\.docker\.internal)(:\d{2,5})?[^\s'"`)\]}>]*"#,
    )
    .unwrap()
});

/// Hint di configurabilita' sulla riga: se l'URL convive con una lettura da
/// env/var di template, e' un default governato e non va segnalato.
const ENV_URL_HINTS: &[&str] = &[
    "process.env.",
    "import.meta.env",
    "os.environ",
    "getenv(",
    "env::var",
    "${",
    "config.",
    "settings.",
];

/// Estensioni di file scansionate. Documentazione e testo esclusi.
const SKIP_EXTENSIONS: &[&str] = &["md", "txt", "rst", "adoc", "lock"];

#[derive(Debug, Clone)]
pub struct UrlFinding {
    pub line: usize,
    pub url: String,
    pub snippet: String,
}

fn is_comment_line(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with('*')
        || trimmed.starts_with("/*")
        || trimmed.starts_with("<!--")
}

fn should_skip_path(path: &str) -> bool {
    let p = std::path::Path::new(path);
    let file_name = p.file_name().and_then(|s| s.to_str()).unwrap_or(path);
    if file_name.to_lowercase().starts_with(".env") {
        return true;
    }
    match p.extension().and_then(|e| e.to_str()) {
        Some(ext) => SKIP_EXTENSIONS.contains(&ext.to_lowercase().as_str()),
        None => false,
    }
}

/// Colleziona gli URL interni hardcoded nel contenuto. Punto unico di parsing
/// (regola L): usato sia dall'enforcement in scrittura sia dal linter sorgenti.
pub fn collect_internal_urls(path: &str, content: &str) -> Vec<UrlFinding> {
    if should_skip_path(path) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (line_idx, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim_start();
        if is_comment_line(trimmed) {
            continue;
        }
        if ENV_URL_HINTS.iter().any(|hint| raw_line.contains(hint)) {
            continue;
        }
        for m in INTERNAL_URL_REGEX.find_iter(raw_line) {
            let snippet: String = raw_line.trim().chars().take(200).collect();
            findings.push(UrlFinding {
                line: line_idx + 1,
                url: m.as_str().trim_end_matches(['"', '\'', '`']).to_string(),
                snippet: snippet.clone(),
            });
        }
    }
    findings
}

/// Messaggio di rifiuto per la scrittura: istruisce la configurazione governata.
pub fn format_url_reject_message(path: &str, findings: &[UrlFinding]) -> String {
    let mut msg = format!(
        "\u{274C} [Errore: scrittura su '{}' rifiutata. Sono stati rilevati {} URL interni hardcoded (localhost/127.0.0.1/host.docker.internal).]\n\nDettaglio:\n",
        path,
        findings.len()
    );
    for f in findings.iter().take(10) {
        msg.push_str(&format!("  - riga {}: {} | {}\n", f.line, f.url, f.snippet));
    }
    if findings.len() > 10 {
        msg.push_str(&format!("  ... e altri {} riscontri.\n", findings.len() - 10));
    }
    msg.push_str(
        "\nGli URL interni hardcoded si rompono quando le porte vengono riallocate \
         (ogni porta del progetto e' governata da Nexus, bucket 20000-39999).\n\n\
         Azione richiesta:\n\
         1. Leggi l'URL da variabile env o config del progetto (es. process.env.API_URL, \
            import.meta.env.VITE_API_URL) con default costruito sulla porta ALLOCATA \
            (verifica con nexus_list_ports, alloca con request_port).\n\
         2. Aggiorna .env con la variabile corrispondente.\n\
         3. Riprova la scrittura.",
    );
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_localhost_url() {
        let f = collect_internal_urls(
            "src/api.js",
            "const api = fetch('http://localhost:3000/api/users');\n",
        );
        assert_eq!(f.len(), 1);
        assert!(f[0].url.starts_with("http://localhost:3000"));
    }

    #[test]
    fn detect_loopback_and_docker_host() {
        let f = collect_internal_urls(
            "src/cfg.py",
            "BASE = 'http://127.0.0.1:8080'\nWS = 'ws://host.docker.internal:4000/feed'\n",
        );
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn allow_env_configured_url() {
        // URL come default accanto a lettura env: governato, non segnalare.
        let f = collect_internal_urls(
            "src/api.js",
            "const api = process.env.API_URL || 'http://localhost:3000';\n",
        );
        assert!(f.is_empty());
        let f = collect_internal_urls(
            "src/api.ts",
            "const api = import.meta.env.VITE_API_URL ?? 'http://localhost:5173';\n",
        );
        assert!(f.is_empty());
    }

    #[test]
    fn allow_comments_and_docs() {
        let f = collect_internal_urls("src/a.js", "// vedi http://localhost:3000/docs\n");
        assert!(f.is_empty());
        let f = collect_internal_urls("README.md", "apri http://localhost:3000\n");
        assert!(f.is_empty());
    }

    #[test]
    fn allow_external_urls() {
        let f = collect_internal_urls(
            "src/a.js",
            "fetch('https://api.github.com/repos');\n",
        );
        assert!(f.is_empty());
    }

    #[test]
    fn skip_env_files() {
        let f = collect_internal_urls(".env.local", "API_URL=http://localhost:3000\n");
        assert!(f.is_empty());
    }
}
