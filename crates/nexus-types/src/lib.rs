use axum::{http::StatusCode, Json};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub use nexus_auth::Claims;

mod templates;
pub use templates::{get_template_or_default, TemplateCache};

pub mod fs_browse;
pub use fs_browse::{
    list_directories, list_root_candidates, validate_directory_name, BrowseDirectoryNode,
};

pub mod admin_dto;
pub mod documents_dto;
pub mod git_exec;
pub mod long_running_dto;
pub mod routing_client;
pub mod settings_dto;
pub mod workspace_paths;
pub use routing_client::resolve_purpose_via_http;

// --- Shared API types ---

pub type ApiError = (StatusCode, Json<Value>);
pub type ApiResult = Result<Json<Value>, ApiError>;

pub fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": message.into() })))
}

/// Validazione nome directory con errore API pronto (BAD_REQUEST).
/// Punto unico (regola L / ADR 0026): prima il wrapper era duplicato
/// identico nei settings.rs di admin-service e mcp-core.
pub fn validate_directory_name_api(name: &str) -> Result<&str, ApiError> {
    fs_browse::validate_directory_name(name)
        .map_err(|msg| api_error(StatusCode::BAD_REQUEST, msg))
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

/// Imposta `projects_base_root` alla cartella `projects/` sotto la working
/// directory del processo, ma solo se la setting e' ancora vuota. Punto unico
/// (regola L / ADR 0026): prima questa logica era duplicata in
/// `mcp-core` e `admin-service`. I default statici delle altre settings stanno
/// nella migrazione `0325_seed_default_settings.sql` (regola G/H).
pub async fn ensure_projects_base_root(db: &PgPool) {
    let default_root = std::env::current_dir()
        .map(|cwd| cwd.join("projects"))
        .ok()
        .and_then(|path| {
            if std::fs::create_dir_all(&path).is_ok() {
                path.canonicalize().ok()
            } else {
                None
            }
        })
        .map(|path| path.to_string_lossy().to_string());

    if let Some(root_value) = default_root {
        // Fix S86: l'errore SQL prima veniva ingoiato con `let _ = ...await;`.
        // Ora viene loggato (regola H): se l'UPDATE fallisce, `projects_base_root`
        // resta vuoto e tutti i nuovi progetti finiscono in un default hardcoded
        // di nascosto. Almeno con tracing::warn l'admin lo vede subito.
        if let Err(e) = sqlx::query(
            "UPDATE settings SET value = $1, updated_at = NOW() \
             WHERE key = 'projects_base_root' AND (value IS NULL OR btrim(value) = '')",
        )
        .bind(root_value)
        .execute(db)
        .await
        {
            tracing::warn!(
                "ensure_projects_base_root: UPDATE settings fallito ({}). \
                 projects_base_root potrebbe restare vuoto.",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_user_id, Claims};
    use axum::http::StatusCode;

    fn claims_with_sub(sub: &str) -> Claims {
        Claims {
            sub: sub.to_string(),
            role: "user".to_string(),
            exp: 0,
        }
    }

    #[test]
    fn parse_user_id_accetta_uuid_valido() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let parsed = parse_user_id(&claims_with_sub(id)).expect("uuid valido accettato");
        assert_eq!(parsed.to_string(), id);
    }

    #[test]
    fn parse_user_id_rifiuta_uuid_invalido() {
        let err =
            parse_user_id(&claims_with_sub("non-un-uuid")).expect_err("uuid invalido rifiutato");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }
}
