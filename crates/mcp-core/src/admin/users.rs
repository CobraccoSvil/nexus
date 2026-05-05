use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub github_username: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserWithProjectsResponse {
    #[serde(flatten)]
    pub user: UserResponse,
    pub project_count: i32,
    pub projects: Vec<UserProjectRole>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserProjectRole {
    pub project_id: String,
    pub project_name: String,
    pub role: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct CreateUserRequest {
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRoleRequest {
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct ListUsersQuery {
    pub page: Option<i32>,
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct SearchUsersQuery {
    pub q: String,
}

#[derive(Debug, Serialize)]
pub struct ListUsersResponse {
    pub users: Vec<UserResponse>,
    pub total: i32,
    pub page: i32,
    pub limit: i32,
}

// List users with pagination
pub async fn list_users(
    State(state): State<AppState>,
    Query(params): Query<ListUsersQuery>,
) -> Result<Json<ListUsersResponse>, StatusCode> {
    tracing::warn!("list_users: CALLED!");

    let page = params.page.unwrap_or(1).max(1);
    let limit = params.limit.unwrap_or(20).min(100).max(1);
    let offset = (page - 1) * limit;

    tracing::warn!("list_users: page={}, limit={}, offset={}", page, limit, offset);

    let total_result = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE deleted_at IS NULL")
        .fetch_one(&state.db)
        .await;

    let total: i32 = match total_result {
        Ok(count) => {
            tracing::warn!("list_users: COUNT returned {}", count);
            count as i32
        },
        Err(e) => {
            tracing::error!("list_users: COUNT query failed: {}", e);
            0
        }
    };

    tracing::warn!("list_users: total={}, page={}, limit={}, offset={}", total, page, limit, offset);

    let users_result = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>, String, String)>(
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
            rows
                .into_iter()
                .map(|(id, email, display_name, github_username, avatar_url, role, created_at)| UserResponse {
                    id,
                    email,
                    display_name,
                    github_username,
                    avatar_url,
                    role,
                    created_at,
                })
                .collect()
        },
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

    let user: (String, String, String, Option<String>, Option<String>, String, String) = sqlx::query_as(
        "SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_uuid)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let (id, email, display_name, github_username, avatar_url, role, created_at) = user;

    let projects: Vec<UserProjectRole> = sqlx::query_as::<_, (String, String, String)>(
        r#"
        SELECT p.id, p.name, pm.role
        FROM project_members pm
        JOIN projects p ON pm.project_id = p.id
        WHERE pm.user_id = $1
        ORDER BY p.name
        "#,
    )
    .bind(user_uuid)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(project_id, project_name, role)| UserProjectRole {
        project_id,
        project_name,
        role,
    })
    .collect();

    let project_count = projects.len() as i32;

    Ok(Json(UserWithProjectsResponse {
        user: UserResponse {
            id,
            email,
            display_name,
            github_username,
            avatar_url,
            role,
            created_at,
        },
        project_count,
        projects,
    }))
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
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND deleted_at IS NULL)")
        .bind(user_uuid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(false);

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
    let query_pattern = format!("%{}%", params.q.to_lowercase());

    let users: Vec<UserResponse> = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>, String, String)>(
        r#"
        SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text
        FROM users
        WHERE deleted_at IS NULL AND (
            LOWER(email) LIKE $1 OR
            LOWER(display_name) LIKE $1 OR
            LOWER(github_username) LIKE $1
        )
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(&query_pattern)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id, email, display_name, github_username, avatar_url, role, created_at)| UserResponse {
        id,
        email,
        display_name,
        github_username,
        avatar_url,
        role,
        created_at,
    })
    .collect();

    Ok(Json(users))
}
