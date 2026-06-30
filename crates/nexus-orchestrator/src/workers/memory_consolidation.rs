//! MemoryConsolidationWorker — consolida memorie simili nel namespace
//!
//! Logica:
//! - Scorre tutte le entry `pattern:*` del namespace
//! - Raggruppa per (agent_type, success) e calcola quality_score medio
//! - Pubblica summary consolidati come entry `consolidated:{agent_type}`
//! - Rimuove i pattern singoli una volta consolidati (opzionale)

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct MemoryConsolidationWorker {
    interval: Duration,
    remove_after_consolidation: bool,
}

impl Default for MemoryConsolidationWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300), // 5 min
            remove_after_consolidation: false,
        }
    }
}

impl MemoryConsolidationWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn remove_patterns_after(mut self, remove: bool) -> Self {
        self.remove_after_consolidation = remove;
        self
    }
}

#[async_trait]
impl LearningWorker for MemoryConsolidationWorker {
    fn name(&self) -> &str {
        "memory_consolidation"
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
                return WorkerOutcome::fail(
                    self.name(),
                    "no namespace",
                    start.elapsed().as_millis() as u64,
                )
            }
        };

        let keys = ns.keys();
        let pattern_keys: Vec<String> = keys
            .into_iter()
            .filter(|k| k.starts_with("pattern:"))
            .collect();

        if pattern_keys.is_empty() {
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                .with_metric("patterns_consolidated", 0.0);
        }

        // Aggrega per agent_type
        let mut buckets: HashMap<String, (u32, f32, u32)> = HashMap::new(); // (count, quality_sum, success_count)
        for key in &pattern_keys {
            if let Some(entry) = ns.get(key) {
                let agent = entry.value["agent_type"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let quality = entry.value["quality_score"].as_f64().unwrap_or(0.0) as f32;
                let success = entry.value["success"].as_bool().unwrap_or(false);

                let bucket = buckets.entry(agent).or_insert((0, 0.0, 0));
                bucket.0 += 1;
                bucket.1 += quality;
                if success {
                    bucket.2 += 1;
                }
            }
        }

        // Pubblica consolidati
        let total_consolidated = pattern_keys.len();
        for (agent, (count, quality_sum, success_count)) in buckets.iter() {
            let avg_quality = if *count > 0 {
                quality_sum / (*count as f32)
            } else {
                0.0
            };
            let success_rate = if *count > 0 {
                (*success_count as f32) / (*count as f32)
            } else {
                0.0
            };

            let summary = serde_json::json!({
                "agent_type": agent,
                "sample_count": count,
                "avg_quality": avg_quality,
                "success_rate": success_rate,
                "consolidated_at": chrono::Utc::now().to_rfc3339(),
            });

            ns.set(format!("consolidated:{}", agent), summary, "memory_consolidation");
        }

        // Opzionalmente rimuovi i pattern singoli
        if self.remove_after_consolidation {
            for key in pattern_keys {
                ns.remove(&key);
            }
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("patterns_consolidated", total_consolidated as f32)
            .with_metric("buckets", buckets.len() as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MemoryNamespace;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_consolidation_aggregates_patterns() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        ns.set(
            "pattern:1",
            serde_json::json!({
                "agent_type": "Coder",
                "quality_score": 0.8,
                "success": true
            }),
            "test",
        );
        ns.set(
            "pattern:2",
            serde_json::json!({
                "agent_type": "Coder",
                "quality_score": 0.6,
                "success": true
            }),
            "test",
        );
        ns.set(
            "pattern:3",
            serde_json::json!({
                "agent_type": "Tester",
                "quality_score": 0.9,
                "success": true
            }),
            "test",
        );

        let worker = MemoryConsolidationWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("patterns_consolidated"), Some(&3.0));
        assert_eq!(outcome.metrics.get("buckets"), Some(&2.0));

        // Verifica consolidated entries
        assert!(ns.get("consolidated:Coder").is_some());
        assert!(ns.get("consolidated:Tester").is_some());
    }

    #[tokio::test]
    async fn test_consolidation_with_removal() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        ns.set(
            "pattern:1",
            serde_json::json!({"agent_type": "Coder", "quality_score": 0.7, "success": true}),
            "test",
        );

        let worker = MemoryConsolidationWorker::new().remove_patterns_after(true);
        let ctx = LearningContext::new().with_namespace(ns.clone());
        worker.run(&ctx).await;

        assert!(ns.get("pattern:1").is_none()); // rimosso
        assert!(ns.get("consolidated:Coder").is_some());
    }
}
