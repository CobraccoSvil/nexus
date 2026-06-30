//! AnomalyDetectionWorker — rileva comportamenti insoliti degli agenti
//!
//! Heuristics semplici:
//! - Esecuzioni troppo lunghe (outlier su execution_time_ms)
//! - Tasso di fallimento elevato in un singolo swarm
//! - Agenti che falliscono ripetutamente
//!
//! Pubblica anomalie rilevate come `anomaly:{id}` nel namespace.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use std::time::Instant;
use uuid::Uuid;

pub struct AnomalyDetectionWorker {
    /// Soglia di tempo di esecuzione (ms) oltre cui un task è "slow"
    pub slow_threshold_ms: u64,
    /// Soglia di failure rate oltre cui uno swarm è "degraded"
    pub degraded_failure_rate: f32,
}

impl Default for AnomalyDetectionWorker {
    fn default() -> Self {
        Self {
            slow_threshold_ms: 30_000, // 30s
            degraded_failure_rate: 0.5,
        }
    }
}

impl AnomalyDetectionWorker {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LearningWorker for AnomalyDetectionWorker {
    fn name(&self) -> &str {
        "anomaly_detection"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::OnTaskComplete
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();
        let outcomes = context.task_outcomes();

        let mut slow_count = 0u32;
        let mut anomalies_published = 0u32;

        for outcome in outcomes {
            if let Ok(result) = &outcome.result {
                if result.execution_time_ms > self.slow_threshold_ms {
                    slow_count += 1;
                    if let Some(ns) = &context.namespace {
                        let anomaly = serde_json::json!({
                            "type": "slow_execution",
                            "task_id": result.task_id,
                            "agent": result.agent_type.name(),
                            "duration_ms": result.execution_time_ms,
                            "threshold_ms": self.slow_threshold_ms,
                        });
                        ns.set(
                            format!("anomaly:{}", Uuid::new_v4()),
                            anomaly,
                            "anomaly_detection",
                        );
                        anomalies_published += 1;
                    }
                }
            }
        }

        // Check aggregate failure rate
        if let Some(swarm) = &context.swarm_result {
            let total = swarm.success_count + swarm.failure_count;
            if total > 0 {
                let failure_rate = swarm.failure_count as f32 / total as f32;
                if failure_rate >= self.degraded_failure_rate {
                    if let Some(ns) = &context.namespace {
                        let anomaly = serde_json::json!({
                            "type": "degraded_swarm",
                            "swarm_id": swarm.swarm_id,
                            "failure_rate": failure_rate,
                            "threshold": self.degraded_failure_rate,
                        });
                        ns.set(
                            format!("anomaly:{}", Uuid::new_v4()),
                            anomaly,
                            "anomaly_detection",
                        );
                        anomalies_published += 1;
                    }
                }
            }
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("slow_tasks", slow_count as f32)
            .with_metric("anomalies_published", anomalies_published as f32)
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

    #[tokio::test]
    async fn test_detects_slow_execution() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        let task_result = TaskResult {
            task_id: "slow".to_string(),
            agent_type: AgentType::Coder,
            success: true,
            output: "".to_string(),
            error: None,
            execution_time_ms: 60_000, // slow
            tokens_used: 0,
        };
        let swarm = Arc::new(SwarmExecutionResult {
            swarm_id: "s".to_string(),
            task_results: vec![SwarmTaskOutcome {
                task_id: "slow".to_string(),
                routing: RoutingDecision {
                    agent_type: AgentType::Coder,
                    q_value: 0.5,
                    confidence: 0.8,
                    candidates: Vec::new(),
                    decision_time_us: 0,
                    strategy: SelectionStrategy::Exploitation,
                },
                result: Ok(task_result),
            }],
            success_count: 1,
            failure_count: 0,
            total_time_ms: 60_000,
        });

        let worker = AnomalyDetectionWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone()).with_swarm(swarm);
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("slow_tasks"), Some(&1.0));
        let anomalies: Vec<String> = ns.keys().into_iter().filter(|k| k.starts_with("anomaly:")).collect();
        assert_eq!(anomalies.len(), 1);
    }

    #[tokio::test]
    async fn test_detects_degraded_swarm() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        let swarm = Arc::new(SwarmExecutionResult {
            swarm_id: "degraded".to_string(),
            task_results: vec![],
            success_count: 1,
            failure_count: 3,
            total_time_ms: 100,
        });

        let worker = AnomalyDetectionWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone()).with_swarm(swarm);
        worker.run(&ctx).await;

        let anomalies: Vec<String> = ns.keys().into_iter().filter(|k| k.starts_with("anomaly:")).collect();
        assert_eq!(anomalies.len(), 1);
    }
}
