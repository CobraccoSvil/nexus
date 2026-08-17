//! Binario del Nexus LLM Gateway: il processo in ascolto sulla porta del
//! gateway, avviato come servizio dal deploy. Qui vive il `Router` che dichiara
//! le rotte, unica fonte del loro elenco.
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
    extract::DefaultBodyLimit,
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

    // Listino configurato? Verifica ALL'AVVIO (regola G). La currency di
    // piattaforma non ha piu' un default hardcoded ('EUR' qui, 'USD' in mcp-core:
    // la divergenza aveva gia' prodotto righe di ledger orfane). Scoprirlo qui
    // costa un avvio fallito; scoprirlo a runtime costerebbe le richieste, perche'
    // il billing sta sul percorso di ogni chiamata LLM.
    nexus_pricing::assert_configured(&db)
        .await
        .context("listino non configurato: valorizzare settings.billing_base_currency (mig 0294)")?;

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

    // Ricarica del catalogo dei codici errore. Loop DEDICATO e non appeso al
    // re-probe (600s): una riga nuova in `nexus_provider_error_code` deve valere
    // entro il minuto — e' la ragione per cui quel catalogo sta nel DB. Il
    // difetto che chiude e' durato 14 giorni proprio perche' il rimedio
    // richiedeva di toccare Rust e ridispiegare.
    {
        let _handle = nexus_gateway::tassonomia_errori::spawn_vocabolario_loop(
            state.vocabolario_errori.clone(),
            db.clone(),
        );
    }

    // Snapshot periodico delle osservazioni di rate limit (mig 0718): persiste
    // le voci cambiate del registro in-process. Solo sensore, nessuna
    // decisione automatica su quelle righe.
    {
        let _handle = nexus_gateway::rate_limit_headers::spawn_snapshot_flusher(db.clone());
    }

    let max_body_bytes = nexus_gateway::resolve_max_body_bytes(&db).await;
    tracing::info!(
        max_body_mb = max_body_bytes / (1024 * 1024),
        "gateway: limite body DB-driven (era il default axum di 2 MB, silenzioso)"
    );

    let app = Router::new()
        .route("/health", get(routes::health))
        .route("/providers", get(routes::providers))
        .route("/v1/models", get(routes::models))
        .route("/v1/models/:provider", get(routes::models_for_provider))
        .route("/v1/complete", post(routes::complete))
        .route("/v1/stream", post(routes::stream))
        // Conteggio token: stesso contratto d'ingresso di /v1/complete, nessuna
        // scrittura di ledger (l'endpoint del fornitore e' gratuito).
        .route("/v1/count_tokens", post(routes::count_tokens))
        .route("/v1/images/generations", post(routes::generate_image))
        .route("/v1/videos", post(routes::generate_video))
        .route("/v1/audio/transcriptions", post(routes::transcribe_audio))
        .route("/v1/audio/speech", post(routes::text_to_speech))
        .route("/v1/batch", post(routes::create_batch))
        .route("/v1/batch/:provider/:batch_id", get(routes::get_batch))
        .route("/admin/reload", post(routes::admin_reload))
        // Auth middleware: esenta /health e /providers (vedi `auth::require_auth`).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        // Limite ESPLICITO sul body (regola G: nel DB, non un default nascosto
        // della libreria). Senza questo layer axum applica il proprio default di
        // 2 MB, mentre mcp-core — che e' il chiamante — ne accetta 50: un prompt
        // agentico cresciuto oltre i 2 MB veniva RIFIUTATO dal gateway, e il
        // rifiuto era INVISIBILE (tower_http::trace classifica come failure solo
        // i 5xx: un 413 non lascia una riga di log). Lato mcp-core arrivava un
        // "error sending request", cioe' un errore di TRASPORTO: il segnale
        // "richiesta troppo grande" era perso per strada (regola M) e il motore
        // non poteva reagire compattando il contesto.
        // Verificato sul campo: body 1.90 MB passa, 2.10 MB -> 413
        // "Failed to buffer the request body: length limit exceeded".
        .layer(DefaultBodyLimit::max(max_body_bytes))
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
