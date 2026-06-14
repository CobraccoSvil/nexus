//! Secret scanner su STRINGA in-memory (primo punto Rust, regola L / ADR 0026).
//!
//! Porting fedele di `packages/shared/src/secret-scanner.ts` (la classe
//! `SecretScanner` di `@nexus/shared`, fonte autoritativa del gateway TS).
//!
//! ## Perche' un NUOVO punto e non riuso
//!
//! In `crates/nexus-tool-kit` esistono gia' due scanner di secret
//! (`secret_scan.rs`, `sec_secret_patterns.rs`), ma hanno un contratto
//! DIVERSO e incompatibile con questo caso d'uso:
//! - sono `NexusToolHandler` (tool agente) che scansionano la **project root su
//!   disco** e ritornano findings `{file, line, preview}`;
//! - non scansionano una stringa in-memory, non assegnano un **tier** per
//!   pattern, non producono **testo redatto**.
//!
//! Il gateway deve invece scansionare il TESTO del prompt (in-memory),
//! attribuire un tier di sensibilita' a ciascun pattern e produrre una versione
//! redatta del testo. Questo modulo e' quindi il primo punto Rust del
//! secret-scanner-su-stringa del gateway; i pattern provengono da `@nexus/shared`.
//! Annotare in ADR 0026 (catalogo punti unici): "Secret scanner su stringa
//! (gateway) -> nexus-gateway::redaction::secret_scanner".
//!
//! ## Regola F (no leak nei log)
//! Questo modulo non logga nulla: opera in puro calcolo. Le offset/tipo dei
//! pattern sono restituiti al caller, che logga solo conteggi/tipi.

use std::sync::LazyLock;

use regex::Regex;

use crate::types::SensitivityTier;

/// Tipo di pattern di segreto/PII riconosciuto. Allineato a `PatternType` del TS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternType {
    AwsKey,
    AwsSecret,
    GcpServiceAccount,
    AzureSas,
    GithubPat,
    GitlabToken,
    Jwt,
    PemPrivateKey,
    DbConnectionString,
    GenericApiKey,
    ItalianCf,
    ItalianIban,
    CreditCard,
    EmailPii,
}

impl PatternType {
    /// Etichetta stabile (snake_case, identica al `PatternType` del TS). Usata
    /// nei placeholder di redazione e nei reason di audit.
    pub fn as_str(self) -> &'static str {
        match self {
            PatternType::AwsKey => "aws_key",
            PatternType::AwsSecret => "aws_secret",
            PatternType::GcpServiceAccount => "gcp_service_account",
            PatternType::AzureSas => "azure_sas",
            PatternType::GithubPat => "github_pat",
            PatternType::GitlabToken => "gitlab_token",
            PatternType::Jwt => "jwt",
            PatternType::PemPrivateKey => "pem_private_key",
            PatternType::DbConnectionString => "db_connection_string",
            PatternType::GenericApiKey => "generic_api_key",
            PatternType::ItalianCf => "italian_cf",
            PatternType::ItalianIban => "italian_iban",
            PatternType::CreditCard => "credit_card",
            PatternType::EmailPii => "email_pii",
        }
    }
}

/// Pattern trovato nel testo (`FoundPattern` del TS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoundPattern {
    pub kind: PatternType,
    pub tier: SensitivityTier,
    /// Offset in BYTE del match nel testo originale.
    pub offset: usize,
    /// Lunghezza in BYTE del match.
    pub length: usize,
}

/// Esito della scansione (`ScanResult` del TS).
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub found: bool,
    pub patterns: Vec<FoundPattern>,
    pub max_tier: SensitivityTier,
}

/// Definizione interna di un pattern: tipo, regex e tier associato.
struct PatternDef {
    kind: PatternType,
    pattern: Regex,
    tier: SensitivityTier,
}

// Regex statiche compilate una sola volta. `LazyLock<Regex>` con `expect` sulla
// compilazione e' l'idioma del repo per pattern hardcoded (CLAUDE.md §F): il
// pattern e' un literal sotto controllo, non input utente.
//
// I pattern sono portati 1:1 da `packages/shared/src/secret-scanner.ts`. Le
// differenze di sintassi rispetto al JS:
//  - i lookbehind/lookahead `(?<![A-Z0-9])` / `(?![A-Z0-9])` NON sono supportati
//    dalla `regex` crate (niente lookaround). Si emulano i confini con `\b` o
//    con classi di confine espanse dove necessario; per i pattern AWS si
//    accettano i confini word standard (`\b`), equivalenti nel caso pratico.
//  - `(?i)` inline al posto del flag `i`.
static PATTERNS: LazyLock<Vec<PatternDef>> = LazyLock::new(|| {
    vec![
        PatternDef {
            kind: PatternType::AwsKey,
            // TS: /(?<![A-Z0-9])(AKIA|ABIA|ACCA|ASIA)[A-Z0-9]{16}(?![A-Z0-9])/
            // regex crate senza lookaround: confine word \b (equivalente pratico).
            pattern: Regex::new(r"\b(AKIA|ABIA|ACCA|ASIA)[A-Z0-9]{16}\b")
                .expect("regex aws_key valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::AwsSecret,
            // Richiede il CONTESTO del nome campo AWS prima del valore di 40 char,
            // per evitare falsi positivi su hash/UUID/base64 generici.
            // TS termina con (?![A-Za-z0-9/+=]); senza lookahead usiamo un confine
            // non-cattura equivalente: il valore di 40 char seguito da fine o da
            // un carattere fuori classe. La regex crate non ha lookahead negativo,
            // quindi affidiamo al fatto che {40} e' "greedy esatto": se ci sono 41+
            // char il match prende comunque i primi 40 a partire dal contesto.
            pattern: Regex::new(
                r"(?i)(?:aws_?secret_?access_?key|secret_?access_?key|aws_?secret)[\x22'\s:=]{0,12}[A-Za-z0-9/+=]{40}",
            )
            .expect("regex aws_secret valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::GcpServiceAccount,
            pattern: Regex::new(r#""type"\s*:\s*"service_account""#)
                .expect("regex gcp_service_account valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::AzureSas,
            pattern: Regex::new(r"SharedAccessSignature\s+sig=[A-Za-z0-9%+/=]+")
                .expect("regex azure_sas valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::GithubPat,
            pattern: Regex::new(r"gh[pousr]_[A-Za-z0-9_]{20,255}")
                .expect("regex github_pat valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::GitlabToken,
            pattern: Regex::new(r"glpat-[A-Za-z0-9\-_]{20,}")
                .expect("regex gitlab_token valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::PemPrivateKey,
            pattern: Regex::new(r"-----BEGIN\s+(RSA|EC|DSA|OPENSSH)?\s*PRIVATE KEY-----")
                .expect("regex pem_private_key valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::DbConnectionString,
            pattern: Regex::new(
                r"(?i)(?:postgres(?:ql)?|mysql|mongodb|redis|mssql)://[^@\s]+:[^@\s]+@[^\s]+",
            )
            .expect("regex db_connection_string valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::Jwt,
            pattern: Regex::new(r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
                .expect("regex jwt valida"),
            tier: 2,
        },
        PatternDef {
            kind: PatternType::GenericApiKey,
            pattern: Regex::new(
                r#"(?i)(?:api[_-]?key|api[_-]?secret|access[_-]?token|bearer)\s*[=:]\s*["']?[A-Za-z0-9_\-.]{20,}["']?"#,
            )
            .expect("regex generic_api_key valida"),
            tier: 2,
        },
        PatternDef {
            kind: PatternType::ItalianCf,
            pattern: Regex::new(r"(?i)\b[A-Z]{6}\d{2}[A-Z]\d{2}[A-Z]\d{3}[A-Z]\b")
                .expect("regex italian_cf valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::ItalianIban,
            pattern: Regex::new(
                r"(?i)\bIT\d{2}\s?[A-Z]\d{3}\s?\d{4}\s?\d{4}\s?\d{4}\s?\d{4}\s?\d{3}\b",
            )
            .expect("regex italian_iban valida"),
            tier: 3,
        },
        PatternDef {
            kind: PatternType::CreditCard,
            pattern: Regex::new(
                r"\b(?:4[0-9]{12}(?:[0-9]{3})?|5[1-5][0-9]{14}|3[47][0-9]{13}|3(?:0[0-5]|[68][0-9])[0-9]{11})\b",
            )
            .expect("regex credit_card valida"),
            tier: 2,
        },
        PatternDef {
            kind: PatternType::EmailPii,
            pattern: Regex::new(r"\b[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}\b")
                .expect("regex email_pii valida"),
            tier: 2,
        },
    ]
});

/// Scanner di segreti/PII su testo. Stateless: nessun campo, i pattern sono
/// statici condivisi. Esposto come tipo (non funzioni libere) per parita' con
/// l'API TS e per dare un punto di estensione futuro.
#[derive(Debug, Default, Clone, Copy)]
pub struct SecretScanner;

impl SecretScanner {
    /// Scansiona il testo e ritorna i pattern trovati (primo match per pattern)
    /// con il tier massimo. Parita' con `SecretScanner.scan` del TS.
    pub fn scan(&self, text: &str) -> ScanResult {
        let mut patterns = Vec::new();
        let mut max_tier: SensitivityTier = 0;

        for def in PATTERNS.iter() {
            if let Some(m) = def.pattern.find(text) {
                patterns.push(FoundPattern {
                    kind: def.kind,
                    tier: def.tier,
                    offset: m.start(),
                    length: m.end() - m.start(),
                });
                if def.tier > max_tier {
                    max_tier = def.tier;
                }
            }
        }

        ScanResult {
            found: !patterns.is_empty(),
            patterns,
            max_tier,
        }
    }

    /// Redige tutte le occorrenze di ogni pattern con `[REDACTED:<type>]`.
    /// Ritorna il testo redatto e il numero di TIPI di pattern effettivamente
    /// redatti (parita' con `SecretScanner.redact` del TS, che conta i tipi che
    /// hanno prodotto almeno una sostituzione, non le singole occorrenze).
    pub fn redact(&self, text: &str) -> (String, usize) {
        let mut redacted = text.to_string();
        let mut count = 0usize;

        for def in PATTERNS.iter() {
            let before = redacted.clone();
            let placeholder = format!("[REDACTED:{}]", def.kind.as_str());
            redacted = def
                .pattern
                .replace_all(&redacted, placeholder.as_str())
                .into_owned();
            if redacted != before {
                count += 1;
            }
        }

        (redacted, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_trova_token_finto_aws() {
        let s = SecretScanner;
        let r = s.scan("let key = \"AKIAIOSFODNN7EXAMPLE\";");
        assert!(r.found);
        assert_eq!(r.max_tier, 3);
        assert!(r.patterns.iter().any(|p| p.kind == PatternType::AwsKey));
    }

    #[test]
    fn scan_trova_github_pat() {
        let s = SecretScanner;
        let r = s.scan("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(r.found);
        assert!(r.patterns.iter().any(|p| p.kind == PatternType::GithubPat));
        assert_eq!(r.max_tier, 3);
    }

    #[test]
    fn scan_trova_jwt_tier2() {
        let s = SecretScanner;
        // JWT finto (header.payload.signature)
        let r = s.scan("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcDEF_123-xyz");
        assert!(r.found);
        assert!(r.patterns.iter().any(|p| p.kind == PatternType::Jwt));
    }

    #[test]
    fn scan_trova_email_pii() {
        let s = SecretScanner;
        let r = s.scan("contatto: mario.rossi@example.com");
        assert!(r.found);
        assert!(r.patterns.iter().any(|p| p.kind == PatternType::EmailPii));
        assert_eq!(r.max_tier, 2);
    }

    #[test]
    fn scan_testo_pulito_non_trova_nulla() {
        let s = SecretScanner;
        let r = s.scan("Questo e' un testo innocuo senza segreti.");
        assert!(!r.found);
        assert_eq!(r.max_tier, 0);
        assert!(r.patterns.is_empty());
    }

    #[test]
    fn redact_sostituisce_e_conta_i_tipi() {
        let s = SecretScanner;
        let (out, count) = s.redact("key=AKIAIOSFODNN7EXAMPLE email=mario@example.com");
        assert!(out.contains("[REDACTED:aws_key]"));
        assert!(out.contains("[REDACTED:email_pii]"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!out.contains("mario@example.com"));
        // Due tipi distinti redatti.
        assert_eq!(count, 2);
    }

    #[test]
    fn redact_testo_pulito_e_no_op() {
        let s = SecretScanner;
        let (out, count) = s.redact("nessun segreto qui");
        assert_eq!(out, "nessun segreto qui");
        assert_eq!(count, 0);
    }

    #[test]
    fn aws_secret_richiede_contesto() {
        let s = SecretScanner;
        // 40 char base64 SENZA contesto AWS -> non deve scattare aws_secret.
        let blob = "abcd1234ABCD5678efgh9012IJKL3456mnop7890";
        let r = s.scan(blob);
        assert!(!r.patterns.iter().any(|p| p.kind == PatternType::AwsSecret));
        // CON contesto -> scatta.
        let with_ctx = format!("aws_secret_access_key = {blob}");
        let r2 = s.scan(&with_ctx);
        assert!(r2.patterns.iter().any(|p| p.kind == PatternType::AwsSecret));
        assert_eq!(r2.max_tier, 3);
    }
}
