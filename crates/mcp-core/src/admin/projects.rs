use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;

// Tipi DTO: punto unico in nexus_types::admin_dto (regola L / ADR 0026, Wave C2).
pub use nexus_types::admin_dto::{
    AddProjectMemberRequest, ListAllProjectsResponse, ListProjectMembersResponse,
    ProjectMemberResponse, UpdateProjectMemberRequest,
};

/// GET /api/admin/projects — list ALL projects (admin only)
pub async fn list_all_projects(
    State(state): State<AppState>,
) -> Result<Json<ListAllProjectsResponse>, StatusCode> {
    // Punto unico SQL in nexus_types::admin_dto (regola L, S63).
    let projects = nexus_types::admin_dto::fetch_all_projects_summary(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ListAllProjectsResponse { projects }))
}

// List project members
pub async fn list_project_members(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<ListProjectMembersResponse>, StatusCode> {
    let project_uuid = Uuid::parse_str(&project_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Verify project exists
    let project_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
            .bind(project_uuid)
            .fetch_one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !project_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    // Fix bug latente S81: propagare errore SQL invece di mascherarlo (regola H).
    let members = nexus_types::admin_dto::fetch_project_members(&state.db, project_uuid)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ListProjectMembersResponse {
        project_id,
        members,
    }))
}

// Add member to project
pub async fn add_project_member(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(payload): Json<AddProjectMemberRequest>,
) -> Result<Json<ProjectMemberResponse>, StatusCode> {
    let project_uuid = Uuid::parse_str(&project_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let user_uuid = Uuid::parse_str(&payload.user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Validate role
    if !["viewer", "editor", "owner"].contains(&payload.role.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Verify project exists
    let project_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
            .bind(project_uuid)
            .fetch_one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !project_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    // Verify user exists
    let user: (String, String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT id, email, display_name, github_username, avatar_url FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_uuid)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Check if already member
    let already_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM project_members WHERE project_id = $1 AND user_id = $2)",
    )
    .bind(project_uuid)
    .bind(user_uuid)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if already_member {
        return Err(StatusCode::CONFLICT);
    }

    // Add member
    sqlx::query("INSERT INTO project_members (project_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(project_uuid)
        .bind(user_uuid)
        .bind(&payload.role)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProjectMemberResponse {
        user_id: user.0,
        email: user.1,
        display_name: user.2,
        github_username: user.3,
        avatar_url: user.4,
        role: payload.role,
        created_at: chrono::Utc::now().to_rfc3339(),
    }))
}

// Update project member role
pub async fn update_project_member(
    State(state): State<AppState>,
    Path((project_id, user_id)): Path<(String, String)>,
    Json(payload): Json<UpdateProjectMemberRequest>,
) -> Result<Json<ProjectMemberResponse>, StatusCode> {
    let project_uuid = Uuid::parse_str(&project_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let user_uuid = Uuid::parse_str(&user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Validate role
    if !["viewer", "editor", "owner"].contains(&payload.role.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Fetch user and member info
    let member: (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        r#"
        SELECT u.id, u.email, u.display_name, u.github_username, u.avatar_url, pm.created_at::text
        FROM project_members pm
        JOIN users u ON pm.user_id = u.id
        WHERE pm.project_id = $1 AND pm.user_id = $2 AND u.deleted_at IS NULL
        "#,
    )
    .bind(project_uuid)
    .bind(user_uuid)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Update role
    sqlx::query("UPDATE project_members SET role = $1 WHERE project_id = $2 AND user_id = $3")
        .bind(&payload.role)
        .bind(project_uuid)
        .bind(user_uuid)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProjectMemberResponse {
        user_id: member.0,
        email: member.1,
        display_name: member.2,
        github_username: member.3,
        avatar_url: member.4,
        role: payload.role,
        created_at: member.5,
    }))
}

// Remove member from project
pub async fn remove_project_member(
    State(state): State<AppState>,
    Path((project_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    let project_uuid = Uuid::parse_str(&project_id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let user_uuid = Uuid::parse_str(&user_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Verify membership exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM project_members WHERE project_id = $1 AND user_id = $2)",
    )
    .bind(project_uuid)
    .bind(user_uuid)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !exists {
        return Err(StatusCode::NOT_FOUND);
    }

    // Remove member
    sqlx::query("DELETE FROM project_members WHERE project_id = $1 AND user_id = $2")
        .bind(project_uuid)
        .bind(user_uuid)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

// ── Project Path Porting ───────────────────────────────────────────────────
// Permette di aggiornare in blocco i path dei progetti quando la directory
// di deploy viene spostata su un altro disco o percorso.

// PortProjectsRequest/Response/Detail: punto unico in nexus_types::admin_dto
// (regola L / ADR 0026, Wave C2). Vecchio prefisso path -> nuovo (deploy move).
pub use nexus_types::admin_dto::{PortDetail, PortProjectsRequest, PortProjectsResponse};

/// POST /api/admin/projects/port — aggiorna i path di tutti i progetti
pub async fn port_projects(
    State(state): State<AppState>,
    Json(payload): Json<PortProjectsRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let old_base = payload.old_base.trim_end_matches('/');
    let new_base = payload.new_base.trim_end_matches('/');

    if old_base.is_empty() || new_base.is_empty() {
        return Ok(Json(
            json!({ "error": "old_base e new_base sono obbligatori" }),
        ));
    }
    if old_base == new_base {
        return Ok(Json(
            json!({ "error": "old_base e new_base sono identici" }),
        ));
    }

    // Verifica che new_base esista come directory
    if !std::path::Path::new(new_base).is_dir() {
        return Ok(Json(json!({
            "error": format!("La directory '{}' non esiste o non è accessibile", new_base)
        })));
    }

    // Raccolta preview: workspaces da aggiornare
    let ws_rows =
        sqlx::query("SELECT id::text, absolute_path FROM workspaces WHERE absolute_path LIKE $1")
            .bind(format!("{}%", old_base))
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut details: Vec<PortDetail> = Vec::new();
    for row in &ws_rows {
        let id: String = row.get("id");
        let old_path: String = row.get("absolute_path");
        let new_path = old_path.replace(old_base, new_base);
        details.push(PortDetail {
            table: "workspaces".to_string(),
            id,
            old_path,
            new_path,
        });
    }

    // Raccolta preview: repositories da aggiornare
    let repo_rows =
        sqlx::query("SELECT id::text, root_path FROM repositories WHERE root_path LIKE $1")
            .bind(format!("{}%", old_base))
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    for row in &repo_rows {
        let id: String = row.get("id");
        let old_path: String = row.get("root_path");
        let new_path = old_path.replace(old_base, new_base);
        details.push(PortDetail {
            table: "repositories".to_string(),
            id,
            old_path,
            new_path,
        });
    }

    // Controlla setting projects_base_root
    let current_base_root: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'projects_base_root'")
            .fetch_optional(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let base_root_needs_update = current_base_root
        .as_deref()
        .map(|v| v.starts_with(old_base))
        .unwrap_or(false);

    if base_root_needs_update {
        let old_val = current_base_root.as_deref().unwrap_or("");
        let new_val = old_val.replace(old_base, new_base);
        details.push(PortDetail {
            table: "settings".to_string(),
            id: "projects_base_root".to_string(),
            old_path: old_val.to_string(),
            new_path: new_val,
        });
    }

    if payload.dry_run {
        return Ok(Json(json!(PortProjectsResponse {
            dry_run: true,
            projects_base_root_updated: base_root_needs_update,
            workspaces_updated: ws_rows.len() as i64,
            repositories_updated: repo_rows.len() as i64,
            details,
        })));
    }

    // ── Esecuzione effettiva ──
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Aggiorna workspaces
    let ws_affected = sqlx::query(
        "UPDATE workspaces SET absolute_path = REPLACE(absolute_path, $1, $2) WHERE absolute_path LIKE $3"
    )
    .bind(old_base)
    .bind(new_base)
    .bind(format!("{}%", old_base))
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .rows_affected() as i64;

    // Aggiorna repositories
    let repo_affected = sqlx::query(
        "UPDATE repositories SET root_path = REPLACE(root_path, $1, $2) WHERE root_path LIKE $3",
    )
    .bind(old_base)
    .bind(new_base)
    .bind(format!("{}%", old_base))
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .rows_affected() as i64;

    // Aggiorna setting projects_base_root
    let base_updated = if base_root_needs_update {
        sqlx::query(
            "UPDATE settings SET value = REPLACE(value, $1, $2), updated_at = NOW() WHERE key = 'projects_base_root'"
        )
        .bind(old_base)
        .bind(new_base)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        true
    } else {
        false
    };

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    tracing::info!(
        "Project porting: '{}' → '{}' — workspaces: {}, repos: {}, base_root: {}",
        old_base,
        new_base,
        ws_affected,
        repo_affected,
        base_updated
    );

    Ok(Json(json!(PortProjectsResponse {
        dry_run: false,
        projects_base_root_updated: base_updated,
        workspaces_updated: ws_affected,
        repositories_updated: repo_affected,
        details,
    })))
}
