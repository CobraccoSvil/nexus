//! Consensus Engine — lightweight quorum-based consensus per swarm
//!
//! Implementa un approccio pragmatico al consensus fra agenti:
//! - Quorum voting (N/2 + 1 majority) per decisioni binarie
//! - Weighted voting basato su Q-value/confidence dell'agente
//! - Aggregation strategies per combinare risultati eterogenei
//!
//! Non è un protocollo Byzantine-fault-tolerant né Raft completo:
//! per quelli servirà integrazione futura con crate `raft-rs`.
//! Questo è sufficiente per un singolo nodo orchestrator che
//! coordina agenti locali, che è lo scenario iniziale di Nexus.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Strategia di aggregazione per i voti degli agenti
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsensusStrategy {
    /// Maggioranza semplice (>50%)
    SimpleMajority,
    /// Maggioranza qualificata (2/3)
    SuperMajority,
    /// Unanimità (100%)
    Unanimous,
    /// Peso dei voti in base a confidence (weighted voting)
    WeightedMajority,
}

/// Un voto di un agente su una proposta
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vote {
    /// Agente votante
    pub agent: String,
    /// Approvazione della proposta
    pub approve: bool,
    /// Confidence del voto (0.0 - 1.0). Usato in WeightedMajority.
    pub confidence: f32,
    /// Motivazione del voto (opzionale)
    pub reason: Option<String>,
}

impl Vote {
    pub fn approve(agent: impl Into<String>, confidence: f32) -> Self {
        Self {
            agent: agent.into(),
            approve: true,
            confidence: confidence.clamp(0.0, 1.0),
            reason: None,
        }
    }

    pub fn reject(agent: impl Into<String>, confidence: f32) -> Self {
        Self {
            agent: agent.into(),
            approve: false,
            confidence: confidence.clamp(0.0, 1.0),
            reason: None,
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Risultato del consensus
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsensusResult {
    /// Decisione finale (true = approvato)
    pub approved: bool,
    /// Numero di voti approvanti
    pub approve_count: usize,
    /// Numero di voti contrari
    pub reject_count: usize,
    /// Score aggregato (per weighted): sum(confidence_approve) - sum(confidence_reject)
    pub aggregate_score: f32,
    /// Strategia usata
    pub strategy: ConsensusStrategy,
    /// Threshold richiesto (0.0 - 1.0)
    pub threshold: f32,
    /// Ratio effettivo raggiunto
    pub achieved_ratio: f32,
}

/// Consensus engine — motore per decisioni collettive
pub struct ConsensusEngine {
    strategy: ConsensusStrategy,
}

impl Default for ConsensusEngine {
    fn default() -> Self {
        Self::new(ConsensusStrategy::SimpleMajority)
    }
}

impl ConsensusEngine {
    pub fn new(strategy: ConsensusStrategy) -> Self {
        Self { strategy }
    }

    pub fn strategy(&self) -> ConsensusStrategy {
        self.strategy
    }

    /// Threshold (ratio 0.0 - 1.0) richiesto dalla strategia
    pub fn required_threshold(&self) -> f32 {
        match self.strategy {
            ConsensusStrategy::SimpleMajority => 0.5 + f32::EPSILON,
            ConsensusStrategy::SuperMajority => 2.0 / 3.0,
            ConsensusStrategy::Unanimous => 1.0,
            ConsensusStrategy::WeightedMajority => 0.5 + f32::EPSILON,
        }
    }

    /// Valuta un insieme di voti e ritorna il risultato del consensus
    pub fn evaluate(&self, votes: &[Vote]) -> ConsensusResult {
        if votes.is_empty() {
            return ConsensusResult {
                approved: false,
                approve_count: 0,
                reject_count: 0,
                aggregate_score: 0.0,
                strategy: self.strategy,
                threshold: self.required_threshold(),
                achieved_ratio: 0.0,
            };
        }

        let approve_count = votes.iter().filter(|v| v.approve).count();
        let reject_count = votes.len() - approve_count;

        let (approved, achieved_ratio, aggregate_score) = match self.strategy {
            ConsensusStrategy::SimpleMajority => {
                let ratio = approve_count as f32 / votes.len() as f32;
                (ratio > 0.5, ratio, approve_count as f32 - reject_count as f32)
            }
            ConsensusStrategy::SuperMajority => {
                let ratio = approve_count as f32 / votes.len() as f32;
                (ratio >= 2.0 / 3.0, ratio, approve_count as f32 - reject_count as f32)
            }
            ConsensusStrategy::Unanimous => {
                let ratio = approve_count as f32 / votes.len() as f32;
                (reject_count == 0, ratio, approve_count as f32 - reject_count as f32)
            }
            ConsensusStrategy::WeightedMajority => {
                let approve_weight: f32 = votes
                    .iter()
                    .filter(|v| v.approve)
                    .map(|v| v.confidence)
                    .sum();
                let reject_weight: f32 = votes
                    .iter()
                    .filter(|v| !v.approve)
                    .map(|v| v.confidence)
                    .sum();
                let total = approve_weight + reject_weight;
                let ratio = if total > 0.0 {
                    approve_weight / total
                } else {
                    0.0
                };
                (ratio > 0.5, ratio, approve_weight - reject_weight)
            }
        };

        ConsensusResult {
            approved,
            approve_count,
            reject_count,
            aggregate_score,
            strategy: self.strategy,
            threshold: self.required_threshold(),
            achieved_ratio,
        }
    }

    /// Aggrega risultati eterogenei di task (non boolean).
    /// Raggruppa i risultati per equivalent output e ritorna quello con più voti.
    pub fn aggregate_results<T: Clone + Eq + std::hash::Hash>(
        &self,
        results: Vec<(String, T, f32)>, // (agent, result, confidence)
    ) -> Option<AggregatedResult<T>> {
        if results.is_empty() {
            return None;
        }

        let mut buckets: HashMap<T, (usize, f32, Vec<String>)> = HashMap::new();
        for (agent, res, conf) in results {
            let entry = buckets.entry(res).or_insert((0, 0.0, Vec::new()));
            entry.0 += 1;
            entry.1 += conf;
            entry.2.push(agent);
        }

        // Trova il bucket "vincente" secondo strategia
        let winning = match self.strategy {
            ConsensusStrategy::WeightedMajority => {
                buckets.into_iter().max_by(|a, b| {
                    a.1 .1
                        .partial_cmp(&b.1 .1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            }
            _ => buckets.into_iter().max_by_key(|(_, (count, _, _))| *count),
        };

        winning.map(|(result, (count, conf_sum, agents))| AggregatedResult {
            result,
            vote_count: count,
            confidence_sum: conf_sum,
            supporting_agents: agents,
        })
    }
}

/// Risultato aggregato con winner
#[derive(Clone, Debug)]
pub struct AggregatedResult<T> {
    pub result: T,
    pub vote_count: usize,
    pub confidence_sum: f32,
    pub supporting_agents: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_majority_approve() {
        let engine = ConsensusEngine::new(ConsensusStrategy::SimpleMajority);
        let votes = vec![
            Vote::approve("a1", 1.0),
            Vote::approve("a2", 1.0),
            Vote::reject("a3", 1.0),
        ];
        let result = engine.evaluate(&votes);
        assert!(result.approved);
        assert_eq!(result.approve_count, 2);
        assert_eq!(result.reject_count, 1);
    }

    #[test]
    fn test_simple_majority_reject_on_tie() {
        let engine = ConsensusEngine::new(ConsensusStrategy::SimpleMajority);
        let votes = vec![Vote::approve("a1", 1.0), Vote::reject("a2", 1.0)];
        let result = engine.evaluate(&votes);
        assert!(!result.approved); // 50% non è > 50%
    }

    #[test]
    fn test_super_majority() {
        let engine = ConsensusEngine::new(ConsensusStrategy::SuperMajority);
        // 2/3 esatti = passa
        let votes = vec![
            Vote::approve("a1", 1.0),
            Vote::approve("a2", 1.0),
            Vote::reject("a3", 1.0),
        ];
        let result = engine.evaluate(&votes);
        assert!(result.approved);

        // 1/2 non passa
        let votes = vec![
            Vote::approve("a1", 1.0),
            Vote::approve("a2", 1.0),
            Vote::reject("a3", 1.0),
            Vote::reject("a4", 1.0),
        ];
        let result = engine.evaluate(&votes);
        assert!(!result.approved);
    }

    #[test]
    fn test_unanimous() {
        let engine = ConsensusEngine::new(ConsensusStrategy::Unanimous);
        let votes = vec![Vote::approve("a1", 1.0), Vote::approve("a2", 1.0)];
        assert!(engine.evaluate(&votes).approved);

        let votes = vec![Vote::approve("a1", 1.0), Vote::reject("a2", 1.0)];
        assert!(!engine.evaluate(&votes).approved);
    }

    #[test]
    fn test_weighted_majority() {
        let engine = ConsensusEngine::new(ConsensusStrategy::WeightedMajority);
        // 2 voti approve con bassa confidence, 1 voto reject con alta confidence
        // Pesato: 0.3 + 0.3 = 0.6 vs 0.95 → reject vince
        let votes = vec![
            Vote::approve("a1", 0.3),
            Vote::approve("a2", 0.3),
            Vote::reject("a3", 0.95),
        ];
        let result = engine.evaluate(&votes);
        assert!(!result.approved);

        // Ora invertito
        let votes = vec![
            Vote::approve("a1", 0.9),
            Vote::approve("a2", 0.9),
            Vote::reject("a3", 0.3),
        ];
        let result = engine.evaluate(&votes);
        assert!(result.approved);
    }

    #[test]
    fn test_empty_votes() {
        let engine = ConsensusEngine::default();
        let result = engine.evaluate(&[]);
        assert!(!result.approved);
        assert_eq!(result.approve_count, 0);
    }

    #[test]
    fn test_aggregate_results_majority() {
        let engine = ConsensusEngine::new(ConsensusStrategy::SimpleMajority);
        let results = vec![
            ("a1".to_string(), "option_a".to_string(), 1.0),
            ("a2".to_string(), "option_a".to_string(), 1.0),
            ("a3".to_string(), "option_b".to_string(), 1.0),
        ];
        let agg = engine.aggregate_results(results).unwrap();
        assert_eq!(agg.result, "option_a");
        assert_eq!(agg.vote_count, 2);
        assert_eq!(agg.supporting_agents.len(), 2);
    }

    #[test]
    fn test_aggregate_results_weighted() {
        let engine = ConsensusEngine::new(ConsensusStrategy::WeightedMajority);
        // option_b ha meno voti ma più confidence totale
        let results = vec![
            ("a1".to_string(), "option_a".to_string(), 0.3),
            ("a2".to_string(), "option_a".to_string(), 0.3),
            ("a3".to_string(), "option_b".to_string(), 0.95),
        ];
        let agg = engine.aggregate_results(results).unwrap();
        assert_eq!(agg.result, "option_b");
    }
}
