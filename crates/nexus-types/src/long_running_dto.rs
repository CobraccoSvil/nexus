//! Tipi DTO condivisi per gli endpoint long_running_patterns
//! (regola L / ADR 0026, step S21). Prima duplicati in
//! crates/admin-service/src/long_running.rs e crates/mcp-core/src/long_running.rs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct LongRunningPattern {
    pub id: uuid::Uuid,
    pub pattern: String,
    pub description: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePatternRequest {
    pub pattern: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePatternRequest {
    pub pattern: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}
