use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::AppState;

// Tipi DTO: punto unico in nexus_types::admin_dto (regola L / ADR 0026, Wave C2).
pub use nexus_types::admin_dto::{
    ListUsersQuery, ListUsersResponse, SearchUsersQuery, UpdateUserRequest, UpdateUserRoleRequest,
    UserResponse, UserWithProjectsResponse,
};

// List users with pagination
pub async fn list_users(
    State(state): State<AppState>,
    Query(params): Query<ListUsersQuery>,
) -> Result<Json<ListUsersResponse>, StatusCode> {
    tracing::warn!("list_users: CALLED!");

    let page = params.page.unwrap_or(1).max(1);
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * limit;

    tracing::warn!(
        "list_users: page={}, limit={}, offset={}",
        page,
        limit,
        offset
    );

    let total_result =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE deleted_at IS NULL")
            .fetch_one(&state.db)
            .await;

    let total: i32 = match total_result {
        Ok(count) => {
            tracing::warn!("list_users: COUNT returned {}", count);
            count as i32
        }
        Err(e) => {
            tracing::error!("list_users: COUNT query failed: {}", e);
            0
        }
    };

    tracing::warn!(
        "list_users: total={}, page={}, limit={}, offset={}",
        total,
        page,
        limit,
        offset
    );

    let users_result = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
        ),
    >(
        r#"
        SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text
        FROM users
        WHERE deleted_at IS NULL
        ORDER BY created_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await;

    let users: Vec<UserResponse> = match users_result {
        Ok(rows) => {
            tracing::info!("list_users: fetched {} rows", rows.len());
            rows.into_iter()
                .map(
                    |(id, email, display_name, github_username, avatar_url, role, created_at)| {
                        UserResponse {
                            id,
                            email,
                            display_name,
                            github_username,
                            avatar_url,
                            role,
                            created_at,
                        }
                    },
                )
                .collect()
        }
        Err(e) => {
            tracing::error!("list_users: query error: {}", e);
            Vec::new()
        }
    };

    Ok(Json(ListUsersResponse {
        users,
        total,
        page,
        limit,
    }))
}

// Get single user with projects
pub async fn get_user(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Result<Json<UserWithProjectsResponse>, StatusCode> {
    let user_uuid = Uuid::parse_str(&user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Query condivisa: punto unico in nexus_types::admin_dto (cluster E6).
    let response = nexus_types::admin_dto::fetch_user_with_projects(&state.db, user_uuid)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(response))
}

// Update user (email, display_name)
pub async fn update_user(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<Json<UserResponse>, StatusCode> {
    let user_uuid = Uuid::parse_str(&user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Fetch current user first
    let current_user: (String, String, Option<String>, Option<String>, String, String) = sqlx::query_as(
        "SELECT email, display_name, github_username, avatar_url, role, created_at::text FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_uuid)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let new_email = payload.email.unwrap_or(current_user.0);
    let new_display_name = payload.display_name.unwrap_or(current_user.1);

    sqlx::query("UPDATE users SET email = $1, display_name = $2 WHERE id = $3")
        .bind(&new_email)
        .bind(&new_display_name)
        .bind(user_uuid)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(UserResponse {
        id: user_uuid.to_string(),
        email: new_email,
        display_name: new_display_name,
        github_username: current_user.2,
        avatar_url: current_user.3,
        role: current_user.4,
        created_at: current_user.5,
    }))
}

// Update user role
pub async fn update_user_role(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(payload): Json<UpdateUserRoleRequest>,
) -> Result<Json<UserResponse>, StatusCode> {
    let user_uuid = Uuid::parse_str(&user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Validate role
    if !["viewer", "editor", "admin"].contains(&payload.role.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let _user: (String, String, Option<String>, Option<String>, String) = sqlx::query_as(
        "SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_uuid)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(&payload.role)
        .bind(user_uuid)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Fetch updated user
    let updated: (String, String, String, Option<String>, Option<String>, String, String) = sqlx::query_as(
        "SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text FROM users WHERE id = $1",
    )
    .bind(user_uuid)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(UserResponse {
        id: updated.0,
        email: updated.1,
        display_name: updated.2,
        github_username: updated.3,
        avatar_url: updated.4,
        role: updated.5,
        created_at: updated.6,
    }))
}

// Delete user (soft delete)
pub async fn delete_user(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let user_uuid = Uuid::parse_str(&user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Verify user exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(user_uuid)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !exists {
        return Err(StatusCode::NOT_FOUND);
    }

    sqlx::query("UPDATE users SET deleted_at = NOW() WHERE id = $1")
        .bind(user_uuid)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

// Search users by email or name
pub async fn search_users(
    State(state): State<AppState>,
    Query(params): Query<SearchUsersQuery>,
) -> Result<Json<Vec<UserResponse>>, StatusCode> {
    // Query condivisa: punto unico in nexus_types::admin_dto (cluster E6).
    // Semantica storica preservata: errore DB -> lista vuota.
    let users = nexus_types::admin_dto::search_users_like(&state.db, &params.q)
        .await
        .unwrap_or_default();

    Ok(Json(users))
}
