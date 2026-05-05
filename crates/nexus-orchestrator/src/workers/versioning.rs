//! VersioningWorker — snapshot versioni dei pattern e Q-values
//!
//! Worker reattivo (OnTaskComplete) che dopo ogni swarm execution:
//! 1. Conta quante entry `pattern:*` esistono nel namespace
//! 2. Legge la dimensione corrente della Q-table dal router
//! 3. Salva un record di versione `version:{timestamp}` nel namespace
//!
//! Questo fornisce una traccia storica dell'evoluzione del sistema:
//! - Quanti pattern sono stati appresi nel tempo
//! - Come è cresciuta la Q-table
//! - Quando si sono verificati cambiamenti significativi
//!
//! Le entry di versione hanno TTL di 24 ore per non accumulare indefinitamente.
//!
//! In produzione, queste versioni possono essere esportate su PostgreSQL
//! per analisi trend e debugging.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Record di versione salvato nel namespace
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionRecord {
    /// Versione progressiva (numero intero)
    pub version_num: u64,
    /// Timestamp UTC
    pub taken_at: String,
    /// Numero di pattern nel namespace
    pub pattern_count: usize,
    /// Dimensione Q-table corrente
    pub q_table_size: usize,
    /// Task completati nello swarm corrente
    pub swarm_tasks: usize,
    /// Tasso di successo nel batch corrente
    pub batch_success_rate: f32,
}

pub struct VersioningWorker {
    /// TTL per i record di versione (default: 24h)
    version_ttl: Duration,
}

impl Default for VersioningWorker {
    fn default() -> Self {
        Self {
            version_ttl: Duration::from_secs(86400),
        }
    }
}

impl VersioningWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.version_ttl = ttl;
        self
    }
}

#[async_trait]
impl LearningWorker for VersioningWorker {
    fn name(&self) -> &str {
        "versioning"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::OnTaskComplete
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        let ns = match &context.namespace {
            Some(ns) => ns,
            None => {
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("version_saved", 0.0);
            }
        };

        // Conta i pattern nel namespace
        let pattern_count = ns
            .keys()
            .into_iter()
            .filter(|k| k.starts_with("pattern:"))
            .count();

        // Dimensione Q-table (se router presente)
        let q_table_size = context
            .router
            .as_ref()
            .map(|r| r.q_table_size())
            .unwrap_or(0);

        // Statistiche batch corrente
        let (swarm_tasks, batch_success_rate) = if let Some(swarm) = &context.swarm_result {
            let total = swarm.task_results.len();
            let success = swarm.success_count;
            let rate = if total > 0 {
                success as f32 / total as f32
            } else {
                0.0
            };
            (total, rate)
        } else {
            (0, 0.0)
        };

        // Calcola numero versione: conta quante entry version:* esistono + 1
        let version_num = ns
            .keys()
            .into_iter()
            .filter(|k| k.starts_with("version:"))
            .count() as u64
            + 1;

        let now_str = chrono::Utc::now().to_rfc3339();
        let record = VersionRecord {
            version_num,
            taken_at: now_str.clone(),
            pattern_count,
            q_table_size,
            swarm_tasks,
            batch_success_rate,
        };

        // Chiave univoca per questo record (ISO timestamp senza caratteri speciali)
        let key = format!(
            "version:{}",
            now_str.replace([':', '.', '+', '-', 'T', 'Z'], "")
                .chars()
                .take(16)
                .collect::<String>()
        );

        let value = serde_json::to_value(&record).unwrap_or(serde_json::Value::Null);
        ns.set_with_ttl(&key, value, self.name(), self.version_ttl);

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("version_saved", 1.0)
            .with_metric("version_num", version_num as f32)
            .with_metric("pattern_count", pattern_count as f32)
            .with_metric("q_table_size", q_table_size as f32)
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

    fn make_swarm(success_count: usize, total: usize) -> Arc<SwarmExecutionResult> {
        let task_results: Vec<SwarmTaskOutcome> = (0..total)
            .map(|i| SwarmTaskOutcome {
                task_id: format!("t{i}"),
                routing: RoutingDecision {
                    agent_type: AgentType::Coder,
                    q_value: 0.5,
                    confidence: 0.8,
                    candidates: vec![],
                    decision_time_us: 5,
                    strategy: SelectionStrategy::Exploitation,
                },
                result: Ok(TaskResult {
                    task_id: format!("t{i}"),
                    agent_type: AgentType::Coder,
                    success: i < success_count,
                    output: "".to_string(),
                    error: None,
                    execution_time_ms: 100,
                    tokens_used: 10,
                }),
            })
            .collect();

        Arc::new(SwarmExecutionResult {
            swarm_id: "s1".to_string(),
            task_results,
            success_count,
            failure_count: total - success_count,
            total_time_ms: 100 * total as u64,
        })
    }

    #[tokio::test]
    async fn test_versioning_saves_record() {
        let ns = Arc::new(MemoryNamespace::new("ver-test"));
        // Aggiungi alcuni pattern
        ns.set("pattern:p1", serde_json::json!({"x": 1}), "ultralearn");
        ns.set("pattern:p2", serde_json::json!({"x": 2}), "ultralearn");

        let worker = VersioningWorker::new();
        let ctx = LearningContext::new()
            .with_namespace(ns.clone())
            .with_swarm(make_swarm(3, 4));

        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("version_saved"), Some(&1.0));
        assert_eq!(outcome.metrics.get("version_num"), Some(&1.0));
        assert_eq!(outcome.metrics.get("pattern_count"), Some(&2.0));

        // Verifica che la chiave version: sia stata scritta
        let version_keys: Vec<_> = ns
            .keys()
            .into_iter()
            .filter(|k| k.starts_with("version:"))
            .collect();
        assert_eq!(version_keys.len(), 1);

        let entry = ns.get(&version_keys[0]).unwrap();
        let record: VersionRecord = serde_json::from_value(entry.value).unwrap();
        assert_eq!(record.pattern_count, 2);
        assert_eq!(record.swarm_tasks, 4);
        assert!((record.batch_success_rate - 0.75).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_versioning_increments() {
        let ns = Arc::new(MemoryNamespace::new("ver-incr"));
        let worker = VersioningWorker::new();

        // Prima versione
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let o1 = worker.run(&ctx).await;
        assert_eq!(o1.metrics.get("version_num"), Some(&1.0));

        // Seconda versione
        let ctx2 = LearningContext::new().with_namespace(ns.clone());
        let o2 = worker.run(&ctx2).await;
        assert_eq!(o2.metrics.get("version_num"), Some(&2.0));
    }

    #[tokio::test]
    async fn test_versioning_no_namespace() {
        let worker = VersioningWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success); // graceful
    }
}
