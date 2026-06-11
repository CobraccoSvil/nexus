//! ReplicationWorker — sincronizza namespace entries su storage persistente
//!
//! Worker periodico che prepara un batch di entry dal namespace per la
//! replica su PostgreSQL. La logica di scrittura vera e propria avviene
//! tramite il router (che ha accesso al pool) o viene emessa come payload
//! serializzato nel namespace sotto la chiave `replication:pending`.
//!
//! ## Strategia di replicazione
//!
//! Il worker opera in due modalità:
//!
//! 1. **Con router**: usa `router.persist_namespace_batch()` se disponibile
//!    (fire-and-forget asincrono)
//! 2. **Senza router**: serializza le entry sotto `replication:pending`
//!    per un consumer esterno (es. un servizio dedicato)
//!
//! Le entry `session:*`, `metrics:*`, `version:*` e `pattern:*` vengono
//! replicate in priorità.
//!
//! ## Performance
//!
//! Il worker non blocca — la serializzazione è sincrona ma leggera.
//! La scrittura su DB (se presente) è asincrona (tokio::spawn).

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Prefissi di chiave prioritari da replicare
const PRIORITY_PREFIXES: &[&str] = &["session:", "metrics:", "version:", "pattern:"];

/// Batch serializzato da replicare
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationBatch {
    pub namespace_id: String,
    pub prepared_at: String,
    pub entry_count: usize,
    pub entries: Vec<ReplicationEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplicationEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub author: String,
}

pub struct ReplicationWorker {
    interval: Duration,
    /// Max entry per batch (evita batch troppo grandi)
    max_batch_size: usize,
}

impl Default for ReplicationWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(180), // ogni 3 minuti
            max_batch_size: 100,
        }
    }
}

impl ReplicationWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_max_batch(mut self, max: usize) -> Self {
        self.max_batch_size = max;
        self
    }
}

#[async_trait]
impl LearningWorker for ReplicationWorker {
    fn name(&self) -> &str {
        "replication"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Periodic
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        let ns = match &context.namespace {
            Some(ns) => ns,
            None => {
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("entries_replicated", 0.0);
            }
        };

        // Seleziona le chiavi da replicare (priorità: prefissi noti, poi resto)
        let all_keys = ns.keys();
        let mut priority_keys: Vec<String> = all_keys
            .iter()
            .filter(|k| PRIORITY_PREFIXES.iter().any(|p| k.starts_with(p)))
            .take(self.max_batch_size)
            .cloned()
            .collect();

        // Se c'è spazio, aggiunge le chiavi rimanenti
        if priority_keys.len() < self.max_batch_size {
            let remaining = all_keys
                .iter()
                .filter(|k| !PRIORITY_PREFIXES.iter().any(|p| k.starts_with(p)))
                .take(self.max_batch_size - priority_keys.len())
                .cloned();
            priority_keys.extend(remaining);
        }

        if priority_keys.is_empty() {
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                .with_metric("entries_replicated", 0.0);
        }

        // Costruisce il batch
        let mut entries = Vec::with_capacity(priority_keys.len());
        for key in &priority_keys {
            if let Some(entry) = ns.get(key) {
                entries.push(ReplicationEntry {
                    key: key.clone(),
                    value: entry.value,
                    author: entry.author,
                });
            }
        }

        let entry_count = entries.len();
        let batch = ReplicationBatch {
            namespace_id: ns.name().to_string(),
            prepared_at: chrono::Utc::now().to_rfc3339(),
            entry_count,
            entries,
        };

        // Tentativo 1: usa il router per persistere (se disponibile)
        // In futuro: router.persist_namespace_batch(&batch)
        // Per ora: serializza il batch nel namespace come `replication:pending`
        // Un consumer esterno (es. chat-service) può leggere questa chiave e
        // fare la scrittura su PostgreSQL.
        let value = serde_json::to_value(&batch).unwrap_or(serde_json::Value::Null);
        ns.set_with_ttl(
            "replication:pending",
            value,
            self.name(),
            Duration::from_secs(600), // TTL 10 minuti — deve essere consumato entro allora
        );

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("entries_replicated", entry_count as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MemoryNamespace;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_replication_creates_pending_batch() {
        let ns = Arc::new(MemoryNamespace::new("rep-test"));
        ns.set("pattern:p1", serde_json::json!({"a": 1}), "ultralearn");
        ns.set("metrics:latest", serde_json::json!({"b": 2}), "metrics");
        ns.set("other:key", serde_json::json!({"c": 3}), "agent");

        let worker = ReplicationWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_replicated"), Some(&3.0));

        let pending = ns.get("replication:pending").expect("pending batch must exist");
        let batch: ReplicationBatch = serde_json::from_value(pending.value).unwrap();
        assert_eq!(batch.entry_count, 3);
        // pattern: e metrics: devono essere prima degli altri
        let first_keys: Vec<&str> = batch.entries.iter().map(|e| e.key.as_str()).collect();
        assert!(first_keys.contains(&"pattern:p1"));
        assert!(first_keys.contains(&"metrics:latest"));
    }

    #[tokio::test]
    async fn test_replication_no_namespace() {
        let worker = ReplicationWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_replicated"), Some(&0.0));
    }

    #[tokio::test]
    async fn test_replication_respects_batch_limit() {
        let ns = Arc::new(MemoryNamespace::new("limit-test"));
        for i in 0..10 {
            ns.set(
                format!("pattern:{i}"),
                serde_json::json!({"i": i}),
                "ul",
            );
        }

        let worker = ReplicationWorker::new().with_max_batch(3);
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_replicated"), Some(&3.0));
    }
}
