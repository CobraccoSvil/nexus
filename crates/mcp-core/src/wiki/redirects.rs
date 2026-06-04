// ═══════════════════════════════════════════════════════════════════════════
// wiki/redirects.rs — Thin redirect 308 dagli endpoint legacy a `/api/wiki/*`.
//
// ADR 0017 v2 fase 6: i vecchi `/api/meta-docs/*` e
// `/api/projects/:id/knowledge/*` restano montati per compatibilita' transitoria
// ma rispondono solo con `308 Permanent Redirect` verso l'equivalente `/api/wiki/*`.
// Le rotte legacy senza equivalente diretto (rebuild knowledge, code-graph,
// export-archive, ecc.) ritornano `410 Gone` con `migration_adr: 0017`.
//
// I moduli `meta_docs/`, `knowledge/`, `docs_core/` restano in compilazione
// come dead code: la fase F8 li rimuovera' fisicamente.
//
// Nota: in axum 0.7 un `Redirect::permanent` usa codice 308 e setta `Location`;
// noi pero' aggiungiamo anche l'header `X-Deprecated` per audit log lato
// client, e logghiamo a WARN ogni hit per consentire l'analisi del traffico
// residuo prima della rimozione definitiva.
// ═══════════════════════════════════════════════════════════════════════════

use axum::{
    body::Body,
    extract::Path,
    http::{header, Response, StatusCode},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// Costruisce una `Response` 308 con `Location` + header di deprecation.
/// Logga sempre a WARN per agevolare audit.
fn build_redirect(old_path: &str, new_location: &str) -> Response<Body> {
    tracing::warn!(
        deprecated_endpoint = old_path,
        use_endpoint = new_location,
        "endpoint deprecato chiamato (ADR 0017 v2 F6)"
    );
    // `Response::builder()...body().unwrap()` e' considerato test-only; qui
    // costruiamo header noti -> non c'e' modo di fallire. Per rispettare la
    // regola "no unwrap fuori test" usiamo `expect` con messaggio diagnostico
    // -- tecnicamente equivalente, ma esplicito sul motivo per cui non puo'
    // fallire. In alternativa potremmo restituire `Result` ma renderebbe il
    // chiamante piu' complesso senza valore aggiunto.
    Response::builder()
        .status(StatusCode::PERMANENT_REDIRECT)
        .header(header::LOCATION, new_location)
        .header("X-Deprecated", format!("use {new_location}"))
        .header("X-Migration-ADR", "0017")
        .body(Body::empty())
        .expect("redirect response header valori statici, build non puo' fallire")
}

/// Risposta `410 Gone` per endpoint legacy senza sostituto in `/api/wiki/*`.
fn build_gone(old_path: &str) -> (StatusCode, Json<Value>) {
    tracing::warn!(
        deprecated_endpoint = old_path,
        "endpoint deprecato 410 Gone (ADR 0017 v2 F6)"
    );
    (
        StatusCode::GONE,
        Json(json!({
            "error": "endpoint deprecated, no replacement",
            "migration_adr": "0017",
            "deprecated_path": old_path,
        })),
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Meta-docs: redirect 308
// ───────────────────────────────────────────────────────────────────────────

pub async fn meta_docs_list() -> Response<Body> {
    build_redirect("/api/meta-docs/list", "/api/wiki/docs?scope=meta")
}

pub async fn meta_docs_get(Path(id): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/meta-docs/:id",
        &format!("/api/wiki/docs/{id}"),
    )
}

pub async fn meta_docs_graph() -> Response<Body> {
    build_redirect("/api/meta-docs/graph", "/api/wiki/graph?scope=meta")
}

pub async fn meta_docs_refresh_all() -> Response<Body> {
    build_redirect(
        "/api/meta-docs/refresh-all",
        "/api/wiki/reingest?scope=meta",
    )
}

pub async fn meta_docs_recompute_links() -> Response<Body> {
    build_redirect(
        "/api/meta-docs/recompute-links",
        "/api/wiki/recompute-links?scope=meta",
    )
}

pub async fn meta_docs_revisions_list(Path(id): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/meta-docs/:id/revisions",
        &format!("/api/wiki/docs/{id}/revisions"),
    )
}

pub async fn meta_docs_revisions_get(Path((id, version)): Path<(Uuid, i32)>) -> Response<Body> {
    build_redirect(
        "/api/meta-docs/:id/revisions/:version",
        &format!("/api/wiki/docs/{id}/revisions/{version}"),
    )
}

pub async fn meta_docs_diff(Path(id): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/meta-docs/:id/diff",
        &format!("/api/wiki/docs/{id}/diff"),
    )
}

pub async fn meta_docs_restore(Path(id): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/meta-docs/:id/restore",
        &format!("/api/wiki/docs/{id}/restore"),
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Project knowledge: redirect 308
// ───────────────────────────────────────────────────────────────────────────

pub async fn knowledge_notes_list(Path(pid): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/notes",
        &format!("/api/wiki/docs?scope=project&project_id={pid}"),
    )
}

pub async fn knowledge_note_get(Path((_pid, note_id)): Path<(Uuid, Uuid)>) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/notes/:note_id",
        &format!("/api/wiki/docs/{note_id}"),
    )
}

pub async fn knowledge_note_revisions_list(
    Path((_pid, note_id)): Path<(Uuid, Uuid)>,
) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/notes/:note_id/revisions",
        &format!("/api/wiki/docs/{note_id}/revisions"),
    )
}

pub async fn knowledge_note_revision_get(
    Path((_pid, note_id, version)): Path<(Uuid, Uuid, i32)>,
) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/notes/:note_id/revisions/:version",
        &format!("/api/wiki/docs/{note_id}/revisions/{version}"),
    )
}

pub async fn knowledge_note_diff(Path((_pid, note_id)): Path<(Uuid, Uuid)>) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/notes/:note_id/diff",
        &format!("/api/wiki/docs/{note_id}/diff"),
    )
}

pub async fn knowledge_note_restore(Path((_pid, note_id)): Path<(Uuid, Uuid)>) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/notes/:note_id/restore",
        &format!("/api/wiki/docs/{note_id}/restore"),
    )
}

pub async fn knowledge_graph(Path(pid): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/graph",
        &format!("/api/wiki/graph?scope=project&project_id={pid}"),
    )
}

pub async fn knowledge_recompute_links(Path(pid): Path<Uuid>) -> Response<Body> {
    build_redirect(
        "/api/projects/:id/knowledge/recompute-links",
        &format!("/api/wiki/recompute-links?scope=project&project_id={pid}"),
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Endpoint legacy senza equivalente -> 410 Gone
// ───────────────────────────────────────────────────────────────────────────

pub async fn gone_meta_docs_ingest_commit() -> (StatusCode, Json<Value>) {
    build_gone("/api/meta-docs/ingest-commit")
}

pub async fn gone_meta_docs_export_archive() -> (StatusCode, Json<Value>) {
    build_gone("/api/meta-docs/export-archive")
}

pub async fn gone_knowledge_similar() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/similar")
}

pub async fn gone_knowledge_links_create() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/links")
}

pub async fn gone_knowledge_links_delete() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/links/:link_id")
}

pub async fn gone_knowledge_tags() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/tags")
}

pub async fn gone_knowledge_rebuild() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/rebuild")
}

pub async fn gone_knowledge_generate_rich() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/generate-rich")
}

pub async fn gone_knowledge_extract_functional() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/extract-functional")
}

pub async fn gone_knowledge_init_or_refresh() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/init-or-refresh")
}

pub async fn gone_knowledge_notes_manual() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/notes/manual")
}

pub async fn gone_knowledge_obsidian_vault() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/obsidian-vault")
}

pub async fn gone_knowledge_code_wiki_generate() -> (StatusCode, Json<Value>) {
    build_gone("/api/projects/:id/knowledge/code-wiki/generate")
}
