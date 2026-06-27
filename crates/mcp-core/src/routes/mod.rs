//! Composizione del Router axum di mcp-core.
//!
//! Questo modulo raccoglie la registrazione delle route, estratta dal god-file
//! `main.rs`. Ogni sottomodulo espone `merge(router, state) -> Router` che
//! registra SOLO le route del proprio dominio con i loro layer/middleware,
//! preservando esattamente path, metodo HTTP, handler, middleware e ordine.
//!
//! `build_app_router` ricompone i sottogruppi nello stesso ordine originale e
//! applica i layer globali (event capture, body limit, CORS) e `with_state`.

mod admin;
mod change_drafts;
mod chat_commands;
mod dispatcher;
mod documents;
mod knowledge;
mod meta_docs;
mod mutations;
mod neural_compat;
mod project_db;
mod prompt_templates;
mod protected;
mod public;
mod security_quota;

/// Prelude condiviso dai sottomoduli route: tipi e helper di routing axum
/// usati ripetutamente. `AppState`, i moduli handler e `middleware` arrivano
/// invece da `use crate::*` nei singoli file.
pub(crate) mod prelude {
    pub(crate) use axum::{
        middleware as axum_mw,
        routing::{delete, get, patch, post, put},
        Router,
    };
}

use axum::extract::DefaultBodyLimit;
use axum::middleware as axum_mw;
use axum::Router;
use tower_http::cors::CorsLayer;

use crate::middleware;
use crate::AppState;

/// Costruisce il Router completo di mcp-core.
///
/// Ordine e layer identici al blocco `let app = Router::new()...` originale di
/// `main.rs`: i gruppi sono mergeati nello stesso ordine, poi si applicano
/// `event_capture_middleware`, `with_state`, `DefaultBodyLimit` (50 MB) e CORS.
pub fn build_app_router(state: AppState, cors: CorsLayer) -> Router {
    let router = Router::new();
    let router = public::merge(router, &state);
    let router = protected::merge(router, &state);
    // Compat REST "neural-core" (`/api/neural/*`): forme 1:1 col brain Python
    // rimosso, consumate dal frontend web-ide via proxy Next.js.
    let router = neural_compat::merge(router, &state);
    let router = project_db::merge(router, &state);
    let router = knowledge::merge(router, &state);
    let router = meta_docs::merge(router, &state);
    // ADR 0017 v2 — endpoint unificati `/api/wiki/*`. Convivono con i vecchi
    // `/api/meta-docs/*` e `/api/projects/:id/knowledge/*` finche' F8 non
    // rimuovera' i moduli legacy (`meta_docs/`, `knowledge/`, `docs_core/`).
    let router = crate::wiki::routes::merge(router, &state);
    let router = change_drafts::merge(router, &state);
    let router = documents::merge(router, &state);
    let router = mutations::merge(router, &state);
    let router = chat_commands::merge(router, &state);
    let router = security_quota::merge(router, &state);
    let router = dispatcher::merge(router, &state);
    let router = admin::merge(router, &state);
    let router = prompt_templates::merge(router, &state);

    router
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::event_capture_middleware,
        ))
        .with_state(state)
        // Body limit globale = 50 MB. Axum default e' 2 MB, troppo basso
        // per gli allegati in chat (immagini, file di codice). Il limit
        // frontend e' 25 MB; con base64 il payload puo' arrivare a ~33 MB,
        // a cui si aggiunge il resto del JSON (system prompt, history,
        // tool definitions). 50 MB lascia margine ragionevole senza
        // esporre a payload abusivi.
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(cors)
        // Timing HTTP piu' esterno di tutto: misura la durata completa della
        // richiesta (CORS e body-limit inclusi) per /nexus/metrics.
        .layer(axum_mw::from_fn(middleware::http_timing_middleware))
}
