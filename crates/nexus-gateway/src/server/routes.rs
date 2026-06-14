//! Handler HTTP del gateway e pipeline di routing.
//!
//! Porting delle route di `server.ts`. La pipeline `/v1/complete` (e `/v1/stream`)
//! replica il flusso di `LLMGateway`:
//!   1. classify tier (secret scanner + Presidio) -> tier effettivo;
//!   2. policy_engine.decide(tier) -> lista ordinata di provider ammessi;
//!   3. per ogni provider candidato non in cooldown: risolve l'alias modello
//!      (skip se non risolvibile per quel provider), poi tenta la completion;
//!   4. su errore marca il cooldown (billing/transient via `is_billing_error`,
//!      punto unico) e passa al successivo; il primo successo vince;
//!   5. redaction strict-mode opzionale (pre-flight redact + post-flight rehydrate)
//!      quando il tier elevato richiede invio cloud;
//!   6. enforce quota PRIMA della completion (guardrail), record ledger DOPO.
//!
//! Regola L: lo stato di cooldown, la classificazione billing, il routing e la
//! risoluzione alias delegano ai punti unici del crate. Regola F: nessun
//! prompt/response nei log.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::stream::Stream;
use serde_json::{json, Value};

use crate::cooldown::CooldownManager;
use crate::model_alias_resolver::ModelAliasResolver;
use crate::provider::LlmProvider;
use crate::providers::is_billing_error;
use crate::redaction::pipeline::{RedactionOptions, RedactionPipeline};
use crate::redaction::sensitivity_classifier::SensitivityClassifier;
use crate::types::{LlmRequest, LlmResponse};

use super::billing::{enforce_quota, record_usage_to_ledger, QuotaExceeded};
use super::bootstrap::{build_runtime, GatewayConfig};
use super::AppState;

/// Errore della pipeline tradotto in HTTP. Mantiene lo status coerente col
/// server.ts (403 per blocchi tier/DLP/quota, 500 per fallimenti provider).
#[derive(Debug)]
struct PipelineError {
    status: StatusCode,
    code: String,
    message: String,
}

impl PipelineError {
    fn blocked(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "TIER_BLOCKED".to_string(),
            message: message.into(),
        }
    }
    fn quota(scope: &str, reason: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "QUOTA_EXCEEDED".to_string(),
            message: format!("quota_exceeded:{scope}:{reason}"),
        }
    }
    fn provider(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROVIDER_ERROR".to_string(),
            message: message.into(),
        }
    }
}

impl IntoResponse for PipelineError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.message, "code": self.code })),
        )
            .into_response()
    }
}

/// Riferimento a un provider concreto piu' il modello reale gia' risolto per la
/// richiesta corrente.
struct ResolvedProvider {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

/// Esegue la pipeline completa di completion (classify -> route -> fallback ->
/// rehydrate -> ledger). Ritorna la risposta o un errore tradotto in HTTP.
async fn run_complete(state: &AppState, req: &LlmRequest) -> Result<LlmResponse, PipelineError> {
    let runtime = state.runtime_snapshot().await;

    // Classify + decide.
    let classifier = SensitivityClassifier::new(runtime.presidio.clone());
    let classification = classifier.classify(&req.messages).await;
    let effective_tier = classification.tier.max(req.metadata.sensitivity_tier);
    runtime
        .policy
        .validate_tier_claim(req.metadata.sensitivity_tier, effective_tier);

    // Pin esplicito: bypassa policy.decide + resolve_providers e costruisce una
    // chain di UN solo provider. La policy DLP (classify/validate_tier_claim/
    // redaction) resta attiva: il pin salta SOLO il routing, non la sicurezza.
    let resolved: Vec<ResolvedProvider> = if let Some(pin) = req.pin_provider.as_deref() {
        vec![resolve_pinned_provider(pin, &runtime.providers, &req.model)?]
    } else {
        let decision = runtime
            .policy
            .decide(effective_tier, &req.metadata.feature, &HashMap::new());
        if decision.blocked {
            return Err(PipelineError::blocked(
                decision
                    .reason
                    .unwrap_or_else(|| "routing bloccato dalla policy".to_string()),
            ));
        }

        // Accoppia ogni nome-provider deciso col provider costruito + modello risolto.
        let resolved = resolve_providers(
            &decision.providers,
            &runtime.providers,
            &runtime.aliases,
            &req.model,
            effective_tier,
        );
        if resolved.is_empty() {
            return Err(PipelineError::blocked(
                "nessun provider configurato/risolvibile per il tier richiesto",
            ));
        }
        resolved
    };

    // Redaction pre-flight: strict mode quando il tier e' elevato (>=2) e il
    // provider scelto e' cloud. La mappa serve per la reidratazione post-flight.
    let strict = effective_tier >= 2;
    let pipeline = RedactionPipeline::new(
        runtime.presidio.clone(),
        RedactionOptions {
            strict_mode: strict,
            ..Default::default()
        },
    );
    let redaction = pipeline
        .redact(req)
        .await
        .map_err(|e| PipelineError::blocked(e.to_string()))?;
    if !redaction.stats.types.is_empty() || redaction.stats.secrets_found > 0 {
        tracing::info!(
            secrets = redaction.stats.secrets_found,
            pii = redaction.stats.pii_found,
            code = redaction.stats.code_anonymized,
            "gateway: redazione applicata"
        );
    }
    let mut redacted_req = req.clone();
    redacted_req.messages = redaction.messages;
    let mut map = redaction.map;

    // Guardrail quota PRIMA della chiamata: usa il primo provider+modello (preview).
    let preview = &resolved[0];
    enforce_quota(&state.db, req, preview.provider.name(), &preview.model)
        .await
        .map_err(|e| {
            if let Some(q) = e.downcast_ref::<QuotaExceeded>() {
                PipelineError::quota(&q.scope, &q.reason)
            } else {
                PipelineError::provider(format!("quota check fallito: {e}"))
            }
        })?;

    // Fallback chain manuale (modello per-provider): primo successo vince.
    let mut response = run_fallback(&resolved, &state.cooldown, &redacted_req).await?;

    // Reidratazione post-flight: ripristina gli originali nei placeholder.
    response = pipeline.rehydrate(&response, &mut map);

    // Telemetria: ledger best-effort (non blocca la risposta).
    record_usage_to_ledger(&state.db, req, &response).await;

    Ok(response)
}

/// Accoppia i nomi-provider della decisione coi provider costruiti e risolve il
/// modello reale per ciascuno. Provider non costruiti (chiave assente) o senza
/// modello risolvibile vengono esclusi (regola G: niente fallback silenzioso).
fn resolve_providers(
    names: &[String],
    built: &[Arc<dyn LlmProvider>],
    aliases: &ModelAliasResolver,
    logical_model: &str,
    tier: u8,
) -> Vec<ResolvedProvider> {
    let mut out = Vec::new();
    for name in names {
        let Some(provider) = built.iter().find(|p| p.name() == name) else {
            // Provider deciso dalla policy ma non costruito (es. chiave mancante).
            continue;
        };
        match aliases.resolve(logical_model, name, tier) {
            Ok(model) => out.push(ResolvedProvider {
                provider: provider.clone(),
                model,
            }),
            Err(e) => {
                // Provider escluso dalla chain: modello non risolvibile per quel
                // provider/tier. Solo nome+motivo nel log (regola F).
                tracing::debug!(provider = %name, reason = %e, "gateway: provider escluso dalla chain");
            }
        }
    }
    out
}

/// Costruisce la chain pinnata di UN SOLO elemento per il provider esplicito
/// (`req.pin_provider`). Bypassa `policy.decide` e `resolve_providers`: nessun
/// alias logico, nessun fallback cross-provider. Il modello e' quello della
/// richiesta, strippato dell'eventuale prefisso `provider/`. Errore chiaro (non
/// fallback) se il provider pinnato non e' tra quelli configurati (regola G:
/// niente ripiego silenzioso). Punto unico del pin per i due path (regola L).
fn resolve_pinned_provider(
    pin: &str,
    built: &[Arc<dyn LlmProvider>],
    logical_model: &str,
) -> Result<ResolvedProvider, PipelineError> {
    let Some(provider) = built.iter().find(|p| p.name() == pin) else {
        return Err(PipelineError::provider(format!(
            "provider pinnato \"{pin}\" non configurato/abilitato nel gateway"
        )));
    };
    Ok(ResolvedProvider {
        provider: provider.clone(),
        // Strip del prefisso "provider/" se presente; modello as-is altrimenti.
        model: strip_model_prefix(logical_model),
    })
}

/// Rimuove il prefisso `provider/` da `provider/modello`, ritornando tutto cio'
/// che segue il primo `/`. Senza `/` ritorna la stringa invariata. Allineato a
/// `strip_provider_prefix` del resolver alias, qui in locale per il path pin
/// (non passa dall'alias resolver).
fn strip_model_prefix(model: &str) -> String {
    match model.split_once('/') {
        Some((_, rest)) => rest.to_string(),
        None => model.to_string(),
    }
}

/// Esegue il fallback sui provider risolti: salta i cooldown, prova in ordine,
/// marca il cooldown sull'errore (billing/transient) e prosegue. Punto unico
/// dello stato cooldown e della classificazione billing (regola L).
async fn run_fallback(
    resolved: &[ResolvedProvider],
    cooldown: &CooldownManager,
    base_req: &LlmRequest,
) -> Result<LlmResponse, PipelineError> {
    let mut failures: Vec<String> = Vec::new();

    for rp in resolved {
        let name = rp.provider.name();
        if cooldown.is_in_cooldown(name) {
            failures.push(format!("{name} (in cooldown, saltato)"));
            continue;
        }

        // Richiesta col modello reale risolto per questo provider.
        let mut req = base_req.clone();
        req.model = rp.model.clone();

        match rp.provider.complete(&req).await {
            Ok(resp) => return Ok(resp),
            Err(err) => {
                let msg = err.to_string();
                if is_billing_error(&msg) {
                    cooldown.mark_billing(name, Some(msg.clone()));
                } else {
                    cooldown.mark_transient(name, Some(msg.clone()));
                }
                failures.push(format!("{name} ({msg})"));
            }
        }
    }

    Err(PipelineError::provider(format!(
        "tutti i provider hanno fallito -> {}",
        failures.join("; ")
    )))
}

// ── Health / providers ──────────────────────────────────────────────────────

/// Recupera lo stato provider da mcp-core (fonte canonica). `None` se irraggiungibile.
async fn fetch_providers_from_mcp_core(state: &AppState) -> Option<Vec<Value>> {
    let url = format!("{}/api/internal/providers/status", state.mcp_core_url);
    let client = reqwest::Client::new();
    let res = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let data: Value = res.json().await.ok()?;
    data.get("providers")
        .and_then(|p| p.as_array())
        .map(|a| a.to_vec())
}

/// Stato provider in-memory dal cooldown manager (fallback se mcp-core e' giu').
fn providers_from_cooldown(state: &AppState) -> Vec<Value> {
    state
        .cooldown
        .snapshot()
        .into_iter()
        .map(|s| {
            json!({
                "name": s.name,
                "healthy": s.healthy,
                "last_check": s.last_check,
            })
        })
        .collect()
}

/// `GET /health` (pubblico): stato + profilo + provider (proxy mcp-core, fallback cooldown).
pub async fn health(State(state): State<AppState>) -> Json<Value> {
    let profile = state.runtime_snapshot().await.profile;
    let providers = match fetch_providers_from_mcp_core(&state).await {
        Some(p) => p,
        None => providers_from_cooldown(&state),
    };
    Json(json!({
        "status": "ok",
        "profile": profile,
        "providers": providers,
    }))
}

/// `GET /providers` (pubblico): stato provider (proxy mcp-core, fallback cooldown).
pub async fn providers(State(state): State<AppState>) -> Json<Value> {
    let providers = match fetch_providers_from_mcp_core(&state).await {
        Some(p) => p,
        None => providers_from_cooldown(&state),
    };
    Json(json!({ "providers": providers }))
}

// ── Autodiscovery modelli (catalog live) ─────────────────────────────────────

/// Aggrega l'autodiscovery live di tutti i provider passati. PUNTO UNICO (regola
/// L) dell'aggregazione, condiviso dai due handler e testabile senza rete: per
/// ogni provider chiama `list_models()` ed e' BEST-EFFORT: un fallimento finisce
/// nella mappa `errors` (chiave = nome provider) senza far fallire l'intera
/// risposta. Il JSON ritornato e' `{ "providers": {<name>: [..]}, "errors": {..} }`.
async fn aggregate_models(providers: &[Arc<dyn LlmProvider>]) -> Value {
    let mut by_provider = serde_json::Map::new();
    let mut errors = serde_json::Map::new();
    for p in providers {
        match p.list_models().await {
            Ok(models) => {
                by_provider.insert(p.name().to_string(), json!(models));
            }
            Err(e) => {
                // Regola F: il messaggio d'errore non contiene prompt/response.
                tracing::warn!(provider = p.name(), "gateway: list_models fallita");
                errors.insert(p.name().to_string(), json!(e.to_string()));
            }
        }
    }
    json!({ "providers": by_provider, "errors": errors })
}

/// `GET /v1/models` (auth richiesta): autodiscovery live aggregato di tutti i
/// provider configurati. Sostituisce il mix attuale (mcp-core chiama `/v1/models`
/// dei singoli provider + delega al brain per Google): il gateway lista TUTTI i
/// provider, Vertex incluso (auth Service Account gia' in `gcp_auth`).
pub async fn models(State(state): State<AppState>) -> Json<Value> {
    let providers = state.runtime_snapshot().await.providers;
    Json(aggregate_models(&providers).await)
}

/// `GET /v1/models/{provider}` (auth richiesta): autodiscovery live del singolo
/// provider. Sostituisce `/providers/{p}/models/live` del brain. 404 se il
/// provider non e' configurato; 502 se l'API del provider fallisce.
pub async fn models_for_provider(
    State(state): State<AppState>,
    axum::extract::Path(provider): axum::extract::Path<String>,
) -> Response {
    let providers = state.runtime_snapshot().await.providers;
    let Some(p) = providers.iter().find(|p| p.name() == provider) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("provider '{provider}' non configurato") })),
        )
            .into_response();
    };
    match p.list_models().await {
        Ok(models) => Json(json!({ "provider": provider, "models": models })).into_response(),
        Err(e) => {
            tracing::warn!(provider = %provider, "gateway: list_models singolo fallita");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "provider": provider, "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

// ── Completion ───────────────────────────────────────────────────────────────

/// `POST /v1/complete`: completion non-streaming (auth richiesta).
pub async fn complete(
    State(state): State<AppState>,
    Json(body): Json<LlmRequest>,
) -> Response {
    if body.messages.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "messages required" })),
        )
            .into_response();
    }
    match run_complete(&state, &body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /v1/stream`: completion in streaming SSE (auth richiesta).
///
/// Differenza dal Node: la `FallbackChain` non espone uno stream multi-provider,
/// quindi lo streaming usa il PRIMO provider risolto non in cooldown (parita'
/// ragionevole: il caso comune e' un solo primario sano). Su errore di apertura
/// dello stream emette un evento `error` e termina, come il `catch` del server.ts.
pub async fn stream(
    State(state): State<AppState>,
    Json(body): Json<LlmRequest>,
) -> Response {
    if body.messages.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "messages required" })),
        )
            .into_response();
    }

    let sse_stream = build_sse_stream(state, body).await;
    Sse::new(sse_stream).into_response()
}

/// Costruisce lo stream SSE: risolve la chain, enforce quota, apre lo stream del
/// primo provider sano e mappa ogni chunk in un evento `data:`. Gli errori
/// diventano un evento JSON `{error}`; alla fine emette `[DONE]`.
async fn build_sse_stream(
    state: AppState,
    body: LlmRequest,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    // Canale -> ReceiverStream (tokio-stream, gia' in dipendenza): nessuna dep extra.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(32);

    tokio::spawn(async move {
        let runtime = state.runtime_snapshot().await;
        let classifier = SensitivityClassifier::new(runtime.presidio.clone());
        let classification = classifier.classify(&body.messages).await;
        let tier = classification.tier.max(body.metadata.sensitivity_tier);

        // Pin esplicito: chain di UN solo provider, niente policy.decide ne'
        // fallback cross-provider (parita' col path non-streaming). La DLP
        // (classify/redaction) resta a monte; qui si salta solo il routing.
        let resolved: Vec<ResolvedProvider> = if let Some(pin) = body.pin_provider.as_deref() {
            match resolve_pinned_provider(pin, &runtime.providers, &body.model) {
                Ok(rp) => vec![rp],
                Err(e) => {
                    let _ = tx
                        .send(Ok(Event::default()
                            .data(json!({ "error": e.message }).to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            }
        } else {
            let decision = runtime.policy.decide(tier, &body.metadata.feature, &HashMap::new());
            if decision.blocked {
                let _ = tx
                    .send(Ok(Event::default().data(
                        json!({ "error": decision.reason.unwrap_or_default() }).to_string(),
                    )))
                    .await;
                let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                return;
            }
            resolve_providers(
                &decision.providers,
                &runtime.providers,
                &runtime.aliases,
                &body.model,
                tier,
            )
        };

        // Primo provider non in cooldown. Con pin la chain ha un solo elemento:
        // se quel provider e' in cooldown non c'e' alcun ripiego (errore), come
        // richiesto dalla semantica pin.
        let Some(rp) = resolved
            .iter()
            .find(|rp| !state.cooldown.is_in_cooldown(rp.provider.name()))
        else {
            let _ = tx
                .send(Ok(Event::default()
                    .data(json!({ "error": "nessun provider disponibile" }).to_string())))
                .await;
            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
            return;
        };

        // Quota guardrail (non blocca lo stream se passa).
        if let Err(e) = enforce_quota(&state.db, &body, rp.provider.name(), &rp.model).await {
            let msg = e
                .downcast_ref::<QuotaExceeded>()
                .map(|q| q.to_string())
                .unwrap_or_else(|| "quota check fallito".to_string());
            let _ = tx
                .send(Ok(Event::default().data(json!({ "error": msg }).to_string())))
                .await;
            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
            return;
        }

        let mut req = body.clone();
        req.model = rp.model.clone();

        match rp.provider.stream(&req).await {
            Ok(mut chunks) => {
                use futures::StreamExt;
                while let Some(item) = chunks.next().await {
                    match item {
                        Ok(chunk) => {
                            // Chunk finale con usage + provider + model: scrivi ledger
                            // (parita' col server.ts che registra l'usage dello stream).
                            if let (Some(usage), Some(provider), Some(model)) =
                                (chunk.usage, &chunk.provider_used, &chunk.model_used)
                            {
                                let resp = LlmResponse {
                                    content: String::new(),
                                    tool_calls: None,
                                    usage,
                                    model_used: model.clone(),
                                    provider_used: provider.clone(),
                                    latency_ms: 0,
                                    finish_reason: chunk.finish_reason.clone().unwrap_or_default(),
                                    privacy_rerouted: None,
                                    reasoning: None,
                                    thinking_signature: None,
                                };
                                record_usage_to_ledger(&state.db, &body, &resp).await;
                            }
                            let payload = serde_json::to_string(&chunk).unwrap_or_default();
                            if tx.send(Ok(Event::default().data(payload))).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            let _ = tx
                                .send(Ok(Event::default()
                                    .data(json!({ "error": err.to_string() }).to_string())))
                                .await;
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                let name = rp.provider.name();
                let msg = err.to_string();
                if is_billing_error(&msg) {
                    state.cooldown.mark_billing(name, Some(msg.clone()));
                } else {
                    state.cooldown.mark_transient(name, Some(msg.clone()));
                }
                let _ = tx
                    .send(Ok(Event::default().data(json!({ "error": msg }).to_string())))
                    .await;
            }
        }

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    tokio_stream::wrappers::ReceiverStream::new(rx)
}

// ── Admin reload ─────────────────────────────────────────────────────────────

/// `POST /admin/reload`: ricarica chiavi/policy/alias dal DB e sostituisce il
/// `RuntimeState` a caldo. Richiede service token o JWT valido (l'auth e' gia'
/// applicata dal middleware globale; qui non c'e' check aggiuntivo perche'
/// `/admin/reload` non e' tra i path esenti).
pub async fn admin_reload(State(state): State<AppState>) -> Response {
    // Ricostruisce la config dai settings correnti (profilo puo' essere cambiato).
    let config = GatewayConfig::load(&state.db).await;
    let http = reqwest::Client::new();
    match build_runtime(&state.db, &http, config).await {
        Ok(new_runtime) => {
            let provider_names: Vec<String> =
                new_runtime.providers.iter().map(|p| p.name().to_string()).collect();
            {
                let mut guard = state.runtime.write().await;
                *guard = new_runtime;
            }
            tracing::info!(providers = ?provider_names, "gateway: ricaricato dal DB");
            Json(json!({ "reloaded": true, "providers": provider_names })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("reload failed: {e}") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, LlmUsage, MessageContent, RequestMetadata};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Provider finto (no rete) ────────────────────────────────────────────
    enum Behaviour {
        Ok,
        ErrBilling,
    }

    struct FakeProvider {
        name: String,
        behaviour: Behaviour,
        calls: AtomicUsize,
        /// Esito di `list_models`: `Some(Ok(..))` lista live, `Some(Err(..))`
        /// fallimento simulato, `None` => default del trait (lista vuota).
        models_result: Option<Result<Vec<String>, String>>,
    }

    impl FakeProvider {
        fn new(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                models_result: None,
            })
        }

        /// Variante per i test di autodiscovery: fissa l'esito di `list_models`.
        fn with_models(name: &str, models_result: Result<Vec<String>, String>) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour: Behaviour::Ok,
                calls: AtomicUsize::new(0),
                models_result: Some(models_result),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn supports_streaming(&self) -> bool {
            true
        }
        fn max_context_tokens(&self) -> u32 {
            1000
        }
        fn tier_compatibility(&self) -> &[u8] {
            &[0, 1, 2]
        }
        async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Ok => Ok(LlmResponse {
                    content: "ok".into(),
                    tool_calls: None,
                    usage: LlmUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_read_tokens: None,
                        cache_creation_tokens: None,
                    },
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                    finish_reason: "stop".into(),
                    privacy_rerouted: None,
                    reasoning: None,
                    thinking_signature: None,
                }),
                Behaviour::ErrBilling => anyhow::bail!("HTTP 402 insufficient_quota"),
            }
        }
        async fn stream(&self, _req: &LlmRequest) -> anyhow::Result<crate::provider::ChunkStream> {
            anyhow::bail!("non usato")
        }
        async fn healthcheck(&self) -> bool {
            true
        }
        async fn list_models(&self) -> anyhow::Result<Vec<String>> {
            match &self.models_result {
                Some(Ok(m)) => Ok(m.clone()),
                Some(Err(e)) => anyhow::bail!("{e}"),
                None => Ok(vec![]),
            }
        }
    }

    fn req() -> LlmRequest {
        LlmRequest {
            model: "openai/gpt-x".into(),
            messages: vec![LlmMessage {
                role: "user".into(),
                content: MessageContent::Text("ciao".into()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
            }],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "chat".into(),
            },
        }
    }

    fn aliases() -> ModelAliasResolver {
        // Modello diretto stesso provider -> passthrough (strip prefisso).
        ModelAliasResolver::from_yaml_str("aliases: {}").unwrap()
    }

    #[test]
    fn resolve_providers_esclude_non_costruiti_e_risolve_modello() {
        let p1: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![p1];
        // Policy decide openai + deepseek; deepseek NON e' costruito -> escluso.
        let resolved = resolve_providers(
            &["openai".into(), "deepseek".into()],
            &built,
            &aliases(),
            "openai/gpt-x",
            0,
        );
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].provider.name(), "openai");
        // openai/gpt-x stesso provider -> strip prefisso.
        assert_eq!(resolved[0].model, "gpt-x");
    }

    #[tokio::test]
    async fn run_fallback_primo_sano_vince() {
        let p1: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p1,
            model: "gpt-x".into(),
        }];
        let cooldown = CooldownManager::new();
        let resp = run_fallback(&resolved, &cooldown, &req()).await.unwrap();
        assert_eq!(resp.provider_used, "openai");
        assert_eq!(resp.model_used, "gpt-x");
    }

    #[tokio::test]
    async fn run_fallback_billing_marca_cooldown_e_passa() {
        let p1: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::ErrBilling);
        let p2: Arc<dyn LlmProvider> = FakeProvider::new("mistral", Behaviour::Ok);
        let resolved = vec![
            ResolvedProvider {
                provider: p1,
                model: "gpt-x".into(),
            },
            ResolvedProvider {
                provider: p2,
                model: "mistral-x".into(),
            },
        ];
        let cooldown = CooldownManager::new();
        let resp = run_fallback(&resolved, &cooldown, &req()).await.unwrap();
        assert_eq!(resp.provider_used, "mistral");
        assert!(cooldown.is_in_cooldown("openai"));
    }

    #[tokio::test]
    async fn aggregate_models_best_effort_un_provider_in_errore() {
        // Due provider sani + uno che fallisce: i sani finiscono in `providers`,
        // il rotto in `errors`, senza far fallire l'aggregazione (best-effort).
        let ok1: Arc<dyn LlmProvider> =
            FakeProvider::with_models("openai", Ok(vec!["gpt-4o".into(), "gpt-4o-mini".into()]));
        let ok2: Arc<dyn LlmProvider> =
            FakeProvider::with_models("google", Ok(vec!["gemini-2.5-flash".into()]));
        let ko: Arc<dyn LlmProvider> =
            FakeProvider::with_models("anthropic", Err("HTTP 402 insufficient_quota".into()));
        let providers = vec![ok1, ok2, ko];

        let out = aggregate_models(&providers).await;

        // Provider sani presenti con le loro liste.
        assert_eq!(
            out["providers"]["openai"],
            json!(["gpt-4o", "gpt-4o-mini"])
        );
        assert_eq!(out["providers"]["google"], json!(["gemini-2.5-flash"]));
        // Il provider rotto NON e' in `providers` ma in `errors`.
        assert!(out["providers"].get("anthropic").is_none());
        assert!(out["errors"]["anthropic"]
            .as_str()
            .unwrap()
            .contains("insufficient_quota"));
    }

    #[tokio::test]
    async fn run_fallback_tutti_falliti_errore_500() {
        let p1: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::ErrBilling);
        let resolved = vec![ResolvedProvider {
            provider: p1,
            model: "gpt-x".into(),
        }];
        let cooldown = CooldownManager::new();
        let err = run_fallback(&resolved, &cooldown, &req()).await.err().unwrap();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("openai"));
    }

    // ── Pin provider (bypass routing) ────────────────────────────────────────

    #[test]
    fn pin_provider_costruisce_chain_di_un_solo_provider_con_strip_prefisso() {
        // Tra i provider configurati ci sono sia openai che anthropic; il pin
        // su anthropic deve produrre SOLO anthropic, ignorando openai.
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let anthropic: Arc<dyn LlmProvider> = FakeProvider::new("anthropic", Behaviour::Ok);
        let built = vec![openai, anthropic];

        let rp = resolve_pinned_provider("anthropic", &built, "anthropic/claude-x")
            .expect("provider pinnato configurato");
        assert_eq!(rp.provider.name(), "anthropic");
        // Strip del prefisso "provider/": resta solo il nome modello.
        assert_eq!(rp.model, "claude-x");
    }

    #[test]
    fn pin_provider_strip_prefisso_solo_prima_componente() {
        let google: Arc<dyn LlmProvider> = FakeProvider::new("google", Behaviour::Ok);
        let built = vec![google];
        // Modello senza prefisso provider: usato as-is sul provider pinnato.
        let rp = resolve_pinned_provider("google", &built, "gemini-2.5-flash").unwrap();
        assert_eq!(rp.provider.name(), "google");
        assert_eq!(rp.model, "gemini-2.5-flash");
    }

    #[test]
    fn pin_provider_non_configurato_e_errore_non_fallback() {
        // Solo openai e' configurato; il pin su un provider assente deve dare
        // errore esplicito, non ripiegare su openai (regola G).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![openai];
        // ResolvedProvider non implementa Debug (contiene un trait object), quindi
        // non si usa expect_err qui: si destruttura il Result a mano.
        let Err(err) = resolve_pinned_provider("anthropic", &built, "anthropic/claude-x") else {
            panic!("provider non configurato deve fallire");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("anthropic"));
    }

    #[tokio::test]
    async fn pin_provider_in_cooldown_errore_senza_fallback_cross_provider() {
        // anthropic pinnato + in cooldown ("unhealthy"); openai sano e' tra i
        // built ma NON nella chain pinnata -> NON deve essere eseguito.
        let openai = FakeProvider::new("openai", Behaviour::Ok);
        let anthropic = FakeProvider::new("anthropic", Behaviour::Ok);
        let built: Vec<Arc<dyn LlmProvider>> = vec![openai.clone(), anthropic.clone()];

        // Chain pinnata = solo anthropic (come fa run_complete con pin_provider).
        let rp = resolve_pinned_provider("anthropic", &built, "anthropic/claude-x").unwrap();
        let resolved = vec![rp];

        let cooldown = CooldownManager::new();
        cooldown.mark_billing("anthropic", Some("credit balance too low".into()));

        let err = run_fallback(&resolved, &cooldown, &req())
            .await
            .expect_err("provider pinnato in cooldown deve fallire");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("anthropic"));
        // Nessun altro provider eseguito: openai mai chiamato (no fallback).
        assert_eq!(openai.calls.load(Ordering::SeqCst), 0);
        assert_eq!(anthropic.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pin_provider_che_fallisce_non_ripiega_su_altro_provider() {
        // anthropic pinnato ma fallisce (billing): openai sano resta inutilizzato.
        let openai = FakeProvider::new("openai", Behaviour::Ok);
        let anthropic = FakeProvider::new("anthropic", Behaviour::ErrBilling);
        let built: Vec<Arc<dyn LlmProvider>> = vec![openai.clone(), anthropic.clone()];

        let rp = resolve_pinned_provider("anthropic", &built, "anthropic/claude-x").unwrap();
        let resolved = vec![rp];

        let cooldown = CooldownManager::new();
        let err = run_fallback(&resolved, &cooldown, &req())
            .await
            .expect_err("provider pinnato fallito deve dare errore, non fallback");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // anthropic provato 1 volta, openai MAI (niente fallback cross-provider).
        assert_eq!(anthropic.calls.load(Ordering::SeqCst), 1);
        assert_eq!(openai.calls.load(Ordering::SeqCst), 0);
        // Il fallimento ha marcato anthropic in cooldown (billing).
        assert!(cooldown.is_in_cooldown("anthropic"));
    }
}
