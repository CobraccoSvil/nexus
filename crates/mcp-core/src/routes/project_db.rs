//! Route project-database (config, migrazioni, connessioni).
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── project database ────────────────────────────────────
        .route(
            "/api/projects/:id/db",
            get(project_db_routes::get_project_db_config).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/config",
            post(project_db_routes::set_project_db_config).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/migrations",
            get(project_db_routes::list_project_migrations).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/migrations/apply",
            post(project_db_routes::apply_project_migrations).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/migrations/rollback",
            post(project_db_routes::rollback_project_migration).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/override-request",
            post(project_db_routes::request_ddl_override).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/detect",
            post(project_db_routes::detect_project_db).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/test-connection",
            post(project_db_routes::test_project_db_connection).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/provision",
            post(project_db_routes::provision_project_db).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/connections",
            get(project_db_routes::list_project_db_connections).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/connections/:conn_id/set-primary",
            post(project_db_routes::set_primary_project_db_connection).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/projects/:id/db/connections/:conn_id",
            axum::routing::delete(project_db_routes::delete_project_db_connection).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/projects/:id/db/query",
            post(project_db_routes::execute_project_db_query).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/db/import-schema",
            post(project_db_routes::import_project_db_schema).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
