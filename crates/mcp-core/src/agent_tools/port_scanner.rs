//! Enforcement porte hardcoded nei tool di scrittura file.
//!
//! Quando l'agente prova a scrivere/modificare codice che contiene una porta
//! TCP hardcoded fuori dal bucket Nexus (20000..40000), il tool restituisce
//! un messaggio di rifiuto che istruisce l'uso di `request_port(label=...)`.
//!
//! Il flag globale e' letto dalla tabella `settings` (chiave
//! `agent.enforce_port_allocation`), con cache 60s.
//!
//! Vedi ADR 0010 per il contesto della decisione.

use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::PgPool;
use tokio::sync::RwLock;

const ENFORCEMENT_CACHE_TTL: Duration = Duration::from_secs(60);

static ENFORCEMENT_CACHE: Lazy<RwLock<Option<(bool, Instant)>>> = Lazy::new(|| RwLock::new(None));

const NEXUS_PORT_MIN: u32 = 20000;
const NEXUS_PORT_MAX: u32 = 40000;

/// Patterns di file da escludere dallo scan.
///
/// Solo .env* resta whitelistato come posto canonico dove dichiarare le
/// porte come variabili d ambiente. docker-compose* e Dockerfile* NON
/// sono piu skippati: i pattern dedicati (ports:, EXPOSE, range) e la
/// whitelist gestiscono le forme legittime, le altre vengono rifiutate.
const SKIP_FILE_PREFIXES: &[&str] = &[".env"];

const ENV_PORT_HINTS: &[&str] = &[
    "process.env.PORT",
    "os.environ.get(\"PORT\")",
    "os.environ.get('PORT')",
    "os.environ[\"PORT\"]",
    "os.environ['PORT']",
    "env::var(\"PORT\")",
    "env::var('PORT')",
    "getenv(\"PORT\")",
    "getenv('PORT')",
    "PORT=$",
    "PORT=${",
    "${PORT}",
    "${PORT_",
    "$PORT_",
    "request_port(",
];

static PORT_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)\.listen\s*\(\s*(\d{2,5})\b").unwrap(),
        Regex::new(r#"(?i)\.bind\s*\(\s*['"]?[\d.]*:(\d{2,5})\b"#).unwrap(),
        Regex::new(r"(?i)\blisten\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bPORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bBACKEND_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bFRONTEND_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bDATABASE_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bDB_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\b(?:host|listen)_port\s*[=:]\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bport\s*:\s*(\d{2,5})\b").unwrap(),
        Regex::new(r#"(?i)\bports["']?\s*:\s*\[\s*(\d{2,5})\b"#).unwrap(),
        // YAML list item: - 3000:3000 (mapping host:container)
        Regex::new(r"^\s*-\s*(\d{2,5})\s*:\s*\d{2,5}\b").unwrap(),
        // YAML list item plain: - 3000
        Regex::new(r"^\s*-\s+(\d{2,5})\s*$").unwrap(),
        Regex::new(r"(?i)\bEXPOSE\s+(\d{2,5})\b").unwrap(),
    ]
});

static RANGE_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\brange\s+(\d{2,5})\s*-\s*(\d{2,5})\b").unwrap());

#[derive(Debug)]
pub enum PortScanOutcome {
    Allowed,
    Reject(Vec<PortFinding>),
}

#[derive(Debug, Clone)]
pub struct PortFinding {
    pub line: usize,
    pub port: u32,
    pub snippet: String,
}

pub async fn is_enforcement_enabled(db: &PgPool) -> bool {
    {
        let guard = ENFORCEMENT_CACHE.read().await;
        if let Some((value, expires_at)) = *guard {
            if Instant::now() < expires_at {
                return value;
            }
        }
    }

    let value: bool = match sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'agent.enforce_port_allocation'",
    )
    .fetch_optional(db)
    .await
    {
        Ok(Some(raw)) => {
            let normalized = raw.trim().to_lowercase();
            matches!(normalized.as_str(), "true" | "1" | "yes" | "on")
        }
        Ok(None) => true,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "port_scanner: lettura setting agent.enforce_port_allocation fallita, default=true"
            );
            true
        }
    };

    let mut guard = ENFORCEMENT_CACHE.write().await;
    *guard = Some((value, Instant::now() + ENFORCEMENT_CACHE_TTL));
    value
}

#[cfg(test)]
pub async fn _reset_cache_for_tests() {
    let mut guard = ENFORCEMENT_CACHE.write().await;
    *guard = None;
}

#[allow(dead_code)]
pub async fn is_enforcement_enabled_arc(db: &Arc<PgPool>) -> bool {
    is_enforcement_enabled(db.as_ref()).await
}

fn should_skip_path(path: &str) -> bool {
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    let lower = file_name.to_lowercase();
    for prefix in SKIP_FILE_PREFIXES {
        if lower.starts_with(&prefix.to_lowercase()) {
            return true;
        }
    }
    false
}

fn port_is_violating(port: u32) -> bool {
    if (NEXUS_PORT_MIN..NEXUS_PORT_MAX).contains(&port) {
        return false;
    }
    if port < 1024 {
        return false;
    }
    true
}

pub fn scan_content(path: &str, content: &str) -> PortScanOutcome {
    if should_skip_path(path) {
        return PortScanOutcome::Allowed;
    }

    let mut findings: Vec<PortFinding> = Vec::new();

    for (line_idx, raw_line) in content.lines().enumerate() {
        if ENV_PORT_HINTS.iter().any(|hint| raw_line.contains(hint)) {
            continue;
        }

        for caps in RANGE_REGEX.captures_iter(raw_line) {
            let lo = caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok());
            let hi = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
            for opt in [lo, hi].iter().flatten() {
                let port = *opt;
                if port_is_violating(port) {
                    let snippet = if raw_line.len() > 200 {
                        format!("{}...", &raw_line[..200])
                    } else {
                        raw_line.to_string()
                    };
                    findings.push(PortFinding {
                        line: line_idx + 1,
                        port,
                        snippet,
                    });
                }
            }
        }

        for regex in PORT_REGEXES.iter() {
            for caps in regex.captures_iter(raw_line) {
                if let Some(port_str) = caps.get(1) {
                    if let Ok(port) = port_str.as_str().parse::<u32>() {
                        if !port_is_violating(port) {
                            continue;
                        }
                        let snippet = if raw_line.len() > 200 {
                            format!("{}...", &raw_line[..200])
                        } else {
                            raw_line.to_string()
                        };
                        findings.push(PortFinding {
                            line: line_idx + 1,
                            port,
                            snippet,
                        });
                    }
                }
            }
        }
    }

    if findings.is_empty() {
        PortScanOutcome::Allowed
    } else {
        PortScanOutcome::Reject(findings)
    }
}

pub fn format_reject_message(path: &str, findings: &[PortFinding]) -> String {
    let mut msg = String::new();
    msg.push_str(&format!(
        "[Errore: scrittura su '{}' rifiutata. Sono state rilevate {} porta/e TCP hardcoded fuori dal bucket Nexus (20000-39999).]\n",
        path,
        findings.len()
    ));
    msg.push_str("\nDettaglio:\n");
    for f in findings.iter().take(10) {
        msg.push_str(&format!(
            "  - riga {}: porta {} | {}\n",
            f.line,
            f.port,
            f.snippet.trim()
        ));
    }
    if findings.len() > 10 {
        msg.push_str(&format!(
            "  ... e altri {} riscontri.\n",
            findings.len() - 10
        ));
    }
    msg.push_str(
        "\nAzione richiesta:\n\
         1. Chiama il tool `request_port(label=\"<nome_servizio>\")` per ottenere una porta libera dal range 20000-39999.\n\
         2. Sostituisci la porta hardcoded con il valore ritornato. In alternativa, leggi la porta da variabile env:\n\
            - JS/TS: process.env.PORT\n\
            - Python: os.environ.get(\"PORT\")\n\
            - Rust: env::var(\"PORT\")\n\
            - Docker/shell: ${PORT} oppure $PORT_BACKEND, $PORT_FRONTEND, ecc.\n\
         3. Riprova la scrittura.\n\
         \nVedi il blocco <port_allocation> nel system prompt e ADR 0010 per i dettagli.",
    );
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_env_files() {
        let res = scan_content(".env", "PORT=3000\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
        let res = scan_content("config/.env.local", "PORT=3000\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn docker_compose_no_longer_skipped() {
        let res = scan_content(
            "docker-compose.yml",
            "services:\n  web:\n    ports:\n      - 3000:3000\n",
        );
        assert!(matches!(res, PortScanOutcome::Reject(_)));
    }

    #[test]
    fn detect_hardcoded_listen() {
        let res = scan_content("src/server.js", "app.listen(3000)\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert_eq!(f.len(), 1);
                assert_eq!(f[0].port, 3000);
                assert_eq!(f[0].line, 1);
            }
            _ => panic!("dovrebbe essere Reject"),
        }
    }

    #[test]
    fn allow_in_bucket() {
        let res = scan_content("src/server.js", "app.listen(25432)\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allow_env_port_line() {
        let res = scan_content("src/server.js", "app.listen(process.env.PORT || 3000)\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_bind_with_host() {
        let res = scan_content("main.py", "s.bind(\"0.0.0.0:8080\")\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert_eq!(f[0].port, 8080);
            }
            _ => panic!("dovrebbe essere Reject"),
        }
    }

    #[test]
    fn detect_port_assignment() {
        let res = scan_content("config.py", "PORT = 5173\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert_eq!(f[0].port, 5173);
            }
            _ => panic!("dovrebbe essere Reject"),
        }
    }

    #[test]
    fn allow_reserved_low_ports() {
        let res = scan_content("docs.md", "Default HTTP port = 80, HTTPS = 443.\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_yaml_port_key() {
        let res = scan_content("config.yaml", "server:\n  port: 3000\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("YAML port deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_dockerfile_expose() {
        let res = scan_content("Dockerfile", "FROM node:20\nEXPOSE 3000\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("EXPOSE deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_backend_port_env_assign() {
        let res = scan_content("config.sh", "BACKEND_PORT=3000\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("BACKEND_PORT=3000 deve essere rifiutato"),
        }
    }

    #[test]
    fn allow_backend_port_in_bucket() {
        let res = scan_content("config.sh", "BACKEND_PORT=32100\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allow_backend_port_template_var() {
        let res = scan_content("config.sh", "BACKEND_PORT=${PORT}\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allow_backend_port_named_var() {
        let res = scan_content("config.sh", "BACKEND_PORT=$PORT_BACKEND\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_yaml_range_out_of_bucket() {
        let res = scan_content("config.yaml", "scan:\n  range 3001-3100\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3001));
                assert!(f.iter().any(|x| x.port == 3100));
            }
            _ => panic!("range 3001-3100 deve essere rifiutato"),
        }
    }

    #[test]
    fn allow_range_inside_bucket() {
        let res = scan_content("config.yaml", "scan:\n  range 25000-26000\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allow_request_port_call() {
        let res = scan_content(
            "src/setup.rs",
            "let port = request_port(label=\"backend\");\n",
        );
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_json_ports_array() {
        let res = scan_content("service.json", "{\"ports\": [3000, 3001]}\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("JSON ports array fuori bucket deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_db_port_var() {
        let res = scan_content("dev.sh", "DB_PORT=5432\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 5432));
            }
            _ => panic!("DB_PORT=5432 deve essere rifiutato"),
        }
    }
}
