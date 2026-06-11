//! Helper condiviso per `project_register_existing_dir` /
//! `project_register_from_git` (regola L): la transazione di registrazione
//! (projects, project_members, workspaces, repositories, quote) vive qui
//! una volta sola.

use super::NexusToolError;
use sqlx::PgPool;
use uuid::Uuid;

/// Dati del progetto da registrare nel DB Nexus.
pub struct NewProjectRecord<'a> {
    pub user_id: Uuid,
    pub name: &'a str,
    pub default_branch: &'a str,
    pub abs_path: &'a str,
    pub remote_url: Option<&'a str>,
    pub is_git_repo: bool,
}

/// Registra progetto + membership + workspace + repository + quote risorse
/// in una transazione atomica. Ritorna `(project_id, slug)`.
pub async fn register_project_records(
    pool: &PgPool,
    rec: &NewProjectRecord<'_>,
) -> Result<(Uuid, String), NexusToolError> {
    let project_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let repository_id = Uuid::new_v4();

    let team_id: Uuid =
        sqlx::query_scalar("SELECT id FROM teams WHERE owner_user_id = $1 LIMIT 1")
            .bind(rec.user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("lookup team: {}", e)))?
            .unwrap_or_else(Uuid::new_v4);

    let slug = format!(
        "{}-{}",
        rec.name.to_lowercase().replace(' ', "-"),
        &project_id.to_string()[..8]
    );

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| NexusToolError::BadInput(format!("begin tx: {}", e)))?;

    sqlx::query(
        r#"INSERT INTO projects (id, team_id, owner_user_id, name, slug, default_branch, visibility, last_opened_by_user_id)
           VALUES ($1, $2, $3, $4, $5, $6, 'private', $3)"#,
    )
    .bind(project_id)
    .bind(team_id)
    .bind(rec.user_id)
    .bind(rec.name)
    .bind(&slug)
    .bind(rec.default_branch)
    .execute(&mut *tx)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("insert projects: {}", e)))?;

    sqlx::query(
        "INSERT INTO project_members (id, project_id, user_id, role) VALUES ($1, $2, $3, 'owner')",
    )
    .bind(Uuid::new_v4())
    .bind(project_id)
    .bind(rec.user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("insert project_members: {}", e)))?;

    sqlx::query(
        "INSERT INTO workspaces (id, project_id, absolute_path, is_primary) VALUES ($1, $2, $3, TRUE)",
    )
    .bind(workspace_id)
    .bind(project_id)
    .bind(rec.abs_path)
    .execute(&mut *tx)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("insert workspaces: {}", e)))?;

    sqlx::query(
        r#"INSERT INTO repositories (id, project_id, provider, remote_url, root_path, is_git_repo, current_branch)
           VALUES ($1, $2, 'local', $3, $4, $5, $6)"#,
    )
    .bind(repository_id)
    .bind(project_id)
    .bind(rec.remote_url)
    .bind(rec.abs_path)
    .bind(rec.is_git_repo)
    .bind(rec.default_branch)
    .execute(&mut *tx)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("insert repositories: {}", e)))?;

    // Auto-provisioning quote risorse (PR hardening)
    let _ = sqlx::query(
        "INSERT INTO nexus_resource_quotas (project_id) VALUES ($1) ON CONFLICT (project_id) DO NOTHING",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await;

    tx.commit()
        .await
        .map_err(|e| NexusToolError::BadInput(format!("commit: {}", e)))?;

    Ok((project_id, slug))
}
