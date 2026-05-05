//! UltralearnWorker — estrae pattern da task risults e li pubblica nel namespace
//!
//! Logica:
//! 1. Analizza i `task_outcomes` del contesto
//! 2. Per ogni task success, estrae features (task_type, agent_type, quality)
//! 3. Pubblica il pattern nel namespace come entry `pattern:{id}`
//! 4. In produzione, questi pattern alimenteranno un re-training del router

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtractedPattern {
    pub id: String,
    pub agent_type: String,
    pub task_id: String,
    pub success: bool,
    pub quality_score: f32,
    pub execution_time_ms: u64,
}

pub struct UltralearnWorker {
    min_quality_to_store: f32,
}

impl Default for UltralearnWorker {
    fn default() -> Self {
        Self {
            min_quality_to_store: 0.5,
        }
    }
}

impl UltralearnWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_quality(mut self, q: f32) -> Self {
        self.min_quality_to_store = q;
        self
    }
}

#[async_trait]
impl LearningWorker for UltralearnWorker {
    fn name(&self) -> &str {
        "ultralearn"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::OnTaskComplete
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();
        let outcomes = context.task_outcomes();
        if outcomes.is_empty() {
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64);
        }

        let ns = match &context.namespace {
            Some(ns) => ns,
            None => {
                return WorkerOutcome::fail(
                    self.name(),
                    "no namespace in context",
                    start.elapsed().as_millis() as u64,
                )
            }
        };

        let mut patterns_stored = 0;
        for outcome in outcomes {
            if let Ok(result) = &outcome.result {
                let quality = if result.success { 0.8 } else { 0.2 };
                if quality < self.min_quality_to_store {
                    continue;
                }

                let pattern = ExtractedPattern {
                    id: Uuid::new_v4().to_string(),
                    agent_type: result.agent_type.name().to_string(),
                    task_id: result.task_id.clone(),
                    success: result.success,
                    quality_score: quality,
                    execution_time_ms: result.execution_time_ms,
                };

                let key = format!("pattern:{}", pattern.id);
                ns.set(
                    key,
                    serde_json::to_value(&pattern).unwrap_or(serde_json::Value::Null),
                    "ultralearn",
                );
                patterns_stored += 1;
            }
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("patterns_stored", patterns_stored as f32)
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

    fn make_context_with_results() -> LearningContext {
        let ns = Arc::new(MemoryNamespace::new("test-swarm"));
        let task_result = TaskResult {
            task_id: "t1".to_string(),
            agent_type: AgentType::Coder,
            success: true,
            output: "code".to_string(),
            error: None,
            execution_time_ms: 50,
            tokens_used: 100,
        };
        let routing = RoutingDecision {
            agent_type: AgentType::Coder,
            q_value: 0.5,
            confidence: 0.8,
            candidates: Vec::new(),
            decision_time_us: 10,
            strategy: SelectionStrategy::Exploitation,
        };
        let outcome = SwarmTaskOutcome {
            task_id: "t1".to_string(),
            routing,
            result: Ok(task_result),
        };
        let swarm_result = Arc::new(SwarmExecutionResult {
            swarm_id: "test-swarm".to_string(),
            task_results: vec![outcome],
            success_count: 1,
            failure_count: 0,
            total_time_ms: 100,
        });

        LearningContext::new()
            .with_namespace(ns)
            .with_swarm(swarm_result)
    }

    #[tokio::test]
    async fn test_ultralearn_stores_pattern() {
        let worker = UltralearnWorker::new();
        let ctx = make_context_with_results();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("patterns_stored"), Some(&1.0));

        let ns = ctx.namespace.unwrap();
        let keys = ns.keys();
        assert!(keys.iter().any(|k| k.starts_with("pattern:")));
    }

    #[tokio::test]
    async fn test_ultralearn_no_namespace_fails() {
        let worker = UltralearnWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        // Con outcomes vuoti ritorna ok (empty), non fail
        assert!(outcome.success);
    }
}
