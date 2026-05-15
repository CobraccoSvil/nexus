//! Fix M33-B: endpoint REST per allocazione dinamica di porte di progetto.
//!
//! POST /api/projects/:id/services/allocate-port
//!
//! Body: `{label: string}` (es. "backend", "frontend", "api")
//!
//! L'agente AI chiama questo endpoint quando deve scegliere una porta per un
//! servizio del progetto (al posto di hardcodare 3002/5173). Nexus:
//! 1. Sceglie una porta libera nel bucket deterministico del progetto via
//!    `find_free_project_port`.
//! 2. INSERT in `nexus_port_allocations` con allocation_mode='dynamic'.
//! 3. Ritorna `{port, label, allocation_mode}` per uso dell'agente.
//!
//! Coerente con il sistema port_allocations descritto in migration 0141.

use super::*;
use super::services::find_free_project_port;

#[derive(serde::Deserialize)]
pub struct AllocatePortBody {
    pub label: String,
}

pub async fn allocate_port(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<AllocatePortBody>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let _context = load_project_context(&state.db, project_id, user_id).await?;

    let label = body.label.trim();
    if label.is_empty() {
        return Err(api_error(StatusCode::BAD_REQUEST, "Campo 'label' obbligatorio"));
    }

    // Se esiste gia' una allocazione con la stessa label per il progetto, riusala
    // (idempotenza: chiamate ripetute dall'agente ritornano la stessa porta).
    if let Ok(Some((existing_port,))) = sqlx::query_as::<_, (i32,)>(
        "SELECT port FROM nexus_port_allocations \
         WHERE project_id = $1 AND label = $2 LIMIT 1",
    )
    .bind(project_id)
    .bind(label)
    .fetch_optional(&state.db)
    .await
    {
        return Ok(Json(json!({
            "port": existing_port,
            "label": label,
            "allocation_mode": "existing",
            "ok": true,
        })));
    }

    let port = find_free_project_port(&project_id, &state.port_registry).await;
    let insert_result = sqlx::query(
        r#"
        INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode)
        VALUES ($1, $2, $3, 'dynamic')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(project_id)
    .bind(port as i32)
    .bind(label)
    .execute(&state.db)
    .await;
    if let Err(e) = insert_result {
        tracing::warn!("allocate_port: INSERT fallito (porta {} label {}): {}", port, label, e);
    }

    Ok(Json(json!({
        "port": port,
        "label": label,
        "allocation_mode": "dynamic",
        "ok": true,
    })))
}
