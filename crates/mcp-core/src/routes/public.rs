//! Route pubbliche (no auth) + endpoint /api/internal/* per il brain.
//!
//! Estratte da `main.rs` durante il refactor del god-file. Nessun
//! cambiamento di path, metodo HTTP, handler o middleware.

use crate::routes::prelude::*;
use crate::*;

pub fn merge(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    router
        // Public routes (no auth)
        .route("/health", get(health))
        .route("/api/health", get(health))
        .route("/api/dashboard", get(dashboard))
        // Server statico integrato per progetti HTML (no auth: deve essere
        // apribile in una nuova scheda del browser, che non porta il JWT).
        // Il path e' confinato rigorosamente alla project_root in
        // static_preview::serve_preview (regola E). `*path` cattura l'intero
        // sotto-percorso; path vuoto -> index.html.
        .route(
            "/preview/:project_id/*path",
            get(static_preview::serve_preview),
        )
        // Nexus (Fase 8) — observability endpoint pubblici
        .route("/nexus/healthz", get(nexus_bridge::nexus_healthz))
        .route("/nexus/stats", get(nexus_bridge::nexus_stats))
        .route("/nexus/tools", get(nexus_bridge::nexus_tools))
        .route("/nexus/metrics", get(nexus_bridge::nexus_prometheus))
        .route(
            "/nexus/test-routing",
            post(nexus_bridge::nexus_test_routing),
        )
        .route(
            "/api/embedder-status",
            get(nexus_bridge::nexus_embedder_status),
        )
        .route("/auth/github", get(auth::github_login))
        .route("/auth/github/callback", get(auth::github_callback))
        .route(
            "/auth/figma/mcp/callback",
            get(plugins::figma_oauth_callback),
        )
        .route("/internal/settings/:key", get(settings::get_raw_value))
        .route(
            "/internal/nexus-database-stats",
            get(nexus_database_stats::nexus_database_stats),
        )
        // /api/internal/routing/decide — esposto al brain Python per
        // eliminare la duplicazione della routing matrix. Vedi
        // crates/mcp-core/src/internal_routing.rs per il contratto.
        .route(
            "/api/internal/routing/decide",
            post(internal_routing::decide_routing).get(internal_routing::decide_routing_get),
        )
        // /api/internal/knowledge/search — NO-AUTH, chiamato dal brain
        // Python per RAG inline sulle note KB del progetto.
        .route(
            "/api/internal/knowledge/search",
            post(crate::wiki::internal::internal_kb_search),
        )
        // /api/internal/agent/backlog/:project_id — NO-AUTH, chiamato dal
        // brain (backlog_brief) per ereditare i todo carry_over nel planner.
        .route(
            "/api/internal/agent/backlog/:project_id",
            get(agent_todos_routes::list_backlog),
        )
        // /api/internal/providers/status — no-auth, ritorna lo stato
        // canonico dei provider (last health probe + cooldown). Usato dal
        // nexus-gateway TypeScript per evitare di tenere una sua cache
        // locale (era fonte di stale/inconsistency).
        .route(
            "/api/internal/providers/status",
            get(environment::providers_status_internal),
        )
        // /api/internal/routing/catalog — Fase D consolidamento: espone
        // il catalogo prezzi LLM al brain Python e dashboard admin.
        // Filtri query: ?tier=heavy&provider=anthropic&requires_capability=tool_use
        .route(
            "/api/internal/routing/catalog",
            get(internal_routing::list_catalog),
        )
        .route(
            "/api/internal/routing/purpose",
            get(internal_routing::resolve_purpose),
        )
        // /api/internal/routing/cooldown — fonte di verita' unica del cooldown
        // provider (ADR 0020). Snapshot in-memory leggero del gate Rust, che
        // accumula anche i cooldown riportati dal brain via provider-error.
        // Il brain lo consulta in fallback/escalation per saltare i provider
        // morti senza duplicare il ragionamento sul cooldown (regola H).
        // Distinto da /providers/status (UI, fa merge col brain + DB health).
        .route(
            "/api/internal/routing/cooldown",
            get(internal_routing::cooldown_snapshot_handler),
        )
        // /api/internal/learning/feedback — sostituisce la chiamata gRPC
        // submit_feedback da brain Python. Rust diventa unico writer
        // della Q-table (vedi internal_learning.rs).
        .route(
            "/api/internal/learning/feedback",
            post(internal_learning::submit_feedback),
        )
        // /api/internal/provider-error — bridge cooldown: il brain Python
        // notifica errori provider non osservati da Rust (es. catena
        // classificatore). Applica cooldown appropriato (lungo per billing,
        // breve per rate_limit/overloaded).
        .route(
            "/api/internal/provider-error",
            post(internal_routing::provider_error_handler),
        )
        .route(
            "/api/internal/prompt-templates/batch-assign-tools",
            post(prompt_templates::internal_batch_assign_tools_handler),
        )
        .route(
            "/api/chat",
            post(chat_messages::legacy_chat).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/orchestrator/chat",
            post(chat_messages::legacy_chat).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/sessions",
            get(chat_sessions::list_chat_sessions)
                .post(chat_sessions::create_chat_session)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/chat/sessions/:id",
            axum::routing::patch(chat_sessions::rename_chat_session)
                .delete(chat_sessions::delete_chat_session)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/chat/sessions/:id/compact",
            axum::routing::post(chat_sessions::compact_chat_session).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/projects/:id/memories",
            axum::routing::get(chat_sessions::list_project_memories).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/memories/:id/toggle",
            axum::routing::patch(chat_sessions::toggle_project_memory).layer(
                axum_mw::from_fn_with_state(state.clone(), middleware::require_auth),
            ),
        )
        .route(
            "/api/chat/sessions/:id/messages",
            get(chat_messages::list_chat_messages)
                .post(chat_messages::send_chat_message)
                .layer(axum_mw::from_fn_with_state(
                    state.clone(),
                    middleware::require_auth,
                )),
        )
        .route(
            "/api/chat/sessions/:id/agent-stream",
            get(chat_agent::agent_stream).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/sessions/:session_id/active-run",
            get(chat_agent::get_active_run_for_session).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/agent-runs/:run_id",
            get(chat_agent::get_agent_run).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/agent-runs/:run_id/confirm",
            post(chat_agent::confirm_agent_run).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/agent-runs/:run_id/cancel",
            post(chat_agent::cancel_agent_run).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/messages/:id/resend",
            post(chat_messages::resend_chat_message).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/messages/:id/feedback-error",
            post(chat_messages::feedback_error).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/messages/:id/feedback-positive",
            post(chat_messages::feedback_positive).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/precheck",
            post(chat_messages::precheck_chat_message).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/feedback-assist",
            post(chat_messages::feedback_assist_handler).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/messages/:id",
            delete(chat_messages::delete_chat_message).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/messages/:id/attachments/index",
            post(chat_attachments::index_attachments_to_kb).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
        .route(
            "/api/chat/attachments/:attachment_id/raw",
            get(chat_attachments::get_attachment_raw).layer(axum_mw::from_fn_with_state(
                state.clone(),
                middleware::require_auth,
            )),
        )
}
