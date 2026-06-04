//! Route change-drafts (ChangeDrafter).
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── change-drafts (ChangeDrafter proposte di modifica) ─
        .route(
            "/api/change-drafts",
            post(change_drafts::create_draft)
                .get(change_drafts::list_drafts)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/change-drafts/:id",
            get(change_drafts::get_draft).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/change-drafts/:id/approve",
            post(change_drafts::approve_draft).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/change-drafts/:id/reject",
            post(change_drafts::reject_draft).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
