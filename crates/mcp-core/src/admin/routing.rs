use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct PurposeModelEntry {
    pub purpose: String,
    pub provider: String,
    pub model_id: String,
    pub notes: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ListPurposeModelsResponse {
    pub items: Vec<PurposeModelEntry>,
}

pub async fn list_purpose_models(
    State(state): State<AppState>,
) -> Result<Json<ListPurposeModelsResponse>, StatusCode> {
    let rows: Vec<(String, String, String, Option<String>, String)> = sqlx::query_as(
        r#"SELECT purpose, provider, model_id, notes, updated_at::text
           FROM nexus_purpose_model
           ORDER BY purpose"#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let items = rows
        .into_iter()
        .map(|(purpose, provider, model_id, notes, updated_at)| PurposeModelEntry {
            purpose,
            provider,
            model_id,
            notes,
            updated_at,
        })
        .collect();

    Ok(Json(ListPurposeModelsResponse { items }))
}

#[derive(Debug, Deserialize)]
pub struct UpdatePurposeModelRequest {
    pub provider: String,
    pub model_id: String,
    pub notes: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdatePurposeModelResponse {
    pub status: String,
    pub purpose: String,
}

pub async fn update_purpose_model(
    State(state): State<AppState>,
    Path(purpose): Path<String>,
    Json(body): Json<UpdatePurposeModelRequest>,
) -> Result<Json<UpdatePurposeModelResponse>, StatusCode> {
    let purpose = purpose.trim();
    if purpose.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.provider.trim().is_empty() || body.model_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    sqlx::query(
        r#"INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (purpose)
           DO UPDATE SET provider = EXCLUDED.provider,
                         model_id = EXCLUDED.model_id,
                         notes = EXCLUDED.notes,
                         updated_at = NOW()"#,
    )
    .bind(purpose)
    .bind(body.provider.trim().to_lowercase())
    .bind(body.model_id.trim())
    .bind(body.notes.clone())
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Invalida la cache routing matrix in modo best-effort: entro 60s si aggiorna comunque.
    // Qui non abbiamo un invalidate esplicito, quindi ci limitiamo a loggare.
    tracing::info!("admin: updated purpose_model {}", purpose);

    Ok(Json(UpdatePurposeModelResponse {
        status: "ok".to_string(),
        purpose: purpose.to_string(),
    }))
}

