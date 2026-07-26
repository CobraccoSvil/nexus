//! Server HTTP del Nexus LLM Gateway: espone il contratto LLM di Nexus (lingua
//! franca OpenAI Chat Completions).
//!
//! L'elenco autoritativo delle rotte e' il `Router` costruito in
//! `bin/server.rs`: non e' duplicato qui perche' un elenco scritto a mano va in
//! drift (questa intestazione ne ometteva quattro).
//!
//! Riuso punti unici (regola L):
//!   - `nexus_auth`: lettura settings (`get_setting`), risoluzione porta DB
//!     (`resolve_port`), validazione JWT (`Claims`, `jsonwebtoken`);
//!   - `nexus_pricing`: listino dei modelli (`resolve_active_price`,
//!     `platform_currency`, `calculate_cost`) — punto unico, ADR 0026;
//!   - `billing` (questo modulo): quota + scrittura del ledger
//!     (`ai_quota_policies`, `ai_usage_ledger`). E' l'UNICO scrittore reale del
//!     ledger: il crate `billing-service`, di cui questo modulo era un porting,
//!     e' stato rimosso senza aver mai scritto una riga;
//!   - moduli del crate (`CooldownManager`, `PolicyEngine`,
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
    /// Chiave di firma della piattaforma (>= 32 char, garantita dal bootstrap).
    ///
    /// NON e' un `Option`: quando lo era, l'assenza faceva passare qualunque
    /// richiesta senza credenziali. Il tipo ora esclude quello stato — il
    /// gateway non parte se non riesce a risolvere la chiave.
    pub jwt_secret: String,
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
    /// Timeout LLM derivati dal punto unico (`nexus_auth::llm_timeouts`). Vivono
    /// qui perche' `/admin/reload` li rilegga dal DB a caldo, insieme al client
    /// HTTP che ne dipende.
    pub timeouts: nexus_auth::llm_timeouts::LlmTimeouts,
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
