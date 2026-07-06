//! Registrazione route di compatibilita' "neural-core" sotto `/api/neural/*`.
//!
//! Il frontend web-ide consuma questi endpoint via proxy Next.js (`/neural/*` e
//! `/api/neural/*`) ripuntato a mcp-core:4000/api/neural/*. Le forme di output
//! sono 1:1 con quelle che il brain Python (`/health`, `/classify-intent`,
//! `/route-model`, `/providers/...`, `/reload-settings`) esponeva.
//!
//! NO-AUTH (parita' col brain): il brain non applicava JWT su questi router; il
//! proxy server-side gira su localhost. `getProviderModels` (frontend) usa
//! `fetchJsonNoAuth` e `settings-panel`/`ide-shell` chiamano `/neural/*` senza
//! `credentials`. Mantenere no-auth evita di rompere il parsing del frontend.
//! Gli handler sono read-only o no-op; nessuna mutazione sensibile esposta.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, _state: &AppState) -> Router<AppState> {
    router
        .route("/api/neural/health", get(neural_compat::health))
        .route(
            "/api/neural/classify-intent",
            post(neural_compat::classify_intent),
        )
        .route("/api/neural/route-model", post(neural_compat::route_model))
        .route(
            "/api/neural/providers/billing-cooldown",
            get(neural_compat::billing_cooldown),
        )
        .route("/api/neural/providers", get(neural_compat::providers))
        .route(
            "/api/neural/providers/:provider/models",
            get(neural_compat::provider_models),
        )
        .route(
            "/api/neural/providers/:provider/health",
            get(neural_compat::provider_health),
        )
        .route(
            "/api/neural/reload-settings",
            post(neural_compat::reload_settings),
        )
        // WebSocket PTY del terminale IDE. Si autentica col token firmato in
        // query (`?token=payload.signature`), NON con Bearer: per questo vive
        // nel blocco NO-AUTH come gli altri `/api/neural/*`. L'upgrade verifica
        // il token PRIMA dell'handshake (vedi project_workspace/terminal_ws.rs).
        .route(
            "/api/neural/ws/terminal/:session_id",
            get(crate::project_workspace::terminal_ws_upgrade),
        )
}
