//! Route meta-docs (Nexus self-documentation vault) — ADR 0017 v2 fase 6.
//!
//! Tutte le rotte `/api/meta-docs/*` sono ora thin redirect 308 verso gli
//! equivalenti `/api/wiki/*` (vedi `crate::wiki::redirects`). Le rotte senza
//! equivalente (`ingest-commit`, `export-archive`) ritornano 410 Gone con
//! `migration_adr: 0017`.
//!
//! Le rotte fuori dominio meta-docs (`/api/claude-agents/*`) restano invariate:
//! non sono toccate da questa ADR.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── claude-agents (NON parte di ADR 0017): invariate ──────────────
        .route(
            "/api/claude-agents/preview",
            get(claude_agents::preview_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/claude-agents/regenerate",
            post(claude_agents::regenerate_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        // ── meta-docs: redirect 308 verso /api/wiki/* ─────────────────────
        .route(
            "/api/meta-docs/list",
            get(crate::wiki::redirects::meta_docs_list),
        )
        .route(
            "/api/meta-docs/refresh-all",
            post(crate::wiki::redirects::meta_docs_refresh_all),
        )
        .route(
            "/api/meta-docs/:id",
            get(crate::wiki::redirects::meta_docs_get).patch(crate::wiki::redirects::meta_docs_get),
        )
        .route(
            "/api/meta-docs/:id/revisions",
            get(crate::wiki::redirects::meta_docs_revisions_list),
        )
        .route(
            "/api/meta-docs/:id/revisions/:version",
            get(crate::wiki::redirects::meta_docs_revisions_get),
        )
        .route(
            "/api/meta-docs/:id/diff",
            get(crate::wiki::redirects::meta_docs_diff),
        )
        .route(
            "/api/meta-docs/:id/restore",
            post(crate::wiki::redirects::meta_docs_restore),
        )
        .route(
            "/api/meta-docs/graph",
            get(crate::wiki::redirects::meta_docs_graph),
        )
        .route(
            "/api/meta-docs/recompute-links",
            post(crate::wiki::redirects::meta_docs_recompute_links),
        )
        // ── meta-docs: 410 Gone (no replacement) ──────────────────────────
        .route(
            "/api/meta-docs/ingest-commit",
            post(crate::wiki::redirects::gone_meta_docs_ingest_commit),
        )
        .route(
            "/api/meta-docs/export-archive",
            get(crate::wiki::redirects::gone_meta_docs_export_archive),
        )
}
