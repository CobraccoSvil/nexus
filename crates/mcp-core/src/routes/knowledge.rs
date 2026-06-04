//! Route knowledge base (project-scope) — ADR 0017 v2 fase 6.
//!
//! Le rotte `/api/projects/:id/knowledge/*` con equivalente sono thin redirect
//! 308 verso `/api/wiki/*` (vedi `crate::wiki::redirects`). Le altre (rebuild,
//! generate-rich, extract-functional, init-or-refresh, manual-note,
//! obsidian-vault, code-wiki/generate, similar, links, tags) tornano 410 Gone
//! con `migration_adr: 0017`.
//!
//! `/api/projects/:id/agent/todos/:run_id/edit` non e' parte di knowledge graph
//! e resta invariata.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── notes: redirect 308 ───────────────────────────────────────────
        .route(
            "/api/projects/:id/knowledge/notes",
            get(crate::wiki::redirects::knowledge_notes_list),
        )
        .route(
            "/api/projects/:id/knowledge/notes/:note_id",
            get(crate::wiki::redirects::knowledge_note_get)
                .patch(crate::wiki::redirects::knowledge_note_get)
                .delete(crate::wiki::redirects::knowledge_note_get),
        )
        .route(
            "/api/projects/:id/knowledge/notes/:note_id/revisions",
            get(crate::wiki::redirects::knowledge_note_revisions_list),
        )
        .route(
            "/api/projects/:id/knowledge/notes/:note_id/revisions/:version",
            get(crate::wiki::redirects::knowledge_note_revision_get),
        )
        .route(
            "/api/projects/:id/knowledge/notes/:note_id/diff",
            get(crate::wiki::redirects::knowledge_note_diff),
        )
        .route(
            "/api/projects/:id/knowledge/notes/:note_id/restore",
            post(crate::wiki::redirects::knowledge_note_restore),
        )
        // ── graph + recompute-links: redirect 308 ─────────────────────────
        .route(
            "/api/projects/:id/knowledge/graph",
            get(crate::wiki::redirects::knowledge_graph),
        )
        .route(
            "/api/projects/:id/knowledge/recompute-links",
            post(crate::wiki::redirects::knowledge_recompute_links),
        )
        // ── agent todos (fuori dominio ADR 0017): invariata ───────────────
        .route(
            "/api/projects/:id/agent/todos/:run_id/edit",
            post(agent_todos_routes::edit_todo).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        // ── knowledge: 410 Gone (no replacement) ──────────────────────────
        .route(
            "/api/projects/:id/knowledge/similar",
            post(crate::wiki::redirects::gone_knowledge_similar),
        )
        .route(
            "/api/projects/:id/knowledge/code-wiki/generate",
            post(crate::wiki::redirects::gone_knowledge_code_wiki_generate),
        )
        .route(
            "/api/projects/:id/knowledge/links",
            post(crate::wiki::redirects::gone_knowledge_links_create),
        )
        .route(
            "/api/projects/:id/knowledge/links/:link_id",
            axum::routing::delete(crate::wiki::redirects::gone_knowledge_links_delete),
        )
        .route(
            "/api/projects/:id/knowledge/tags",
            get(crate::wiki::redirects::gone_knowledge_tags),
        )
        .route(
            "/api/projects/:id/knowledge/rebuild",
            post(crate::wiki::redirects::gone_knowledge_rebuild),
        )
        .route(
            "/api/projects/:id/knowledge/generate-rich",
            post(crate::wiki::redirects::gone_knowledge_generate_rich),
        )
        .route(
            "/api/projects/:id/knowledge/extract-functional",
            post(crate::wiki::redirects::gone_knowledge_extract_functional),
        )
        .route(
            "/api/projects/:id/knowledge/init-or-refresh",
            post(crate::wiki::redirects::gone_knowledge_init_or_refresh),
        )
        .route(
            "/api/projects/:id/knowledge/notes/manual",
            post(crate::wiki::redirects::gone_knowledge_notes_manual),
        )
        .route(
            "/api/projects/:id/knowledge/obsidian-vault",
            get(crate::wiki::redirects::gone_knowledge_obsidian_vault)
                .put(crate::wiki::redirects::gone_knowledge_obsidian_vault),
        )
}
