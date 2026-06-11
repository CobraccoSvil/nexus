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
use sqlx::PgPool;
use uuid::Uuid;

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

// -- Query SQL condivise (regola L / ADR 0026, step S63) --

/// SELECT progetti con member_count per la view `list_all_projects` dell'admin.
/// Punto unico fra admin-service e mcp-core (prima 28L cluster jscpd).
pub async fn fetch_all_projects_summary(
    db: &PgPool,
) -> Result<Vec<AdminProjectSummary>, sqlx::Error> {
    let rows: Vec<(String, String, String, String, Option<String>, i64)> = sqlx::query_as(
        r#"
        SELECT
            p.id::text,
            p.name,
            p.slug,
            p.owner_user_id::text,
            u.email,
            (SELECT COUNT(*)::bigint FROM project_members pm WHERE pm.project_id = p.id)
        FROM projects p
        LEFT JOIN users u ON u.id = p.owner_user_id AND u.deleted_at IS NULL
        ORDER BY p.name ASC
        "#,
    )
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, name, slug, owner_user_id, owner_email, member_count)| AdminProjectSummary {
                id,
                name,
                slug,
                owner_user_id,
                owner_email,
                member_count,
            },
        )
        .collect())
}

/// Riga utente come tornata dalle query admin (id, email, display_name,
/// github_username, avatar_url, role, created_at).
type UserRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn map_user_row(
    (id, email, display_name, github_username, avatar_url, role, created_at): UserRow,
) -> UserResponse {
    UserResponse {
        id,
        email,
        display_name,
        github_username,
        avatar_url,
        role,
        created_at,
    }
}

/// Fetch utente singolo + progetti per la view `get_user` dell'admin.
/// Punto unico fra admin-service e mcp-core (cluster jscpd E6).
/// `Ok(None)` = utente inesistente o soft-deleted.
///
/// NOTA: la query progetti conserva la semantica storica `.unwrap_or_default()`
/// (errore DB -> lista vuota) per parita' di comportamento con i due handler
/// originali (refactor puro, regola sul comportamento identico).
pub async fn fetch_user_with_projects(
    db: &PgPool,
    user_uuid: Uuid,
) -> Result<Option<UserWithProjectsResponse>, sqlx::Error> {
    let row: Option<UserRow> = sqlx::query_as(
        "SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text FROM users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(user_uuid)
    .fetch_optional(db)
    .await?;

    let Some(user_row) = row else {
        return Ok(None);
    };

    let projects: Vec<UserProjectRole> = sqlx::query_as::<_, (String, String, String)>(
        "SELECT p.id, p.name, pm.role FROM project_members pm JOIN projects p ON pm.project_id = p.id WHERE pm.user_id = $1 ORDER BY p.name",
    )
    .bind(user_uuid)
    .fetch_all(db)
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

    Ok(Some(UserWithProjectsResponse {
        user: map_user_row(user_row),
        project_count,
        projects,
    }))
}

/// Ricerca utenti per email/display_name/github_username (LIKE case-insensitive,
/// max 50 risultati). Punto unico fra admin-service e mcp-core (cluster jscpd E6).
pub async fn search_users_like(db: &PgPool, q: &str) -> Result<Vec<UserResponse>, sqlx::Error> {
    let pattern = format!("%{}%", q.to_lowercase());
    let rows: Vec<UserRow> = sqlx::query_as(
        r#"
        SELECT id::text, email, display_name, github_username, avatar_url, role, created_at::text
        FROM users
        WHERE deleted_at IS NULL AND (
            LOWER(email) LIKE $1 OR LOWER(display_name) LIKE $1 OR LOWER(github_username) LIKE $1
        )
        ORDER BY created_at DESC
        LIMIT 50
        "#,
    )
    .bind(&pattern)
    .fetch_all(db)
    .await?;

    Ok(rows.into_iter().map(map_user_row).collect())
}

/// SELECT membri di un progetto (join con users). Propaga l'errore SQL al
/// chiamante (regola H: fail-loud invece di ingoiare). Punto unico per
/// `list_project_members` admin.
///
/// NOTA: l'originale (sia in mcp-core sia in admin-service) usava
/// `.unwrap_or_default()` mascherando errori DB. Spostato qui nel punto unico,
/// quella semantica era un bug latente: l'admin vedeva "nessun membro" sia
/// per progetti realmente vuoti sia quando il DB era irraggiungibile o la
/// query falliva. Ora il chiamante DEVE propagare l'errore (es. HTTP 500).
pub async fn fetch_project_members(
    db: &PgPool,
    project_uuid: Uuid,
) -> Result<Vec<ProjectMemberResponse>, sqlx::Error> {
    let rows: Vec<(String, String, String, Option<String>, Option<String>, String, String)> =
        sqlx::query_as(
            r#"
            SELECT u.id, u.email, u.display_name, u.github_username, u.avatar_url, pm.role, pm.created_at::text
            FROM project_members pm
            JOIN users u ON pm.user_id = u.id
            WHERE pm.project_id = $1 AND u.deleted_at IS NULL
            ORDER BY pm.created_at DESC
            "#,
        )
        .bind(project_uuid)
        .fetch_all(db)
        .await?;

    Ok(rows
        .into_iter()
        .map(
            |(user_id, email, display_name, github_username, avatar_url, role, created_at)| {
                ProjectMemberResponse {
                    user_id,
                    email,
                    display_name,
                    github_username,
                    avatar_url,
                    role,
                    created_at,
                }
            },
        )
        .collect())
}
