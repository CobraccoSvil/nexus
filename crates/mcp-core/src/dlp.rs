//! Data Loss Prevention: classificazione sensibilità testo e policy routing.
//!
//! Estratto da `agent_loop.rs` durante la Fase 4 del refactor Nexus.
//! I settings DLP vengono letti dal DB (`settings` table) con cache 60s.
//! Nessuna lettura di variabili d'ambiente a runtime: la configurazione
//! viene gestita tramite Admin → Sicurezza → DLP (chiavi dlp_enabled,
//! dlp_allow_cloud_tier2, dlp_allow_cloud_tier3).

use sqlx::PgPool;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, warn};

const DLP_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Configurazione DLP caricata dal DB con timestamp per il refresh.
#[derive(Clone, Debug)]
struct DlpConfigCache {
    enabled: bool,
    allow_cloud_tier2: bool,
    allow_cloud_tier3: bool,
    loaded_at: Instant,
}

impl DlpConfigCache {
    /// Valori predefiniti conservativi: DLP attivo, Tier 2 permesso, Tier 3 bloccato.
    fn default_conservative() -> Self {
        Self {
            enabled: true,
            allow_cloud_tier2: true,
            allow_cloud_tier3: false,
            loaded_at: Instant::now() - DLP_REFRESH_INTERVAL * 2, // forza reload immediato
        }
    }

    fn is_stale(&self) -> bool {
        self.loaded_at.elapsed() >= DLP_REFRESH_INTERVAL
    }
}

static DLP_CACHE: OnceLock<Mutex<DlpConfigCache>> = OnceLock::new();

fn get_cache() -> &'static Mutex<DlpConfigCache> {
    DLP_CACHE.get_or_init(|| Mutex::new(DlpConfigCache::default_conservative()))
}

/// Carica i 3 parametri DLP dalla tabella `settings`.
/// In caso di errore DB usa i valori conservativi (DLP attivo, Tier 3 bloccato).
async fn fetch_dlp_config_from_db(db: &PgPool) -> DlpConfigCache {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM settings WHERE key IN ('dlp_enabled', 'dlp_allow_cloud_tier2', 'dlp_allow_cloud_tier3')",
    )
    .fetch_all(db)
    .await;

    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "DLP: impossibile caricare config dal DB ({}). Uso valori conservativi.",
                e
            );
            return DlpConfigCache {
                enabled: true,
                allow_cloud_tier2: true,
                allow_cloud_tier3: false,
                loaded_at: Instant::now(),
            };
        }
    };

    let mut enabled = true;
    let mut allow_tier2 = true;
    let mut allow_tier3 = false;

    for (key, value) in rows {
        let v = value.trim().to_lowercase();
        match key.as_str() {
            "dlp_enabled" => enabled = v != "false",
            "dlp_allow_cloud_tier2" => allow_tier2 = v != "false",
            "dlp_allow_cloud_tier3" => allow_tier3 = v == "true",
            _ => {}
        }
    }

    DlpConfigCache {
        enabled,
        allow_cloud_tier2: allow_tier2,
        allow_cloud_tier3: allow_tier3,
        loaded_at: Instant::now(),
    }
}

/// Invalida la cache DLP: il prossimo check ricaricherà dal DB.
/// Chiamato da `settings.rs` dopo il salvataggio di una chiave DLP.
pub fn invalidate_dlp_cache() {
    if let Some(mutex) = DLP_CACHE.get() {
        // try_lock per non bloccare mai — se il lock è preso da un check concorrente
        // il reload avverrà comunque entro 60s.
        if let Ok(mut guard) = mutex.try_lock() {
            guard.loaded_at = Instant::now() - DLP_REFRESH_INTERVAL * 2;
            debug!("DLP: cache invalidata, prossimo check ricarica dal DB");
        }
    }
}

/// Verifica la policy DLP leggendo la configurazione dal DB (cache 60s).
/// Ritorna `None` se il provider/tier sono compatibili, `Some(msg)` se c'è
/// un blocco o un avviso.
///
/// Il caller decide come gestire `Some`:
/// - Messaggio contenente "DLP Block" → ritornare errore 403
/// - Messaggio contenente "DLP Warning"/"DLP Notice" → loggare e proseguire
pub async fn check_dlp_policy_db(
    provider: &str,
    tier: SensitivityTier,
    db: &PgPool,
) -> Option<String> {
    let cache = get_cache();
    let config = {
        let guard = cache.lock().await;
        if !guard.is_stale() {
            // Cache valida: clona e rilascia subito il lock
            guard.clone()
        } else {
            drop(guard);
            let fresh = fetch_dlp_config_from_db(db).await;
            let mut guard = cache.lock().await;
            *guard = fresh.clone();
            fresh
        }
    };

    check_dlp_policy_with_config(
        provider,
        tier,
        config.enabled,
        config.allow_cloud_tier2,
        config.allow_cloud_tier3,
    )
}

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

/// Verifica la policy DLP con configurazione esplicita (bool).
/// Funzione core usata da `check_dlp_policy_db` dopo il caricamento della cache.
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

    let is_local_or_eu = matches!(
        provider.to_lowercase().as_str(),
        "ollama" | "mistral" | "onprem"
    );
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
