//! HTTP API per `file_mutations` (mig 0349):
//!   - GET  /api/projects/:id/mutations            -> lista mutazioni recenti
//!   - GET  /api/projects/:id/mutations/:mid       -> dettaglio con before/after
//!   - POST /api/projects/:id/mutations/:mid/revert -> ripristina la mutazione
//!
//! Il punto unico di logica vive in `crate::file_mutations`; qui solo parsing
//! input HTTP, mapping esiti -> status code, e (opzionale) `force` come query.

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::Claims,
    chat_learning::{api_error, parse_user_id, ApiResult},
    projects::load_project_context,
    AppState,
};

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
}

pub async fn list_mutations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let rows =
        crate::file_mutations::list_recent_mutations(&state.db, project_id, q.limit.unwrap_or(100))
            .await
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    Ok(Json(json!({ "mutations": rows })))
}

pub async fn get_mutation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, mid)): AxumPath<(String, i64)>,
) -> ApiResult {
    let _user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    match crate::file_mutations::get_mutation_full(&state.db, project_id, mid).await {
        Ok(Some(v)) => Ok(Json(v)),
        Ok(None) => Err(api_error(StatusCode::NOT_FOUND, "Mutazione non trovata")),
        Err(e) => Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {e}"),
        )),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct RevertBody {
    /// Se true sovrascrive il file anche se lo stato corrente non corrisponde
    /// all'after_sha256 registrato (cioe' qualcuno ha modificato il file dopo
    /// la mutazione che stiamo annullando). L'UI lo passa solo dopo conferma
    /// esplicita dell'utente.
    #[serde(default)]
    pub force: bool,
}

pub async fn revert_mutation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, mid)): AxumPath<(String, i64)>,
    Json(body): Json<RevertBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    // Risolvi project root dalla AppState (verifica accesso utente).
    let ctx = load_project_context(&state.db, project_id, user_id).await?;

    match crate::file_mutations::revert_mutation(
        &state.db,
        project_id,
        &ctx.root_path,
        Some(user_id),
        None, // session_id non necessario per il revert via REST
        mid,
        body.force,
    )
    .await
    {
        crate::file_mutations::RevertOutcome::Reverted { new_mutation_id } => {
            // Notifica i pannelli (Explorer/Editor) che il file e' cambiato.
            // Per non re-ispezionare la riga, leggiamo file_path dalla query.
            let file_path: Option<String> =
                sqlx::query_scalar("SELECT file_path FROM file_mutations WHERE id = $1")
                    .bind(new_mutation_id)
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten();
            if let Some(path) = file_path {
                let _ = nexus_events::dispatcher::emit_global(
                    project_id,
                    nexus_events::event::ProjectEvent::FileChanged {
                        path,
                        op: "modified".to_string(),
                    },
                );
            }
            Ok(Json(json!({
                "ok": true,
                "new_mutation_id": new_mutation_id,
                "message": "File ripristinato",
            })))
        }
        crate::file_mutations::RevertOutcome::NotFound => {
            Err(api_error(StatusCode::NOT_FOUND, "Mutazione non trovata"))
        }
        crate::file_mutations::RevertOutcome::AlreadyReverted => {
            Err(api_error(StatusCode::CONFLICT, "Mutazione gia' revertita"))
        }
        crate::file_mutations::RevertOutcome::NotRevertible(reason) => Err(api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Ripristino non disponibile: {reason}"),
        )),
        crate::file_mutations::RevertOutcome::Conflict {
            current_sha,
            expected_sha,
        } => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Il file e' stato modificato dopo questa mutazione. \
                          Conferma per sovrascrivere con `force=true`.",
                "conflict": {
                    "current_sha": current_sha,
                    "expected_sha": expected_sha,
                }
            })),
        )),
        crate::file_mutations::RevertOutcome::IoError(e) => Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Errore ripristino: {e}"),
        )),
    }
}

/// "Annulla l'ultima mutazione". Endpoint di comodo per la UI: trova l'ultima
/// mutazione non ancora revertita per il progetto e la annulla.
pub async fn revert_last(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RevertBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    // Ultima mutazione non gia' revertita. Esclude le mutazioni op='reverted'
    // per non andare in loop (il revert di un revert e' un'operazione esplicita
    // su un mutation_id, non "ultima").
    let last: Option<i64> = sqlx::query_scalar(
        r#"SELECT id FROM file_mutations
            WHERE project_id = $1
              AND reverted_at IS NULL
              AND op <> 'reverted'
              AND before_content IS NOT NULL
            ORDER BY created_at DESC, id DESC
            LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {e}")))?;

    let Some(mid) = last else {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "Nessuna mutazione annullabile in questo progetto",
        ));
    };

    // Delega all'handler singolo (riuso totale, regola L).
    revert_mutation(
        State(state),
        Extension(claims),
        AxumPath((id, mid)),
        Json(body),
    )
    .await
}

// I tipi axum::Json e State sono sopra; necessari per il binding handler.
#[allow(dead_code)]
fn _ensure_imports(_v: Value) {}
