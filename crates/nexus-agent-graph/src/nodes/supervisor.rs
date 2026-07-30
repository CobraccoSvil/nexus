//! `SupervisorNode` — superstep dedicato del supervisore worker.
//!
//! Dopo ogni giro di `tool_dispatch`, se `SupervisorMode` lo richiede, il grafo
//! instrada qui PRIMA di rientrare nell'executor. Una sola LLM-call isolata
//! (replay-safe, ADR 0036-style): la decisione e' persistita in `extra` alla
//! chiave `supervisor_decision::{iterations}`.

use std::sync::Arc;

use async_trait::async_trait;
use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;
use serde_json::json;

use crate::decisions::supervisor::{
    build_anomaly_block, build_steps_summary, detect_anomalies, supervisor_cache_key,
    SupervisorConfig, SupervisorDecision,
};
use crate::decisions::turn_task::extract_original_task;
use crate::runtime::ports::{MetaReasonerPort, SupervisorContext};
use crate::runtime::AgentNodeCtx;
use crate::state::{
    put_extra, AgentState, Message, MessageContent, MetaStep, StateDelta, StopReason,
    SupervisorMode,
};

/// Chiave in `extra` quando il supervisore decide di abbandonare (edge -> learner).
pub const SUPERVISOR_ABANDON_KEY: &str = "supervisor_abandon";

pub struct SupervisorNode {
    reasoner: Arc<dyn MetaReasonerPort>,
    cfg: SupervisorConfig,
}

impl SupervisorNode {
    pub fn new(reasoner: Arc<dyn MetaReasonerPort>, cfg: SupervisorConfig) -> Self {
        Self { reasoner, cfg }
    }

    fn cached_decision(state: &AgentState, key: &str) -> Option<SupervisorDecision> {
        state
            .extra
            .get(key)
            .map(crate::decisions::supervisor::validate_supervisor_response)
    }

    fn decision_to_json(decision: &SupervisorDecision) -> serde_json::Value {
        match decision {
            SupervisorDecision::Continue => json!({ "action": "continue" }),
            SupervisorDecision::Redirect { message } => {
                json!({ "action": "redirect", "message": message })
            }
            SupervisorDecision::Abandon { reason } => {
                json!({ "action": "abandon", "reason": reason })
            }
        }
    }

    fn apply_decision(state: &AgentState, decision: SupervisorDecision) -> StateDelta {
        let ms = match &decision {
            SupervisorDecision::Continue => MetaStep {
                kind: "supervisor".into(),
                title: "Supervisor: continua".into(),
                payload: json!({ "action": "continue" }),
                correlation_id: None,
                created_at: None,
            },
            SupervisorDecision::Redirect { message } => MetaStep {
                kind: "supervisor".into(),
                title: "Supervisor: redirect".into(),
                payload: json!({ "action": "redirect", "message": message }),
                correlation_id: None,
                created_at: None,
            },
            SupervisorDecision::Abandon { reason } => MetaStep {
                kind: "supervisor".into(),
                title: "Supervisor: abbandona".into(),
                payload: json!({ "action": "abandon", "reason": reason }),
                correlation_id: None,
                created_at: None,
            },
        };

        match decision {
            SupervisorDecision::Continue => StateDelta {
                meta_steps: Some(vec![ms]),
                stop_reason: Some(Some(StopReason::SupervisorResolved)),
                ..Default::default()
            },
            SupervisorDecision::Redirect { message } => {
                let redirect_text = format!(
                    "[Supervisor] Correggi l'approccio seguendo questa istruzione:\n{message}"
                );
                StateDelta {
                    messages: Some(vec![Message::Human {
                        content: MessageContent::text(redirect_text),
                    }]),
                    meta_steps: Some(vec![ms]),
                    stop_reason: Some(Some(StopReason::SupervisorResolved)),
                    ..Default::default()
                }
            }
            SupervisorDecision::Abandon { reason } => {
                let mut extra = put_extra(state, SUPERVISOR_ABANDON_KEY, json!(true));
                let merged = AgentState {
                    extra,
                    ..state.clone()
                };
                extra = put_extra(
                    &merged,
                    supervisor_cache_key(state.iterations.unwrap_or(0)),
                    json!({ "action": "abandon", "reason": reason.clone() }),
                );
                StateDelta {
                    extra: Some(extra),
                    meta_steps: Some(vec![ms]),
                    result: Some(Some(format!("[Supervisor] Task abbandonato: {reason}"))),
                    stop_reason: Some(Some(StopReason::SupervisorAbandon)),
                    ..Default::default()
                }
            }
        }
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for SupervisorNode {
    fn id(&self) -> NodeId {
        NodeId::Supervisor
    }

    async fn run(&self, state: &AgentState, _ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        let mode = state.supervisor_mode.unwrap_or(SupervisorMode::None);
        if mode == SupervisorMode::None {
            return Ok(StateDelta {
                stop_reason: Some(Some(StopReason::SupervisorResolved)),
                ..Default::default()
            }
            .into_opaque());
        }

        let iterations = state.iterations.unwrap_or(0);
        let cache_key = supervisor_cache_key(iterations);

        let (decision, from_cache) = if let Some(cached) = Self::cached_decision(state, &cache_key)
        {
            (cached, true)
        } else {
            let anomalies = detect_anomalies(state, self.cfg);
            let sup_ctx = SupervisorContext {
                task: extract_original_task(state),
                steps_summary: build_steps_summary(state, 12),
                anomaly_block: build_anomaly_block(&anomalies),
            };

            let decision = match self
                .reasoner
                .supervise(sup_ctx)
                .await
            {
                Ok(Some(d)) => d,
                Ok(None) => SupervisorDecision::Continue,
                Err(e) => {
                    tracing::warn!(
                        target: "nexus_agent_graph::supervisor",
                        error = %e,
                        iterations,
                        "supervisor: consultazione LLM fallita, degrado a continue"
                    );
                    SupervisorDecision::Continue
                }
            };
            (decision, false)
        };

        let mut delta = Self::apply_decision(state, decision.clone());
        if !from_cache {
            let persisted = Self::decision_to_json(&decision);
            let base_extra = delta
                .extra
                .take()
                .or_else(|| Some(state.extra.clone()));
            let mut merged = AgentState {
                extra: base_extra.unwrap_or_default(),
                ..state.clone()
            };
            merged.extra = put_extra(&merged, &cache_key, persisted);
            delta.extra = Some(merged.extra);
        }

        Ok(delta.into_opaque())
    }
}
