//! Server HTTP del Nexus LLM Gateway (Fase 5).
//!
//! Porting di `apps/nexus-gateway/src/server.ts`. Espone gli stessi endpoint e
//! lo stesso contratto del gateway Node:
//!   - `GET  /health`     stato profilo + provider (proxy mcp-core, fallback cooldown);
//!   - `GET  /providers`  stato provider;
//!   - `GET  /v1/models`  autodiscovery live aggregato di tutti i provider;
//!   - `GET  /v1/models/{provider}` autodiscovery live del singolo provider;
//!   - `POST /v1/complete` completion non-streaming;
//!   - `POST /v1/stream`   completion SSE;
//!   - `POST /v1/batch`    crea un batch (Anthropic completo, Google 501);
//!   - `GET  /v1/batch/{provider}/{batch_id}` stato + risultati del batch;
//!   - `POST /admin/reload` ricarica chiavi/policy dal DB.
//!
//! VINCOLO di migrazione (vedi `lib.rs`): a runtime il gateway Node resta
//! autoritativo finche' la parita' non e' validata (Fase 6). Questo binario si
//! limita a compilare/testare; non deve essere avviato in produzione ne' rubare
//! la porta 4060.
//!
//! Riuso punti unici (regola L):
//!   - `nexus_auth`: lettura settings (`get_setting`), risoluzione porta DB
//!     (`resolve_port`), validazione JWT (`Claims`, `jsonwebtoken`);
//!   - `billing` (questo modulo): porting fedele della logica quota/ledger del
//!     `billing-service` (`ai_quota_policies`, `ai_usage_ledger`, `ai_price_catalog`);
//!   - moduli del crate (`FallbackChain`, `CooldownManager`, `PolicyEngine`,
//!     `ModelAliasResolver`, `RedactionPipeline`, `SensitivityClassifier`).

pub mod auth;
pub mod billing;
pub mod bootstrap;
pub mod routes;

use std::sync::Arc;

use sqlx::PgPool;

use crate::cooldown::CooldownManager;
use crate::model_alias_resolver::ModelAliasResolver;
use crate::policy_engine::PolicyEngine;
use crate::provider::LlmProvider;
use crate::redaction::presidio_client::PresidioClient;

use self::bootstrap::GatewayConfig;

/// Token di servizio di default in dev (parita' col `?? "dev-internal-token"`
/// del server.ts). Non e' un segreto di produzione: il token reale viaggia
/// nell'env `NEXUS_GATEWAY_SERVICE_TOKEN`.
pub const DEV_SERVICE_TOKEN: &str = "dev-internal-token";

/// Stato condiviso del gateway. Clonabile a basso costo: i campi pesanti vivono
/// dietro `Arc`/`RwLock`. Il blocco `runtime` e' protetto da `RwLock` perche'
/// `/admin/reload` lo sostituisce a caldo (provider, policy, alias) senza
/// fermare il server.
#[derive(Clone)]
pub struct AppState {
    /// Pool Postgres condiviso (settings, ledger, quota, prezzi).
    pub db: PgPool,
    /// Token di servizio per le chiamate interne (mcp-core -> gateway).
    pub service_token: String,
    /// JWT secret: `Some` se valido (>= 32 char), `None` in dev (auth permissiva).
    pub jwt_secret: Option<String>,
    /// URL di mcp-core per il proxy dello stato provider (`/health`, `/providers`).
    pub mcp_core_url: String,
    /// Manager dei cooldown (condiviso col re-probe loop).
    pub cooldown: CooldownManager,
    /// Stato ricaricabile a caldo (provider/policy/alias) protetto da `RwLock`.
    pub runtime: Arc<tokio::sync::RwLock<RuntimeState>>,
}

/// Stato sostituibile a caldo da `/admin/reload`. La pipeline ne prende uno
/// snapshot (clone economico: tutto e' `Arc`) all'inizio di ogni richiesta, cosi'
/// un reload concorrente non interrompe le richieste in volo.
#[derive(Clone)]
pub struct RuntimeState {
    /// Provider abilitati e costruiti (cloud + onprem), dietro `Arc`.
    pub providers: Vec<Arc<dyn LlmProvider>>,
    /// Motore di policy di routing per tier.
    pub policy: Arc<PolicyEngine>,
    /// Risolutore alias logico -> modello reale.
    pub aliases: Arc<ModelAliasResolver>,
    /// Client Presidio (per il classificatore di sensibilita').
    pub presidio: PresidioClient,
    /// Profilo operativo corrente (cloud/onprem/hybrid).
    pub profile: String,
    /// Config caricata (file policy/alias, profilo): serve al reload.
    pub config: Arc<GatewayConfig>,
}

impl AppState {
    /// Snapshot atomico dello stato runtime corrente (clone economico).
    pub async fn runtime_snapshot(&self) -> RuntimeState {
        self.runtime.read().await.clone()
    }
}
