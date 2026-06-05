//! Tipi DTO condivisi per gli endpoint settings di mcp-core e admin-service
//! (regola L / ADR 0026, step S8). Prima erano definiti pari-pari in
//! crates/admin-service/src/settings.rs e crates/mcp-core/src/settings.rs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub category: String,
    pub description: String,
    pub is_secret: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingRequest {
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct BulkUpdateRequest {
    pub settings: Vec<BulkSettingEntry>,
}

#[derive(Debug, Deserialize)]
pub struct BulkSettingEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct FsBrowseQuery {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDirectoryRequest {
    pub parent_path: String,
    pub name: String,
}
