use axum::{http::StatusCode, Json};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub use nexus_auth::Claims;

// --- Shared API types ---

pub type ApiError = (StatusCode, Json<Value>);
pub type ApiResult = Result<Json<Value>, ApiError>;

pub fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": message.into() })))
}

pub fn parse_user_id(claims: &Claims) -> Result<Uuid, ApiError> {
    Uuid::parse_str(&claims.sub)
        .map_err(|_| api_error(StatusCode::UNAUTHORIZED, "Sessione utente non valida"))
}

pub fn parse_project_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))
}

pub async fn ensure_project_access(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let row = sqlx::query(
        r#"
        SELECT
            p.owner_user_id,
            (
                SELECT pm.role
                FROM project_members pm
                WHERE pm.project_id = p.id
                  AND pm.user_id = $2
                LIMIT 1
            ) AS member_role
        FROM projects p
        WHERE p.id = $1
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::NOT_FOUND, "Progetto non trovato"));
    };

    let owner_user_id: Uuid = row
        .try_get("owner_user_id")
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let member_role: Option<String> = row.try_get("member_role").unwrap_or(None);
    if owner_user_id == user_id || member_role.is_some() {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::FORBIDDEN,
            "Non sei autorizzato su questo progetto",
        ))
    }
}
