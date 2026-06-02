
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

mod admin_projects;
mod admin_users;
mod browser_bridge;
mod environment;
mod experiments;
mod long_running;
mod orchestrator_panel;
mod prompt_templates;
mod settings;
mod shared_directives;

pub use prompt_templates::TemplateCache;

async fn require_admin(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let claims = nexus_auth::validate_token(&state.db, req.headers()).await?;
    if claims.role != "admin" {
        tracing::warn!(
            "require_admin: access denied - role={} is not admin, path={}",
            claims.role,
            req.uri()
        );
        return Err(StatusCode::FORBIDDEN);
    }
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub template_cache: TemplateCache,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "admin_service=info,tower_http=info".into()),
        )
        .init();

    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    tracing::info!("Admin Service: connected to PostgreSQL");

    let state = AppState {
        db: db.clone(),
        template_cache: TemplateCache::new(),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_credentials(false);

    // Admin routes (require admin role)
    let admin_routes = Router::new()
        // Users
        .route("/users", get(admin_users::list_users))
        .route("/users/search", get(admin_users::search_users))
        .route(
            "/users/:user_id",
            get(admin_users::get_user)
                .put(admin_users::update_user)
                .delete(admin_users::delete_user),
        )
        .route("/users/:user_id/role", put(admin_users::update_user_role))
        // Projects
        .route("/projects", get(admin_projects::list_all_projects))
        .route("/projects/port", post(admin_projects::port_projects))
        .route(
            "/projects/:project_id/members",
            get(admin_projects::list_project_members)
                .post(admin_projects::add_project_member),
        )
        .route(
            "/projects/:project_id/members/:user_id",
            put(admin_projects::update_project_member)
                .delete(admin_projects::remove_project_member),
        )
        // Settings
        .route(
            "/settings",
            get(settings::list_settings).put(settings::bulk_update),
        )
        .route("/setting/:key", put(settings::update_setting))
        .route(
            "/settings-by-category/:category",
            get(settings::list_by_category),
        )
        .route("/fs/directories", get(settings::browse_directories))
        .route("/fs/directories/create", post(settings::create_directory))
        // Long running patterns
        .route(
            "/long-running",
            get(long_running::list_patterns).post(long_running::create_pattern),
        )
        .route(
            "/long-running/:id",
            put(long_running::update_pattern).delete(long_running::delete_pattern),
        )
        // Prompt templates
        .route(
            "/prompt-templates",
            get(prompt_templates::list_templates_handler),
        )
        .route(
            "/prompt-templates/:key",
            get(prompt_templates::get_template_handler)
                .put(prompt_templates::upsert_template_handler),
        )
        .route(
            "/prompt-templates/:key/disable",
            post(prompt_templates::disable_template_handler),
        )
        .route(
            "/prompt-templates/:key/enable",
            post(prompt_templates::enable_template_handler),
        )
        .route(
            "/prompt-templates/:key/ai-suggest",
            post(prompt_templates::ai_suggest_handler),
        )
        .route(
            "/prompt-templates/:key/preview",
            post(prompt_templates::preview_template_handler),
        )
        .route(
            "/prompt-templates/:key/tools",
            get(prompt_templates::get_prompt_tools_handler)
                .put(prompt_templates::update_prompt_tools_handler),
        )
        .route(
            "/available-mcp-tools",
            get(prompt_templates::get_available_mcp_tools_handler),
        )
        .route(
            "/prompt-templates/batch-assign-tools",
            post(prompt_templates::batch_assign_tools_handler),
        )
        // Quality false positives
        .route(
            "/quality/findings/:finding_id/false-positive",
            post(prompt_templates::mark_false_positive_handler),
        )
        .route(
            "/quality/false-positive-stats",
            get(prompt_templates::false_positive_stats_handler),
        )
        // Usage (billing view for admin)
        .route("/usage", get(settings::get_raw_value))
        // Environment checks & fix
        .route("/environment/status", get(environment::get_environment_status))
        .route("/environment/fix", post(environment::fix_environment))
        // Browser bridge (proxy verso browser-bridge-mcp daemon)
        .nest("/browser-bridge", browser_bridge::router())
        // Esperimenti A/B prompt (Fase 3)
        .route(
            "/prompt-experiments",
            get(experiments::list_experiments),
        )
        .route(
            "/prompt-experiments/:id",
            get(experiments::get_experiment),
        )
        .route(
            "/prompt-experiments/:id/promote",
            post(experiments::force_promote),
        )
        .route(
            "/prompt-experiments/:id/discard",
            post(experiments::force_discard),
        )
        // Direttive condivise agenti
        .route(
            "/shared-directives",
            get(shared_directives::list_directives)
                .post(shared_directives::create_directive),
        )
        .route(
            "/shared-directives/:key",
            get(shared_directives::get_directive)
                .put(shared_directives::update_directive)
                .delete(shared_directives::delete_directive),
        )
        .route(
            "/shared-directives/:key/toggle",
            post(shared_directives::toggle_directive),
        )
        // Dashboard riepilogo metriche prompt
        .route(
            "/prompt-dashboard",
            get(experiments::prompt_dashboard),
        )
        // PR-4 Orchestrator panel (Plan/Act/Verify + Sub-agents)
        .route(
            "/orchestrator/plans",
            get(orchestrator_panel::list_plans),
        )
        .route(
            "/orchestrator/plans/:run_id",
            get(orchestrator_panel::get_plan),
        )
        .route(
            "/orchestrator/subagents/definitions",
            get(orchestrator_panel::list_subagent_definitions)
                .post(orchestrator_panel::upsert_subagent_definition),
        )
        .route(
            "/orchestrator/subagents/definitions/:kind",
            axum::routing::patch(orchestrator_panel::upsert_subagent_definition)
                .delete(orchestrator_panel::delete_subagent_definition),
        )
        .route(
            "/orchestrator/subagents/runs",
            get(orchestrator_panel::list_subagent_runs),
        )
        .layer(axum_mw::from_fn_with_state(state.clone(), require_admin))
        .with_state(state.clone());

    // Internal routes (no auth, only accessible from localhost)
    let internal_routes = Router::new()
        .route("/settings/:key", get(settings::get_raw_value))
        .with_state(state.clone());

    let app = Router::new()
        .nest("/api/admin", admin_routes)
        .nest("/internal", internal_routes)
        .route("/health", get(|| async { "ok" }))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    // Porta dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    let port = nexus_auth::resolve_port(&db, "admin_service_port").await;

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("Admin Service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
