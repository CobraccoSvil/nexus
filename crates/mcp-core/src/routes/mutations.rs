//! Route per `file_mutations` (mig 0349). Mirror dello stile di `documents.rs`:
//! ogni endpoint protetto da `middleware::require_auth`.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        .route(
            "/api/projects/:id/mutations",
            get(mutations_api::list_mutations).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/mutations/revert-last",
            post(mutations_api::revert_last).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/mutations/:mid",
            get(mutations_api::get_mutation).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/mutations/:mid/revert",
            post(mutations_api::revert_mutation).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
