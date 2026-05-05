//! SessionPersistenceWorker — salva lo stato di sessione nel namespace
//!
//! Worker periodico che:
//! 1. Legge tutte le entry correnti nel namespace
//! 2. Costruisce un snapshot compatto della sessione (conteggi, autori, chiavi attive)
//! 3. Salva il snapshot come `session:state` nel namespace (con TTL 10 min)
//!
//! In produzione questo snapshot può essere usato per:
//! - Ripristino della sessione dopo crash di un agente
//! - Debug post-mortem (cosa era nel namespace quando il task è fallito)
//! - Audit trail delle sessioni
//!
//! Il worker NON persiste su DB — è responsabilità del ReplicationWorker
//! sincronizzare lo snapshot su PostgreSQL se necessario.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Snapshot dello stato di sessione
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub taken_at: String,
    pub namespace_id: String,
    pub total_entries: usize,
    pub entries_by_author: HashMap<String, usize>,
    pub active_keys: Vec<String>,
    pub swarm_tasks_total: usize,
    pub swarm_tasks_success: usize,
}

pub struct SessionPersistenceWorker {
    interval: Duration,
    /// Max chiavi da includere nello snapshot (le prime N ordinate)
    max_keys_in_snapshot: usize,
}

impl Default for SessionPersistenceWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300), // ogni 5 minuti
            max_keys_in_snapshot: 50,
        }
    }
}

impl SessionPersistenceWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_max_keys(mut self, max: usize) -> Self {
        self.max_keys_in_snapshot = max;
        self
    }
}

#[async_trait]
impl LearningWorker for SessionPersistenceWorker {
    fn name(&self) -> &str {
        "session_persistence"
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
                // Senza namespace non c'è niente da persistere — OK silenzioso
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("entries_saved", 0.0);
            }
        };

        // Raccoglie tutte le entry correnti
        let all_keys = ns.keys();
        let total_entries = all_keys.len();

        // Conta per autore
        let mut entries_by_author: HashMap<String, usize> = HashMap::new();
        for key in &all_keys {
            if let Some(entry) = ns.get(key) {
                *entries_by_author.entry(entry.author.clone()).or_insert(0) += 1;
            }
        }

        // Chiavi attive (prime N)
        let active_keys: Vec<String> = all_keys
            .iter()
            .take(self.max_keys_in_snapshot)
            .cloned()
            .collect();

        // Statistiche swarm (se presenti)
        let (swarm_tasks_total, swarm_tasks_success) =
            if let Some(swarm) = &context.swarm_result {
                (swarm.task_results.len(), swarm.success_count as usize)
            } else {
                (0, 0)
            };

        let snapshot = SessionSnapshot {
            taken_at: chrono::Utc::now().to_rfc3339(),
            namespace_id: ns.name().to_string(),
            total_entries,
            entries_by_author,
            active_keys,
            swarm_tasks_total,
            swarm_tasks_success,
        };

        let value = serde_json::to_value(&snapshot).unwrap_or(serde_json::Value::Null);
        ns.set_with_ttl(
            "session:state",
            value,
            self.name(),
            Duration::from_secs(600), // TTL 10 minuti
        );

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("entries_saved", total_entries as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MemoryNamespace;
    use crate::swarm_types::{SwarmExecutionResult, SwarmTaskOutcome};
    use crate::types::{RoutingDecision, SelectionStrategy};
    use crate::{AgentType, TaskResult};
    use std::sync::Arc;

    fn make_swarm() -> Arc<SwarmExecutionResult> {
        Arc::new(SwarmExecutionResult {
            swarm_id: "s1".to_string(),
            task_results: vec![SwarmTaskOutcome {
                task_id: "t1".to_string(),
                routing: RoutingDecision {
                    agent_type: AgentType::Coder,
                    q_value: 0.5,
                    confidence: 0.8,
                    candidates: vec![],
                    decision_time_us: 5,
                    strategy: SelectionStrategy::Exploitation,
                },
                result: Ok(TaskResult {
                    task_id: "t1".to_string(),
                    agent_type: AgentType::Coder,
                    success: true,
                    output: "done".to_string(),
                    error: None,
                    execution_time_ms: 100,
                    tokens_used: 50,
                }),
            }],
            success_count: 1,
            failure_count: 0,
            total_time_ms: 100,
        })
    }

    #[tokio::test]
    async fn test_session_snapshot_written() {
        let ns = Arc::new(MemoryNamespace::new("test-sess"));
        ns.set("k1", serde_json::json!({"x": 1}), "agent_a");
        ns.set("k2", serde_json::json!({"y": 2}), "agent_b");
        ns.set("k3", serde_json::json!({"z": 3}), "agent_a");

        let worker = SessionPersistenceWorker::new();
        let ctx = LearningContext::new()
            .with_namespace(ns.clone())
            .with_swarm(make_swarm());

        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_saved"), Some(&3.0));

        let entry = ns.get("session:state").expect("snapshot should be written");
        let snap: SessionSnapshot =
            serde_json::from_value(entry.value).expect("valid snapshot JSON");
        assert_eq!(snap.total_entries, 3);
        assert_eq!(snap.swarm_tasks_total, 1);
        assert_eq!(snap.swarm_tasks_success, 1);
        assert!(snap.entries_by_author.contains_key("agent_a"));
    }

    #[tokio::test]
    async fn test_session_persistence_no_namespace() {
        let worker = SessionPersistenceWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("entries_saved"), Some(&0.0));
    }
}
