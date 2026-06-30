//! QLearningReplayWorker — replay esperienze passate per reinforcement learning
//!
//! Worker periodico che implementa *experience replay*:
//! 1. Legge le entry `pattern:*` dal namespace (prodotte da UltralearnWorker)
//! 2. Converte ogni pattern in un `ExecutionOutcome`
//! 3. Chiama `router.update_q_value()` per ciascuno
//!
//! Il replay batch migliora la stabilità del Q-Learning riducendo la correlazione
//! temporale tra i campioni (tecnica standard in DQN).
//!
//! Il worker è periodico per evitare over-fitting su pattern recenti e per
//! consolidare l'apprendimento durante i periodi di idle.
//!
//! ## Configurazione
//!
//! `max_replay_per_tick` — quanti pattern processare per tick (default: 20).
//! Un valore alto aumenta la velocità di apprendimento ma consuma più CPU.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use crate::types::ExecutionOutcome;
use async_trait::async_trait;
use crate::AgentType;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Schema atteso delle entry `pattern:*` nel namespace (prodotte da UltralearnWorker)
#[derive(Debug, Deserialize, Serialize)]
struct PatternEntry {
    pub agent_type: String,
    pub task_id: String,
    pub success: bool,
    pub quality_score: f32,
    pub execution_time_ms: u64,
}

pub struct QLearningReplayWorker {
    interval: Duration,
    max_replay_per_tick: usize,
}

impl Default for QLearningReplayWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(120), // ogni 2 minuti
            max_replay_per_tick: 20,
        }
    }
}

impl QLearningReplayWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_max_replay(mut self, max: usize) -> Self {
        self.max_replay_per_tick = max;
        self
    }
}

#[async_trait]
impl LearningWorker for QLearningReplayWorker {
    fn name(&self) -> &str {
        "q_learning_replay"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Periodic
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        let router = match &context.router {
            Some(r) => r,
            None => {
                // Senza router non possiamo aggiornare Q-values
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("replays_done", 0.0);
            }
        };

        let ns = match &context.namespace {
            Some(ns) => ns,
            None => {
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("replays_done", 0.0);
            }
        };

        // Recupera le chiavi pattern:* dal namespace
        let pattern_keys: Vec<String> = ns
            .keys()
            .into_iter()
            .filter(|k| k.starts_with("pattern:"))
            .take(self.max_replay_per_tick)
            .collect();

        if pattern_keys.is_empty() {
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                .with_metric("replays_done", 0.0);
        }

        let mut replays_done = 0u32;
        let mut replays_skipped = 0u32;

        for key in &pattern_keys {
            let entry = match ns.get(key) {
                Some(e) => e,
                None => continue,
            };

            let pattern: PatternEntry = match serde_json::from_value(entry.value.clone()) {
                Ok(p) => p,
                Err(_) => {
                    replays_skipped += 1;
                    continue;
                }
            };

            // Risolve l'AgentType dal nome stringa
            let agent_type = AgentType::from_name(&pattern.agent_type);

            // Costruisce un ExecutionOutcome per il Q-update
            let outcome = ExecutionOutcome {
                task_id: pattern.task_id.clone(),
                task_type: pattern.agent_type.to_lowercase(), // euristico: usa nome agente come task_type
                agent_type,
                success: pattern.success,
                quality_score: pattern.quality_score,
                execution_time_ms: pattern.execution_time_ms,
                error: None,
            };

            router.update_q_value(&outcome);
            replays_done += 1;
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("replays_done", replays_done as f32)
            .with_metric("replays_skipped", replays_skipped as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::HashEmbedder;
    use crate::namespace::MemoryNamespace;
    use crate::q_learning::QLearningRouter;
    use crate::types::QLearningConfig;
    use std::sync::Arc;

    fn make_router() -> Arc<QLearningRouter> {
        let config = QLearningConfig::default();
        let embedder = Arc::new(HashEmbedder::new(128));
        Arc::new(QLearningRouter::new(config, embedder))
    }

    fn make_pattern_entry() -> serde_json::Value {
        serde_json::json!({
            "id": "p1",
            "agent_type": "Coder",
            "task_id": "t1",
            "success": true,
            "quality_score": 0.9,
            "execution_time_ms": 120
        })
    }

    #[tokio::test]
    async fn test_replay_updates_q_values() {
        let router = make_router();
        let ns = Arc::new(MemoryNamespace::new("replay-test"));
        ns.set("pattern:p1", make_pattern_entry(), "ultralearn");
        ns.set("pattern:p2", make_pattern_entry(), "ultralearn");

        let worker = QLearningReplayWorker::new();
        let ctx = LearningContext::new()
            .with_router(router.clone())
            .with_namespace(ns.clone());

        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("replays_done"), Some(&2.0));
        assert_eq!(outcome.metrics.get("replays_skipped"), Some(&0.0));
    }

    #[tokio::test]
    async fn test_replay_no_patterns() {
        let router = make_router();
        let ns = Arc::new(MemoryNamespace::new("empty-ns"));

        let worker = QLearningReplayWorker::new();
        let ctx = LearningContext::new()
            .with_router(router)
            .with_namespace(ns);

        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("replays_done"), Some(&0.0));
    }

    #[tokio::test]
    async fn test_replay_no_router() {
        let ns = Arc::new(MemoryNamespace::new("no-router"));
        ns.set("pattern:p1", make_pattern_entry(), "ultralearn");

        let worker = QLearningReplayWorker::new();
        let ctx = LearningContext::new().with_namespace(ns);

        let outcome = worker.run(&ctx).await;
        assert!(outcome.success); // graceful degradation
        assert_eq!(outcome.metrics.get("replays_done"), Some(&0.0));
    }
}
