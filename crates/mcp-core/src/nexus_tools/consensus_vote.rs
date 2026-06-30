//! `utility::consensus_vote` — valuta una lista di voti multi-agente
//! tramite il `ConsensusEngine` del `NexusBridge`.
//!
//! Accetta in input:
//! - `strategy`: opzionale, uno tra `simple_majority` | `super_majority`
//!   | `unanimous` | `weighted_majority`. Se assente usa la strategia
//!   globale del bridge (default: `simple_majority`).
//! - `votes`: array di voti `{agent, approve, confidence, reason?}`
//!
//! Ritorna:
//! - `approved` (bool finale)
//! - `approve_count`, `reject_count`
//! - `aggregate_score`, `achieved_ratio`, `threshold`
//! - `strategy` usata
//!
//! Questo tool espone la capability di consensus come strumento MCP
//! callabile da orchestration layer (es. code review multi-reviewer,
//! security review multi-auditor, voting su PR merge).

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::nexus_bridge::NexusBridge;
use async_trait::async_trait;
use nexus_orchestrator::{ConsensusEngine, ConsensusStrategy, Vote};
use serde_json::{json, Value};

pub struct ConsensusVoteTool;

fn parse_strategy(s: &str) -> Option<ConsensusStrategy> {
    match s.to_lowercase().as_str() {
        "simple_majority" | "simple" | "majority" => Some(ConsensusStrategy::SimpleMajority),
        "super_majority" | "super" | "2/3" => Some(ConsensusStrategy::SuperMajority),
        "unanimous" | "unanimity" => Some(ConsensusStrategy::Unanimous),
        "weighted_majority" | "weighted" => Some(ConsensusStrategy::WeightedMajority),
        _ => None,
    }
}

#[async_trait]
impl NexusToolHandler for ConsensusVoteTool {
    async fn execute(
        &self,
        _ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let votes_raw = args
            .get("votes")
            .and_then(Value::as_array)
            .ok_or_else(|| NexusToolError::BadInput("votes array required".into()))?;

        if votes_raw.is_empty() {
            return Err(NexusToolError::BadInput(
                "votes array must be non-empty".into(),
            ));
        }

        let mut votes: Vec<Vote> = Vec::with_capacity(votes_raw.len());
        for (i, v) in votes_raw.iter().enumerate() {
            let agent = v
                .get("agent")
                .and_then(Value::as_str)
                .ok_or_else(|| NexusToolError::BadInput(format!("votes[{}].agent missing", i)))?;
            let approve = v
                .get("approve")
                .and_then(Value::as_bool)
                .ok_or_else(|| NexusToolError::BadInput(format!("votes[{}].approve missing", i)))?;
            let confidence = v.get("confidence").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let reason = v
                .get("reason")
                .and_then(Value::as_str)
                .map(|s| s.to_string());

            let mut vote = if approve {
                Vote::approve(agent.to_string(), confidence)
            } else {
                Vote::reject(agent.to_string(), confidence)
            };
            if let Some(r) = reason {
                vote = vote.with_reason(r);
            }
            votes.push(vote);
        }

        // Strategy: preferiamo quella esplicita, fallback al bridge se disponibile,
        // altrimenti SimpleMajority.
        let engine: ConsensusEngine = if let Some(s) = args.get("strategy").and_then(Value::as_str)
        {
            match parse_strategy(s) {
                Some(strat) => ConsensusEngine::new(strat),
                None => {
                    return Err(NexusToolError::BadInput(format!(
                        "unknown strategy '{}': use simple_majority | super_majority | unanimous | weighted_majority",
                        s
                    )))
                }
            }
        } else if let Some(bridge) = NexusBridge::global() {
            ConsensusEngine::new(bridge.consensus().strategy())
        } else {
            ConsensusEngine::new(ConsensusStrategy::SimpleMajority)
        };

        let result = engine.evaluate(&votes);

        Ok(json!({
            "ok": true,
            "approved": result.approved,
            "approve_count": result.approve_count,
            "reject_count": result.reject_count,
            "aggregate_score": result.aggregate_score,
            "achieved_ratio": result.achieved_ratio,
            "threshold": result.threshold,
            "strategy": format!("{:?}", result.strategy),
            "total_votes": votes.len(),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["votes"],
            "properties": {
                "strategy": {
                    "type": "string",
                    "enum": ["simple_majority", "super_majority", "unanimous", "weighted_majority"],
                    "description": "Override della strategy del bridge"
                },
                "votes": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["agent", "approve"],
                        "properties": {
                            "agent": {"type": "string"},
                            "approve": {"type": "boolean"},
                            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                            "reason": {"type": "string"}
                        }
                    }
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simple_majority_approved() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = ConsensusVoteTool
            .execute(
                &ctx,
                &json!({
                    "strategy": "simple_majority",
                    "votes": [
                        {"agent": "a1", "approve": true, "confidence": 0.9},
                        {"agent": "a2", "approve": true, "confidence": 0.7},
                        {"agent": "a3", "approve": false, "confidence": 0.5}
                    ]
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["approved"], true);
        assert_eq!(out["approve_count"], 2);
        assert_eq!(out["reject_count"], 1);
    }

    #[tokio::test]
    async fn test_unanimous_rejected() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let out = ConsensusVoteTool
            .execute(
                &ctx,
                &json!({
                    "strategy": "unanimous",
                    "votes": [
                        {"agent": "a1", "approve": true},
                        {"agent": "a2", "approve": false}
                    ]
                }),
            )
            .await
            .unwrap();
        assert_eq!(out["approved"], false);
    }

    #[tokio::test]
    async fn test_bad_strategy() {
        let ctx = NexusToolContext::new(std::env::temp_dir(), uuid::Uuid::nil(), uuid::Uuid::nil());
        let res = ConsensusVoteTool
            .execute(
                &ctx,
                &json!({"strategy": "bogus", "votes": [{"agent": "a", "approve": true}]}),
            )
            .await;
        assert!(matches!(res, Err(NexusToolError::BadInput(_))));
    }

    #[test]
    fn test_safety_readonly() {
        assert!(ConsensusVoteTool.safety().read_only);
    }
}
