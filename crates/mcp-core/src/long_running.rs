use crate::AppState;
use sqlx::PgPool;

// Tipi DTO e logica handler: punto unico in nexus_types::long_running_dto
// (regola L, S21 + cluster E6). I 4 wrapper axum (list/create/update/delete_pattern)
// sono generati dalla macro del punto unico per non duplicare il boilerplate fra
// mcp-core e admin-service. Qui resta solo l'extra load_enabled_patterns.
nexus_types::long_running_axum_handlers!(AppState);

/// Carica tutti i pattern abilitati dal DB.
/// Usato dall'agent_tools per il rilevamento long-running.
pub async fn load_enabled_patterns(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT pattern FROM long_running_patterns WHERE enabled = TRUE",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
}
