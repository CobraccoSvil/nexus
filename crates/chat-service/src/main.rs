use std::sync::Arc;

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::{self as axum_mw, Next},
    response::Response,
    routing::{delete, get, patch, post, put},
    Router,
};
use dashmap::DashMap;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

mod chat_sessions;
mod chat_messages;
mod chat_agent;
mod profiles;

async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = nexus_auth::validate_token(&state.db, req.headers()).await?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Agent step events streamed via SSE
#[derive(Clone, Debug, serde::Serialize)]
pub struct AgentStepEvent {
    pub run_id: Uuid,
    pub event_type: String,
    pub data: serde_json::Value,
}

/// Map of run_id -> broadcast sender for SSE streaming
pub type AgentChannels = Arc<DashMap<Uuid, broadcast::Sender<AgentStepEvent>>>;

/// Terminal consumer tracking
pub type TerminalConsumers = Arc<DashMap<Uuid, Vec<String>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::MultiplexedConnection,
    pub agent_channels: AgentChannels,
    pub terminal_consumers: TerminalConsumers,
    pub billing_url: String,
    pub plugin_url: String,
    pub core_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chat_service=info,tower_http=info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    let db = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await?;

    tracing::info!("Chat Service: connected to PostgreSQL");

    let redis_client = redis::Client::open(redis_url)?;
    let redis_conn = redis_client.get_multiplexed_async_connection().await?;

    tracing::info!("Chat Service: connected to Redis");

    let state = AppState {
        db: db.clone(),
        redis: redis_conn,
        agent_channels: Arc::new(DashMap::new()),
        terminal_consumers: Arc::new(DashMap::new()),
        billing_url: std::env::var("BILLING_SERVICE_URL").unwrap_or_else(|_| "http://127.0.0.1:4040".to_string()),
        plugin_url: std::env::var("PLUGIN_SERVICE_URL").unwrap_or_else(|_| "http://127.0.0.1:4050".to_string()),
        core_url: std::env::var("CORE_SERVICE_URL").unwrap_or_else(|_| "http://127.0.0.1:4000".to_string()),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_credentials(false);

    // Chat routes (require auth)
    let chat_routes = Router::new()
        // Sessions
        .route("/sessions", get(chat_sessions::list_sessions).post(chat_sessions::create_session))
        .route("/sessions/:id", patch(chat_sessions::rename_session).delete(chat_sessions::delete_session))
        .route("/sessions/:id/compact", post(chat_sessions::compact_session))
        .route("/sessions/:id/messages", get(chat_messages::list_messages).post(chat_messages::send_message))
        // Messages
        .route("/messages/:id/resend", post(chat_messages::resend_message))
        .route("/messages/:id/feedback-error", post(chat_messages::feedback_error))
        .route("/messages/:id", delete(chat_messages::delete_message))
        .route("/precheck", post(chat_messages::precheck_message))
        .route("/feedback-assist", post(chat_messages::feedback_assist))
        // Agent runs
        .route("/sessions/:id/agent-stream", get(chat_agent::agent_stream))
        .route("/sessions/:session_id/active-run", get(chat_agent::get_active_run))
        .route("/agent-runs/:run_id", get(chat_agent::get_agent_run))
        .route("/agent-runs/:run_id/confirm", post(chat_agent::confirm_run))
        .route("/agent-runs/:run_id/cancel", post(chat_agent::cancel_run))
        // Legacy endpoints
        .layer(axum_mw::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    // Profile routes (require auth)
    let profile_routes = Router::new()
        .route("/", get(profiles::list_profiles).post(profiles::create_profile))
        .route("/:id", put(profiles::update_profile).delete(profiles::delete_profile))
        .route("/:id/default", post(profiles::set_default_profile))
        .layer(axum_mw::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    // Memory routes
    let memory_routes = Router::new()
        .route("/projects/:id/memories", get(chat_sessions::list_project_memories))
        .route("/memories/:id/toggle", patch(chat_sessions::toggle_memory))
        .layer(axum_mw::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    let app = Router::new()
        .nest("/api/chat", chat_routes)
        .nest("/api/profiles", profile_routes)
        .nest("/api", memory_routes)
        .route("/health", get(|| async { "ok" }))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // Porta dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    let port = nexus_auth::resolve_port(&db, "chat_service_port").await;

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("Chat Service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
