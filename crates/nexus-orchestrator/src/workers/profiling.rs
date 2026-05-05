//! ProfilingWorker — raccoglie profiling data per agent type
//!
//! Calcola p50/p95/max dell'execution_time_ms per tipo di agente
//! e li pubblica come `profile:{agent_type}`.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Instant;

pub struct ProfilingWorker;

impl Default for ProfilingWorker {
    fn default() -> Self {
        Self
    }
}

impl ProfilingWorker {
    pub fn new() -> Self {
        Self
    }

    fn percentile(sorted: &[u64], p: f32) -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((sorted.len() as f32 - 1.0) * p).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
}

#[async_trait]
impl LearningWorker for ProfilingWorker {
    fn name(&self) -> &str {
        "profiling"
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

        // Raggruppa tempi per agent type
        let mut times_by_agent: HashMap<String, Vec<u64>> = HashMap::new();
        for outcome in outcomes {
            if let Ok(result) = &outcome.result {
                times_by_agent
                    .entry(result.agent_type.name().to_string())
                    .or_insert_with(Vec::new)
                    .push(result.execution_time_ms);
            }
        }

        let agent_count = times_by_agent.len();

        if let Some(ns) = &context.namespace {
            for (agent, mut times) in times_by_agent {
                times.sort_unstable();
                let p50 = Self::percentile(&times, 0.50);
                let p95 = Self::percentile(&times, 0.95);
                let max = times.last().copied().unwrap_or(0);
                let sum: u64 = times.iter().sum();
                let avg = sum as f32 / times.len() as f32;

                let profile = serde_json::json!({
                    "agent_type": agent,
                    "samples": times.len(),
                    "p50_ms": p50,
                    "p95_ms": p95,
                    "max_ms": max,
                    "avg_ms": avg,
                });
                ns.set(format!("profile:{}", agent), profile, "profiling");
            }
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("profiled_agents", agent_count as f32)
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

    fn task(t: u64, a: AgentType) -> SwarmTaskOutcome {
        SwarmTaskOutcome {
            task_id: "t".to_string(),
            routing: RoutingDecision {
                agent_type: a.clone(),
                q_value: 0.5,
                confidence: 0.8,
                candidates: Vec::new(),
                decision_time_us: 0,
                strategy: SelectionStrategy::Exploitation,
            },
            result: Ok(TaskResult {
                task_id: "t".to_string(),
                agent_type: a,
                success: true,
                output: "".to_string(),
                error: None,
                execution_time_ms: t,
                tokens_used: 0,
            }),
        }
    }

    #[tokio::test]
    async fn test_profiling_computes_percentiles() {
        let ns = Arc::new(MemoryNamespace::new("test"));
        let swarm = Arc::new(SwarmExecutionResult {
            swarm_id: "s".to_string(),
            task_results: vec![
                task(10, AgentType::Coder),
                task(20, AgentType::Coder),
                task(30, AgentType::Coder),
                task(40, AgentType::Coder),
                task(50, AgentType::Coder),
            ],
            success_count: 5,
            failure_count: 0,
            total_time_ms: 150,
        });

        let worker = ProfilingWorker::new();
        let ctx = LearningContext::new().with_namespace(ns.clone()).with_swarm(swarm);
        let wo = worker.run(&ctx).await;
        assert!(wo.success);
        assert_eq!(wo.metrics.get("profiled_agents"), Some(&1.0));

        let profile = ns.get("profile:Coder").unwrap();
        assert_eq!(profile.value["samples"], 5);
        assert_eq!(profile.value["max_ms"], 50);
        assert_eq!(profile.value["p50_ms"], 30); // mediana
    }
}
