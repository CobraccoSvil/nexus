//! CleanupWorker — rimuove entry scadute dal namespace
//!
//! Worker periodico che fa `evict_expired()` sul namespace attivo.
//! Semplice ma essenziale per evitare memory leak nel lungo termine.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use std::time::{Duration, Instant};

pub struct CleanupWorker {
    interval: Duration,
}

impl Default for CleanupWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(60),
        }
    }
}

impl CleanupWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

#[async_trait]
impl LearningWorker for CleanupWorker {
    fn name(&self) -> &str {
        "cleanup"
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
                    .with_metric("evicted", 0.0);
            }
        };

        let evicted = ns.evict_expired();
        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("evicted", evicted as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MemoryNamespace;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_cleanup_removes_expired() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        ns.set_with_ttl(
            "k1",
            serde_json::json!(1),
            "a",
            Duration::from_millis(10),
        );
        ns.set("k2", serde_json::json!(2), "a");

        tokio::time::sleep(Duration::from_millis(30)).await;

        let worker = CleanupWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("evicted"), Some(&1.0));
        assert!(ns.get("k2").is_some());
    }

    #[tokio::test]
    async fn test_cleanup_without_namespace_ok() {
        let worker = CleanupWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
    }
}
