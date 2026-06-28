//! Binario del Nexus LLM Gateway (Rust, Fase 5).
//!
//! ATTENZIONE (vincolo di migrazione): il gateway Node resta AUTORITATIVO a
//! runtime finche' la parita' non e' validata (Fase 6). Questo binario si compila
//! e si testa, ma NON va avviato in produzione ne' deve rubare la porta 4060 al
//! gateway Node. Il deploy/systemd NON e' toccato in questa fase.
//!
//! Bootstrap:
//!   1. pool Postgres (DATABASE_URL);
//!   2. stato applicativo (provider, policy, alias, presidio, JWT, token) via
//!      `bootstrap::build_state`;
//!   3. re-probe loop del CooldownManager (rientro reattivo dei provider);
//!   4. porta da `settings.nexus_gateway_port` (regola G, punto unico
//!      `nexus_auth::resolve_port`);
//!   5. router axum con auth middleware (esenta `/health` e `/providers`).

use std::sync::Arc;

use anyhow::Context;
use axum::{
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use tower_http::trace::TraceLayer;

use nexus_gateway::cooldown::spawn_recovery_loop;
use nexus_gateway::server::{auth, bootstrap, routes, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nexus_gateway=info,tower_http=info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .or_else(|_| std::env::var("POSTGRES_URL"))
        .context("DATABASE_URL/POSTGRES_URL non impostata")?;

    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("connessione Postgres")?;
    tracing::info!("nexus-gateway: connesso a PostgreSQL");

    // Stato applicativo (provider, policy, alias, presidio, auth).
    let state: AppState = bootstrap::build_state(db.clone()).await?;

    // Re-probe loop: rientro reattivo dei provider in cooldown (il fix "OpenAI
    // non torna dopo la ricarica"). Usa i provider correnti dello snapshot.
    {
        let providers: Vec<Arc<dyn nexus_gateway::provider::LlmProvider>> =
            state.runtime_snapshot().await.providers;
        let _handle = spawn_recovery_loop(state.cooldown.clone(), providers, db.clone());
        // L'handle vive per tutta la durata del processo (loop infinito); non lo
        // si attende esplicitamente. Allo shutdown il task viene terminato col
        // runtime tokio.
    }

    let app = Router::new()
        .route("/health", get(routes::health))
        .route("/providers", get(routes::providers))
        .route("/v1/models", get(routes::models))
        .route("/v1/models/:provider", get(routes::models_for_provider))
        .route("/v1/complete", post(routes::complete))
        .route("/v1/stream", post(routes::stream))
        .route("/v1/images/generations", post(routes::generate_image))
        .route("/v1/batch", post(routes::create_batch))
        .route("/v1/batch/:provider/:batch_id", get(routes::get_batch))
        .route("/admin/reload", post(routes::admin_reload))
        // Auth middleware: esenta /health e /providers (vedi `auth::require_auth`).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Porta: override operativo opzionale da env NEXUS_GATEWAY_PORT (utile per
    // test o avvio in parallelo durante la migrazione dal gateway Node); in
    // assenza, dal DB (regola G, punto unico nexus_auth::resolve_port). L'env
    // qui e' un parametro operativo di bind, non configurazione di business.
    let port = match std::env::var("NEXUS_GATEWAY_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
    {
        Some(p) => p,
        None => nexus_auth::resolve_port(&db, "nexus_gateway_port").await,
    };
    let addr = format!("0.0.0.0:{port}");
    tracing::info!("nexus-gateway in ascolto su {addr}");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app).await.context("axum::serve")?;

    Ok(())
}
