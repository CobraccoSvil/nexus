// ═══════════════════════════════════════════════════════════════════════════
// wiki/routes.rs — Endpoint REST `/api/wiki/*` (ADR 0017 v2).
//
// Tutti gli handler richiedono auth (`middleware::require_auth` viene
// applicato dal modulo `routes::wiki` che invoca `merge`). Ogni handler:
//   1. Estrae i `Claims` da `Extension`.
//   2. Costruisce `WikiAcl::from_claims(&state, &claims).await`.
//   3. Delega a `storage::*` / `revisions::*`, che applicano ACL + DB.
// ═══════════════════════════════════════════════════════════════════════════

use crate::auth::Claims;
use crate::wiki::acl::WikiAcl;
use crate::wiki::model::{WikiDocPatch, WikiScope};
use crate::wiki::{
    links_worker, reingest, revisions, search as wiki_search, storage, title_gen, triple_extractor,
};
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

// ───────────────────────────────────────────────────────────────────────────
// Tipi di richiesta
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub scope: Option<String>,
    pub project_id: Option<Uuid>,
    pub kind: Option<String>,
    pub q: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub scope: String,
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    pub slug: Option<String>,
    #[serde(default)]
    pub body_md: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub intent: Option<String>,
    #[serde(default)]
    pub public_read: bool,
    pub vault_file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub from: i32,
    pub to: i32,
}

#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    pub version: i32,
}

#[derive(Debug, Deserialize)]
pub struct ReingestQuery {
    /// `meta` | `project` | `all` (default `all`).
    pub scope: Option<String>,
    /// Filtro opzionale per progetto (valido solo con `scope=project`).
    pub project_id: Option<Uuid>,
    /// Se `true`, l'handler attende il completamento e ritorna il report.
    /// Default `false` -> task in background con risposta 202.
    pub wait: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RecomputeLinksQuery {
    /// `meta` | `project` | `all` (default `all`).
    pub scope: Option<String>,
    pub project_id: Option<Uuid>,
    /// Se settato, ignora `scope`/`project_id` e ricalcola solo questo doc.
    pub doc_id: Option<Uuid>,
    pub wait: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RecomputeTitlesQuery {
    /// `meta` | `project` | `all` (default `all`). Ignorato se `doc_id` settato.
    pub scope: Option<String>,
    pub project_id: Option<Uuid>,
    /// Se settato, rigenera il titolo solo per questo doc (anche oltre il cap).
    pub doc_id: Option<Uuid>,
    pub wait: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ExtractTriplesQuery {
    /// `meta` | `project` | `all` (default `all`). Ignorato se `doc_id` settato.
    pub scope: Option<String>,
    pub project_id: Option<Uuid>,
    /// Se settato, forza l'estrazione su UN singolo doc anche oltre il cap.
    pub doc_id: Option<Uuid>,
    pub wait: Option<bool>,
    /// Se true e `doc_id` settato, bypassa il cap diurno (utile per smoke test).
    pub override_cap: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ListTriplesQuery {
    pub predicate: Option<String>,
    pub source: Option<String>,
    pub min_confidence: Option<f32>,
    pub subj_id: Option<Uuid>,
    pub obj_id: Option<Uuid>,
    /// Full-text su `obj_text` (concept libero).
    pub q: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// `meta` | `project` (assente = qualunque scope visibile dall'utente,
    /// limitato comunque dall'ACL). Filtra le triple per scope del documento
    /// soggetto via JOIN su wiki_docs.scope.
    pub scope: Option<String>,
    /// Restringe a un singolo progetto via JOIN su wiki_docs.project_id.
    /// Il frontend lo passa sempre quando l'utente lavora dentro un progetto
    /// (altrimenti si mischiavano triple di progetti diversi).
    pub project_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Default)]
pub struct GraphQuery {
    /// `meta` | `project` (assente = qualunque scope visibile dall'utente).
    pub scope: Option<String>,
    pub project_id: Option<Uuid>,
    /// Soglia confidence minima per includere un edge (default 0.5).
    pub confidence_min: Option<f32>,
    /// CSV di predicate da includere (default = tutti).
    pub predicate: Option<String>,
    /// Cap nodi (default 500). Vengono presi i piu' connessi (degree DESC).
    pub max_nodes: Option<usize>,
    /// Cap edges (default 5000). Vengono presi quelli a piu' alta confidence.
    pub max_edges: Option<usize>,
    /// Se settato, centra il grafo: include solo `seed_doc_id` + vicini di 1 hop.
    pub seed_doc_id: Option<Uuid>,
}

// ───────────────────────────────────────────────────────────────────────────
// Helper: error mapping anyhow -> (StatusCode, String)
// ───────────────────────────────────────────────────────────────────────────

fn err500<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}"))
}

fn err400<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, format!("{e}"))
}

fn err403(msg: &str) -> (StatusCode, String) {
    (StatusCode::FORBIDDEN, msg.to_string())
}

fn err404(msg: &str) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, msg.to_string())
}

async fn build_acl(state: &AppState, claims: &Claims) -> Result<WikiAcl, (StatusCode, String)> {
    WikiAcl::from_claims(&state.wiki_deps(), claims)
        .await
        .map_err(err500)
}

/// Verifica che il chiamante sia admin; altrimenti 403 con `msg`.
/// Punto unico del gate admin per gli handler worker (regola L).
fn require_admin(acl: &WikiAcl, msg: &str) -> Result<(), (StatusCode, String)> {
    if acl.is_admin {
        Ok(())
    } else {
        Err(err403(msg))
    }
}

/// Traduce il parametro `scope` (`meta|project|all|assente|vuoto`) nel filtro
/// `Option<WikiScope>` usato dai worker admin. Punto unico del parsing (regola
/// L): `reingest`, `recompute-links` ed `extract-triples` lo condividono.
fn parse_scope_filter(raw: Option<&str>) -> Result<Option<WikiScope>, (StatusCode, String)> {
    match raw {
        None | Some("") | Some("all") => Ok(None),
        Some("meta") => Ok(Some(WikiScope::Meta)),
        Some("project") => Ok(Some(WikiScope::Project)),
        Some(other) => Err(err400(format!(
            "scope invalido: {other} (atteso meta|project|all)"
        ))),
    }
}

/// Risposta `202 Accepted` per un worker lanciato in background: `run_id` opaco
/// piu' l'eco dei filtri. Costruisce il corpo standard dei quattro handler async.
fn accepted_run(run_id: Uuid, extra: Value) -> (StatusCode, Json<Value>) {
    let mut obj = serde_json::Map::new();
    obj.insert("ok".to_string(), json!(true));
    obj.insert("run_id".to_string(), json!(run_id.to_string()));
    obj.insert("wait".to_string(), json!(false));
    if let Value::Object(map) = extra {
        for (k, v) in map {
            obj.insert(k, v);
        }
    }
    (StatusCode::ACCEPTED, Json(Value::Object(obj)))
}

// ───────────────────────────────────────────────────────────────────────────
// Handler
// ───────────────────────────────────────────────────────────────────────────

/// `GET /api/wiki/docs?scope=meta|project&project_id=&kind=&q=&limit=&offset=`
pub async fn list_docs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let scope = match q.scope.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(WikiScope::parse(raw).ok_or_else(|| err400("scope invalido"))?),
    };
    let qparams = storage::WikiListQuery {
        scope,
        project_id: q.project_id,
        kind: q.kind,
        q: q.q,
        limit: q.limit.unwrap_or(50),
        offset: q.offset.unwrap_or(0),
    };
    let (items, total) = storage::list_docs(&state.wiki_deps(), &acl, qparams)
        .await
        .map_err(err500)?;
    Ok(Json(json!({
        "items": items,
        "total": total,
    })))
}

/// `GET /api/wiki/docs/:id`
pub async fn get_doc(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let doc = storage::get_doc(&state.wiki_deps(), &acl, id)
        .await
        .map_err(err500)?
        .ok_or_else(|| err404("documento non trovato o non accessibile"))?;
    Ok(Json(serde_json::to_value(doc).map_err(err500)?))
}

/// `POST /api/wiki/docs`
pub async fn create_doc(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let scope = WikiScope::parse(&body.scope).ok_or_else(|| err400("scope invalido"))?;
    let input = storage::WikiDocCreate {
        scope,
        project_id: body.project_id,
        kind: body.kind,
        title: body.title,
        slug: body.slug,
        body_md: body.body_md,
        tags: body.tags,
        intent: body.intent,
        public_read: body.public_read,
        vault_file_path: body.vault_file_path,
    };
    let doc = storage::create_doc(&state.wiki_deps(), &acl, input)
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            // I bail dell'ACL diventano 403, gli altri 400/500.
            if msg.contains("permesso negato")
                || msg.contains("non membro")
                || msg.contains("solo admin")
            {
                err403(&msg)
            } else if msg.contains("scope") || msg.contains("slug") {
                err400(msg)
            } else {
                err500(msg)
            }
        })?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(doc).unwrap_or_else(|_| json!({}))),
    ))
}

/// `PATCH /api/wiki/docs/:id`
pub async fn patch_doc(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(patch): Json<WikiDocPatch>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let outcome = storage::update_doc(&state.wiki_deps(), &acl, id, patch)
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            if msg.contains("permesso negato") || msg.contains("frozen") {
                err403(&msg)
            } else if msg.contains("non trovato") {
                err404(&msg)
            } else {
                err500(msg)
            }
        })?;
    Ok(Json(json!({
        "ok": true,
        "version": outcome.version_no,
        "body_changed": outcome.body_changed,
    })))
}

/// `DELETE /api/wiki/docs/:id`
pub async fn delete_doc(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    storage::delete_doc(&state.wiki_deps(), &acl, id)
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            if msg.contains("permesso negato") || msg.contains("frozen") {
                err403(&msg)
            } else if msg.contains("non trovato") {
                err404(&msg)
            } else {
                err500(msg)
            }
        })?;
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/wiki/docs/:id/revisions`
pub async fn list_revisions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let items = revisions::list_revisions(&state.wiki_deps(), &acl, id)
        .await
        .map_err(err500)?;
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

/// `GET /api/wiki/docs/:id/revisions/:version`
pub async fn get_revision(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, version)): Path<(Uuid, i32)>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let rev = revisions::get_revision(&state.wiki_deps(), &acl, id, version)
        .await
        .map_err(err500)?
        .ok_or_else(|| err404("revisione non trovata"))?;
    Ok(Json(serde_json::to_value(rev).map_err(err500)?))
}

/// `GET /api/wiki/docs/:id/diff?from=&to=`
pub async fn diff(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Query(q): Query<DiffQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let (a, b) = revisions::diff(&state.wiki_deps(), &acl, id, q.from, q.to)
        .await
        .map_err(err500)?;
    Ok(Json(json!({ "from": a, "to": b })))
}

/// `POST /api/wiki/docs/:id/restore`
pub async fn restore(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<RestoreBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    let new_version = revisions::restore_revision(&state.wiki_deps(), &acl, id, body.version)
        .await
        .map_err(|e| {
            let msg = format!("{e}");
            if msg.contains("permesso negato") || msg.contains("frozen") {
                err403(&msg)
            } else if msg.contains("non trovata") || msg.contains("non trovato") {
                err404(&msg)
            } else {
                err500(msg)
            }
        })?;
    Ok(Json(json!({
        "ok": true,
        "restored_from": body.version,
        "version": new_version,
    })))
}

/// `POST /api/wiki/reingest?scope=meta|project|all&project_id=&wait=true|false`
///
/// Admin-only. Avvia il worker `reingest_all` con i filtri richiesti. Quando
/// `wait=true` esegue in sincrono e ritorna il `ReingestReport` come JSON.
/// Quando `wait=false` (default) lo lancia come task `tokio::spawn` e
/// risponde immediatamente con `202 Accepted` + `run_id` opaco (usato solo
/// per correlare i log: lo storage runs non e' ancora implementato).
pub async fn reingest_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ReingestQuery>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    require_admin(&acl, "solo admin puo' lanciare wiki.reingest")?;

    let scope_filter = parse_scope_filter(q.scope.as_deref())?;
    let project_filter = q.project_id;

    if q.wait.unwrap_or(false) {
        let report = reingest::reingest_all(&state.wiki_deps(), scope_filter, project_filter)
            .await
            .map_err(err500)?;
        return Ok((
            StatusCode::OK,
            Json(serde_json::to_value(report).unwrap_or_else(|_| json!({}))),
        ));
    }

    let run_id = Uuid::new_v4();
    spawn_reingest_bg(state.clone(), run_id, scope_filter, project_filter);
    Ok(accepted_run(
        run_id,
        json!({
            "scope": scope_filter.map(|s| s.as_str()).unwrap_or("all"),
            "project_id": project_filter,
        }),
    ))
}

/// Task background di `reingest_all`: logga avvio, esito e durata. Estratto da
/// `reingest_handler` per contenerne la lunghezza (behavior-preserving).
fn spawn_reingest_bg(
    state: AppState,
    run_id: Uuid,
    scope_filter: Option<WikiScope>,
    project_filter: Option<Uuid>,
) {
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        tracing::info!(
            run_id = %run_id,
            scope = ?scope_filter.map(|s| s.as_str()),
            project_id = ?project_filter,
            "wiki.reingest: avvio task background"
        );
        match reingest::reingest_all(&state.wiki_deps(), scope_filter, project_filter).await {
            Ok(report) => tracing::info!(
                run_id = %run_id,
                meta = report.meta_docs_ingested,
                projects = report.project_docs_ingested_by_project.len(),
                skipped = report.files_skipped,
                errors = report.errors.len(),
                elapsed_ms = report.elapsed_ms,
                "wiki.reingest: completato"
            ),
            Err(e) => tracing::error!(
                run_id = %run_id,
                error = %e,
                elapsed_ms = started.elapsed().as_millis(),
                "wiki.reingest: fallito"
            ),
        }
    });
}

/// `POST /api/wiki/recompute-links?scope=&project_id=&doc_id=&wait=`
///
/// Admin-only. Rilancia il worker `links_worker` con i filtri richiesti.
/// `wait=true` blocca fino al report; default async (202 + run_id opaco).
pub async fn recompute_links_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<RecomputeLinksQuery>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    require_admin(&acl, "solo admin puo' lanciare wiki.recompute-links")?;

    let wait = q.wait.unwrap_or(false);

    // Path "singolo documento" — ignora scope/project_id.
    if let Some(doc_id) = q.doc_id {
        if wait {
            let report = links_worker::recompute_links_for_doc(&state.wiki_deps(), doc_id)
                .await
                .map_err(err500)?;
            return Ok((
                StatusCode::OK,
                Json(serde_json::to_value(report).unwrap_or_else(|_| json!({}))),
            ));
        }
        let run_id = Uuid::new_v4();
        spawn_recompute_links_doc_bg(state.clone(), run_id, doc_id);
        return Ok(accepted_run(run_id, json!({})));
    }

    // Path "scope-wide".
    let scope_filter = parse_scope_filter(q.scope.as_deref())?;
    let project_filter = q.project_id;

    if wait {
        let report = links_worker::recompute_links_for_scope(
            &state.wiki_deps(),
            scope_filter,
            project_filter,
        )
        .await
        .map_err(err500)?;
        return Ok((
            StatusCode::OK,
            Json(serde_json::to_value(report).unwrap_or_else(|_| json!({}))),
        ));
    }

    let run_id = Uuid::new_v4();
    spawn_recompute_links_scope_bg(state.clone(), run_id, scope_filter, project_filter);
    Ok(accepted_run(
        run_id,
        json!({
            "scope": scope_filter.map(|s| s.as_str()).unwrap_or("all"),
            "project_id": project_filter,
        }),
    ))
}

/// Task background di `recompute_links_for_doc` (singolo doc): logga esito.
fn spawn_recompute_links_doc_bg(state: AppState, run_id: Uuid, doc_id: Uuid) {
    tokio::spawn(async move {
        tracing::info!(
            run_id = %run_id,
            doc_id = %doc_id,
            "wiki.recompute-links: avvio task background (singolo doc)"
        );
        match links_worker::recompute_links_for_doc(&state.wiki_deps(), doc_id).await {
            Ok(rep) => tracing::info!(
                run_id = %run_id,
                scanned = rep.docs_scanned,
                wikilinks = rep.wikilinks_resolved,
                semantic_new = rep.semantic_links_created,
                semantic_upd = rep.semantic_links_updated,
                elapsed_ms = rep.elapsed_ms,
                "wiki.recompute-links: completato (singolo doc)"
            ),
            Err(e) => tracing::error!(
                run_id = %run_id,
                error = %e,
                "wiki.recompute-links: fallito (singolo doc)"
            ),
        }
    });
}

/// Task background di `recompute_links_for_scope` (scope-wide): logga esito.
fn spawn_recompute_links_scope_bg(
    state: AppState,
    run_id: Uuid,
    scope_filter: Option<WikiScope>,
    project_filter: Option<Uuid>,
) {
    tokio::spawn(async move {
        tracing::info!(
            run_id = %run_id,
            scope = ?scope_filter.map(|s| s.as_str()),
            project_id = ?project_filter,
            "wiki.recompute-links: avvio task background"
        );
        match links_worker::recompute_links_for_scope(
            &state.wiki_deps(),
            scope_filter,
            project_filter,
        )
        .await
        {
            Ok(rep) => tracing::info!(
                run_id = %run_id,
                scanned = rep.docs_scanned,
                wikilinks = rep.wikilinks_resolved,
                semantic_new = rep.semantic_links_created,
                semantic_upd = rep.semantic_links_updated,
                errors = rep.errors.len(),
                elapsed_ms = rep.elapsed_ms,
                "wiki.recompute-links: completato"
            ),
            Err(e) => tracing::error!(
                run_id = %run_id,
                error = %e,
                "wiki.recompute-links: fallito"
            ),
        }
    });
}

/// `POST /api/wiki/recompute-titles?scope=&project_id=&doc_id=&wait=`
///
/// Admin-only. Rigenera via LLM i titoli descrittivi dei doc con titolo-
/// artefatto (kind chat_note/run_summary/other), saltando quelli editati a mano
/// e i veri documenti redatti. `wait=true` blocca fino al report; default async
/// (202 + run_id opaco). Modellato su `recompute_links_handler`.
pub async fn recompute_titles_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<RecomputeTitlesQuery>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    require_admin(&acl, "solo admin puo' lanciare wiki.recompute-titles")?;

    let wait = q.wait.unwrap_or(false);

    // Path "singolo documento" — ignora scope/project_id, bypassa il cap.
    if let Some(doc_id) = q.doc_id {
        if wait {
            let report = title_gen::generate_title_for_doc(&state.wiki_deps(), doc_id)
                .await
                .map_err(err500)?;
            return Ok((
                StatusCode::OK,
                Json(serde_json::to_value(report).unwrap_or_else(|_| json!({}))),
            ));
        }
        let run_id = Uuid::new_v4();
        spawn_recompute_titles_doc_bg(state.clone(), run_id, doc_id);
        return Ok(accepted_run(run_id, json!({})));
    }

    // Path "scope-wide".
    let scope_label = q.scope.as_deref().unwrap_or("all").to_lowercase();
    let do_meta = scope_label == "meta" || scope_label == "all";
    let do_project = scope_label == "project" || scope_label == "all";
    if !do_meta && !do_project {
        return Err(err400(format!(
            "scope invalido: {scope_label} (atteso meta|project|all)"
        )));
    }
    let project_id = q.project_id;

    if wait {
        let (processed, updated, details) =
            run_recompute_titles_batch(state.clone(), do_meta, do_project, project_id)
                .await
                .map_err(err500)?;
        return Ok((
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "processed_total": processed,
                "updated_total": updated,
                "details": details,
            })),
        ));
    }

    let run_id = Uuid::new_v4();
    spawn_recompute_titles_batch_bg(
        state.clone(),
        run_id,
        scope_label.clone(),
        do_meta,
        do_project,
        project_id,
    );
    Ok(accepted_run(
        run_id,
        json!({ "scope": scope_label, "project_id": project_id }),
    ))
}

/// Batch di rigenerazione titoli su scope meta e/o progetto. Estratto dalla
/// closure inline di `recompute_titles_handler` in funzione libera riusabile
/// dal path sincrono e dal task background (behavior-preserving).
async fn run_recompute_titles_batch(
    state: AppState,
    do_meta: bool,
    do_project: bool,
    project_id: Option<Uuid>,
) -> Result<(usize, usize, serde_json::Map<String, Value>), anyhow::Error> {
    let mut aggregated = serde_json::Map::new();
    let mut overall_processed = 0usize;
    let mut overall_updated = 0usize;

    if do_meta {
        let rep =
            title_gen::generate_titles_for_scope(&state.wiki_deps(), title_gen::TitleScope::Meta)
                .await?;
        overall_processed += rep.processed_count;
        overall_updated += rep.updated_count;
        aggregated.insert(
            "meta".to_string(),
            serde_json::to_value(rep).unwrap_or(Value::Null),
        );
    }
    if do_project {
        let project_ids: Vec<Uuid> = if let Some(pid) = project_id {
            vec![pid]
        } else {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects")
                .fetch_all(&state.db)
                .await
                .map_err(|e| anyhow::anyhow!("SELECT projects per recompute-titles: {e}"))?
        };
        let mut projects_map = serde_json::Map::new();
        for pid in project_ids {
            let rep = title_gen::generate_titles_for_scope(
                &state.wiki_deps(),
                title_gen::TitleScope::Project(pid),
            )
            .await?;
            overall_processed += rep.processed_count;
            overall_updated += rep.updated_count;
            projects_map.insert(
                pid.to_string(),
                serde_json::to_value(rep).unwrap_or(Value::Null),
            );
        }
        aggregated.insert("projects".to_string(), Value::Object(projects_map));
    }
    Ok((overall_processed, overall_updated, aggregated))
}

/// Task background di `generate_title_for_doc` (singolo doc): logga esito.
fn spawn_recompute_titles_doc_bg(state: AppState, run_id: Uuid, doc_id: Uuid) {
    tokio::spawn(async move {
        tracing::info!(
            run_id = %run_id,
            doc_id = %doc_id,
            "wiki.recompute-titles: avvio task background (singolo doc)"
        );
        match title_gen::generate_title_for_doc(&state.wiki_deps(), doc_id).await {
            Ok(rep) => tracing::info!(
                run_id = %run_id,
                updated = rep.updated,
                elapsed_ms = rep.elapsed_ms,
                "wiki.recompute-titles: completato (singolo doc)"
            ),
            Err(e) => tracing::error!(
                run_id = %run_id,
                error = %e,
                "wiki.recompute-titles: fallito (singolo doc)"
            ),
        }
    });
}

/// Task background del batch titoli: logga avvio ed esito aggregato.
fn spawn_recompute_titles_batch_bg(
    state: AppState,
    run_id: Uuid,
    scope_label: String,
    do_meta: bool,
    do_project: bool,
    project_id: Option<Uuid>,
) {
    tokio::spawn(async move {
        tracing::info!(
            run_id = %run_id,
            scope = %scope_label,
            project_id = ?project_id,
            "wiki.recompute-titles: avvio task background (batch)"
        );
        match run_recompute_titles_batch(state, do_meta, do_project, project_id).await {
            Ok((processed, updated, _)) => tracing::info!(
                run_id = %run_id,
                processed = processed,
                updated = updated,
                "wiki.recompute-titles: completato (batch)"
            ),
            Err(e) => tracing::error!(
                run_id = %run_id,
                error = %e,
                "wiki.recompute-titles: fallito (batch)"
            ),
        }
    });
}

/// `GET /api/wiki/docs/:id/links` — outbound + inbound del documento.
pub async fn list_doc_links(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;

    // Verifica che il doc sorgente sia visibile.
    let source = storage::get_doc(&state.wiki_deps(), &acl, id)
        .await
        .map_err(err500)?;
    if source.is_none() {
        return Err(err404("documento non trovato o non accessibile"));
    }

    let (acl_clause, acl_projects) = acl.scope_clause(1);
    let acl_param_used = !acl_projects.is_empty();

    let (outbound_sql, inbound_sql) = build_link_queries(&acl_clause, acl_param_used);

    let mut q_out = sqlx::query(&outbound_sql);
    let mut q_in = sqlx::query(&inbound_sql);
    if acl_param_used {
        q_out = q_out.bind(acl_projects.clone());
        q_in = q_in.bind(acl_projects.clone());
    }
    q_out = q_out.bind(id);
    q_in = q_in.bind(id);

    let outbound_rows = q_out.fetch_all(&state.db).await.map_err(err500)?;
    let inbound_rows = q_in.fetch_all(&state.db).await.map_err(err500)?;

    let outbound: Vec<Value> = outbound_rows
        .into_iter()
        .map(|r| map_link_row(r, "outbound"))
        .collect();
    let inbound: Vec<Value> = inbound_rows
        .into_iter()
        .map(|r| map_link_row(r, "inbound"))
        .collect();

    Ok(Json(json!({
        "doc_id": id,
        "outbound": outbound,
        "inbound": inbound,
        "totals": {
            "outbound": outbound.len(),
            "inbound": inbound.len(),
        }
    })))
}

/// Costruisce le due SQL (outbound + inbound) di `list_doc_links` con i
/// placeholder corretti in base a `acl_param_used` ($1 = acl projects; l'id del
/// doc va in $2 se l'acl usa un parametro, altrimenti $1). Estratta per
/// contenere la lunghezza dell'handler (behavior-preserving).
///
/// Inbound: edges verso `id` (l.to_doc_id = id), il doc "altro" da mostrare e'
/// la SORGENTE (l.from_doc_id). Costruito esplicitamente (NON via replace di
/// stringhe: la vecchia logica produceva "l.to_doc_id = 22" -> errore SQL
/// uuid = integer).
fn build_link_queries(acl_clause: &str, acl_param_used: bool) -> (String, String) {
    let id_placeholder = if acl_param_used { "$2" } else { "$1" };
    let outbound_sql = format!(
        "SELECT l.from_doc_id, l.to_doc_id, l.rel_type, l.confidence, l.created_by, \
                l.evidence, l.created_at, \
                d.id AS target_id, d.scope AS target_scope, d.project_id AS target_project_id, \
                d.slug AS target_slug, d.title AS target_title, d.kind AS target_kind \
         FROM wiki_links l JOIN wiki_docs ON wiki_docs.id = l.to_doc_id \
                           JOIN wiki_docs d ON d.id = l.to_doc_id \
         WHERE l.from_doc_id = {id_placeholder} AND {acl_clause} \
         ORDER BY l.confidence DESC, l.rel_type ASC"
    );
    let inbound_sql = format!(
        "SELECT l.from_doc_id, l.to_doc_id, l.rel_type, l.confidence, l.created_by, \
                l.evidence, l.created_at, \
                d.id AS target_id, d.scope AS target_scope, d.project_id AS target_project_id, \
                d.slug AS target_slug, d.title AS target_title, d.kind AS target_kind \
         FROM wiki_links l JOIN wiki_docs ON wiki_docs.id = l.from_doc_id \
                           JOIN wiki_docs d ON d.id = l.from_doc_id \
         WHERE l.to_doc_id = {id_placeholder} AND {acl_clause} \
         ORDER BY l.confidence DESC, l.rel_type ASC"
    );
    (outbound_sql, inbound_sql)
}

/// Serializza una riga di `wiki_links` (con doc target arricchito) nel JSON
/// atteso dal frontend. Estratta dalla closure di `list_doc_links`.
/// Contratto TS WikiLinksResponse: outbound[].to_doc, inbound[].from_doc
/// (oggetti WikiDoc con .title). Il frontend usa l.to_doc/l.from_doc; "target"
/// (flat) faceva l.from_doc undefined -> crash reading 'title'. Esponiamo la
/// chiave nidificata attesa (piu' "target" per retrocompatibilita').
fn map_link_row(row: sqlx::postgres::PgRow, edge_dir: &str) -> Value {
    use sqlx::Row;
    let from_id: Uuid = row.try_get("from_doc_id").unwrap_or_default();
    let to_id: Uuid = row.try_get("to_doc_id").unwrap_or_default();
    let rel_type: String = row.try_get("rel_type").unwrap_or_default();
    let confidence: f32 = row.try_get("confidence").unwrap_or(0.0);
    let created_by: String = row.try_get("created_by").unwrap_or_default();
    let evidence: Option<String> = row.try_get("evidence").ok();
    let target_id: Uuid = row.try_get("target_id").unwrap_or_default();
    let target_scope: String = row.try_get("target_scope").unwrap_or_default();
    let target_project_id: Option<Uuid> = row.try_get("target_project_id").ok();
    let target_slug: String = row.try_get("target_slug").unwrap_or_default();
    let target_title: String = row.try_get("target_title").unwrap_or_default();
    let target_kind: String = row.try_get("target_kind").unwrap_or_default();
    let doc_obj = json!({
        "id": target_id,
        "scope": target_scope,
        "project_id": target_project_id,
        "slug": target_slug,
        "title": target_title,
        "kind": target_kind,
    });
    let doc_key = if edge_dir == "outbound" {
        "to_doc"
    } else {
        "from_doc"
    };
    let mut obj = serde_json::Map::new();
    obj.insert("from_doc_id".to_string(), json!(from_id));
    obj.insert("to_doc_id".to_string(), json!(to_id));
    obj.insert("rel_type".to_string(), json!(rel_type));
    obj.insert("confidence".to_string(), json!(confidence));
    obj.insert("created_by".to_string(), json!(created_by));
    obj.insert("evidence".to_string(), json!(evidence));
    obj.insert("direction".to_string(), json!(edge_dir));
    obj.insert(doc_key.to_string(), doc_obj.clone());
    obj.insert("target".to_string(), doc_obj);
    Value::Object(obj)
}

/// `GET /api/wiki/graph` — JSON Cytoscape-compatible filtrato ACL.
pub async fn get_graph(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<GraphQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;

    let scope_filter: Option<WikiScope> = match q.scope.as_deref() {
        None | Some("") | Some("all") => None,
        Some(raw) => Some(WikiScope::parse(raw).ok_or_else(|| err400("scope invalido"))?),
    };
    let confidence_min = q.confidence_min.unwrap_or(0.5).clamp(0.0, 1.0);
    let max_nodes = q.max_nodes.unwrap_or(500).clamp(1, 5000);
    let max_edges = q.max_edges.unwrap_or(5000).clamp(1, 50000);
    let predicates: Option<Vec<String>> = q.predicate.as_ref().map(|s| {
        s.split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    });

    let (acl_clause, acl_projects) = acl.scope_clause(1);
    let seed_doc_id = q.seed_doc_id;

    // ── 1) Selezione nodi ─────────────────────────────────────────────────
    let (node_ids, nodes_json) = fetch_graph_nodes(
        &state,
        &acl_clause,
        &acl_projects,
        scope_filter,
        q.project_id,
        seed_doc_id,
        max_nodes,
    )
    .await?;

    if nodes_json.is_empty() {
        return Ok(Json(json!({
            "nodes": Value::Array(vec![]),
            "edges": Value::Array(vec![]),
            "totals": { "nodes": 0, "edges": 0 },
        })));
    }

    // ── 2) Selezione edges fra nodi selezionati ───────────────────────────
    let node_id_vec: Vec<Uuid> = node_ids.iter().copied().collect();
    let edges_json = fetch_graph_edges(
        &state,
        &node_id_vec,
        confidence_min,
        predicates.as_ref(),
        max_edges,
    )
    .await?;

    Ok(Json(json!({
        "nodes": nodes_json,
        "edges": edges_json,
        "totals": {
            "nodes": nodes_json.len(),
            "edges": edges_json.len(),
        },
        "filters": {
            "scope": scope_filter.map(|s| s.as_str()),
            "project_id": q.project_id,
            "confidence_min": confidence_min,
            "predicate": predicates,
            "max_nodes": max_nodes,
            "max_edges": max_edges,
            "seed_doc_id": seed_doc_id,
        }
    })))
}

/// Costruisce ed esegue la query di selezione dei nodi del grafo (con filtri
/// scope/project/seed e ordinamento per degree), poi li serializza. Ritorna
/// l'insieme degli id selezionati (per il filtro edge) e i nodi JSON. Estratta
/// da `get_graph` (behavior-preserving: stessi placeholder e ordine di bind).
#[allow(clippy::too_many_arguments)]
async fn fetch_graph_nodes(
    state: &AppState,
    acl_clause: &str,
    acl_projects: &[Uuid],
    scope_filter: Option<WikiScope>,
    project_id: Option<Uuid>,
    seed_doc_id: Option<Uuid>,
    max_nodes: usize,
) -> Result<(std::collections::HashSet<Uuid>, Vec<Value>), (StatusCode, String)> {
    let acl_param_used = !acl_projects.is_empty();
    let start_idx = if acl_param_used { 2 } else { 1 };
    let (nodes_sql, bind_scope, bind_project) =
        build_graph_nodes_sql(acl_clause, start_idx, scope_filter, project_id, seed_doc_id);

    let mut q_nodes = sqlx::query(&nodes_sql);
    if acl_param_used {
        q_nodes = q_nodes.bind(acl_projects.to_vec());
    }
    if let Some(s) = bind_scope {
        q_nodes = q_nodes.bind(s);
    }
    if let Some(pid) = bind_project {
        q_nodes = q_nodes.bind(pid);
    }
    if let Some(seed) = seed_doc_id {
        q_nodes = q_nodes.bind(seed);
    }
    q_nodes = q_nodes.bind(max_nodes as i64);

    let node_rows = q_nodes.fetch_all(&state.db).await.map_err(err500)?;
    let mut node_ids: std::collections::HashSet<Uuid> =
        std::collections::HashSet::with_capacity(node_rows.len());
    let mut nodes_json: Vec<Value> = Vec::with_capacity(node_rows.len());
    for row in node_rows {
        let (id, value) = map_graph_node_row(row);
        node_ids.insert(id);
        nodes_json.push(value);
    }
    Ok((node_ids, nodes_json))
}

/// Compone la SQL di selezione nodi del grafo con i filtri scope/project/seed a
/// partire da `start_idx`, e ritorna anche i valori scope/project da bindare (nel
/// medesimo ordine dei placeholder). Il placeholder di `LIMIT` e' l'indice finale.
/// Estratta da `fetch_graph_nodes` (behavior-preserving).
fn build_graph_nodes_sql(
    acl_clause: &str,
    start_idx: usize,
    scope_filter: Option<WikiScope>,
    project_id: Option<Uuid>,
    seed_doc_id: Option<Uuid>,
) -> (String, Option<String>, Option<Uuid>) {
    let mut next_idx = start_idx;
    let mut node_where: Vec<String> = vec![acl_clause.to_string()];
    let mut bind_scope: Option<String> = None;
    let mut bind_project: Option<Uuid> = None;

    if let Some(s) = scope_filter {
        node_where.push(format!("wiki_docs.scope = ${next_idx}"));
        bind_scope = Some(s.as_str().to_string());
        next_idx += 1;
    }
    if let Some(pid) = project_id {
        node_where.push(format!("wiki_docs.project_id = ${next_idx}"));
        bind_project = Some(pid);
        next_idx += 1;
    }
    // Se seed_doc_id e' presente: restringi nodi ai vicini di 1 hop + seed.
    if seed_doc_id.is_some() {
        let idx = next_idx;
        next_idx += 1;
        node_where.push(format!(
            "wiki_docs.id IN ( \
                SELECT ${idx}::uuid UNION \
                SELECT to_doc_id FROM wiki_links WHERE from_doc_id = ${idx}::uuid UNION \
                SELECT from_doc_id FROM wiki_links WHERE to_doc_id = ${idx}::uuid \
             )"
        ));
    }

    // Order: degree DESC (numero edge cui partecipa) per privilegiare i piu' centrali.
    let nodes_sql = format!(
        "SELECT wiki_docs.id, wiki_docs.scope, wiki_docs.project_id, wiki_docs.slug, \
                wiki_docs.title, wiki_docs.kind, \
                ( (SELECT COUNT(*) FROM wiki_links WHERE from_doc_id = wiki_docs.id) \
                + (SELECT COUNT(*) FROM wiki_links WHERE to_doc_id   = wiki_docs.id) ) AS degree \
         FROM wiki_docs WHERE {} \
         ORDER BY degree DESC, wiki_docs.updated_at DESC \
         LIMIT ${}",
        node_where.join(" AND "),
        next_idx
    );
    (nodes_sql, bind_scope, bind_project)
}

/// Serializza una riga nodo del grafo. Ritorna `(id, json)`: l'id serve al
/// chiamante per popolare l'insieme dei nodi selezionati. Formato RAW conforme
/// al contratto TS WikiGraphNode (id/title/kind/...): NON wrappare in
/// {data:{label}} (il frontend ricostruisce gli elementi Cytoscape da questi
/// campi; con {data:{}} n.title era undefined -> crash).
fn map_graph_node_row(row: sqlx::postgres::PgRow) -> (Uuid, Value) {
    use sqlx::Row;
    let id: Uuid = row.try_get("id").unwrap_or_default();
    let scope: String = row.try_get("scope").unwrap_or_default();
    let project_id: Option<Uuid> = row.try_get("project_id").ok();
    let slug: String = row.try_get("slug").unwrap_or_default();
    let title: String = row.try_get("title").unwrap_or_default();
    let kind: String = row.try_get("kind").unwrap_or_default();
    let degree: i64 = row.try_get("degree").unwrap_or(0);
    let value = json!({
        "id": id.to_string(),
        "title": title,
        "slug": slug,
        "kind": kind,
        "scope": scope,
        "project_id": project_id,
        "degree": degree,
    });
    (id, value)
}

/// Serializza una riga edge del grafo. Formato RAW conforme al contratto TS
/// WikiGraphEdge (from/to/rel_type/...).
fn map_graph_edge_row(row: sqlx::postgres::PgRow) -> Value {
    use sqlx::Row;
    let from_id: Uuid = row.try_get("from_doc_id").unwrap_or_default();
    let to_id: Uuid = row.try_get("to_doc_id").unwrap_or_default();
    let rel_type: String = row.try_get("rel_type").unwrap_or_default();
    let confidence: f32 = row.try_get("confidence").unwrap_or(0.0);
    let created_by: String = row.try_get("created_by").unwrap_or_default();
    let evidence: Option<String> = row.try_get("evidence").ok();
    json!({
        "from": from_id.to_string(),
        "to": to_id.to_string(),
        "rel_type": rel_type,
        "confidence": confidence,
        "created_by": created_by,
        "evidence": evidence,
    })
}

/// Costruisce ed esegue la query degli edge fra i nodi selezionati, applicando
/// soglia di confidence e (opzionale) filtro predicate. Estratta da `get_graph`.
async fn fetch_graph_edges(
    state: &AppState,
    node_id_vec: &[Uuid],
    confidence_min: f32,
    predicates: Option<&Vec<String>>,
    max_edges: usize,
) -> Result<Vec<Value>, (StatusCode, String)> {
    let mut edge_where: Vec<String> = vec![
        "wiki_links.from_doc_id = ANY($1::uuid[])".to_string(),
        "wiki_links.to_doc_id   = ANY($1::uuid[])".to_string(),
        "wiki_links.confidence >= $2".to_string(),
    ];
    let mut edge_next_idx = 3usize;
    if predicates.is_some() {
        edge_where.push(format!(
            "wiki_links.rel_type = ANY(${edge_next_idx}::text[])"
        ));
        edge_next_idx += 1;
    }
    let edges_sql = format!(
        "SELECT from_doc_id, to_doc_id, rel_type, confidence, created_by, evidence \
         FROM wiki_links WHERE {} \
         ORDER BY confidence DESC \
         LIMIT ${}",
        edge_where.join(" AND "),
        edge_next_idx
    );

    let mut q_edges = sqlx::query(&edges_sql)
        .bind(node_id_vec)
        .bind(confidence_min);
    if let Some(preds) = predicates {
        q_edges = q_edges.bind(preds.clone());
    }
    q_edges = q_edges.bind(max_edges as i64);

    let edge_rows = q_edges.fetch_all(&state.db).await.map_err(err500)?;
    Ok(edge_rows.into_iter().map(map_graph_edge_row).collect())
}

// ───────────────────────────────────────────────────────────────────────────
// Triple extraction (ADR 0017 v2 F5)
// ───────────────────────────────────────────────────────────────────────────

/// `POST /api/wiki/extract-triples?scope=&project_id=&doc_id=&wait=&override_cap=`
///
/// Admin-only. Forza l'estrazione LLM delle triple semantiche.
/// - Con `doc_id`: estrazione sincrona (o async se wait=false) di UN solo doc;
///   `override_cap=true` bypassa il cap diurno per smoke test.
/// - Senza `doc_id`: batch su scope (rispetta il cap diurno).
pub async fn extract_triples_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ExtractTriplesQuery>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;
    require_admin(&acl, "solo admin puo' lanciare wiki.extract-triples")?;

    let wait = q.wait.unwrap_or(true);
    // override_cap segnala la volonta' di forzare anche se cap raggiunto;
    // `extract_triples_for_doc` non controlla il cap (lo fa solo il batch),
    // quindi qui e' un no-op esplicito.
    let _ = q.override_cap.unwrap_or(false);

    // Path: singolo doc.
    if let Some(doc_id) = q.doc_id {
        if wait {
            let report = triple_extractor::extract_triples_for_doc(&state.wiki_deps(), doc_id)
                .await
                .map_err(err500)?;
            return Ok((
                StatusCode::OK,
                Json(serde_json::to_value(report).unwrap_or_else(|_| json!({}))),
            ));
        }
        let run_id = Uuid::new_v4();
        spawn_extract_triples_doc_bg(state.clone(), run_id, doc_id);
        return Ok(accepted_run(run_id, json!({})));
    }

    // Path: batch per scope.
    let scope_label = q.scope.as_deref().unwrap_or("all").to_lowercase();

    if wait {
        // wait=true sul batch e' supportato ma puo' essere lungo (tanti doc x ~1s LLM).
        let (processed, details) =
            run_extract_triples_batch(&state, &scope_label, q.project_id).await?;
        return Ok((
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "processed_total": processed,
                "details": details,
            })),
        ));
    }

    // wait=false: lancia in background.
    let run_id = Uuid::new_v4();
    spawn_extract_triples_batch_bg(state.clone(), run_id, scope_label.clone(), q.project_id);
    Ok(accepted_run(
        run_id,
        json!({ "scope": scope_label, "project_id": q.project_id }),
    ))
}

/// Task background di `extract_triples_for_doc` (singolo doc): logga esito.
fn spawn_extract_triples_doc_bg(state: AppState, run_id: Uuid, doc_id: Uuid) {
    tokio::spawn(async move {
        tracing::info!(
            run_id = %run_id,
            doc_id = %doc_id,
            "wiki.extract-triples: avvio task background (singolo doc)"
        );
        match triple_extractor::extract_triples_for_doc(&state.wiki_deps(), doc_id).await {
            Ok(rep) => tracing::info!(
                run_id = %run_id,
                extracted = rep.triples_extracted,
                low_conf = rep.triples_skipped_low_conf,
                unresolved = rep.triples_unresolved_doc,
                elapsed_ms = rep.elapsed_ms,
                "wiki.extract-triples: completato (singolo doc)"
            ),
            Err(e) => tracing::error!(
                run_id = %run_id,
                error = %e,
                "wiki.extract-triples: fallito (singolo doc)"
            ),
        }
    });
}

/// Batch sincrono di estrazione triple su scope meta e/o progetto. Ritorna
/// `(processed_total, dettaglio_aggregato)`. Estratto da `extract_triples_handler`.
async fn run_extract_triples_batch(
    state: &AppState,
    scope_label: &str,
    project_id: Option<Uuid>,
) -> Result<(usize, serde_json::Map<String, Value>), (StatusCode, String)> {
    let mut aggregated = serde_json::Map::new();
    let mut overall_processed = 0usize;

    if scope_label == "meta" || scope_label == "all" {
        let rep = triple_extractor::extract_triples_for_scope(
            &state.wiki_deps(),
            triple_extractor::ExtractScope::Meta,
        )
        .await
        .map_err(err500)?;
        overall_processed += rep.processed_count;
        aggregated.insert(
            "meta".to_string(),
            serde_json::to_value(rep).unwrap_or(Value::Null),
        );
    }
    if scope_label == "project" || scope_label == "all" {
        // Se project_id specifico, batch solo su quello; altrimenti tutti.
        let project_ids = resolve_project_ids(&state.db, project_id).await?;
        let mut projects_map = serde_json::Map::new();
        for pid in project_ids {
            let rep = triple_extractor::extract_triples_for_scope(
                &state.wiki_deps(),
                triple_extractor::ExtractScope::Project(pid),
            )
            .await
            .map_err(err500)?;
            overall_processed += rep.processed_count;
            projects_map.insert(
                pid.to_string(),
                serde_json::to_value(rep).unwrap_or(Value::Null),
            );
        }
        aggregated.insert("projects".to_string(), Value::Object(projects_map));
    }
    Ok((overall_processed, aggregated))
}

/// Elenco dei progetti su cui iterare: il singolo `project_id` se dato, altrimenti
/// tutti quelli in `projects`. Punto unico della SELECT (regola L).
async fn resolve_project_ids(
    db: &sqlx::PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    if let Some(pid) = project_id {
        return Ok(vec![pid]);
    }
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM projects")
        .fetch_all(db)
        .await
        .map_err(err500)
}

/// Task background del batch triple: itera scope meta/progetto loggando i fallimenti
/// come WARN (best-effort, non blocca). Estratto da `extract_triples_handler`.
fn spawn_extract_triples_batch_bg(
    state: AppState,
    run_id: Uuid,
    scope_label: String,
    project_id: Option<Uuid>,
) {
    tokio::spawn(async move {
        tracing::info!(
            run_id = %run_id,
            scope = %scope_label,
            project_id = ?project_id,
            "wiki.extract-triples: avvio task background (batch)"
        );
        if scope_label == "meta" || scope_label == "all" {
            if let Err(e) = triple_extractor::extract_triples_for_scope(
                &state.wiki_deps(),
                triple_extractor::ExtractScope::Meta,
            )
            .await
            {
                tracing::warn!(run_id=%run_id, error=%e, "batch meta fallito");
            }
        }
        if scope_label == "project" || scope_label == "all" {
            let pids: Vec<Uuid> = if let Some(pid) = project_id {
                vec![pid]
            } else {
                // Punto unico dell'elenco progetti (regola L): degrada a vuoto
                // con WARN, coerente col best-effort di questo task background.
                crate::project_db_routes::list_all_project_ids(&state.db).await
            };
            for pid in pids {
                if let Err(e) = triple_extractor::extract_triples_for_scope(
                    &state.wiki_deps(),
                    triple_extractor::ExtractScope::Project(pid),
                )
                .await
                {
                    tracing::warn!(run_id=%run_id, project_id=%pid, error=%e, "batch project fallito");
                }
            }
        }
        tracing::info!(run_id=%run_id, "wiki.extract-triples: task background terminato");
    });
}

// SQL di `list_doc_triples`. Outbound: subj=this, arricchimento del target SOLO
// se obj_doc_id non nullo (i concetti liberi/external non hanno doc visibile) ->
// LEFT JOIN. Inbound: obj=this, il subject e' sempre una doc -> JOIN.
const SQL_DOC_TRIPLES_OUTBOUND: &str =
    "SELECT t.id, t.predicate, t.obj_doc_id, t.obj_text, t.obj_external, \
            t.source, t.confidence, t.evidence, t.created_at, \
            d.scope AS target_scope, d.slug AS target_slug, \
            d.title AS target_title, d.kind AS target_kind, \
            d.project_id AS target_project_id \
     FROM wiki_concept_triples t \
     LEFT JOIN wiki_docs d ON d.id = t.obj_doc_id \
     WHERE t.subj_doc_id = $1 \
     ORDER BY t.confidence DESC, t.predicate ASC";

const SQL_DOC_TRIPLES_INBOUND: &str =
    "SELECT t.id, t.predicate, t.subj_doc_id, t.source, t.confidence, t.evidence, t.created_at, \
            d.scope AS subj_scope, d.slug AS subj_slug, d.title AS subj_title, \
            d.kind AS subj_kind, d.project_id AS subj_project_id \
     FROM wiki_concept_triples t \
     JOIN wiki_docs d ON d.id = t.subj_doc_id \
     WHERE t.obj_doc_id = $1 \
     ORDER BY t.confidence DESC, t.predicate ASC";

/// `GET /api/wiki/docs/:id/triples` — outbound (subj=this) + inbound (obj_doc=this).
pub async fn list_doc_triples(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;

    // Verifica visibilita' del doc sorgente.
    let source = storage::get_doc(&state.wiki_deps(), &acl, id)
        .await
        .map_err(err500)?;
    if source.is_none() {
        return Err(err404("documento non trovato o non accessibile"));
    }

    let outbound_rows = sqlx::query(SQL_DOC_TRIPLES_OUTBOUND)
        .bind(id)
        .fetch_all(&state.db)
        .await
        .map_err(err500)?;

    let inbound_rows = sqlx::query(SQL_DOC_TRIPLES_INBOUND)
        .bind(id)
        .fetch_all(&state.db)
        .await
        .map_err(err500)?;

    let outbound: Vec<Value> = outbound_rows
        .into_iter()
        .map(map_triple_outbound_row)
        .collect();
    let inbound: Vec<Value> = inbound_rows
        .into_iter()
        .map(map_triple_inbound_row)
        .collect();

    Ok(Json(json!({
        "doc_id": id,
        "outbound": outbound,
        "inbound": inbound,
        "totals": {
            "outbound": outbound.len(),
            "inbound": inbound.len(),
        }
    })))
}

/// Serializza una tripla outbound (subj=doc corrente) col target arricchito.
/// Estratta dalla closure di `list_doc_triples`.
fn map_triple_outbound_row(r: sqlx::postgres::PgRow) -> Value {
    use sqlx::Row;
    let tid: Uuid = r.try_get("id").unwrap_or_default();
    let predicate: String = r.try_get("predicate").unwrap_or_default();
    let obj_doc_id: Option<Uuid> = r.try_get("obj_doc_id").ok();
    let obj_text: Option<String> = r.try_get("obj_text").ok();
    let obj_external: Option<String> = r.try_get("obj_external").ok();
    let source: String = r.try_get("source").unwrap_or_default();
    let confidence: f32 = r.try_get("confidence").unwrap_or(0.0);
    let evidence: Option<String> = r.try_get("evidence").ok();
    let target_title: Option<String> = r.try_get("target_title").ok();
    let target_slug: Option<String> = r.try_get("target_slug").ok();
    let target_scope: Option<String> = r.try_get("target_scope").ok();
    let target_kind: Option<String> = r.try_get("target_kind").ok();
    let target_project_id: Option<Uuid> = r.try_get("target_project_id").ok();
    json!({
        "id": tid,
        "direction": "outbound",
        "predicate": predicate,
        "source": source,
        "confidence": confidence,
        "evidence": evidence,
        "object": {
            "doc_id": obj_doc_id,
            "text": obj_text,
            "external": obj_external,
            "title": target_title,
            "slug": target_slug,
            "scope": target_scope,
            "kind": target_kind,
            "project_id": target_project_id,
        }
    })
}

/// Serializza una tripla inbound (obj=doc corrente) col subject arricchito.
/// Estratta dalla closure di `list_doc_triples`.
fn map_triple_inbound_row(r: sqlx::postgres::PgRow) -> Value {
    use sqlx::Row;
    let tid: Uuid = r.try_get("id").unwrap_or_default();
    let predicate: String = r.try_get("predicate").unwrap_or_default();
    let subj_doc_id: Uuid = r.try_get("subj_doc_id").unwrap_or_default();
    let source: String = r.try_get("source").unwrap_or_default();
    let confidence: f32 = r.try_get("confidence").unwrap_or(0.0);
    let evidence: Option<String> = r.try_get("evidence").ok();
    let subj_title: String = r.try_get("subj_title").unwrap_or_default();
    let subj_slug: String = r.try_get("subj_slug").unwrap_or_default();
    let subj_scope: String = r.try_get("subj_scope").unwrap_or_default();
    let subj_kind: String = r.try_get("subj_kind").unwrap_or_default();
    let subj_project_id: Option<Uuid> = r.try_get("subj_project_id").ok();
    json!({
        "id": tid,
        "direction": "inbound",
        "predicate": predicate,
        "source": source,
        "confidence": confidence,
        "evidence": evidence,
        "subject": {
            "doc_id": subj_doc_id,
            "title": subj_title,
            "slug": subj_slug,
            "scope": subj_scope,
            "kind": subj_kind,
            "project_id": subj_project_id,
        }
    })
}

/// Valori da bindare ai placeholder dei filtri di `list_triples`, nell'ordine
/// in cui le clausole vengono aggiunte al WHERE (posizionali $N).
#[derive(Default)]
struct TripleListBinds {
    predicate: Option<String>,
    source: Option<String>,
    min_conf: Option<f32>,
    subj: Option<Uuid>,
    obj: Option<Uuid>,
    q: Option<String>,
    scope: Option<String>,
    project: Option<Uuid>,
}

/// Compone dinamicamente le clausole WHERE dei filtri di `list_triples`,
/// assegnando i placeholder `$N` a partire da `start_idx`. Ritorna le clausole
/// (inclusa quella ACL passata come primo elemento), il prossimo indice libero
/// e i valori da bindare. Estratta da `list_triples` (behavior-preserving:
/// stesso ordine di clausole e placeholder).
fn build_triple_filters(
    q: &ListTriplesQuery,
    acl_clause: String,
    start_idx: usize,
) -> (Vec<String>, usize, TripleListBinds) {
    let mut where_parts: Vec<String> = vec![acl_clause];
    let mut next_idx = start_idx;
    let mut binds = TripleListBinds::default();

    if let Some(p) = q.predicate.as_ref().filter(|s| !s.is_empty()) {
        where_parts.push(format!("t.predicate = ${next_idx}"));
        binds.predicate = Some(p.clone());
        next_idx += 1;
    }
    if let Some(s) = q.source.as_ref().filter(|s| !s.is_empty()) {
        where_parts.push(format!("t.source = ${next_idx}"));
        binds.source = Some(s.clone());
        next_idx += 1;
    }
    if let Some(c) = q.min_confidence {
        where_parts.push(format!("t.confidence >= ${next_idx}"));
        binds.min_conf = Some(c.clamp(0.0, 1.0));
        next_idx += 1;
    }
    if let Some(sid) = q.subj_id {
        where_parts.push(format!("t.subj_doc_id = ${next_idx}"));
        binds.subj = Some(sid);
        next_idx += 1;
    }
    if let Some(oid) = q.obj_id {
        where_parts.push(format!("t.obj_doc_id = ${next_idx}"));
        binds.obj = Some(oid);
        next_idx += 1;
    }
    if let Some(qs) = q.q.as_ref().filter(|s| !s.is_empty()) {
        where_parts.push(format!("t.obj_text ILIKE ${next_idx}"));
        binds.q = Some(format!("%{qs}%"));
        next_idx += 1;
    }
    // Filtri scope/project_id: erano accettati dal frontend ma IGNORATI dal
    // backend (i campi mancavano dalla struct), causando cross-contaminazione
    // tra progetti — le triple di Beauty-Book apparivano anche in Marco perche'
    // l'ACL utente vedeva entrambi. JOIN su wiki_docs gia' esistente.
    if let Some(sc) = q.scope.as_ref().filter(|s| !s.is_empty()) {
        where_parts.push(format!("wiki_docs.scope = ${next_idx}"));
        binds.scope = Some(sc.clone());
        next_idx += 1;
    }
    if let Some(pid) = q.project_id {
        where_parts.push(format!("wiki_docs.project_id = ${next_idx}"));
        binds.project = Some(pid);
        next_idx += 1;
    }

    (where_parts, next_idx, binds)
}

/// `GET /api/wiki/triples?predicate=&source=&min_confidence=&subj_id=&obj_id=&q=&limit=&offset=`
///
/// Lista triple paginata. ACL: l'utente vede solo triple il cui `subj_doc`
/// rientra nel suo scope visibile. Filtri composti dinamicamente.
pub async fn list_triples(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ListTriplesQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let acl = build_acl(&state, &claims).await?;

    let (acl_clause, acl_projects) = acl.scope_clause(1);
    let acl_param_used = !acl_projects.is_empty();

    let start_idx = if acl_param_used { 2usize } else { 1usize };
    let (where_parts, next_idx, binds) = build_triple_filters(&q, acl_clause, start_idx);

    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);

    let items = fetch_triples_page(
        &state,
        &where_parts,
        next_idx,
        acl_param_used,
        &acl_projects,
        binds,
        limit,
        offset,
    )
    .await?;

    // NB: ritorniamo sia `total` (atteso dal frontend per la paginazione) sia
    // `count` (alias storico). Per ora `total = items.len()` (non e' una vera
    // COUNT(*) con i filtri applicati — il client comunque paginando ne ottiene
    // solo gli items del lotto corrente). Una COUNT separata e' debito noto.
    Ok(Json(json!({
        "items": items,
        "limit": limit,
        "offset": offset,
        "count": items.len(),
        "total": items.len(),
    })))
}

/// Compone la SQL finale (con LIMIT/OFFSET agli indici `next_idx`/`next_idx+1`),
/// binda i parametri nell'ordine ESATTO in cui le clausole sono state aggiunte (i
/// placeholder $N sono posizionali: acl, filtri, poi limit/offset) ed esegue la
/// query, mappando le righe. Estratta da `list_triples` (behavior-preserving).
#[allow(clippy::too_many_arguments)]
async fn fetch_triples_page(
    state: &AppState,
    where_parts: &[String],
    next_idx: usize,
    acl_param_used: bool,
    acl_projects: &[Uuid],
    binds: TripleListBinds,
    limit: i64,
    offset: i64,
) -> Result<Vec<Value>, (StatusCode, String)> {
    let sql = format!(
        "SELECT t.id, t.subj_doc_id, t.predicate, t.obj_doc_id, t.obj_text, t.obj_external, \
                t.source, t.confidence, t.evidence, t.created_at \
         FROM wiki_concept_triples t \
         JOIN wiki_docs ON wiki_docs.id = t.subj_doc_id \
         WHERE {} \
         ORDER BY t.created_at DESC, t.confidence DESC \
         LIMIT ${} OFFSET ${}",
        where_parts.join(" AND "),
        next_idx,
        next_idx + 1,
    );

    let mut query = sqlx::query(&sql);
    if acl_param_used {
        query = query.bind(acl_projects.to_vec());
    }
    if let Some(v) = binds.predicate {
        query = query.bind(v);
    }
    if let Some(v) = binds.source {
        query = query.bind(v);
    }
    if let Some(v) = binds.min_conf {
        query = query.bind(v);
    }
    if let Some(v) = binds.subj {
        query = query.bind(v);
    }
    if let Some(v) = binds.obj {
        query = query.bind(v);
    }
    if let Some(v) = binds.q {
        query = query.bind(v);
    }
    if let Some(v) = binds.scope {
        query = query.bind(v);
    }
    if let Some(v) = binds.project {
        query = query.bind(v);
    }
    query = query.bind(limit).bind(offset);

    let rows = query.fetch_all(&state.db).await.map_err(err500)?;
    Ok(rows.into_iter().map(map_triple_list_row).collect())
}

/// Serializza una riga della lista triple paginata (`list_triples`). Estratta
/// dalla closure inline; formato piatto atteso dal frontend.
fn map_triple_list_row(r: sqlx::postgres::PgRow) -> Value {
    use sqlx::Row;
    let id: Uuid = r.try_get("id").unwrap_or_default();
    let subj: Uuid = r.try_get("subj_doc_id").unwrap_or_default();
    let predicate: String = r.try_get("predicate").unwrap_or_default();
    let obj_doc_id: Option<Uuid> = r.try_get("obj_doc_id").ok();
    let obj_text: Option<String> = r.try_get("obj_text").ok();
    let obj_external: Option<String> = r.try_get("obj_external").ok();
    let source: String = r.try_get("source").unwrap_or_default();
    let confidence: f32 = r.try_get("confidence").unwrap_or(0.0);
    let evidence: Option<String> = r.try_get("evidence").ok();
    json!({
        "id": id,
        "subj_doc_id": subj,
        "predicate": predicate,
        "obj_doc_id": obj_doc_id,
        "obj_text": obj_text,
        "obj_external": obj_external,
        "source": source,
        "confidence": confidence,
        "evidence": evidence,
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Router merge
// ───────────────────────────────────────────────────────────────────────────

use crate::middleware;
use axum::{
    middleware as axum_mw,
    routing::{get, post},
    Router,
};

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    // Tutte le route wiki richiedono `require_auth`: prima le registr
    // (senza layer per-route), poi applico il middleware UNA volta con
    // `route_layer` — equivalente osservabile ai `.layer` per-endpoint
    // precedenti (il layer copre le route ma non il fallback), regola L.
    let wiki = Router::<AppState>::new()
        .route("/api/wiki/docs", get(list_docs).post(create_doc))
        .route(
            "/api/wiki/docs/:id",
            get(get_doc).patch(patch_doc).delete(delete_doc),
        )
        .route("/api/wiki/docs/:id/revisions", get(list_revisions))
        .route("/api/wiki/docs/:id/revisions/:version", get(get_revision))
        .route("/api/wiki/docs/:id/diff", get(diff))
        .route("/api/wiki/docs/:id/restore", post(restore))
        .route("/api/wiki/reingest", post(reingest_handler))
        .route("/api/wiki/recompute-links", post(recompute_links_handler))
        .route("/api/wiki/recompute-titles", post(recompute_titles_handler))
        .route("/api/wiki/docs/:id/links", get(list_doc_links))
        .route("/api/wiki/graph", get(get_graph))
        .route("/api/wiki/extract-triples", post(extract_triples_handler))
        .route("/api/wiki/docs/:id/triples", get(list_doc_triples))
        .route("/api/wiki/triples", get(list_triples))
        .route("/api/wiki/search", post(wiki_search::search))
        .route_layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::require_auth,
        ));
    router.merge(wiki)
}
