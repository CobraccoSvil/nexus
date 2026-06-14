//! Policy di whitelist/blacklist sui path dei file riferiti nei prompt.
//!
//! Porting di `packages/llm-gateway/src/redaction/path-policy.ts` (che usa
//! `minimatch`). Qui non aggiungiamo una dipendenza glob: i pattern usati sono
//! semplici (`**`, `*`, estensioni), quindi convertiamo ciascun glob in una
//! `Regex` equivalente con `dot=true` (il `*` matcha anche i file dotfile,
//! come `minimatch(..., { dot: true })`).
//!
//! - blacklist: file mai inviabili a provider esterni (`.env`, chiavi, ecc.) ->
//!   `PathDecision::Blocked`.
//! - whitelist: file pubblici che escono senza redaction (README, docs, lock) ->
//!   `PathDecision::Whitelisted`.
//! - altrimenti `PathDecision::Redact`.
//!
//! Regola F: nessun log; calcolo puro sul path (che non e' di per se' un segreto,
//! ma evitiamo comunque di loggarlo da qui — lo decide il caller).

use std::sync::LazyLock;

use regex::Regex;

/// Esito del controllo di un path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathDecision {
    /// File in blacklist: invio bloccato.
    Blocked,
    /// File in whitelist: passa senza redaction.
    Whitelisted,
    /// File da sottoporre a redaction standard.
    Redact,
}

/// Blacklist di default (parita' con `DEFAULT_BLACKLIST` del TS).
const DEFAULT_BLACKLIST: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/secrets/**",
    "**/customers/*/private/**",
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/*_rsa",
    "**/*_ed25519",
    "**/id_rsa",
    "**/id_ed25519",
    "**/credentials.json",
    "**/service-account*.json",
];

/// Whitelist di default (parita' con `DEFAULT_WHITELIST` del TS).
const DEFAULT_WHITELIST: &[&str] = &[
    "**/README*",
    "**/docs/**/*.md",
    "**/LICENSE*",
    "**/CHANGELOG*",
    "**/node_modules/**",
    "**/*.lock",
];

/// Converte un pattern glob in una `Regex` ancorata. Supporta:
/// - `**` -> qualsiasi sequenza (inclusi `/`);
/// - `*`  -> qualsiasi sequenza esclusi i separatori `/`;
/// - `?`  -> singolo carattere non separatore;
/// - resto: escaped letterale.
///
/// Equivale a `minimatch(path, glob, { dot: true })` per i pattern usati qui.
fn glob_to_regex(glob: &str) -> Regex {
    let mut re = String::with_capacity(glob.len() * 2 + 4);
    re.push('^');
    let bytes = glob.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '*' => {
                // `**` -> tutto; `*` -> tutto tranne '/'
                if i + 1 < bytes.len() && bytes[i + 1] as char == '*' {
                    re.push_str(".*");
                    i += 2;
                    // Salta un eventuale '/' subito dopo `**` per far matchare
                    // anche zero segmenti (es. `**/foo` matcha `foo`).
                    if i < bytes.len() && bytes[i] as char == '/' {
                        re.push_str("/?");
                        i += 1;
                    }
                    continue;
                }
                re.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                re.push_str("[^/]");
                i += 1;
            }
            // Metacaratteri regex da escapare.
            '.' | '+' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(c);
                i += 1;
            }
            other => {
                re.push(other);
                i += 1;
            }
        }
    }
    re.push('$');
    // I pattern sono literal sotto controllo: compilazione sempre valida.
    Regex::new(&re).expect("glob convertito in regex valida")
}

fn compile_all(globs: &[&str]) -> Vec<Regex> {
    globs.iter().map(|g| glob_to_regex(g)).collect()
}

static DEFAULT_BLACKLIST_RE: LazyLock<Vec<Regex>> = LazyLock::new(|| compile_all(DEFAULT_BLACKLIST));
static DEFAULT_WHITELIST_RE: LazyLock<Vec<Regex>> = LazyLock::new(|| compile_all(DEFAULT_WHITELIST));

/// Policy di path. Mantiene le regex compilate di whitelist/blacklist.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    blacklist: Vec<Regex>,
    whitelist: Vec<Regex>,
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self {
            blacklist: DEFAULT_BLACKLIST_RE.clone(),
            whitelist: DEFAULT_WHITELIST_RE.clone(),
        }
    }
}

impl PathPolicy {
    /// Crea la policy con override opzionali. `None` su un campo usa il default.
    pub fn new(whitelist: Option<&[String]>, blacklist: Option<&[String]>) -> Self {
        let whitelist = match whitelist {
            Some(list) => compile_all(&list.iter().map(String::as_str).collect::<Vec<_>>()),
            None => DEFAULT_WHITELIST_RE.clone(),
        };
        let blacklist = match blacklist {
            Some(list) => compile_all(&list.iter().map(String::as_str).collect::<Vec<_>>()),
            None => DEFAULT_BLACKLIST_RE.clone(),
        };
        Self {
            blacklist,
            whitelist,
        }
    }

    /// `true` se il file e' in blacklist.
    pub fn is_blocked(&self, file_path: &str) -> bool {
        self.blacklist.iter().any(|re| re.is_match(file_path))
    }

    /// `true` se il file e' in whitelist.
    pub fn is_whitelisted(&self, file_path: &str) -> bool {
        self.whitelist.iter().any(|re| re.is_match(file_path))
    }

    /// Decisione complessiva: blacklist ha precedenza, poi whitelist, infine redact.
    pub fn check_path(&self, file_path: &str) -> PathDecision {
        if self.is_blocked(file_path) {
            PathDecision::Blocked
        } else if self.is_whitelisted(file_path) {
            PathDecision::Whitelisted
        } else {
            PathDecision::Redact
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blacklist_blocca_env() {
        let p = PathPolicy::default();
        assert_eq!(p.check_path("project/.env"), PathDecision::Blocked);
        assert_eq!(p.check_path("a/b/.env.production"), PathDecision::Blocked);
        assert_eq!(p.check_path("server/cert.pem"), PathDecision::Blocked);
        assert_eq!(p.check_path("keys/deploy_rsa"), PathDecision::Blocked);
        assert_eq!(p.check_path("config/credentials.json"), PathDecision::Blocked);
    }

    #[test]
    fn blacklist_secrets_dir() {
        let p = PathPolicy::default();
        assert_eq!(
            p.check_path("infra/secrets/prod/token.txt"),
            PathDecision::Blocked
        );
    }

    #[test]
    fn whitelist_passa_readme_e_docs() {
        let p = PathPolicy::default();
        assert_eq!(p.check_path("README.md"), PathDecision::Whitelisted);
        assert_eq!(p.check_path("repo/README"), PathDecision::Whitelisted);
        assert_eq!(
            p.check_path("project/docs/guide/intro.md"),
            PathDecision::Whitelisted
        );
        assert_eq!(p.check_path("pnpm-lock.yaml.lock"), PathDecision::Whitelisted);
    }

    #[test]
    fn file_generico_va_in_redact() {
        let p = PathPolicy::default();
        assert_eq!(p.check_path("src/main.rs"), PathDecision::Redact);
        assert_eq!(p.check_path("app/handler.ts"), PathDecision::Redact);
    }

    #[test]
    fn override_blacklist_personalizzata() {
        let bl = vec!["**/*.secret".to_string()];
        let p = PathPolicy::new(None, Some(&bl));
        assert_eq!(p.check_path("a/b/file.secret"), PathDecision::Blocked);
        // Il default .env non e' piu' in blacklist (override totale).
        assert_eq!(p.check_path(".env"), PathDecision::Redact);
    }

    #[test]
    fn glob_doppia_stella_matcha_zero_segmenti() {
        // `**/README*` deve matchare anche un README a radice.
        let re = glob_to_regex("**/README*");
        assert!(re.is_match("README.md"));
        assert!(re.is_match("a/b/README"));
    }
}
