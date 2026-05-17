//! Classifier LLM asincrono per eventi non coperti dalle regole hardcoded.
//!
//! Strategia in 2 livelli:
//! 1. `classifier::Classifier::classify` (sincrono, regole) copre il 90% noto
//! 2. Per `ProjectEvent::Custom` o varianti per cui le regole ritornano None,
//!    questo modulo prova un LLM (Anthropic Haiku) con cache Redis (TTL 1h).
//!
//! L'integrazione concreta del client LLM vive in `mcp-core` (dipende
//! da orchestrator). Qui esponiamo solo il trait `LlmEnricher` e uno
//! stub `NoOpEnricher` per i casi in cui il LLM non e' configurato.
//!
//! Cache key: `dispatcher:hint:{kind}:{sha256(payload[..256])}`. Cosi'
//! eventi identici (es. mille FileChanged sullo stesso path) consumano
//! una sola chiamata LLM.

use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::event::{ProjectEvent, UiHint};

/// TTL della cache Redis (1 ora). Bilanciamento tra freschezza e costi LLM.
pub const CACHE_TTL_SECS: u64 = 3600;

/// Risultato dell'arricchimento (campi aggiuntivi rispetto a `UiHint`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnrichmentDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_hint: Option<UiHint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub semantic_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_inferred: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panel_target: Option<String>,
}

impl EnrichmentDelta {
    /// True se non c'e' nulla di utile da emettere come `EventEnriched`.
    pub fn is_empty(&self) -> bool {
        self.ui_hint.is_none()
            && self.semantic_tags.is_empty()
            && self.severity_inferred.is_none()
            && self.panel_target.is_none()
    }
}

/// Trait implementato dai client LLM concreti (Anthropic, OpenAI, ecc.).
/// Implementatori in `mcp-core` lo usano per delegare la chiamata all'LLM
/// senza accoppiare `nexus-events` all'orchestrator.
#[async_trait]
pub trait LlmEnricher: Send + Sync {
    /// Classifica un evento e ritorna eventuali metadati AI.
    /// Deve essere veloce (timeout consigliato 800ms) e non bloccare.
    /// Ritorna `None` se l'LLM e' down o se non ha hint utili da emettere.
    async fn classify(&self, event: &ProjectEvent) -> Option<EnrichmentDelta>;
}

/// Stub che non chiama mai LLM. Default in test e quando l'LLM non e'
/// configurato. L'enricher loop usa questo se nessun altro client e'
/// registrato — il sistema resta funzionante senza arricchimento.
pub struct NoOpEnricher;

#[async_trait]
impl LlmEnricher for NoOpEnricher {
    async fn classify(&self, _event: &ProjectEvent) -> Option<EnrichmentDelta> {
        None
    }
}

/// Wrapper di un `LlmEnricher` con cache Redis. Riduce chiamate LLM
/// duplicate per eventi identici nella stessa ora. Cache miss → chiama
/// l'inner e salva il risultato.
pub struct CachedLlmEnricher<E: LlmEnricher> {
    inner: E,
    redis: redis::aio::MultiplexedConnection,
}

impl<E: LlmEnricher> CachedLlmEnricher<E> {
    pub fn new(inner: E, redis: redis::aio::MultiplexedConnection) -> Self {
        Self { inner, redis }
    }
}

#[async_trait]
impl<E: LlmEnricher> LlmEnricher for CachedLlmEnricher<E> {
    async fn classify(&self, event: &ProjectEvent) -> Option<EnrichmentDelta> {
        let key = cache_key(event);
        let mut conn = self.redis.clone();

        // Hit?
        if let Ok(Some(s)) = conn.get::<_, Option<String>>(&key).await {
            if s == NEGATIVE_CACHE_MARKER {
                return None;
            }
            if let Ok(d) = serde_json::from_str::<EnrichmentDelta>(&s) {
                return Some(d);
            }
        }

        // Miss → chiama inner
        let result = self.inner.classify(event).await;

        // Salva (anche negativo come "no hint" per non rifare la stessa call)
        match &result {
            Some(d) => {
                if let Ok(s) = serde_json::to_string(d) {
                    let _: Result<(), _> = conn.set_ex(&key, s, CACHE_TTL_SECS).await;
                }
            }
            None => {
                let _: Result<(), _> = conn.set_ex(&key, NEGATIVE_CACHE_MARKER, CACHE_TTL_SECS).await;
            }
        }

        result
    }
}

/// Marker speciale per indicare "ho gia' chiesto al LLM e non ha dato hint".
/// Evita di ripetere la chiamata per eventi che il LLM dice di ignorare.
const NEGATIVE_CACHE_MARKER: &str = "__no_hint__";

/// Cache key per un evento: `dispatcher:hint:{kind}:{sha256[..16]}`.
/// L'hash include kind+payload serializzato (truncato a 256 byte per
/// evitare cache esplosa su payload grandi).
fn cache_key(event: &ProjectEvent) -> String {
    let kind = event.kind_name();
    let serialized = serde_json::to_string(event).unwrap_or_default();
    let truncated = if serialized.len() > 256 {
        &serialized[..256]
    } else {
        &serialized
    };
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(truncated.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("dispatcher:hint:{}:{}", kind, &hex[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn cache_key_is_stable_per_event() {
        let ev = ProjectEvent::PortReleased { port: 3000 };
        let k1 = cache_key(&ev);
        let k2 = cache_key(&ev);
        assert_eq!(k1, k2);
        assert!(k1.starts_with("dispatcher:hint:PortReleased:"));
    }

    #[test]
    fn cache_key_differs_per_payload() {
        let a = ProjectEvent::PortReleased { port: 3000 };
        let b = ProjectEvent::PortReleased { port: 8080 };
        assert_ne!(cache_key(&a), cache_key(&b));
    }

    #[test]
    fn enrichment_delta_is_empty_when_default() {
        assert!(EnrichmentDelta::default().is_empty());
        let d = EnrichmentDelta {
            semantic_tags: vec!["x".into()],
            ..Default::default()
        };
        assert!(!d.is_empty());
    }

    #[tokio::test]
    async fn noop_enricher_returns_none() {
        let e = NoOpEnricher;
        let ev = ProjectEvent::Custom {
            event_name: "x".into(),
            resource: "y".into(),
            payload: serde_json::Value::Null,
        };
        assert!(e.classify(&ev).await.is_none());
    }

    #[test]
    fn enrichment_delta_serializes_correctly() {
        let d = EnrichmentDelta {
            ui_hint: Some(UiHint {
                toast_severity: Some("info".into()),
                ..Default::default()
            }),
            semantic_tags: vec!["build".into(), "fast".into()],
            severity_inferred: Some("warning".into()),
            panel_target: Some("output".into()),
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"semantic_tags\""));
        assert!(s.contains("\"build\""));

        let back: EnrichmentDelta = serde_json::from_str(&s).unwrap();
        assert_eq!(back.semantic_tags.len(), 2);
        assert_eq!(back.severity_inferred.as_deref(), Some("warning"));
    }

    #[test]
    fn unused_uuid_import_silenced() {
        // Verifica che `Uuid` resti utilizzabile per scenari futuri di test
        // (es. correlazione event_id ↔ cache key). Evita warning unused.
        let _ = Uuid::new_v4();
    }
}
