//! Tipi DTO condivisi tra `mcp-core` e `admin-service` per gli endpoint admin
//! (regola L / ADR 0026, Wave C2).
//!
//! Prima questi 17 tipi erano duplicati identicamente nei rispettivi `admin_*`
//! / `admin/*` di entrambi i crate. Ora vivono qui una volta sola, mentre la
//! LOGICA degli handler axum resta nei singoli crate (intenzionalmente: i due
//! servizi rispondono su path leggermente diversi e hanno tracing/auth diversi,
//! consolidarne gli handler richiederebbe una decisione architetturale piu'
//! grande - vedi nota Wave C2 in docs/tech-debt-dup.md).

use serde::{Deserialize, Serialize};

// -- Users --

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

// -- Project members --

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectMemberResponse {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    pub github_username: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct AddProjectMemberRequest {
    pub user_id: String,
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectMemberRequest {
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct ListProjectMembersResponse {
    pub project_id: String,
    pub members: Vec<ProjectMemberResponse>,
}

// -- Admin project listing --

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdminProjectSummary {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub owner_user_id: String,
    pub owner_email: Option<String>,
    pub member_count: i64,
}

#[derive(Debug, Serialize)]
pub struct ListAllProjectsResponse {
    pub projects: Vec<AdminProjectSummary>,
}

// -- Port projects (rebase delle workspace e dei repo verso un nuovo prefisso) --

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortProjectsRequest {
    pub old_base: String,
    pub new_base: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortProjectsResponse {
    pub dry_run: bool,
    pub projects_base_root_updated: bool,
    pub workspaces_updated: i64,
    pub repositories_updated: i64,
    pub details: Vec<PortDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortDetail {
    pub table: String,
    pub id: String,
    pub old_path: String,
    pub new_path: String,
}
