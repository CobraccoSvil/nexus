//! Route `GET /api/ui-flags` (ADR 0037).
//!
//! Espone la whitelist di flag UI non sensibili dal DB `settings` a qualunque
//! utente autenticato (`require_auth`, NON `require_admin`): i flag di rendering
//! della chat devono essere leggibili anche dai non admin, altrimenti la feature
//! resterebbe attivabile solo per gli admin (feature morta silenziosa, regola H).
//! La logica dell'handler vive in [`crate::ui_flags`].

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router.route(
        "/api/ui-flags",
        get(ui_flags::get_ui_flags).layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::require_auth,
        )),
    )
}
