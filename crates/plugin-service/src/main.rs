use axum::{
    middleware as axum_mw,
    routing::{delete, get, post, put},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod mcp_client;
mod mcp_connectors;
mod plugins;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "plugin_service=info,tower_http=info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let db = PgPoolOptions::new()
        .max_connections(15)
        .connect(&database_url)
        .await?;

    tracing::info!("Plugin Service: connected to PostgreSQL");

    let state = AppState { db: db.clone() };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_credentials(false);

    // -- Plugin routes (auth required) --
    let plugin_routes = Router::new()
        // Plugin catalog & installed
        .route("/catalog", get(plugins::list_plugin_catalog))
        .route("/installed", get(plugins::list_installed_plugins))
        .route("/install", post(plugins::install_plugin))
        .route(
            "/:id",
            delete(plugins::uninstall_plugin),
        )
        .route("/:id/toggle", put(plugins::toggle_plugin))
        .route("/:id/test", post(plugins::test_plugin))
        .route("/:id/health", get(plugins::get_plugin_health))
        .route("/:id/tool-policy", put(plugins::update_plugin_tool_policy))
        .route(
            "/:id/migrate-legacy",
            post(plugins::migrate_legacy_mcp_server),
        )
        // Figma OAuth
        .route("/figma/oauth/status", get(plugins::get_figma_oauth_status))
        .route("/figma/oauth/start", post(plugins::start_figma_oauth))
        // Middleware auth dal punto unico nexus-auth (regola L, cluster E4).
        .layer(axum_mw::from_fn_with_state(
            db.clone(),
            nexus_auth::require_auth::<AppState>,
        ))
        .with_state(state.clone());

    // -- MCP connector routes (auth required) --
    let mcp_routes = Router::new()
        .route("/", get(mcp_connectors::list_mcp_servers).post(mcp_connectors::create_mcp_server))
        .route(
            "/:id",
            put(mcp_connectors::update_mcp_server).delete(mcp_connectors::delete_mcp_server),
        )
        .route("/:id/test", post(mcp_connectors::test_mcp_server))
        .route("/:id/toggle", put(mcp_connectors::toggle_mcp_server))
        .layer(axum_mw::from_fn_with_state(
            db.clone(),
            nexus_auth::require_auth::<AppState>,
        ))
        .with_state(state.clone());

    // -- Internal routes (no auth - localhost only) --
    let internal_routes = Router::new()
        .route(
            "/mcp/tools/:user_id/:project_id",
            get(mcp_connectors::load_mcp_tools_for_agent),
        )
        .route("/mcp/execute", post(mcp_connectors::execute_mcp_tool))
        .with_state(state.clone());

    // -- Figma OAuth callback (no auth - redirect from Figma) --
    let oauth_callback = Router::new()
        .route(
            "/auth/figma/mcp/callback",
            get(plugins::figma_oauth_callback),
        )
        .with_state(state.clone());

    let app = Router::new()
        .nest("/api/plugins", plugin_routes)
        .nest("/api/mcp-servers", mcp_routes)
        .nest("/internal", internal_routes)
        .merge(oauth_callback)
        .route("/health", get(|| async { "ok" }))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // Porta dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    let port = nexus_auth::resolve_port(&db, "plugin_service_port").await;

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("Plugin Service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
