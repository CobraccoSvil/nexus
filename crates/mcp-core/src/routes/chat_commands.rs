//! Route esecuzione comandi dalla chat.
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── Esecuzione comandi dalla chat ─────────────────────────────────
        .route(
            "/api/projects/:id/execute-command",
            post(project_workspace::execute_command).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
