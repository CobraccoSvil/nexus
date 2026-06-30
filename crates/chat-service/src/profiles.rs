use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use nexus_auth::Claims;
use nexus_types::{api_error, parse_user_id, ApiResult};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateProfileRequest {
    pub name: String,
    pub description: Option<String>,
    pub avatar_emoji: Option<String>,
    pub system_prompt: Option<String>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_automation: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_emoji: Option<String>,
    pub system_prompt: Option<String>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_automation: Option<String>,
}

/// GET /api/profiles
pub async fn list_profiles(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;

    let rows = sqlx::query(
        "SELECT id, name, description, avatar_emoji, system_prompt, default_provider, default_model, default_automation, is_default, created_at, updated_at FROM user_profiles WHERE user_id = $1 ORDER BY is_default DESC, name ASC"
    )
    .bind(user_id).fetch_all(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let profiles: Vec<Value> = rows.iter().map(|r| json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "name": r.get::<String, _>("name"),
        "description": r.get::<Option<String>, _>("description"),
        "avatar_emoji": r.get::<String, _>("avatar_emoji"),
        "system_prompt": r.get::<Option<String>, _>("system_prompt"),
        "default_provider": r.get::<Option<String>, _>("default_provider"),
        "default_model": r.get::<Option<String>, _>("default_model"),
        "default_automation": r.get::<Option<String>, _>("default_automation"),
        "is_default": r.get::<bool, _>("is_default"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
    })).collect();

    Ok(Json(json!({ "profiles": profiles })))
}

/// POST /api/profiles
pub async fn create_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateProfileRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO user_profiles (id, user_id, name, description, avatar_emoji, system_prompt, default_provider, default_model, default_automation) VALUES ($1, $2, $3, $4, COALESCE($5, '🤖'), $6, $7, $8, $9)"
    )
    .bind(profile_id).bind(user_id).bind(&req.name)
    .bind(&req.description).bind(&req.avatar_emoji)
    .bind(&req.system_prompt).bind(&req.default_provider)
    .bind(&req.default_model).bind(&req.default_automation)
    .execute(&state.db).await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "id": profile_id.to_string(), "name": req.name })))
}

/// PUT /api/profiles/:id
pub async fn update_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateProfileRequest>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    // Verify ownership
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_profiles WHERE id = $1 AND user_id = $2)")
        .bind(profile_id).bind(user_id).fetch_one(&state.db).await.unwrap_or(false);
    if !exists { return Err(api_error(StatusCode::NOT_FOUND, "Profilo non trovato")); }

    if let Some(name) = &req.name {
        sqlx::query("UPDATE user_profiles SET name = $1, updated_at = NOW() WHERE id = $2")
            .bind(name).bind(profile_id).execute(&state.db).await.ok();
    }
    if let Some(desc) = &req.description {
        sqlx::query("UPDATE user_profiles SET description = $1, updated_at = NOW() WHERE id = $2")
            .bind(desc).bind(profile_id).execute(&state.db).await.ok();
    }
    if let Some(emoji) = &req.avatar_emoji {
        sqlx::query("UPDATE user_profiles SET avatar_emoji = $1, updated_at = NOW() WHERE id = $2")
            .bind(emoji).bind(profile_id).execute(&state.db).await.ok();
    }
    if let Some(sp) = &req.system_prompt {
        sqlx::query("UPDATE user_profiles SET system_prompt = $1, updated_at = NOW() WHERE id = $2")
            .bind(sp).bind(profile_id).execute(&state.db).await.ok();
    }
    if let Some(p) = &req.default_provider {
        sqlx::query("UPDATE user_profiles SET default_provider = $1, updated_at = NOW() WHERE id = $2")
            .bind(p).bind(profile_id).execute(&state.db).await.ok();
    }
    if let Some(m) = &req.default_model {
        sqlx::query("UPDATE user_profiles SET default_model = $1, updated_at = NOW() WHERE id = $2")
            .bind(m).bind(profile_id).execute(&state.db).await.ok();
    }
    if let Some(a) = &req.default_automation {
        sqlx::query("UPDATE user_profiles SET default_automation = $1, updated_at = NOW() WHERE id = $2")
            .bind(a).bind(profile_id).execute(&state.db).await.ok();
    }

    Ok(Json(json!({ "ok": true })))
}

/// DELETE /api/profiles/:id
pub async fn delete_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    sqlx::query("DELETE FROM user_profiles WHERE id = $1 AND user_id = $2")
        .bind(profile_id).bind(user_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/profiles/:id/default
pub async fn set_default_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let profile_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Profile id non valido"))?;

    // Clear all defaults first
    sqlx::query("UPDATE user_profiles SET is_default = FALSE WHERE user_id = $1")
        .bind(user_id).execute(&state.db).await.ok();

    // Set this one as default
    sqlx::query("UPDATE user_profiles SET is_default = TRUE, updated_at = NOW() WHERE id = $1 AND user_id = $2")
        .bind(profile_id).bind(user_id).execute(&state.db).await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true })))
}
