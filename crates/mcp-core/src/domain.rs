use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSummary {
    pub service: String,
    pub version: String,
    pub build_time: String,   // timestamp di compilazione — cambia ad ogni build
    pub status: String,
    pub timestamp: DateTime<Utc>,
    pub components: ComponentHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub database: bool,
    pub redis: bool,
    pub neural_core: bool,
    /// gRPC ToolRunner (porta 50071): se giù, l'AI non può eseguire tool MCP
    /// (read_file, str_replace, ecc.) e gli agenti finiscono con "0 step".
    #[serde(default)]
    pub tools_grpc: bool,
    /// Qdrant vector DB: se giù, le operazioni vettoriali (arricchimento
    /// quality scan, ricerca semantica) vengono saltate. Aggiornato dal
    /// task_watchdog ogni 60s.
    #[serde(default)]
    pub qdrant: bool,
    /// Embedder (gRPC al brain Python): se giù, nessuna vettorializzazione.
    #[serde(default)]
    pub embedder: bool,
    /// Brain REST (porta 8001): se giù, gli agent run non possono partire.
    /// `neural_core` verifica solo gRPC 50051; questo campo verifica il server
    /// HTTP che serve `/agent/run/stream`.
    #[serde(default)]
    pub brain_rest: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorAudit {
    pub project_id: String,
    pub profile_id: String,
    pub intent: String,
    pub provider: String,
    pub model: String,
    pub token_budget: u32,
    pub tokens_saved: u32,
    pub resources: Vec<String>,
    pub guardrail_result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
pub struct TokenStats {
    pub total_consumed: i64,
    pub total_cost: f64,
}
