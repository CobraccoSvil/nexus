//! Route sicurezza/quote (PR hardening).
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── PR hardening: endpoint sicurezza/quote ────────────────────────
        .route(
            "/api/projects/:id/security/audit",
            get(security::api::get_project_audit).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/security/quota",
            get(security::api::get_project_quota).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/playwright/runs",
            get(project_workspace::get_playwright_runs)
                .delete(project_workspace::clear_playwright_runs)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/projects/:id/playwright/artifact",
            get(project_workspace::serve_playwright_artifact).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/playwright/runs/:run_id",
            get(project_workspace::get_playwright_run_detail).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/playwright/runs/:run_id/stream",
            get(project_workspace::stream_playwright_run).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
