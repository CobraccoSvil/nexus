use crate::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use sqlx::PgPool;

// Tipi DTO e logica handler: punto unico in nexus_types::long_running_dto
// (regola L, S21 + cluster E6). Qui restano solo i wrapper axum che
// estraggono lo State del crate e delegano.
pub use nexus_types::long_running_dto::{CreatePatternRequest, UpdatePatternRequest};
use nexus_types::ApiResult;

pub async fn list_patterns(State(state): State<AppState>) -> ApiResult {
    nexus_types::long_running_dto::list_patterns_core(&state.db).await
}

pub async fn create_pattern(
    State(state): State<AppState>,
    Json(body): Json<CreatePatternRequest>,
) -> ApiResult {
    nexus_types::long_running_dto::create_pattern_core(&state.db, body).await
}

pub async fn update_pattern(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<UpdatePatternRequest>,
) -> ApiResult {
    nexus_types::long_running_dto::update_pattern_core(&state.db, id, body).await
}

pub async fn delete_pattern(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult {
    nexus_types::long_running_dto::delete_pattern_core(&state.db, id).await
}

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
