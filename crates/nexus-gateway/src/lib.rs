//! Nexus LLM Gateway (Rust).
//!
//! Proxy multi-provider: e' il punto di uscita LLM di Nexus, in esercizio come
//! servizio a se' (unit systemd / WinSW). Il contratto e' la lingua franca
//! OpenAI Chat Completions, fedele a `packages/shared/src/llm-types.ts`.
//! Vedi ADR 0032 ("il compilato e' autoritativo").
//!
//! Il cooldown billing vive qui dentro, non nel chiamante: il re-probe loop
//! (`bin/server.rs`) e' avviato incondizionatamente, quindi un provider rientra
//! da solo appena torna a rispondere.

/// Chiave DB del limite body. Seed: mig **0588**.
pub const MAX_BODY_MB_SETTING: &str = "gateway.max_request_body_mb";
/// Default allineato al `DefaultBodyLimit` di mcp-core (`routes/mod.rs`, 50 MB):
/// il gateway non puo' accettare meno di chi lo chiama, o rifiuta richieste che
/// il chiamante considera legittime.
pub const DEFAULT_MAX_BODY_MB: usize = 50;

/// Limite del body delle richieste al gateway, dal DB (regola G).
///
/// Perche' esiste: senza un `DefaultBodyLimit` esplicito axum applica il proprio
/// default di **2 MB**, mentre mcp-core ne accetta 50. L'asimmetria rifiutava i
/// prompt agentici cresciuti oltre i 2 MB, e in modo SILENZIOSO: un 413 non
/// viene loggato (`tower_http::trace` classifica come failure solo i 5xx), e a
/// mcp-core arrivava un "error sending request" — un errore di TRASPORTO al
/// posto del segnale "richiesta troppo grande" (regola M).
pub async fn resolve_max_body_bytes(db: &sqlx::PgPool) -> usize {
    let mb = nexus_auth::get_setting(db, MAX_BODY_MB_SETTING)
        .await
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_BODY_MB);
    mb.saturating_mul(1024 * 1024)
}

pub mod batch;
pub mod cooldown;
pub mod history_sanitizer;
pub mod model_alias_resolver;
pub mod policy_engine;
pub mod provider;
pub mod providers;
pub mod rate_limit_headers;
pub mod rate_limiter;
pub mod redaction;
pub mod server;
/// Punto unico della classificazione strutturale degli errori fornitore
/// (mig 0705): il criterio, il catalogo dei codici e il registro di cio' che
/// non sappiamo ancora leggere.
pub mod tassonomia_errori;
pub mod types;
