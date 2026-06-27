use axum::{
    middleware as axum_mw,
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod documents;
mod vector;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub neural_url: String,
    pub qdrant_url: String,
    /// URL HTTP di mcp-core (porta 4000). doc-service fa embed via il PUNTO UNICO
    /// `mcp-core POST /api/embed` (ONNX MiniLM in-process, regola L), non piu' via
    /// il brain Python. Letto da `settings.mcp_core_url` (mig 0190), override di
    /// emergenza `MCP_CORE_URL` (regola G: niente porta hardcoded).
    pub mcp_core_url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "doc_service=info,tower_http=info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    tracing::info!("Document Service: connected to PostgreSQL");

    let neural_url = nexus_auth::get_setting(&db, "neural_core_url")
        .await
        .unwrap_or_else(|| {
            std::env::var("NEURAL_CORE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string())
        });

    let qdrant_url = nexus_auth::get_setting(&db, "qdrant_url")
        .await
        .unwrap_or_else(|| {
            std::env::var("QDRANT_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6333".to_string())
        });

    // URL mcp-core per gli embedding ONNX (override env > settings, regola G).
    let mcp_core_url = std::env::var("MCP_CORE_URL").ok().unwrap_or(
        nexus_auth::get_setting(&db, "mcp_core_url")
            .await
            .unwrap_or_else(|| "http://127.0.0.1:4000".to_string()),
    );

    let state = AppState {
        db: db.clone(),
        neural_url,
        qdrant_url,
        mcp_core_url,
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_credentials(false);

    // Document routes (require auth)
    let doc_routes = Router::new()
        .route(
            "/projects/:project_id/documents",
            get(documents::list_documents),
        )
        .route(
            "/projects/:project_id/documents/:doc_id",
            get(documents::get_document).delete(documents::delete_document),
        )
        .route(
            "/projects/:project_id/documents/:doc_id/download",
            get(documents::download_document),
        )
        .route(
            "/projects/:project_id/documents/:doc_id/versions",
            get(documents::list_versions),
        )
        .route("/documents/generate", post(documents::generate_document))
        .route("/documents/search", post(documents::search_documents))
        // Middleware auth dal punto unico nexus-auth (regola L, cluster E4).
        .layer(axum_mw::from_fn_with_state(
            db.clone(),
            nexus_auth::require_auth::<AppState>,
        ))
        .with_state(state.clone());

    let app = Router::new()
        .nest("/api", doc_routes)
        .route("/health", get(|| async { "ok" }))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // Porta dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    let port = nexus_auth::resolve_port(&db, "doc_service_port").await;

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("Document Service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
