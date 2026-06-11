// Modulo projects — entry point della sottodirectory.
// Contiene: use condivisi, tipi/struct, helper privati, dichiarazioni sotto-moduli e re-export.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};


use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tokio::{fs, process::Command};
use uuid::Uuid;

use crate::{auth::Claims, vector_memory, AppState};

// ── Tipi alias condivisi ──────────────────────────────────────────────────────

pub(crate) type ApiError = (StatusCode, Json<Value>);
pub(crate) type ApiResult = Result<Json<Value>, ApiError>;

// Helper di identita'/errore HTTP: il punto unico e' in nexus-types
// (regola L / ADR 0026). Qui solo re-export per i call site interni che usano
// `crate::projects::{api_error, parse_user_id}`.
pub(crate) use nexus_types::{api_error, parse_user_id};

// ── Costanti condivise ────────────────────────────────────────────────────────

pub(crate) const EXCLUDED_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".next",
    "dist",
    "coverage",
    ".turbo",
    "__pycache__",
];

/// Estensioni dei file indicizzati nella knowledge base (chunk + embedding nel
/// vector store, reindex real-time del file watcher, deep review).
///
/// Punto unico (regola L / ADR 0026): prima questa lista era DUPLICATA in
/// `indexing.rs` (x4), `file_watcher.rs` e `deep_review.rs`; aggiungere
/// un'estensione richiedeva di toccarle tutte. Include i linguaggi di
/// programmazione E il markup `html`/`htm` (le pagine sono contenuto
/// indicizzabile e ricercabile semanticamente nella KB del progetto).
pub(crate) const CODE_EXTENSIONS: &[&str] = &[
    "tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue", "html", "htm",
];

// ── Struct request/response pubbliche ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterProjectRequest {
    pub absolute_path: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TreeQuery {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FsBrowseQuery {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDirectoryRequest {
    pub parent_path: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SaveFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateEntryRequest {
    pub path: String,
    pub kind: String,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenameEntryRequest {
    pub old_path: String,
    pub new_path: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteEntryRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct GitPathsRequest {
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GitCommitRequest {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct GitCheckoutRequest {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct GitCreateBranchRequest {
    pub name: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct GitRemoteRequest {
    pub remote: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct GitDiffQuery {
    pub path: String,
    pub staged: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct GitUiPreferencesUpdateRequest {
    pub show_hunk_map: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchStateUpdateRequest {
    pub state: Value,
    pub active_file_paths: Option<Vec<String>>,
    pub terminal_cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStreamQuery {
    pub consumer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAckRequest {
    pub consumer_id: String,
    pub delivered: bool,
    pub output_preview: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFinishRequest {
    pub consumer_id: String,
    pub exit_code: Option<i32>,
    pub full_output: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPresenceRequest {
    pub consumer_id: String,
    pub connected: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectAccessPolicy {
    pub(crate) current_user_role: String,
    pub(crate) can_write: bool,
    pub(crate) can_manage_git: bool,
    pub(crate) is_shared: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserProjectSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) owner_user_id: String,
    pub(crate) current_user_role: String,
    pub(crate) can_write: bool,
    pub(crate) can_manage_git: bool,
    pub(crate) is_shared: bool,
    pub(crate) visibility: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) root_path: Option<String>,
    pub(crate) is_git_repo: bool,
    pub(crate) current_branch: Option<String>,
    pub(crate) last_opened_at: Option<String>,
    pub(crate) analyzed_at: Option<String>,
    pub(crate) is_analyzed: bool,
    pub(crate) nexus_ready: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserProjectDetails {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) owner_user_id: String,
    pub(crate) visibility: String,
    pub(crate) current_user_role: String,
    pub(crate) can_write: bool,
    pub(crate) can_manage_git: bool,
    pub(crate) is_shared: bool,
    pub(crate) workspace_id: Option<String>,
    pub(crate) root_path: Option<String>,
    pub(crate) repository_root_path: Option<String>,
    pub(crate) is_git_repo: bool,
    pub(crate) current_branch: Option<String>,
    pub(crate) analyzed_at: Option<String>,
    pub(crate) is_analyzed: bool,
    pub(crate) nexus_ready: bool,
    pub(crate) default_profile_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceTreeNode {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) has_children: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowseDirectoryNode {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) has_children: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitBranchInfo {
    pub(crate) name: String,
    pub(crate) is_current: bool,
    pub(crate) upstream: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitLogEntry {
    pub(crate) commit: String,
    pub(crate) short_commit: String,
    pub(crate) author: String,
    pub(crate) date: String,
    pub(crate) subject: String,
    pub(crate) body: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitFileChange {
    pub(crate) path: String,
    pub(crate) staged_status: String,
    pub(crate) worktree_status: String,
    pub(crate) kind: String,
    pub(crate) staged: bool,
    pub(crate) unstaged: bool,
    pub(crate) untracked: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GitRepositoryState {
    pub(crate) is_git_repo: bool,
    pub(crate) current_branch: Option<String>,
    pub(crate) staged: Vec<GitFileChange>,
    pub(crate) unstaged: Vec<GitFileChange>,
    pub(crate) untracked: Vec<GitFileChange>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectContext {
    pub(crate) project_id: Uuid,
    pub(crate) workspace_id: Option<Uuid>,
    pub(crate) root_path: PathBuf,
    pub(crate) repository_root_path: PathBuf,
    pub(crate) is_git_repo: bool,
    pub(crate) current_branch: Option<String>,
    pub(crate) access: ProjectAccessPolicy,
    pub(crate) details: UserProjectDetails,
}

// Punto unico esecuzione git: nexus_types::git_exec (regola L).
pub(crate) use nexus_types::git_exec::{
    run_git_command, run_git_command_with_options, GitCommandOptions,
};

#[derive(Debug)]
pub(crate) struct GitRepoInfo {
    pub(crate) is_git_repo: bool,
    pub(crate) root_path: PathBuf,
    pub(crate) current_branch: Option<String>,
    pub(crate) remotes: Vec<(String, String, String)>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalSessionResponse {
    pub(crate) session_id: String,
    pub(crate) token: String,
    pub(crate) working_directory: String,
    pub(crate) shell: String,
    pub(crate) expires_at: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct TerminalSessionClaims<'a> {
    pub(crate) sid: &'a str,
    pub(crate) uid: &'a str,
    pub(crate) pid: &'a str,
    pub(crate) root: &'a str,
    pub(crate) cwd: &'a str,
    pub(crate) shell: &'a str,
    pub(crate) exp: u64,
}

// ── Struct per generate_system_prompt ─────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GeneratePromptRequest {
    pub profile_name: String,
    pub description: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

// ── Helper pubblici condivisi ─────────────────────────────────────────────────

pub(crate) fn terminal_shell() -> String {
    if cfg!(windows) {
        std::env::var("TERMINAL_SHELL").unwrap_or_else(|_| "powershell.exe".to_string())
    } else {
        std::env::var("TERMINAL_SHELL").unwrap_or_else(|_| "bash".to_string())
    }
}

pub(crate) async fn terminal_session_secret(db: &PgPool) -> String {
    if let Ok(secret) = std::env::var("TERMINAL_SESSION_SECRET") {
        if !secret.trim().is_empty() {
            return secret;
        }
    }

    if let Ok(Some(secret)) = sqlx::query_scalar::<_, String>(
        "SELECT value FROM settings WHERE key = 'jwt_secret' AND value IS NOT NULL AND value <> ''",
    )
    .fetch_optional(db)
    .await
    {
        return secret;
    }

    "development-terminal-secret-change-me".to_string()
}

pub(crate) fn sign_terminal_token(payload_base64: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":");
    hasher.update(payload_base64.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(crate) fn terminal_consumer_key(user_id: Uuid, project_id: Uuid) -> String {
    format!("{user_id}:{project_id}")
}

// Punto unico path-safety workspace: nexus_types::workspace_paths (regola L).
pub(crate) use nexus_types::workspace_paths::path_within;

pub(crate) fn to_relative(root: &Path, target: &Path) -> String {
    target
        .strip_prefix(root)
        .unwrap_or(target)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) async fn load_projects_base_root(db: &PgPool) -> Result<PathBuf, ApiError> {
    let value = sqlx::query_scalar::<_, String>(
        r#"
        SELECT value
        FROM settings
        WHERE key = 'projects_base_root'
        "#,
    )
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_default();

    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        let default_root = std::env::current_dir()
            .map(|cwd| cwd.join("projects"))
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        std::fs::create_dir_all(&default_root)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let canonical = default_root
            .canonicalize()
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let canonical_str = canonical.to_string_lossy().to_string();

        sqlx::query(
            r#"
            INSERT INTO settings (key, value, category, description, is_secret, updated_at)
            VALUES (
                'projects_base_root',
                $1,
                'infrastructure',
                'Root assoluta sotto cui e'' consentita la registrazione/navigazione dei progetti',
                FALSE,
                NOW()
            )
            ON CONFLICT (key)
            DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
            "#,
        )
        .bind(&canonical_str)
        .execute(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        return Ok(canonical);
    }

    let canonical = PathBuf::from(trimmed).canonicalize().map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "La setting 'projects_base_root' non punta a una directory valida",
        )
    })?;

    if !canonical.is_dir() {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "La setting 'projects_base_root' deve essere una directory esistente",
        ));
    }

    Ok(canonical)
}

pub(crate) fn resolve_relative_path(root: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    let raw = relative.trim();
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let target = if raw.is_empty() {
        root_canonical.clone()
    } else {
        let as_path = PathBuf::from(raw);
        if as_path.is_absolute() {
            as_path
        } else {
            let clean = raw.trim_start_matches(['\\', '/']);
            root_canonical.join(clean)
        }
    };

    let canonical = target
        .canonicalize()
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "Percorso non trovato"))?;

    if !path_within(&root_canonical, &canonical) {
        return Err(api_error(StatusCode::FORBIDDEN, "Percorso non autorizzato"));
    }

    Ok(canonical)
}

/// Adapter HTTP del punto unico `nexus_types::workspace_paths` (regola L):
/// stessa logica, errore neutro mappato su StatusCode per i call site axum.
pub(crate) fn resolve_workspace_target(
    root: &Path,
    relative: &str,
) -> Result<(String, PathBuf), ApiError> {
    use nexus_types::workspace_paths::WorkspaceTargetError;
    nexus_types::workspace_paths::resolve_workspace_target(root, relative).map_err(|e| {
        let status = match e {
            WorkspaceTargetError::OutsideRoot => StatusCode::FORBIDDEN,
            WorkspaceTargetError::EmptyPath | WorkspaceTargetError::InvalidChars => {
                StatusCode::BAD_REQUEST
            }
        };
        api_error(status, e.message())
    })
}


pub(crate) async fn record_git_operation(
    db: &PgPool,
    user_id: Uuid,
    context: &ProjectContext,
    operation: &str,
    status: &str,
    stdout: &str,
    stderr: &str,
    metadata: Value,
) {
    let _ = sqlx::query(
        r#"
        INSERT INTO git_operations (user_id, project_id, workspace_id, branch, operation, status, stdout, stderr, metadata)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(user_id)
    .bind(context.project_id)
    .bind(context.workspace_id)
    .bind(context.current_branch.clone())
    .bind(operation)
    .bind(status)
    .bind(stdout)
    .bind(stderr)
    .bind(metadata)
    .execute(db)
    .await;
}

pub(crate) fn parse_branch_line(line: &str) -> Option<GitBranchInfo> {
    let mut parts = line.split('\t');
    let name = parts.next()?.trim();
    let head = parts.next().unwrap_or_default().trim();
    let upstream = parts.next().unwrap_or_default().trim();

    Some(GitBranchInfo {
        name: name.to_string(),
        is_current: head == "*",
        upstream: if upstream.is_empty() {
            None
        } else {
            Some(upstream.to_string())
        },
    })
}

pub(crate) fn parse_git_status_porcelain(output: &str, is_git_repo: bool) -> GitRepositoryState {
    if !is_git_repo {
        return GitRepositoryState {
            is_git_repo: false,
            current_branch: None,
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
        };
    }

    let mut current_branch = None;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for (idx, line) in output.lines().enumerate() {
        if idx == 0 && line.starts_with("## ") {
            let header = line.trim_start_matches("## ");
            let branch = header
                .split("...")
                .next()
                .unwrap_or(header)
                .split_whitespace()
                .next()
                .unwrap_or(header);
            current_branch = Some(branch.to_string());
            continue;
        }

        if line.len() < 4 {
            continue;
        }

        let staged_status = line[0..1].to_string();
        let worktree_status = line[1..2].to_string();
        let raw_path = line[3..].trim();
        let path = raw_path
            .split(" -> ")
            .last()
            .unwrap_or(raw_path)
            .to_string();
        let untracked_flag = staged_status == "?" && worktree_status == "?";
        let staged_flag = staged_status != " " && staged_status != "?";
        let unstaged_flag = worktree_status != " " && worktree_status != "?";
        let kind = if untracked_flag {
            "untracked"
        } else if staged_status == "D" || worktree_status == "D" {
            "deleted"
        } else if staged_status == "R" || worktree_status == "R" {
            "renamed"
        } else if staged_status == "A" {
            "added"
        } else {
            "modified"
        };

        let change = GitFileChange {
            path,
            staged_status,
            worktree_status,
            kind: kind.to_string(),
            staged: staged_flag,
            unstaged: unstaged_flag,
            untracked: untracked_flag,
        };

        if untracked_flag {
            untracked.push(change);
        } else {
            if staged_flag {
                staged.push(change.clone());
            }
            if unstaged_flag {
                unstaged.push(change);
            }
        }
    }

    GitRepositoryState {
        is_git_repo,
        current_branch,
        staged,
        unstaged,
        untracked,
    }
}

pub(crate) async fn refresh_git_snapshot(
    db: &PgPool,
    context: &ProjectContext,
) -> Result<GitRepositoryState, ApiError> {
    if !context.is_git_repo {
        return Ok(GitRepositoryState {
            is_git_repo: false,
            current_branch: None,
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
        });
    }

    let (stdout, _) = run_git_command(
        &context.repository_root_path,
        &["status", "--porcelain=1", "--branch"],
    )
    .await
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    let state = parse_git_status_porcelain(&stdout, true);
    let status_json = serde_json::to_value(&state)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let _ = sqlx::query(
        r#"
        INSERT INTO git_status_snapshots (project_id, workspace_id, branch, status_json)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(context.project_id)
    .bind(context.workspace_id)
    .bind(state.current_branch.clone())
    .bind(status_json)
    .execute(db)
    .await;

    let _ = sqlx::query("UPDATE repositories SET current_branch = $1 WHERE project_id = $2")
        .bind(state.current_branch.clone())
        .bind(context.project_id)
        .execute(db)
        .await;

    Ok(state)
}

pub(crate) async fn load_project_context(
    db: &PgPool,
    project_id: Uuid,
    user_id: Uuid,
) -> Result<ProjectContext, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT
            p.id,
            p.name,
            p.slug,
            p.owner_user_id,
            p.visibility,
            p.default_branch,
            p.analyzed_at,
            p.default_profile_id,
            w.id AS workspace_id,
            w.absolute_path,
            r.root_path,
            COALESCE(r.is_git_repo, FALSE) AS is_git_repo,
            COALESCE(r.current_branch, p.default_branch) AS current_branch,
            CASE
                WHEN p.owner_user_id = $2 THEN 'owner'
                ELSE pm.role
            END AS current_user_role
        FROM projects p
        LEFT JOIN project_members pm
            ON pm.project_id = p.id AND pm.user_id = $2
        LEFT JOIN workspaces w
            ON w.project_id = p.id AND w.is_primary = TRUE
        LEFT JOIN repositories r
            ON r.project_id = p.id
        WHERE p.id = $1
          AND (p.owner_user_id = $2 OR pm.user_id IS NOT NULL)
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let Some(row) = row else {
        return Err(api_error(StatusCode::FORBIDDEN, "Progetto non accessibile"));
    };

    let role = row
        .try_get::<Option<String>, _>("current_user_role")
        .ok()
        .flatten()
        .unwrap_or_else(|| "viewer".to_string());
    let access = map_access(&role);

    let root_path = row
        .try_get::<Option<String>, _>("absolute_path")
        .ok()
        .flatten()
        .map(PathBuf::from)
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "Workspace principale non configurato",
            )
        })?;
    let mut repository_root_path = row
        .try_get::<Option<String>, _>("root_path")
        .ok()
        .flatten()
        .map(PathBuf::from)
        .unwrap_or_else(|| root_path.clone());
    let mut is_git_repo = row.get::<bool, _>("is_git_repo");
    let mut current_branch = row
        .try_get::<Option<String>, _>("current_branch")
        .ok()
        .flatten();

    if !is_git_repo {
        let detected = detect_git_repo(&root_path).await;
        if detected.is_git_repo {
            is_git_repo = true;
            repository_root_path = detected.root_path.clone();
            current_branch = detected.current_branch.clone().or(current_branch);
            let remote_url = detected.remotes.iter().find_map(|(_, fetch_url, _)| {
                let trimmed = fetch_url.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });

            let update_result = sqlx::query(
                r#"
                UPDATE repositories
                SET is_git_repo = TRUE,
                    root_path = $2,
                    current_branch = COALESCE($3, current_branch),
                    remote_url = COALESCE($4, remote_url)
                WHERE project_id = $1
                "#,
            )
            .bind(project_id)
            .bind(repository_root_path.to_string_lossy().to_string())
            .bind(current_branch.clone())
            .bind(remote_url.clone())
            .execute(db)
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            if update_result.rows_affected() == 0 {
                sqlx::query(
                    r#"
                    INSERT INTO repositories (id, project_id, provider, remote_url, root_path, is_git_repo, current_branch)
                    VALUES ($1, $2, 'local', $3, $4, TRUE, $5)
                    "#,
                )
                .bind(Uuid::new_v4())
                .bind(project_id)
                .bind(remote_url)
                .bind(repository_root_path.to_string_lossy().to_string())
                .bind(current_branch.clone())
                .execute(db)
                .await
                .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            }
        }
    }

    // Fix M20: la verifica root e' un constraint di CREATE (register/clone), non di LOAD.
    // Se il record esiste in DB significa che a suo tempo e' passato dalla verifica.
    // Bloccare LOAD/DELETE per progetti registrati prima di un cambio di
    // `projects_base_root` (es. Fix M5) rende impossibile la cancellazione legittima
    // dei progetti pre-esistenti dalla UI di Nexus. Le scritture sul filesystem
    // restano protette dai path_within sulla project root, non dalla root globale.

    let details = UserProjectDetails {
        id: row.get::<Uuid, _>("id").to_string(),
        name: row.get::<String, _>("name"),
        slug: row.get::<String, _>("slug"),
        owner_user_id: row.get::<Uuid, _>("owner_user_id").to_string(),
        visibility: row.get::<String, _>("visibility"),
        current_user_role: access.current_user_role.clone(),
        can_write: access.can_write,
        can_manage_git: access.can_manage_git,
        is_shared: access.is_shared,
        workspace_id: row
            .try_get::<Option<Uuid>, _>("workspace_id")
            .ok()
            .flatten()
            .map(|id| id.to_string()),
        root_path: Some(root_path.to_string_lossy().to_string()),
        repository_root_path: Some(repository_root_path.to_string_lossy().to_string()),
        is_git_repo,
        current_branch: current_branch.clone(),
        analyzed_at: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("analyzed_at")
            .ok()
            .flatten()
            .map(|ts| ts.to_rfc3339()),
        is_analyzed: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("analyzed_at")
            .ok()
            .flatten()
            .is_some(),
        nexus_ready: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("analyzed_at")
            .ok()
            .flatten()
            .is_some(),
        default_profile_id: row
            .try_get::<Option<Uuid>, _>("default_profile_id")
            .ok()
            .flatten()
            .map(|id| id.to_string()),
    };

    Ok(ProjectContext {
        project_id,
        workspace_id: row
            .try_get::<Option<Uuid>, _>("workspace_id")
            .ok()
            .flatten(),
        root_path,
        repository_root_path,
        is_git_repo,
        current_branch,
        access,
        details,
    })
}

pub(crate) async fn upsert_open_session(
    db: &PgPool,
    user_id: Uuid,
    context: &ProjectContext,
    active_file_paths: &[String],
    terminal_cwd: Option<&str>,
) -> Result<(), ApiError> {
    let active_files_json = serde_json::to_value(active_file_paths)
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        r#"
        INSERT INTO project_open_sessions (user_id, project_id, workspace_id, active_file_paths, terminal_cwd, last_opened_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
        ON CONFLICT (user_id, project_id) DO UPDATE
        SET workspace_id = EXCLUDED.workspace_id,
            active_file_paths = EXCLUDED.active_file_paths,
            terminal_cwd = EXCLUDED.terminal_cwd,
            last_opened_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(context.project_id)
    .bind(context.workspace_id)
    .bind(active_files_json)
    .bind(terminal_cwd)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query("UPDATE projects SET last_opened_by_user_id = $1 WHERE id = $2")
        .bind(user_id)
        .bind(context.project_id)
        .execute(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(())
}

pub(crate) fn list_directory_nodes(
    root: &Path,
    target: &Path,
) -> Result<Vec<WorkspaceTreeNode>, ApiError> {
    let read_dir = std::fs::read_dir(target)
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "Directory non trovata"))?;

    let mut nodes = read_dir
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            !EXCLUDED_NAMES.contains(&name.as_str())
        })
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            let kind = if metadata.is_dir() {
                "directory"
            } else {
                "file"
            };
            let has_children = if metadata.is_dir() {
                std::fs::read_dir(&path)
                    .ok()
                    .map(|entries| {
                        entries.filter_map(|item| item.ok()).any(|item| {
                            let name = item.file_name().to_string_lossy().to_string();
                            !EXCLUDED_NAMES.contains(&name.as_str())
                        })
                    })
                    .unwrap_or(false)
            } else {
                false
            };

            Some(WorkspaceTreeNode {
                name: entry.file_name().to_string_lossy().to_string(),
                path: to_relative(root, &path),
                kind: kind.to_string(),
                has_children,
            })
        })
        .collect::<Vec<_>>();

    nodes.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.name.cmp(&right.name)));
    Ok(nodes)
}

pub(crate) async fn execute_git_paths_operation(
    db: &PgPool,
    user_id: Uuid,
    context: &ProjectContext,
    operation: &str,
    args: &[&str],
    paths: &[String],
) -> ApiResult {
    if !context.access.can_manage_git {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi Git su questo progetto",
        ));
    }
    if !context.is_git_repo {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il progetto selezionato non e' un repository Git",
        ));
    }

    let mut full_args = args.to_vec();
    if !paths.is_empty() {
        full_args.push("--");
        for path in paths {
            full_args.push(path.as_str());
        }
    }

    match run_git_command(&context.repository_root_path, &full_args).await {
        Ok((stdout, stderr)) => {
            record_git_operation(
                db,
                user_id,
                context,
                operation,
                "success",
                &stdout,
                &stderr,
                json!({ "paths": paths }),
            )
            .await;
            let git_state = refresh_git_snapshot(db, context).await?;
            Ok(Json(json!({ "ok": true, "git": git_state })))
        }
        Err(error) => {
            record_git_operation(
                db,
                user_id,
                context,
                operation,
                "error",
                "",
                &error.to_string(),
                json!({ "paths": paths }),
            )
            .await;
            Err(api_error(StatusCode::BAD_REQUEST, error.to_string()))
        }
    }
}

pub(crate) async fn execute_git_remote_operation(
    db: &PgPool,
    user_id: Uuid,
    context: &ProjectContext,
    operation: &str,
    body: GitRemoteRequest,
) -> ApiResult {
    if !context.access.can_manage_git {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "Non hai permessi Git su questo progetto",
        ));
    }
    if !context.is_git_repo {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il progetto selezionato non e' un repository Git",
        ));
    }

    let mut args = vec![operation];
    if let Some(remote) = body
        .remote
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        args.push(remote);
    }
    if let Some(branch) = body
        .branch
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        args.push(branch);
    }

    let git_options = crate::github::resolve_github_git_command_options(
        db,
        user_id,
        &context.repository_root_path,
        body.remote.as_deref(),
    )
    .await?;

    match run_git_command_with_options(&context.repository_root_path, &args, &git_options).await {
        Ok((stdout, stderr)) => {
            record_git_operation(
                db,
                user_id,
                context,
                operation,
                "success",
                &stdout,
                &stderr,
                json!({ "remote": body.remote, "branch": body.branch }),
            )
            .await;
            let git_state = refresh_git_snapshot(db, context).await?;
            Ok(Json(json!({ "ok": true, "git": git_state })))
        }
        Err(error) => {
            record_git_operation(
                db,
                user_id,
                context,
                operation,
                "error",
                "",
                &error.to_string(),
                json!({ "remote": body.remote, "branch": body.branch }),
            )
            .await;
            Err(api_error(StatusCode::BAD_REQUEST, error.to_string()))
        }
    }
}

pub(crate) async fn load_user_project_preferences(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<Value, ApiError> {
    let preferences = sqlx::query_scalar::<_, Value>(
        r#"
        SELECT preferences
        FROM user_project_preferences
        WHERE user_id = $1 AND project_id = $2
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .unwrap_or_else(|| json!({}));
    Ok(preferences)
}

pub(crate) async fn save_user_project_preferences(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    preferences: Value,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO user_project_preferences (id, user_id, project_id, preferences, created_at, updated_at)
        VALUES (gen_random_uuid(), $1, $2, $3, NOW(), NOW())
        ON CONFLICT (user_id, project_id)
        DO UPDATE SET preferences = EXCLUDED.preferences, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(project_id)
    .bind(preferences)
    .execute(db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(())
}

// ── Helper privati (usati da più sotto-moduli, esposti come pub(super)) ───────

pub(super) fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut last_dash = false;

    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

pub(super) fn extra_roots_allowed(path: &Path) -> bool {
    let Ok(extra) = std::env::var("NEXUS_EXTRA_ROOTS") else {
        return false;
    };
    for raw in extra.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let extra_path = PathBuf::from(trimmed);
        let canonical_extra = match extra_path.canonicalize() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if path_within(&canonical_extra, path) {
            return true;
        }
    }
    false
}

pub(super) async fn assert_allowed_workspace(db: &PgPool, path: &Path) -> Result<(), ApiError> {
    let base_root = load_projects_base_root(db).await?;
    if path_within(&base_root, path) {
        return Ok(());
    }

    if extra_roots_allowed(path) {
        return Ok(());
    }

    Err(api_error(
        StatusCode::FORBIDDEN,
        "La directory selezionata e' fuori dalla root progetti configurata",
    ))
}

pub(super) async fn resolve_browse_path(
    db: &PgPool,
    target: Option<&str>,
) -> Result<(PathBuf, PathBuf), ApiError> {
    let base_root = load_projects_base_root(db).await?;
    let current = if let Some(raw) = target {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            base_root.clone()
        } else {
            PathBuf::from(trimmed)
                .canonicalize()
                .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Percorso directory non valido"))?
        }
    } else {
        base_root.clone()
    };

    if !current.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il percorso selezionato non e' una directory",
        ));
    }

    if !path_within(&base_root, &current) {
        let allowed_extra = extra_roots_allowed(&current);
        if !allowed_extra {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "Non e' possibile uscire dalla root progetti configurata",
            ));
        }
    }

    Ok((current, base_root))
}

pub(super) fn list_browse_directories(target: &Path) -> Vec<BrowseDirectoryNode> {
    let mut directories = std::fs::read_dir(target)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if EXCLUDED_NAMES.contains(&name.as_str()) {
                return None;
            }

            let has_children = std::fs::read_dir(&path)
                .ok()
                .map(|children| {
                    children.filter_map(|child| child.ok()).any(|child| {
                        let child_name = child.file_name().to_string_lossy().to_string();
                        if EXCLUDED_NAMES.contains(&child_name.as_str()) {
                            return false;
                        }
                        child.metadata().map(|m| m.is_dir()).unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            Some(BrowseDirectoryNode {
                name,
                path: path.to_string_lossy().to_string(),
                has_children,
            })
        })
        .collect::<Vec<_>>();

    directories.sort_by(|left, right| left.name.cmp(&right.name));
    directories
}

pub(super) fn validate_directory_name(name: &str) -> Result<&str, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome della directory e' obbligatorio",
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome della directory non e' valido",
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Il nome della directory non puo' contenere separatori di percorso",
        ));
    }
    Ok(trimmed)
}

pub(super) fn map_create_dir_error(error: std::io::Error) -> ApiError {
    let status = match error.kind() {
        std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.to_string())
}

pub(super) fn map_access(role: &str) -> ProjectAccessPolicy {
    let can_write = matches!(role, "owner" | "maintainer" | "developer");
    ProjectAccessPolicy {
        current_user_role: role.to_string(),
        can_write,
        can_manage_git: can_write,
        is_shared: role != "owner",
    }
}

pub(super) async fn ensure_personal_team(db: &PgPool, user_id: Uuid) -> Result<Uuid, ApiError> {
    let slug = format!("user-{}", user_id.simple());

    if let Some(existing) = sqlx::query_scalar::<_, Uuid>("SELECT id FROM teams WHERE slug = $1")
        .bind(&slug)
        .fetch_optional(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Ok(existing);
    }

    let team_id = Uuid::new_v4();
    sqlx::query("INSERT INTO teams (id, name, slug) VALUES ($1, $2, $3)")
        .bind(team_id)
        .bind(format!("Workspace {}", &user_id.simple().to_string()[..8]))
        .bind(&slug)
        .execute(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(team_id)
}

pub(super) async fn ensure_unique_slug(
    db: &PgPool,
    owner_user_id: Uuid,
    base: &str,
) -> Result<String, ApiError> {
    let base_slug = {
        let slug = slugify(base);
        if slug.is_empty() {
            "project".to_string()
        } else {
            slug
        }
    };

    let mut attempt = 0;
    loop {
        let candidate = if attempt == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{attempt}")
        };

        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM projects
                WHERE owner_user_id = $1 AND slug = $2
            )
            "#,
        )
        .bind(owner_user_id)
        .bind(&candidate)
        .fetch_one(db)
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if !exists {
            return Ok(candidate);
        }
        attempt += 1;
    }
}

pub(super) async fn detect_git_repo(path: &Path) -> GitRepoInfo {
    let root_output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .await;

    let Ok(root_output) = root_output else {
        return GitRepoInfo {
            is_git_repo: false,
            root_path: path.to_path_buf(),
            current_branch: None,
            remotes: Vec::new(),
        };
    };

    if !root_output.status.success() {
        return GitRepoInfo {
            is_git_repo: false,
            root_path: path.to_path_buf(),
            current_branch: None,
            remotes: Vec::new(),
        };
    }

    let root_path = PathBuf::from(
        String::from_utf8_lossy(&root_output.stdout)
            .trim()
            .to_string(),
    );

    let current_branch = Command::new("git")
        .arg("-C")
        .arg(&root_path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty());

    let remotes_output = Command::new("git")
        .arg("-C")
        .arg(&root_path)
        .arg("remote")
        .arg("-v")
        .output()
        .await
        .ok();

    let mut remotes_by_name: BTreeMap<String, (String, String)> = BTreeMap::new();
    if let Some(output) = remotes_output.filter(|output| output.status.success()) {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() < 3 {
                continue;
            }
            let entry = remotes_by_name
                .entry(parts[0].to_string())
                .or_insert_with(|| (String::new(), String::new()));
            match parts[2] {
                "(fetch)" => entry.0 = parts[1].to_string(),
                "(push)" => entry.1 = parts[1].to_string(),
                _ => {}
            }
        }
    }

    let remotes = remotes_by_name
        .into_iter()
        .map(|(name, (fetch_url, push_url))| (name, fetch_url, push_url))
        .collect();

    GitRepoInfo {
        is_git_repo: true,
        root_path,
        current_branch,
        remotes,
    }
}

// ── Dichiarazioni sotto-moduli ────────────────────────────────────────────────

pub mod analyze;
pub mod browse;
pub mod cleanup;
pub mod clone;
pub mod crud;
pub mod custom_instructions;
pub mod deep_analyze;
pub mod deep_review;
pub mod file_watcher;
pub mod indexing;
pub mod quality;
pub mod terminal;

// ── Re-export di tutti i simboli pubblici ─────────────────────────────────────

pub use analyze::*;
pub use browse::*;
pub use clone::*;
pub use crud::*;
pub use custom_instructions::*;
pub use deep_analyze::*;
pub use deep_review::*;
pub use indexing::*;
pub use quality::*;
pub use terminal::*;
