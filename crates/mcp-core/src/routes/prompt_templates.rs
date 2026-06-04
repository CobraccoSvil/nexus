//! Route prompt templates.
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // Prompt Templates
        .route(
            "/api/prompt-templates",
            get(prompt_templates::list_templates_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/prompt-templates/:key",
            get(prompt_templates::get_template_handler)
                .put(prompt_templates::upsert_template_handler)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/prompt-templates/:key/disable",
            post(prompt_templates::disable_template_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/prompt-templates/:key/enable",
            post(prompt_templates::enable_template_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/admin/prompt-templates/batch-assign-tools",
            post(prompt_templates::batch_assign_tools_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_admin,
            )),
        )
        .route(
            "/api/admin/available-mcp-tools",
            get(prompt_templates::available_mcp_tools_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_admin,
            )),
        )
        .route(
            "/api/admin/prompt-templates/:key/tools",
            get(prompt_templates::get_prompt_tools_handler)
                .put(prompt_templates::update_prompt_tools_handler)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_admin,
                )),
        )
        .route(
            "/api/prompt-templates/:key/ai-suggest",
            post(prompt_templates::ai_suggest_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/quality/findings/:id/false-positive",
            post(prompt_templates::mark_false_positive_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/quality/false-positive-stats",
            get(prompt_templates::false_positive_stats_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
