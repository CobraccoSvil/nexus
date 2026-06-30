//! AuditWorker — security scanning dei task output
//!
//! Logica (placeholder pragmatico):
//! 1. Scansiona gli `output` dei task result cercando pattern sospetti
//!    (API keys hardcoded, password literal, SQL injection hint, ecc.)
//! 2. Per ogni hit, pubblica un alert nel namespace
//! 3. Ritorna metrics con numero di issue trovate
//!
//! In produzione sarebbe integrato con tool tipo cargo-audit, trivy, semgrep.
//! Qui è una regex-based heuristic per dimostrare il pattern.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use std::time::Instant;

/// Pattern sospetti noti — heuristic
const SUSPICIOUS_PATTERNS: &[(&str, &str)] = &[
    ("api_key", "api_key="),
    ("api_key", "apikey="),
    ("aws_secret", "AKIA"),
    ("private_key", "BEGIN PRIVATE KEY"),
    ("password_literal", "password = \""),
    ("sql_injection_hint", "' OR '1'='1"),
    ("eval_use", "eval("),
    ("unsafe_exec", "exec("),
];

#[derive(Default)]
pub struct AuditWorker {
    strict: bool,
}


impl AuditWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    fn scan(&self, text: &str) -> Vec<&'static str> {
        let lower = text.to_lowercase();
        SUSPICIOUS_PATTERNS
            .iter()
            .filter_map(|(kind, pat)| {
                if lower.contains(&pat.to_lowercase()) {
                    Some(*kind)
                } else {
                    None
                }
            })
            .collect()
    }
}

#[async_trait]
impl LearningWorker for AuditWorker {
    fn name(&self) -> &str {
        "audit"
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

        let mut total_issues = 0u32;
        let mut tasks_with_issues = 0u32;

        for outcome in outcomes {
            if let Ok(result) = &outcome.result {
                let findings = self.scan(&result.output);
                if !findings.is_empty() {
                    tasks_with_issues += 1;
                    total_issues += findings.len() as u32;

                    if let Some(ns) = &context.namespace {
                        let alert = serde_json::json!({
                            "task_id": result.task_id,
                            "agent": result.agent_type.name(),
                            "findings": findings,
                        });
                        ns.set(
                            format!("audit_alert:{}", result.task_id),
                            alert,
                            "audit",
                        );
                    }
                }
            }
        }

        // Modalità strict: se trova issue considera il worker "failed"
        // per segnalarlo a monitoring/alerting
        let success = !self.strict || total_issues == 0;
        let duration = start.elapsed().as_millis() as u64;

        let outcome = if success {
            WorkerOutcome::ok(self.name(), duration)
        } else {
            WorkerOutcome::fail(
                self.name(),
                format!("{} security issues detected (strict mode)", total_issues),
                duration,
            )
        };

        outcome
            .with_metric("total_issues", total_issues as f32)
            .with_metric("tasks_with_issues", tasks_with_issues as f32)
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

    fn make_ctx(output: &str) -> LearningContext {
        let ns = Arc::new(MemoryNamespace::new("test"));
        let task_result = TaskResult {
            task_id: "t1".to_string(),
            agent_type: AgentType::Coder,
            success: true,
            output: output.to_string(),
            error: None,
            execution_time_ms: 0,
            tokens_used: 0,
        };
        let routing = RoutingDecision {
            agent_type: AgentType::Coder,
            q_value: 0.5,
            confidence: 0.8,
            candidates: Vec::new(),
            decision_time_us: 10,
            strategy: SelectionStrategy::Exploitation,
        };
        let swarm = Arc::new(SwarmExecutionResult {
            swarm_id: "s".to_string(),
            task_results: vec![SwarmTaskOutcome {
                task_id: "t1".to_string(),
                routing,
                result: Ok(task_result),
            }],
            success_count: 1,
            failure_count: 0,
            total_time_ms: 0,
        });

        LearningContext::new().with_namespace(ns).with_swarm(swarm)
    }

    #[tokio::test]
    async fn test_audit_clean_output() {
        let worker = AuditWorker::new();
        let ctx = make_ctx("fn add(a: i32, b: i32) -> i32 { a + b }");
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("total_issues"), Some(&0.0));
    }

    #[tokio::test]
    async fn test_audit_detects_api_key() {
        let worker = AuditWorker::new();
        let ctx = make_ctx("let config = { api_key=\"sk-abc123\" };");
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success); // non strict
        assert!(outcome.metrics.get("total_issues").unwrap() >= &1.0);
    }

    #[tokio::test]
    async fn test_audit_strict_fails_on_issue() {
        let worker = AuditWorker::new().strict();
        let ctx = make_ctx("let pwd = password = \"hardcoded\";");
        let outcome = worker.run(&ctx).await;
        assert!(!outcome.success);
    }

    #[tokio::test]
    async fn test_audit_publishes_alert() {
        let worker = AuditWorker::new();
        let ctx = make_ctx("eval(\"rm -rf /\")");
        let _ = worker.run(&ctx).await;
        let ns = ctx.namespace.unwrap();
        let keys = ns.keys();
        assert!(keys.iter().any(|k| k.starts_with("audit_alert:")));
    }
}
