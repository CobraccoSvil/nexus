//! Tipi dati di task/feedback — copiati da `nexus-agents/src/base.rs`
//! durante la fase 5e (opzione B) del refactor.
//!
//! Il trait `Agent` e la sua infrastruttura di esecuzione sono stati
//! eliminati: l'esecuzione vive nel brain LangGraph e l'unico feedback
//! rientra via gRPC `AgentRouter.SubmitFeedback`. Qui rimangono solo
//! i tipi necessari al router Q-Learning e alla superficie pubblica
//! dell'orchestrator.

use crate::agent_types::AgentType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Task che un agente deve eseguire
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub task_type: String,
    pub instructions: String,
    pub context: TaskContext,
    pub constraints: TaskConstraints,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskContext {
    pub project_id: String,
    pub codebase_path: Option<String>,
    pub issue_id: Option<String>,
    pub additional_context: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskConstraints {
    pub max_tokens: Option<u32>,
    pub timeout_seconds: Option<u32>,
    pub cost_limit: Option<f32>,
}

/// Risultato di una task
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub agent_type: AgentType,
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub execution_time_ms: u64,
    pub tokens_used: u32,
}

/// Feedback su un'esecuzione
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Feedback {
    pub task_id: String,
    pub quality_score: f32, // 0.0 - 1.0
    pub comments: String,
    pub corrections: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentMetadata {
    pub id: String,
    pub name: String,
    pub agent_type: AgentType,
    pub version: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Builder per task
#[derive(Clone, Debug)]
pub struct TaskBuilder {
    task_type: String,
    instructions: String,
    project_id: String,
    codebase_path: Option<String>,
    issue_id: Option<String>,
    max_tokens: Option<u32>,
    timeout_seconds: Option<u32>,
    cost_limit: Option<f32>,
}

impl TaskBuilder {
    pub fn new(task_type: String, instructions: String, project_id: String) -> Self {
        Self {
            task_type,
            instructions,
            project_id,
            codebase_path: None,
            issue_id: None,
            max_tokens: None,
            timeout_seconds: None,
            cost_limit: None,
        }
    }

    pub fn with_codebase(mut self, path: String) -> Self {
        self.codebase_path = Some(path);
        self
    }

    pub fn with_issue(mut self, id: String) -> Self {
        self.issue_id = Some(id);
        self
    }

    pub fn with_constraints(
        mut self,
        max_tokens: Option<u32>,
        timeout_seconds: Option<u32>,
    ) -> Self {
        self.max_tokens = max_tokens;
        self.timeout_seconds = timeout_seconds;
        self
    }

    pub fn build(self) -> Task {
        Task {
            id: Uuid::new_v4().to_string(),
            task_type: self.task_type,
            instructions: self.instructions,
            context: TaskContext {
                project_id: self.project_id,
                codebase_path: self.codebase_path,
                issue_id: self.issue_id,
                additional_context: HashMap::new(),
            },
            constraints: TaskConstraints {
                max_tokens: self.max_tokens,
                timeout_seconds: self.timeout_seconds,
                cost_limit: self.cost_limit,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_builder() {
        let task = TaskBuilder::new(
            "code_review".to_string(),
            "Review this code".to_string(),
            "project1".to_string(),
        )
        .with_codebase("/repo".to_string())
        .build();

        assert_eq!(task.task_type, "code_review");
        assert_eq!(task.context.codebase_path, Some("/repo".to_string()));
    }
}
