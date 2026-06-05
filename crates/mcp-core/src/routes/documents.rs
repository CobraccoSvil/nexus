//! Route documenti e servizi/comandi progetto.
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // ── documents ──────────────────────────────────────────
        .route(
            "/api/projects/:id/documents",
            get(documents::list_documents).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/documents/:doc_id",
            get(documents::get_document)
                .delete(documents::delete_document)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/projects/:id/documents/:doc_id/download",
            get(documents::download_document).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/documents/:doc_id/versions",
            get(documents::list_versions).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/problems",
            get(project_workspace::get_project_problems).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/output/channels",
            get(project_workspace::get_output_channels).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/output/events",
            get(project_workspace::get_output_events).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        // NOTA: rimosse le 3 route /api/projects/:id/services/:service/proxy{,/,/*path}
        // che facevano riferimento a `project_workspace::proxy_root` e `proxy_path`.
        // Il modulo `proxy.rs` non era mai stato committato (untracked) ed e' stato
        // rimosso dal filesystem dopo una sessione di pulizia. Riferimenti orfani
        // bloccavano la compilazione. Quando si vorra' reimplementare il proxy
        // servizi-progetto, vanno create sia `project_workspace/proxy.rs` con gli
        // handler che le route qui sopra.
        // Route azione servizio — ripristino /:action (POST only) per mantenere la
        // firma originale di control_project_service (Path<(String, String, String)>).
        // matchit dà priorità al segmento statico "proxy" su "/:action" parametrico.
        .route(
            "/api/projects/:id/services/:service/:action",
            post(project_workspace::control_project_service).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services",
            get(project_workspace::get_project_services_status).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services/wizard/detect",
            get(project_workspace::wizard_detect_services).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        // Rileva se il progetto e' un sito HTML statico servibile dal server
        // integrato (static_preview), con entry e URL di preview. Il pannello
        // SERVIZI mostra la card "Sito statico" con il pulsante Apri.
        .route(
            "/api/projects/:id/static-site",
            get(static_preview::static_site_info).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services/wizard/install",
            post(project_workspace::wizard_install_service).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services/restart-all",
            post(project_workspace::restart_all_project_services).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/projects/:id/services/cleanup-ports",
            post(project_workspace::cleanup_project_ports).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services/allocate-port",
            post(project_workspace::allocate_project_port).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services/kill-orphan-processes",
            post(project_workspace::kill_project_orphan_processes).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/projects/:id/services/kill-port-process",
            post(project_workspace::kill_project_port_process).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/services/:service",
            axum::routing::delete(project_workspace::uninstall_project_service).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/projects/:id/changes",
            get(project_workspace::get_project_changes).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/ports",
            get(project_workspace::get_project_ports).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/projects/:id/port-allocations",
            get(project_workspace::get_port_allocations)
                .post(project_workspace::create_port_allocation)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/projects/:id/port-allocations/:port",
            axum::routing::delete(project_workspace::delete_port_allocation).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
}
