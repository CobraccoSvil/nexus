//! Compatibilita' REST "neural-core": espone in mcp-core gli endpoint che il
//! brain Python (servizio REST :8001, ora rimosso) offriva e che il frontend
//! web-ide consuma ancora via proxy Next.js (`/neural/*` e `/api/neural/*`).
//!
//! Obiettivo: forme di output IDENTICHE a quelle del brain (vedi
//! `brain/grpc_server/routes/core.py`) cosi' il frontend continua a funzionare
//! ripuntando il solo host al posto del path. NIENTE re-implementazione di
//! logica (regola L): ogni handler DELEGA al punto unico Rust gia' esistente.
//!
//! Punti unici riusati:
//! - classify-intent / route-model: `Orchestrator::classify_intent_full`
//!   (dispatch rust/python interno, punto unico classificazione) +
//!   `Orchestrator::resolve_agent_provider_detailed` (routing matrix DB).
//! - providers/{p}/models: catalog `ai_price_catalog` (modelli enabled).
//! - providers/{p}/health + billing-cooldown: snapshot cooldown in-process
//!   (`provider_cooldown`), fonte di verita' canonica dello stato provider in
//!   mcp-core (la stessa di `/api/internal/routing/cooldown`).
//! - reload-settings: no-op (mcp-core ha cache TTL DB-driven, regola G:
//!   nessun servizio esterno da ricaricare).

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;

// ── Richieste (forme accettate dal brain, retrocompatibili) ──────────────────

/// Body di `/classify-intent` e `/route-model`. `project_id`/`profile_id` sono
/// accettati per parita' di contratto col brain ma la classificazione e' pura
/// (testuale), quindi non usati per la decisione.
#[derive(Debug, Deserialize)]
pub struct IntentRequest {
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub message: String,
}

/// Body di `/reload-settings`. `mcp_core_url` accettato per parita' col brain;
/// no-op (mcp-core non ha un servizio esterno da ricaricare).
#[derive(Debug, Deserialize)]
pub struct ReloadSettingsRequest {
    /// Accettato per parita' col contratto del brain ma non usato: mcp-core non
    /// ha un servizio esterno da ricaricare (cache TTL DB-driven, regola G).
    #[serde(default)]
    #[allow(dead_code, reason = "campo del contratto brain /reload-settings, mantenuto per parita' API")]
    pub mcp_core_url: Option<String>,
}

// ── Risposte (1:1 con il brain) ──────────────────────────────────────────────

/// Forma di `/classify-intent`: `{intent, confidence}` (confidence stringa a 2
/// decimali, come il brain `f"{result.confidence:.2f}"`).
#[derive(Debug, Serialize)]
pub struct ClassifyIntentResponse {
    pub intent: String,
    pub confidence: String,
}

/// Forma di `/route-model`: tutti i campi stringa (parita' col brain).
#[derive(Debug, Serialize)]
pub struct RouteModelResponse {
    pub intent: String,
    pub provider: String,
    pub model: String,
    pub rationale: String,
    pub confidence: String,
}

/// Forma di `/providers/{provider}/models`: `{provider, status, models[]}`
/// (consumata dal frontend come `ProviderModelsResponse`).
#[derive(Debug, Serialize)]
pub struct ProviderModelsResponse {
    pub provider: String,
    pub status: String,
    pub models: Vec<String>,
}

/// Forma di `/providers`: `{status, providers[]}` — elenco dei provider che
/// hanno almeno un modello abilitato nel catalog. Controparte simmetrica di
/// `ProviderModelsResponse`: alimenta il dropdown provider della chat (regola G,
/// unica fonte di verita' nel DB, nessun elenco hardcoded lato client).
#[derive(Debug, Serialize)]
pub struct ProvidersResponse {
    pub status: String,
    pub providers: Vec<String>,
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// GET /health (mount: `/api/neural/health`).
/// Statico, parita' col brain `{"service","status","version"}`.
pub async fn health() -> impl IntoResponse {
    Json(json!({
        "service": "neural-core",
        "status": "ok",
        "version": "0.2.0",
    }))
}

/// POST /classify-intent (mount: `/api/neural/classify-intent`).
/// Delega al punto unico `Orchestrator::classify_intent_full` (dispatch
/// rust/python interno). Forma: `{intent, confidence:"x.xx"}`.
pub async fn classify_intent(
    State(state): State<AppState>,
    Json(body): Json<IntentRequest>,
) -> impl IntoResponse {
    let classified = state
        .orchestrator
        .classify_intent_full(&state.db, &body.message)
        .await;
    Json(ClassifyIntentResponse {
        intent: classified.intent.to_string(),
        confidence: format!("{:.2}", classified.confidence),
    })
}

/// POST /route-model (mount: `/api/neural/route-model`).
/// Classifica l'intent (punto unico) e poi risolve provider+model via routing
/// matrix DB (`resolve_agent_provider_detailed`). Forma: tutti i campi stringa.
///
/// Cooldown enforcement (ADR 0020): se nessun provider e' utilizzabile,
/// l'endpoint ritorna comunque 200 col risultato (provider/model possono essere
/// vuoti e `rationale` riporta il motivo), come faceva il thin-client del brain
/// che non distingueva lo status — il chiamante leggeva i campi cosi' com'erano.
pub async fn route_model(
    State(state): State<AppState>,
    Json(body): Json<IntentRequest>,
) -> impl IntoResponse {
    // Classificazione (punto unico): da' l'intent e la confidence da esporre.
    let classified = state
        .orchestrator
        .classify_intent_full(&state.db, &body.message)
        .await;
    let intent = classified.intent.to_string();

    // Routing (punto unico, regola L): provider+model+rationale dalla matrice DB.
    // Passiamo l'intent gia' classificato (intent_hint) per non ri-classificare.
    let decision = state
        .orchestrator
        .resolve_agent_provider_detailed(
            &state.db,
            &body.project_id,
            &body.profile_id,
            &body.message,
            None,
            None,
            0,
            None,
            Some(intent.as_str()),
            // /route-model non riceve gli allegati del turno (solo message/intent):
            // nessun override vision possibile qui -> routing testuale invariato.
            // Il path con immagine e' la chat reale (agent_run) che passa il segnale.
            false,
        )
        .await;

    // Rationale: la decisione di routing lo popola; se assente, fonte sintetica.
    let rationale = if decision.rationale.trim().is_empty() {
        decision.source.clone()
    } else {
        decision.rationale.clone()
    };

    Json(RouteModelResponse {
        intent,
        provider: decision.provider,
        model: decision.model,
        rationale,
        confidence: format!("{:.2}", classified.confidence),
    })
}

/// GET /providers (mount: `/api/neural/providers`).
/// Elenca i provider DISTINCT che hanno almeno un modello ENABLED nel catalog
/// `ai_price_catalog` (regola G: nessun provider hardcoded, tutto da DB). E' la
/// controparte simmetrica di `provider_models`: il dropdown chat mostra solo
/// provider che poi espongono modelli selezionabili, e un provider aggiunto o
/// rimosso dal catalog/routing matrix si riflette qui senza toccare il frontend.
/// Forma: `{status, providers[]}`.
pub async fn providers(State(state): State<AppState>) -> impl IntoResponse {
    let result = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT provider FROM ai_price_catalog \
         WHERE is_enabled = TRUE \
         ORDER BY provider",
    )
    .fetch_all(&state.db)
    .await;

    match result {
        Ok(providers) => Json(ProvidersResponse {
            status: "ok".to_string(),
            providers,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!("neural_compat: query provider attivi catalog fallita: {e}");
            // Forma stabile anche in errore: status != ok, lista vuota. Il
            // frontend (getProviders) usa `response.providers ?? []`: il caso
            // vuoto degrada a "solo Auto", nessun fallback hardcoded (regola G).
            Json(ProvidersResponse {
                status: "error".to_string(),
                providers: Vec::new(),
            })
            .into_response()
        }
    }
}

/// GET /providers/{provider}/models (mount: `/api/neural/providers/:provider/models`).
/// Lista i nomi modello ENABLED del provider dal catalog `ai_price_catalog`
/// (regola G: nessun nome modello hardcoded, tutto da DB). Forma:
/// `{provider, status, models[]}`.
pub async fn provider_models(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> impl IntoResponse {
    let provider = provider.trim().to_lowercase();
    let result = sqlx::query_scalar::<_, String>(
        "SELECT model FROM ai_price_catalog \
         WHERE provider = $1 AND is_enabled = TRUE \
         ORDER BY is_featured DESC, input_cost_per_million_tokens ASC NULLS LAST",
    )
    .bind(&provider)
    .fetch_all(&state.db)
    .await;

    match result {
        Ok(models) => Json(ProviderModelsResponse {
            provider,
            status: "ok".to_string(),
            models,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!(provider = %provider, "neural_compat: query modelli catalog fallita: {e}");
            // Forma stabile anche in errore: status != ok, lista vuota. Il
            // frontend (getProviderModels) usa `response.models ?? []`.
            Json(ProviderModelsResponse {
                provider,
                status: "error".to_string(),
                models: Vec::new(),
            })
            .into_response()
        }
    }
}

/// GET /providers/{provider}/health (mount: `/api/neural/providers/:provider/health`).
/// Stato del provider derivato dallo snapshot cooldown in-process (punto unico
/// `provider_cooldown`, la stessa fonte di `/api/internal/routing/cooldown`):
///   - provider in cooldown -> `{status:"cooldown", ok:false, reason, seconds_remaining}`
///   - altrimenti           -> `{status:"ok", ok:true}`
///
/// Il consumer (settings-panel "Testa provider") legge `data.status` e
/// `data.reason || data.message || data.error`. NB: lo stato canonico del
/// provider in mcp-core e' il cooldown (i provider sono raggiunti via gateway,
/// non c'e' piu' un brain da interrogare con un probe live).
pub async fn provider_health(Path(provider): Path<String>) -> impl IntoResponse {
    let provider = provider.trim().to_lowercase();
    let cooldown = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .find(|(name, _, _)| name == &provider);

    match cooldown {
        Some((_, seconds_remaining, reason)) => Json(json!({
            "provider": provider,
            "status": "cooldown",
            "ok": false,
            "reason": reason.unwrap_or_else(|| "provider in cooldown".to_string()),
            "seconds_remaining": seconds_remaining,
        })),
        None => Json(json!({
            "provider": provider,
            "status": "ok",
            "ok": true,
        })),
    }
}

/// GET /providers/billing-cooldown (mount: `/api/neural/providers/billing-cooldown`).
/// Snapshot dei provider in cooldown (secondi rimanenti per provider). Forma
/// identica al brain: `{providers: {"anthropic": 540, ...}}`. Fonte: punto unico
/// `provider_cooldown::cooldown_snapshot` (in-process).
pub async fn billing_cooldown() -> impl IntoResponse {
    let providers: HashMap<String, u64> = crate::provider_cooldown::cooldown_snapshot()
        .into_iter()
        .map(|(name, seconds_remaining, _reason)| (name, seconds_remaining))
        .collect();
    Json(json!({ "providers": providers }))
}

/// POST /reload-settings (mount: `/api/neural/reload-settings`).
/// No-op: mcp-core legge le settings dal DB con cache TTL (regola G), non c'e'
/// alcun servizio esterno da ricaricare. Forma: `{"ok": true}`.
pub async fn reload_settings(
    Json(_body): Json<ReloadSettingsRequest>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "ok": true })))
}
