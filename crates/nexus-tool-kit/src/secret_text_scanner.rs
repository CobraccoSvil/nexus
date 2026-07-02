//! Secret scanner su STRINGA in-memory — punto unico Rust (regola L / ADR 0026).
//!
//! Nato in `nexus-gateway::redaction::secret_scanner` come porting fedele di
//! `packages/shared/src/secret-scanner.ts`; spostato qui perche' serve anche a
//! `mcp-core` (redazione output processi in `agent_processes.rs` /
//! `terminal_ws.rs`, difesa in profondita' post-incidente Beaty-Book
//! 2026-07-02: connection string Postgres in chiaro nei tool_result). Il
//! gateway re-esporta questo modulo, i suoi call site restano invariati.
//!
//! ## Perche' un modulo distinto dagli altri scanner del crate
//!
//! `secret_scan.rs` e `sec_secret_patterns.rs` hanno un contratto DIVERSO:
//! sono `NexusToolHandler` (tool agente) che scansionano la project root su
//! disco e ritornano findings `{file, line, preview}`. Questo modulo scansiona
//! una stringa in-memory, assegna un tier per pattern e produce testo redatto.
//!
//! ## Due profili di redazione
//!
//! - [`SecretScanner::redact`]: placeholder totale `[REDACTED:<type>]` su TUTTI
//!   i pattern (segreti + PII). E' il profilo del gateway per i prompt in
//!   uscita verso i provider (parita' col TS).
//! - [`SecretScanner::redact_secrets_preserving_context`]: SOLO i pattern
//!   classificati segreto tecnico, mantenendo il contesto utile al debugging.
//!   Nelle connection URL maschera la sola password (host/porta/db restano
//!   leggibili: l'incidente "placeholder copiato come valore" nasce proprio da
//!   una redazione totale che distrugge l'informazione di connessione); nei
//!   pattern con prefisso di campo (`api_key=`, `aws_secret_access_key=`)
//!   conserva il nome campo e maschera il valore. Le PII (email, CF, IBAN,
//!   carte) NON vengono toccate: negli output di processo sono spesso dati di
//!   test necessari alla diagnosi. E' il profilo della persistenza output
//!   processi. La funzione e' idempotente: riapplicarla a testo gia' redatto
//!   non cambia nulla.
//!
//! ## Divergenze dai pattern TS (documentate)
//!
//! `db_connection_string` e' esteso rispetto al TS: user opzionale
//! (`redis://:pwd@host`), schemi `amqp(s)`, `rediss`, `mongodb+srv`. Il TS
//! richiedeva user non vuoto e non copriva AMQP.
//!
//! ## Regola F (no leak nei log)
//! Questo modulo non logga nulla: opera in puro calcolo. I caller loggano solo
//! conteggi/tipi.

use std::sync::LazyLock;

use regex::Regex;

/// Tier di sensibilita' del dato (0 = pubblico ... 3 = massimo riservato).
/// Alias identico a `nexus_gateway::types::SensitivityTier` (u8): i due alias
/// sono lo stesso tipo, nessuna conversione richiesta nei call site.
pub type SensitivityTier = u8;

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

/// Classe del pattern: segreto tecnico (credenziale/chiave/token) oppure PII.
/// Governa quali pattern partecipano al profilo
/// [`SecretScanner::redact_secrets_preserving_context`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternClass {
    Secret,
    Pii,
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

/// Definizione interna di un pattern: tipo, regex, tier, classe e template di
/// redazione context-preserving.
struct PatternDef {
    kind: PatternType,
    pattern: Regex,
    tier: SensitivityTier,
    class: PatternClass,
    /// Template `replace_all` per la redazione che preserva il contesto
    /// (gruppi nominati della regex). `None` = placeholder totale
    /// `[REDACTED:<type>]` anche nel profilo context-preserving.
    keep_context: Option<&'static str>,
}

// Regex statiche compilate una sola volta. `LazyLock<Regex>` con `expect` sulla
// compilazione e' l'idioma del repo per pattern hardcoded (CLAUDE.md §F): il
// pattern e' un literal sotto controllo, non input utente.
//
// I pattern sono portati da `packages/shared/src/secret-scanner.ts`. Le
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
            class: PatternClass::Secret,
            keep_context: None,
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
                r"(?i)(?P<prefix>(?:aws_?secret_?access_?key|secret_?access_?key|aws_?secret)[\x22'\s:=]{0,12})[A-Za-z0-9/+=]{40}",
            )
            .expect("regex aws_secret valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: Some("${prefix}[REDACTED:aws_secret]"),
        },
        PatternDef {
            kind: PatternType::GcpServiceAccount,
            pattern: Regex::new(r#""type"\s*:\s*"service_account""#)
                .expect("regex gcp_service_account valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::AzureSas,
            pattern: Regex::new(r"SharedAccessSignature\s+sig=[A-Za-z0-9%+/=]+")
                .expect("regex azure_sas valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::GithubPat,
            pattern: Regex::new(r"gh[pousr]_[A-Za-z0-9_]{20,255}")
                .expect("regex github_pat valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::GitlabToken,
            pattern: Regex::new(r"glpat-[A-Za-z0-9\-_]{20,}")
                .expect("regex gitlab_token valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::PemPrivateKey,
            pattern: Regex::new(r"-----BEGIN\s+(RSA|EC|DSA|OPENSSH)?\s*PRIVATE KEY-----")
                .expect("regex pem_private_key valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::DbConnectionString,
            // Esteso rispetto al TS (vedi doc modulo): user opzionale per coprire
            // `redis://:pwd@host`, schemi amqp(s)/rediss/mongodb+srv. I gruppi
            // nominati servono alla redazione password-only: host/porta/db name
            // devono restare leggibili per il debugging.
            pattern: Regex::new(
                r"(?i)\b(?P<scheme>(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|rediss?|mssql|amqps?)://)(?P<user>[^@\s:/]*):(?P<pwd>[^@\s]+)@(?P<rest>[^\s]+)",
            )
            .expect("regex db_connection_string valida"),
            tier: 3,
            class: PatternClass::Secret,
            keep_context: Some("${scheme}${user}:[REDACTED:db_password]@${rest}"),
        },
        PatternDef {
            kind: PatternType::Jwt,
            pattern: Regex::new(r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
                .expect("regex jwt valida"),
            tier: 2,
            class: PatternClass::Secret,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::GenericApiKey,
            pattern: Regex::new(
                r#"(?i)(?P<prefix>(?:api[_-]?key|api[_-]?secret|access[_-]?token|bearer)\s*[=:]\s*["']?)[A-Za-z0-9_\-.]{20,}["']?"#,
            )
            .expect("regex generic_api_key valida"),
            tier: 2,
            class: PatternClass::Secret,
            keep_context: Some("${prefix}[REDACTED:generic_api_key]"),
        },
        PatternDef {
            kind: PatternType::ItalianCf,
            pattern: Regex::new(r"(?i)\b[A-Z]{6}\d{2}[A-Z]\d{2}[A-Z]\d{3}[A-Z]\b")
                .expect("regex italian_cf valida"),
            tier: 3,
            class: PatternClass::Pii,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::ItalianIban,
            pattern: Regex::new(
                r"(?i)\bIT\d{2}\s?[A-Z]\d{3}\s?\d{4}\s?\d{4}\s?\d{4}\s?\d{4}\s?\d{3}\b",
            )
            .expect("regex italian_iban valida"),
            tier: 3,
            class: PatternClass::Pii,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::CreditCard,
            pattern: Regex::new(
                r"\b(?:4[0-9]{12}(?:[0-9]{3})?|5[1-5][0-9]{14}|3[47][0-9]{13}|3(?:0[0-5]|[68][0-9])[0-9]{11})\b",
            )
            .expect("regex credit_card valida"),
            tier: 2,
            class: PatternClass::Pii,
            keep_context: None,
        },
        PatternDef {
            kind: PatternType::EmailPii,
            pattern: Regex::new(r"\b[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}\b")
                .expect("regex email_pii valida"),
            tier: 2,
            class: PatternClass::Pii,
            keep_context: None,
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

    /// Redige i SOLI segreti tecnici preservando il contesto di debugging.
    /// Vedi la doc del modulo per il razionale (profilo persistenza output
    /// processi). Ritorna il testo redatto e il numero di TIPI di pattern che
    /// hanno prodotto almeno una sostituzione. Idempotente.
    pub fn redact_secrets_preserving_context(&self, text: &str) -> (String, usize) {
        let mut redacted = text.to_string();
        let mut count = 0usize;

        for def in PATTERNS.iter() {
            if def.class != PatternClass::Secret {
                continue;
            }
            let before = redacted.clone();
            redacted = match def.keep_context {
                Some(template) => def.pattern.replace_all(&redacted, template).into_owned(),
                None => {
                    let placeholder = format!("[REDACTED:{}]", def.kind.as_str());
                    def.pattern
                        .replace_all(&redacted, placeholder.as_str())
                        .into_owned()
                }
            };
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

    // --- redact_secrets_preserving_context (profilo output processi) ---

    #[test]
    fn preserving_maschera_solo_la_password_della_connection_string() {
        // Regression test incidente Beaty-Book 2026-07-02: la connection string
        // stampata da un backend Node finiva in chiaro in agent_processes.
        let s = SecretScanner;
        let log = "DB ready at postgres://nexus:nexus@localhost:5433/nexus (pool=5)";
        let (out, count) = s.redact_secrets_preserving_context(log);
        assert_eq!(
            out,
            "DB ready at postgres://nexus:[REDACTED:db_password]@localhost:5433/nexus (pool=5)"
        );
        // Host, porta e nome db restano leggibili per il debugging.
        assert!(out.contains("localhost:5433/nexus"));
        assert!(!out.contains(":nexus@"));
        assert_eq!(count, 1);
    }

    #[test]
    fn preserving_copre_redis_senza_user_e_amqp() {
        let s = SecretScanner;
        let (redis_out, _) =
            s.redact_secrets_preserving_context("cache: redis://:s3cretpwd@10.0.0.5:6379/0");
        assert_eq!(
            redis_out,
            "cache: redis://:[REDACTED:db_password]@10.0.0.5:6379/0"
        );

        let (amqp_out, _) =
            s.redact_secrets_preserving_context("broker amqp://guest:guest@rabbit:5672/vhost up");
        assert_eq!(
            amqp_out,
            "broker amqp://guest:[REDACTED:db_password]@rabbit:5672/vhost up"
        );
    }

    #[test]
    fn preserving_mantiene_il_nome_campo_delle_api_key() {
        let s = SecretScanner;
        let (out, count) = s
            .redact_secrets_preserving_context("API_KEY=sk_live_abcdefghij0123456789 caricata");
        assert!(out.starts_with("API_KEY="));
        assert!(out.contains("[REDACTED:generic_api_key]"));
        assert!(!out.contains("sk_live_abcdefghij0123456789"));
        assert_eq!(count, 1);
    }

    #[test]
    fn preserving_redige_token_senza_contesto_in_toto() {
        let s = SecretScanner;
        let (out, _) = s.redact_secrets_preserving_context(
            "push con ghp_abcdefghijklmnopqrstuvwxyz0123456789 ok",
        );
        assert!(out.contains("[REDACTED:github_pat]"));
        assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn preserving_non_tocca_le_pii() {
        // Negli output di processo email/CF sono spesso dati di test necessari
        // alla diagnosi: il profilo secrets non li redige.
        let s = SecretScanner;
        let log = "utente mario.rossi@example.com creato, cf RSSMRA80A01H501U";
        let (out, count) = s.redact_secrets_preserving_context(log);
        assert_eq!(out, log);
        assert_eq!(count, 0);
    }

    #[test]
    fn preserving_e_idempotente() {
        let s = SecretScanner;
        let log = "postgres://nexus:nexus@localhost:5433/nexus API_KEY=sk_live_abcdefghij0123456789 ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let (once, _) = s.redact_secrets_preserving_context(log);
        let (twice, count_twice) = s.redact_secrets_preserving_context(&once);
        assert_eq!(once, twice);
        // La seconda passata puo' ri-matchare il placeholder della password
        // (sostituzione identica) ma non deve alterare il testo.
        let _ = count_twice;
    }

    #[test]
    fn preserving_url_senza_credenziali_e_no_op() {
        let s = SecretScanner;
        let log = "listening on postgres://localhost:5433/nexus e http://0.0.0.0:8080";
        let (out, count) = s.redact_secrets_preserving_context(log);
        assert_eq!(out, log);
        assert_eq!(count, 0);
    }
}
