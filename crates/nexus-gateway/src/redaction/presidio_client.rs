//! Client HTTP al servizio Presidio (PII detector).
//!
//! Porting di `packages/llm-gateway/src/router/presidio-client.ts`.
//!
//! ## Fallback graceful (regola contratto + F)
//! Presidio e' un microservizio Python opzionale: se non e' raggiungibile, il
//! gateway NON deve bloccare. In quel caso il client ritorna un risultato vuoto
//! (`has_pii=false`, `max_tier=0`): il secret scanner copre comunque i segreti
//! strutturati. La non-disponibilita' e' loggata come WARN, MAI con il testo
//! analizzato o le entita' rilevate (regola F: solo conteggi/tipi).
//!
//! ## Config da settings (regola G)
//! - `dlp_presidio_base_url`  : URL base REST del servizio (es. `http://127.0.0.1:5051`).
//!                              Se assente/vuoto -> Presidio considerato non
//!                              configurato -> fallback graceful (no PII).
//! - `dlp_presidio_language`  : lingua di analisi (default `it`).
//! - `dlp_presidio_timeout_ms`: timeout per chiamata HTTP (default 5000).
//!
//! I valori sono cachati 60s via `nexus_cache::TtlCache` (punto unico, regola L),
//! stesso pattern di `policy_engine.rs`. Niente URL/porta hardcoded nel codice.

use std::collections::HashMap;
use std::time::Duration;

use nexus_cache::TtlCache;
use serde::Deserialize;
use sqlx::PgPool;

use crate::types::SensitivityTier;

/// TTL della config Presidio letta dai settings (60s, come il policy engine).
const CONFIG_TTL: Duration = Duration::from_secs(60);

/// Lingua di default se il setting non e' valorizzato.
const DEFAULT_LANGUAGE: &str = "it";

/// Timeout di default (ms) per la chiamata di analisi.
const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// Entita' PII restituita da Presidio.
#[derive(Debug, Clone, Deserialize)]
pub struct PresidioEntity {
    pub entity_type: String,
    pub start: usize,
    pub end: usize,
    #[serde(default)]
    pub score: f32,
}

/// Esito dell'analisi PII.
#[derive(Debug, Clone, Default)]
pub struct PresidioResult {
    pub entities: Vec<PresidioEntity>,
    pub max_tier: SensitivityTier,
    pub has_pii: bool,
}

/// Mappa `entity_type` -> tier di sensibilita'. Parita' 1:1 con `ENTITY_TIER_MAP`
/// del TS. Entita' non mappate ricadono su tier 1 (vedi `tier_for_entity`).
fn tier_for_entity(entity_type: &str) -> SensitivityTier {
    match entity_type {
        "PERSON" | "EMAIL_ADDRESS" | "PHONE_NUMBER" | "IT_VAT_CODE" | "IP_ADDRESS" => 2,
        "LOCATION" | "DATE_TIME" | "URL" => 1,
        "IT_FISCAL_CODE" | "IT_DRIVER_LICENSE" | "CREDIT_CARD" | "IBAN_CODE" | "MEDICAL_LICENSE"
        | "NRP" | "CRYPTO" | "US_SSN" | "US_PASSPORT" | "US_DRIVER_LICENSE" => 3,
        // Entita' sconosciuta -> tier 1 (`?? 1` del TS).
        _ => 1,
    }
}

/// Config runtime del client, letta dai settings.
#[derive(Debug, Clone, Default)]
struct PresidioConfig {
    /// URL base REST (es. `http://host:5051`). `None`/vuoto -> non configurato.
    base_url: Option<String>,
    language: String,
    timeout: Duration,
}

/// Client HTTP verso Presidio. Clonabile a basso costo (reqwest::Client e
/// TtlCache condividono lo stato interno via Arc).
#[derive(Debug, Clone)]
pub struct PresidioClient {
    http: reqwest::Client,
    // Singola entry di config globale, scadenza gestita da TtlCache (regola L).
    config: TtlCache<(), PresidioConfig>,
}

impl Default for PresidioClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PresidioClient {
    /// Crea il client. La config viene caricata pigramente al primo `analyze`
    /// (o esplicitamente con `refresh_config`).
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            config: TtlCache::new(CONFIG_TTL),
        }
    }

    /// Ricarica la config dai settings DB. `force=true` ignora la cache.
    /// Fallback graceful: se il DB e' down, mantiene la config corrente (o, se
    /// mai caricata, lascia Presidio "non configurato" -> nessun blocco).
    pub async fn refresh_config(&self, pool: &PgPool, force: bool) {
        if !force && self.config.get(&()).is_some() {
            return;
        }

        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT key, value FROM settings \
             WHERE key IN ('dlp_presidio_base_url', 'dlp_presidio_language', 'dlp_presidio_timeout_ms')",
        )
        .fetch_all(pool)
        .await;

        match rows {
            Ok(rows) => {
                let map: HashMap<&str, &str> =
                    rows.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

                let base_url = map
                    .get("dlp_presidio_base_url")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());

                let language = map
                    .get("dlp_presidio_language")
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(DEFAULT_LANGUAGE)
                    .to_string();

                let timeout_ms = map
                    .get("dlp_presidio_timeout_ms")
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(DEFAULT_TIMEOUT_MS);

                self.config.insert(
                    (),
                    PresidioConfig {
                        base_url,
                        language,
                        timeout: Duration::from_millis(timeout_ms),
                    },
                );
            }
            Err(err) => {
                // DB down: non azzerare la config nota. Solo conteggio/errore nel log.
                tracing::warn!(
                    error = %err,
                    "presidio-client: refresh config fallito, mantengo i valori correnti"
                );
            }
        }
    }

    /// Config corrente (cache) o default vuoto (Presidio non configurato).
    fn current_config(&self) -> PresidioConfig {
        self.config.get(&()).unwrap_or_else(|| PresidioConfig {
            base_url: None,
            language: DEFAULT_LANGUAGE.to_string(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        })
    }

    /// Analizza il testo per PII. Fallback graceful su ogni errore/non-config:
    /// ritorna risultato vuoto senza propagare (il caller non si blocca).
    ///
    /// Regola F: in caso di errore non logga il testo ne' la risposta; logga
    /// solo lo stato (servizio non configurato / non raggiungibile).
    pub async fn analyze(&self, text: &str) -> PresidioResult {
        let config = self.current_config();

        let Some(base_url) = config.base_url.as_deref() else {
            // Presidio non configurato -> nessuna PII rilevata (secret scanner copre).
            return PresidioResult::default();
        };

        let url = format!("{}/analyze", base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "text": text, "language": config.language });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .timeout(config.timeout)
            .send()
            .await;

        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(
                    status = %r.status(),
                    "presidio-client: risposta non-2xx, fallback senza PII"
                );
                return PresidioResult::default();
            }
            Err(_) => {
                // Servizio non raggiungibile/timeout: fallback graceful (no leak).
                tracing::warn!("presidio-client: servizio non raggiungibile, fallback senza PII");
                return PresidioResult::default();
            }
        };

        let entities: Vec<PresidioEntity> = match resp.json().await {
            Ok(e) => e,
            Err(_) => {
                tracing::warn!("presidio-client: parsing risposta fallito, fallback senza PII");
                return PresidioResult::default();
            }
        };

        Self::build_result(entities)
    }

    /// Costruisce il `PresidioResult` da una lista di entita' (separato per
    /// poterlo testare senza rete: mapping entity->tier deterministico).
    pub fn build_result(entities: Vec<PresidioEntity>) -> PresidioResult {
        let mut max_tier: SensitivityTier = 0;
        for e in &entities {
            let tier = tier_for_entity(&e.entity_type);
            if tier > max_tier {
                max_tier = tier;
            }
        }
        PresidioResult {
            has_pii: !entities.is_empty(),
            max_tier,
            entities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(t: &str, start: usize, end: usize) -> PresidioEntity {
        PresidioEntity {
            entity_type: t.to_string(),
            start,
            end,
            score: 0.9,
        }
    }

    #[test]
    fn mapping_entity_tier_da_json_statico() {
        // Risposta JSON statica (nessuna rete).
        let json = r#"[
            {"entity_type": "PERSON", "start": 0, "end": 5, "score": 0.99},
            {"entity_type": "IT_FISCAL_CODE", "start": 10, "end": 26, "score": 0.95}
        ]"#;
        let entities: Vec<PresidioEntity> = serde_json::from_str(json).expect("json valido");
        let r = PresidioClient::build_result(entities);
        assert!(r.has_pii);
        // CF e' tier 3 -> max_tier 3.
        assert_eq!(r.max_tier, 3);
        assert_eq!(r.entities.len(), 2);
    }

    #[test]
    fn entity_sconosciuta_ricade_su_tier1() {
        let r = PresidioClient::build_result(vec![entity("QUALCOSA_DI_IGNOTO", 0, 3)]);
        assert!(r.has_pii);
        assert_eq!(r.max_tier, 1);
    }

    #[test]
    fn nessuna_entita_nessuna_pii() {
        let r = PresidioClient::build_result(vec![]);
        assert!(!r.has_pii);
        assert_eq!(r.max_tier, 0);
    }

    #[test]
    fn tier_per_entita_note() {
        assert_eq!(tier_for_entity("CREDIT_CARD"), 3);
        assert_eq!(tier_for_entity("EMAIL_ADDRESS"), 2);
        assert_eq!(tier_for_entity("LOCATION"), 1);
        assert_eq!(tier_for_entity("IBAN_CODE"), 3);
    }

    #[tokio::test]
    async fn fallback_graceful_se_non_configurato() {
        // Senza refresh dal DB la config e' "non configurata" -> analyze ritorna
        // risultato vuoto senza rete e senza bloccare.
        let client = PresidioClient::new();
        let r = client.analyze("Mario Rossi vive a Milano").await;
        assert!(!r.has_pii);
        assert_eq!(r.max_tier, 0);
        assert!(r.entities.is_empty());
    }

    #[tokio::test]
    async fn fallback_graceful_se_url_irraggiungibile() {
        // URL valido ma servizio inesistente (porta chiusa) -> fallback no PII.
        let client = PresidioClient::new();
        // Inietta una config che punta a una porta sicuramente chiusa.
        client.config.insert(
            (),
            PresidioConfig {
                base_url: Some("http://127.0.0.1:1".to_string()),
                language: "it".to_string(),
                timeout: Duration::from_millis(200),
            },
        );
        let r = client.analyze("testo qualunque").await;
        assert!(!r.has_pii);
        assert_eq!(r.max_tier, 0);
    }
}
