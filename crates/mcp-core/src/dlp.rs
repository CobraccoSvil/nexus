//! Data Loss Prevention: classificazione sensibilità testo e policy routing.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus. Tipi e
//! funzioni restano invariati; il loop agente vero e proprio e' ora nel
//! brain LangGraph (Python).

/// Tier di sensibilità dei dati inviati al provider LLM.
/// Usato per applicare policy di routing: tier alto = solo provider locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SensitivityTier {
    /// Tier 0: dati pubblici o generici (commenti, domande generali)
    Public = 0,
    /// Tier 1: dati interni normali (codice sorgente generico)
    Internal = 1,
    /// Tier 2: dati sensibili (credenziali parziali, config con env var)
    Sensitive = 2,
    /// Tier 3: dati critici (PII, chiavi API, segreti in chiaro, token JWT)
    Critical = 3,
}

/// Classifica la sensibilità del testo in base a pattern riconoscibili.
/// Approccio leggero basato su regex — nessuna dipendenza ML.
pub fn classify_sensitivity(text: &str) -> SensitivityTier {
    // Pattern Tier 3: dati critici (mai inviare a cloud non autorizzato)
    let tier3_patterns: &[&str] = &[
        // API key e secret key generici
        r"(?i)(api_key|secret_key|access_token)\s*[:=]\s*\S{20,}",
        // AWS access keys
        r"AKIA[0-9A-Z]{16}",
        // JWT tokens (3 parti base64url separate da punto)
        r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
        // Codice fiscale italiano
        r"[A-Z]{6}[0-9]{2}[A-Z][0-9]{2}[A-Z][0-9]{3}[A-Z]",
        // Chiavi private PEM
        r"-----BEGIN (RSA |EC |OPENSSH |)PRIVATE KEY-----",
        // Anthropic API keys
        r"sk-ant-api",
        // OpenAI API keys
        r"sk-proj-[A-Za-z0-9]{40}",
        // Google API keys
        r"AIzaSy[A-Za-z0-9_-]{33}",
        // Password in chiaro (pattern comune)
        r"(?i)password\s*[:=]\s*\S{6,}",
    ];

    // Pattern Tier 2: dati sensibili
    let tier2_patterns: &[&str] = &[
        // URL con credenziali (user:pass@host)
        r"://[^:/\s]+:[^@/\s]+@",
        // Variabili d'ambiente con segreti
        r"(?i)(SECRET|TOKEN|KEY|PASSWORD|PASSWD|CREDENTIAL)\s*=\s*\S",
        // Indirizzi email
        r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
        // Numeri di telefono italiani
        r"(\+39|0039)?\s*3[0-9]{9}",
        // Connection string database con credenziali
        r"(?i)postgres(ql)?://[^/\s]+:[^@/\s]+@",
    ];

    // Pattern Tier 1: dati interni (codice, percorsi, config generici)
    let tier1_patterns: &[&str] = &[
        // Percorsi assoluti Unix/Linux
        r"/(home|etc|var|usr|opt)/[^\s]+",
        // Percorsi Windows
        r"[A-Za-z]:[/\\][^\s]+",
        // localhost o 127.0.0.1
        r"(localhost|127\.0\.0\.1|0\.0\.0\.0)",
    ];

    for pattern in tier3_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if re.is_match(text) {
                return SensitivityTier::Critical;
            }
        }
    }

    for pattern in tier2_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if re.is_match(text) {
                return SensitivityTier::Sensitive;
            }
        }
    }

    for pattern in tier1_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if re.is_match(text) {
                return SensitivityTier::Internal;
            }
        }
    }

    SensitivityTier::Public
}

/// Determina se il provider scelto è compatibile con il tier di sensibilità.
/// Rispetta le env var `NEXUS_DLP_ENABLED`, `NEXUS_ALLOW_CLOUD_TIER2`, `NEXUS_ALLOW_CLOUD_TIER3`.
/// Ritorna None se compatibile, Some(warning_message) se incompatibile o con avviso.
#[allow(dead_code)]
pub fn check_dlp_policy(provider: &str, tier: SensitivityTier) -> Option<String> {
    let dlp_enabled = std::env::var("NEXUS_DLP_ENABLED")
        .map(|v| v.trim().to_lowercase() != "false")
        .unwrap_or(true);

    if !dlp_enabled {
        return None; // DLP disabilitato, nessun controllo
    }

    // Provider locali/EU non hanno restrizioni
    let is_local_or_eu = matches!(provider.to_lowercase().as_str(), "ollama" | "mistral" | "onprem");
    if is_local_or_eu {
        return None;
    }

    // Provider cloud (openai, anthropic, google, deepseek)
    match tier {
        SensitivityTier::Critical => {
            let allow_tier3 = std::env::var("NEXUS_ALLOW_CLOUD_TIER3")
                .map(|v| v.trim().to_lowercase() == "true")
                .unwrap_or(false);
            if !allow_tier3 {
                Some(format!(
                    "DLP Block: il messaggio contiene dati critici (Tier 3: chiavi API, password, PII) \
che non possono essere inviati a **{}** (provider cloud). \
Usa un modello locale (Ollama) o rimuovi i dati sensibili. \
Puoi abilitare il bypass impostando `NEXUS_ALLOW_CLOUD_TIER3=true` (sconsigliato).",
                    provider
                ))
            } else {
                Some(format!(
                    "DLP Warning: il messaggio contiene dati critici (Tier 3) inviati a **{}** \
— NEXUS_ALLOW_CLOUD_TIER3=true è attivo ma sconsigliato.",
                    provider
                ))
            }
        }
        SensitivityTier::Sensitive => {
            let allow_tier2 = std::env::var("NEXUS_ALLOW_CLOUD_TIER2")
                .map(|v| v.trim().to_lowercase() != "false")
                .unwrap_or(true);
            if !allow_tier2 {
                Some(format!(
                    "DLP Block: il messaggio contiene dati sensibili (Tier 2: email, credenziali env) \
che non possono essere inviati a **{}**. \
Imposta `NEXUS_ALLOW_CLOUD_TIER2=true` per abilitare oppure usa Ollama.",
                    provider
                ))
            } else {
                Some(format!(
                    "DLP Notice: il messaggio contiene dati sensibili (Tier 2) inviati a **{}** \
— assicurati che sia intenzionale.",
                    provider
                ))
            }
        }
        _ => None, // Tier 0-1: nessun problema
    }
}

/// Versione di check_dlp_policy con configurazione dal DB (override rispetto alle env var).
/// Usata dai chiamanti che caricano i settings DLP dal database.
#[allow(dead_code)]
pub fn check_dlp_policy_with_config(
    provider: &str,
    tier: SensitivityTier,
    dlp_enabled: bool,
    allow_tier2: bool,
    allow_tier3: bool,
) -> Option<String> {
    if !dlp_enabled {
        return None;
    }

    let is_local_or_eu = matches!(provider.to_lowercase().as_str(), "ollama" | "mistral" | "onprem");
    if is_local_or_eu {
        return None;
    }

    match tier {
        SensitivityTier::Critical => {
            if !allow_tier3 {
                Some(format!(
                    "DLP Block: il messaggio contiene dati critici (Tier 3: chiavi API, password, PII, JWT) che non possono essere inviati a **{}** (provider cloud). Rimuovi i dati sensibili o usa Ollama (locale) o Mistral (EU). Configurabile in Admin → Sicurezza → dlp_allow_cloud_tier3.",
                    provider
                ))
            } else {
                Some(format!(
                    "DLP Warning — Tier 3: dati critici inviati a **{}** (dlp_allow_cloud_tier3=true). Verifica che sia intenzionale.",
                    provider
                ))
            }
        }
        SensitivityTier::Sensitive => {
            if !allow_tier2 {
                Some(format!(
                    "DLP Block — Tier 2: il messaggio contiene dati sensibili (email, credenziali) che non possono essere inviati a **{}**. Configura dlp_allow_cloud_tier2=true in Admin → Sicurezza o usa Ollama/Mistral.",
                    provider
                ))
            } else {
                Some(format!(
                    "DLP Notice — Tier 2: il messaggio contiene dati sensibili inviati a **{}**. Assicurati che sia intenzionale.",
                    provider
                ))
            }
        }
        _ => None,
    }
}
