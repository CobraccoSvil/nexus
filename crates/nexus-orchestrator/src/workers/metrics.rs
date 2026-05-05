//! MetricsAggregationWorker — calcola metriche aggregate dello swarm
//!
//! Raccoglie statistiche su:
//! - total tasks
//! - success rate
//! - average execution time
//! - tasks per agent type
//!
//! Pubblica il summary nel namespace come `metrics:latest`.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Instant;

pub struct MetricsAggregationWorker;

impl Default for MetricsAggregationWorker {
    fn default() -> Self {
        Self
    }
}

impl MetricsAggregationWorker {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LearningWorker for MetricsAggregationWorker {
    fn name(&self) -> &str {
        "metrics_aggregation"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Both
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();
        let outcomes = context.task_outcomes();

        let total = outcomes.len();
        let mut success = 0;
        let mut total_time_ms = 0u64;
        let mut per_agent: HashMap<String, u32> = HashMap::new();

        for outcome in outcomes {
            if let Ok(result) = &outcome.result {
                if result.success {
                    success += 1;
                }
                total_time_ms += result.execution_time_ms;
                *per_agent
                    .entry(result.agent_type.name().to_string())
                    .or_insert(0) += 1;
            }
        }

        let success_rate = if total > 0 {
            (success as f32) / (total as f32)
        } else {
            0.0
        };
        let avg_time = if total > 0 {
            (total_time_ms as f32) / (total as f32)
        } else {
            0.0
        };

        if let Some(ns) = &context.namespace {
            let summary = serde_json::json!({
                "total": total,
                "success": success,
                "success_rate": success_rate,
                "avg_time_ms": avg_time,
                "per_agent": per_agent,
                "ts": chrono::Utc::now().to_rfc3339(),
            });
            ns.set("metrics:latest", summary, "metrics_aggregation");
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("total_tasks", total as f32)
            .with_metric("success_rate", success_rate)
            .with_metric("avg_time_ms", avg_time)
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

    fn outcome(success: bool, time: u64, agent: AgentType) -> SwarmTaskOutcome {
        let task_result = TaskResult {
            task_id: "t".to_string(),
            agent_type: agent.clone(),
            success,
            output: "".to_string(),
            error: None,
            execution_time_ms: time,
            tokens_used: 0,
        };
        SwarmTaskOutcome {
            task_id: "t".to_string(),
            routing: RoutingDecision {
                agent_type: agent,
                q_value: 0.5,
                confidence: 0.8,
                candidates: Vec::new(),
                decision_time_us: 0,
                strategy: SelectionStrategy::Exploitation,
            },
            result: Ok(task_result),
        }
    }

    #[tokio::test]
    async fn test_metrics_aggregation() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        let swarm = Arc::new(SwarmExecutionResult {
            swarm_id: "s".to_string(),
            task_results: vec![
                outcome(true, 100, AgentType::Coder),
                outcome(true, 200, AgentType::Tester),
                outcome(false, 50, AgentType::Coder),
            ],
            success_count: 2,
            failure_count: 1,
            total_time_ms: 350,
        });

        let worker = MetricsAggregationWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone()).with_swarm(swarm);
        let wo = worker.run(&ctx).await;

        assert!(wo.success);
        assert_eq!(wo.metrics.get("total_tasks"), Some(&3.0));
        assert!((wo.metrics.get("success_rate").unwrap() - 2.0 / 3.0).abs() < 0.01);

        let metrics = ns.get("metrics:latest").unwrap();
        assert_eq!(metrics.value["total"], 3);
        assert_eq!(metrics.value["per_agent"]["Coder"], 2);
    }
}
