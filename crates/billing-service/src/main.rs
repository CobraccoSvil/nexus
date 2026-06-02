use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::{self as axum_mw, Next},
    response::Response,
    routing::{get, post, put},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod billing;

async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = nexus_auth::validate_token(&state.db, req.headers()).await?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

async fn require_admin(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = nexus_auth::validate_token(&state.db, req.headers()).await?;
    if claims.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

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
                .unwrap_or_else(|_| "billing_service=info,tower_http=info".into()),
        )
        .init();

    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    tracing::info!("Billing Service: connected to PostgreSQL");

    let state = AppState { db: db.clone() };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_credentials(false);

    // Billing routes (require auth)
    let billing_routes = Router::new()
        .route("/prices", get(billing::list_prices).post(billing::create_price))
        .route("/prices/:id", put(billing::update_price))
        .route("/quotas", get(billing::list_quotas).post(billing::create_quota))
        .route("/quotas/:id", put(billing::update_quota))
        .route("/my-usage", get(billing::my_usage_report))
        .route("/project-usage/:id", get(billing::project_usage_report))
        .route("/session-usage/:id", get(billing::get_session_usage))
        .layer(axum_mw::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    // Admin routes (require admin role)
    let admin_routes = Router::new()
        .route("/usage", get(billing::admin_usage_report))
        .layer(axum_mw::from_fn_with_state(state.clone(), require_admin))
        .with_state(state.clone());

    // Internal routes (no auth — only accessible from localhost / internal network)
    let internal_routes = Router::new()
        .route("/billing/reserve", post(billing::internal_reserve))
        .route("/billing/finalize", post(billing::internal_finalize))
        .route("/billing/release", post(billing::internal_release))
        .with_state(state.clone());

    let app = Router::new()
        .nest("/api/billing", billing_routes)
        .nest("/api/admin", admin_routes)
        .nest("/internal", internal_routes)
        .route("/health", get(|| async { "ok" }))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // Porta dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    let port = nexus_auth::resolve_port(&db, "billing_service_port").await;

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("Billing Service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
