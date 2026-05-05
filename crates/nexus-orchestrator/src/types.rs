use crate::agent_types::AgentType;
use serde::{Deserialize, Serialize};

/// Chiave per Q-value nella Q-table: (task_type, agent_type)
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QKey {
    pub task_type: String,
    pub agent_type: String,
}

impl QKey {
    pub fn new(task_type: impl Into<String>, agent_type: impl Into<String>) -> Self {
        Self {
            task_type: task_type.into(),
            agent_type: agent_type.into(),
        }
    }

    pub fn from_agent(task_type: impl Into<String>, agent: &AgentType) -> Self {
        Self {
            task_type: task_type.into(),
            agent_type: agent.name().to_string(),
        }
    }
}

/// Q-value con metadata
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QValue {
    pub value: f32,
    pub visit_count: u32,
    pub last_reward: f32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Default for QValue {
    fn default() -> Self {
        Self {
            value: 0.0,
            visit_count: 0,
            last_reward: 0.0,
            updated_at: chrono::Utc::now(),
        }
    }
}

/// Decisione di routing
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// Agente selezionato
    pub agent_type: AgentType,
    /// Q-value dell'agente scelto
    pub q_value: f32,
    /// Confidence della decisione (0.0 - 1.0)
    pub confidence: f32,
    /// Agenti candidati considerati
    pub candidates: Vec<CandidateAgent>,
    /// Tempo di decisione in microsecondi
    pub decision_time_us: u64,
    /// Strategia usata (exploration/exploitation)
    pub strategy: SelectionStrategy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateAgent {
    pub agent_type: AgentType,
    pub similarity_score: f32,
    pub q_value: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum SelectionStrategy {
    /// Exploitation: scegli l'agente con Q-value più alto
    Exploitation,
    /// Exploration: esplora agenti random (epsilon-greedy)
    Exploration,
    /// Cold start: nessun dato, scegli in base a similarity
    ColdStart,
    /// Forced: il client ha specificato esplicitamente l'agent type — bypassa Q-Learning
    Forced,
}

/// Configurazione Q-Learning
#[derive(Clone, Debug)]
pub struct QLearningConfig {
    /// Learning rate (alpha) - quanto velocemente update dei Q-values
    pub learning_rate: f32,
    /// Discount factor (gamma) - importanza reward futuri
    pub discount_factor: f32,
    /// Epsilon per exploration/exploitation trade-off
    pub epsilon: f32,
    /// Minimum epsilon (decay non va sotto questo valore)
    pub min_epsilon: f32,
    /// Epsilon decay rate per iterazione
    pub epsilon_decay: f32,
    /// Numero di candidati considerati via HNSW
    pub k_candidates: usize,
    /// Initial Q-value per cold start
    pub initial_q_value: f32,
}

impl Default for QLearningConfig {
    fn default() -> Self {
        Self {
            learning_rate: 0.1,
            discount_factor: 0.95,
            epsilon: 0.3,
            min_epsilon: 0.05,
            epsilon_decay: 0.995,
            k_candidates: 8,
            initial_q_value: 0.5,
        }
    }
}

/// Risultato di un'esecuzione per feedback
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub task_id: String,
    pub task_type: String,
    pub agent_type: AgentType,
    pub success: bool,
    pub quality_score: f32, // 0.0 - 1.0
    pub execution_time_ms: u64,
    pub error: Option<String>,
}

impl ExecutionOutcome {
    /// Calcola il reward per il Q-Learning update
    /// Reward formula: success_factor + quality_bonus - time_penalty
    pub fn compute_reward(&self) -> f32 {
        let base_reward = if self.success { 1.0 } else { -0.5 };
        let quality_bonus = self.quality_score * 0.5;

        // Penalizza esecuzioni troppo lunghe (>30s)
        let time_penalty = if self.execution_time_ms > 30_000 {
            -0.2
        } else {
            0.0
        };

        (base_reward + quality_bonus + time_penalty).clamp(-1.0, 1.5)
    }
}

/// Statistiche Q-Learning router
#[derive(Clone, Debug, Default)]
pub struct RouterStats {
    pub total_decisions: u64,
    pub exploration_count: u64,
    pub exploitation_count: u64,
    pub cold_start_count: u64,
    /// Conteggio routing forzati dal client (bypassa Q-Learning)
    pub forced_count: u64,
    pub avg_decision_time_us: f64,
    pub total_rewards: f32,
    pub current_epsilon: f32,
}
