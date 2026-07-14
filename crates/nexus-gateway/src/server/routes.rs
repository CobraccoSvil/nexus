//! Handler HTTP del gateway e pipeline di routing.
//!
//! Porting delle route di `server.ts`. La pipeline `/v1/complete` (e `/v1/stream`)
//! replica il flusso di `LLMGateway`:
//!   1. classify tier (secret scanner + Presidio) -> tier effettivo;
//!   2. policy_engine.decide(tier) -> lista ordinata di provider ammessi;
//!   3. per ogni provider candidato non in cooldown: risolve l'alias modello
//!      (skip se non risolvibile per quel provider), poi tenta la completion;
//!   4. su errore classifica via [`classify_provider_error`] (status+codice
//!      strutturato, regola M) e applica cooldown/retry/sanificazione history;
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

use crate::batch::{
    anthropic_batch_url, anthropic_batches_url, anthropic_headers, anthropic_results_url,
    build_anthropic_batch_body, parse_anthropic_batch_id, parse_anthropic_results,
    parse_anthropic_status, BatchStatusResponse, CreateBatchBody, CreateBatchResponse,
};
use crate::cooldown::{CooldownManager, RetryPolicy};
use crate::history_sanitizer::{self, SanitizeMode};
use crate::model_alias_resolver::ModelAliasResolver;
use crate::provider::LlmProvider;
use crate::providers::{classify_provider_error, ProviderErrorKind, ProviderHttpError};
use crate::redaction::pipeline::{RedactionOptions, RedactionPipeline};
use crate::redaction::sensitivity_classifier::SensitivityClassifier;
use crate::types::{
    ImageGenRequest, ImageGenResponse, LlmRequest, LlmResponse, RequestMetadata, TranscribeRequest,
    TranscribeResponse, TtsRequest, TtsResponse, VideoGenRequest, VideoGenResponse,
};

use super::billing::{enforce_quota, record_usage_to_ledger, QuotaExceeded};
use super::bootstrap::{build_http_client, build_runtime, GatewayConfig};
use nexus_auth::llm_timeouts::LlmTimeouts;
use super::AppState;

/// Errore della pipeline tradotto in HTTP. Mantiene lo status coerente col
/// server.ts (403 per blocchi tier/DLP/quota, 500 per fallimenti provider).
///
/// `details` (regola M): payload JSON STRUTTURATO addizionale nel body della
/// risposta, accanto a `error`/`code`. Il chiamante (motore agentico) decide
/// dalla struttura — classe del fallimento per-provider, tier rilevato,
/// provider ammessi — mai dal testo umano di `message`.
#[derive(Debug)]
struct PipelineError {
    status: StatusCode,
    code: String,
    message: String,
    // Boxed: tiene la Err-variant dei Result della pipeline sotto la soglia
    // clippy `result_large_err` (il details e' raro, il Result e' ovunque).
    details: Option<Box<Value>>,
}

impl PipelineError {
    fn blocked(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "TIER_BLOCKED".to_string(),
            message: message.into(),
            details: None,
        }
    }
    /// Provider escluso/i dalla policy per il sensitivity tier EFFETTIVO
    /// (contenuto riservato). Codice dedicato, distinto sia dal generico
    /// `TIER_BLOCKED` sia da `PROVIDER_ERROR`: il motore lo usa per riportare
    /// il motivo onesto ("escluso per policy", non "provider instabile") e per
    /// scegliere un sostituto tra gli `allowed_providers`.
    fn policy_tier_excluded(
        provider: Option<&str>,
        detected_tier: u8,
        allowed: &[String],
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "POLICY_TIER_EXCLUDED".to_string(),
            message: message.into(),
            details: Some(Box::new(json!({
                "provider": provider,
                "detected_tier": detected_tier,
                "allowed_providers": allowed,
            }))),
        }
    }
    fn quota(scope: &str, reason: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "QUOTA_EXCEEDED".to_string(),
            message: format!("quota_exceeded:{scope}:{reason}"),
            details: None,
        }
    }
    fn provider(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROVIDER_ERROR".to_string(),
            message: message.into(),
            details: None,
        }
    }
    /// Variante di [`Self::provider`] col dettaglio strutturato dei fallimenti
    /// per-provider (`details.failures` + `details.primary_cause`).
    fn provider_with_details(message: impl Into<String>, details: Value) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROVIDER_ERROR".to_string(),
            message: message.into(),
            details: Some(Box::new(details)),
        }
    }
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_REQUEST".to_string(),
            message: message.into(),
            details: None,
        }
    }
}

/// Guardia d'ingresso della pipeline (punto unico, regola L): una richiesta con
/// modello vuoto — `""` oppure `"provider/"` col segmento modello vuoto — e'
/// sempre un bug del chiamante (es. purpose non risolto a monte). Senza guardia
/// il resolver alias la propaga "as-is" a TUTTI i provider della chain e la
/// cascata fallisce con 400/404 fuorvianti su ogni provider ("you must provide
/// a model parameter", incidente 2026-07-02 con clarify_expand). Errore 400
/// chiaro subito, nessuna chiamata provider a vuoto.
fn validate_logical_model(model: &str) -> Result<(), PipelineError> {
    let logical = model.trim();
    let effective = match logical.split_once('/') {
        Some((_, rest)) => rest.trim(),
        None => logical,
    };
    if effective.is_empty() {
        return Err(PipelineError::invalid_request(format!(
            "modello mancante nella richiesta (model=\"{model}\"): il chiamante deve \
             risolvere provider/modello a monte (routing matrix o nexus_purpose_model)"
        )));
    }
    Ok(())
}

impl IntoResponse for PipelineError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message, "code": self.code });
        if let Some(details) = self.details {
            body["details"] = *details;
        }
        (self.status, Json(body)).into_response()
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
    validate_logical_model(&req.model)?;
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
    // redaction) resta attiva: il pin salta SOLO il routing, non la sicurezza —
    // incluso il gate cloud per-tier qui sotto (`pin_tier_gate`).
    let resolved: Vec<ResolvedProvider> = if let Some(pin) = req.pin_provider.as_deref() {
        pin_tier_gate(&runtime.policy, pin, effective_tier, &req.metadata.feature)?;
        vec![resolve_pinned_provider(pin, &runtime.providers, &req.model)?]
    } else {
        let decision = runtime
            .policy
            .decide(effective_tier, &req.metadata.feature, &HashMap::new());
        if decision.blocked {
            let reason = decision
                .reason
                .unwrap_or_else(|| "routing bloccato dalla policy".to_string());
            // Blocco dal gate DLP per-tier (segnale strutturato, non parsing
            // della reason): codice dedicato con tier rilevato e ammessi.
            if decision.dlp_blocked {
                return Err(PipelineError::policy_tier_excluded(
                    None,
                    effective_tier,
                    &decision.providers,
                    reason,
                ));
            }
            return Err(PipelineError::blocked(reason));
        }

        // Accoppia ogni nome-provider deciso col provider costruito + modello risolto.
        let (resolved, any_tier_mismatch) = resolve_providers(
            &decision.providers,
            &runtime.providers,
            &runtime.aliases,
            &req.model,
            effective_tier,
        );
        if resolved.is_empty() {
            // Chain svuotata da esclusioni per tier (alias min/max): il motivo
            // e' la sensitivity del contenuto, non la configurazione -> codice
            // dedicato cosi' il chiamante non lo scambia per un guasto.
            if any_tier_mismatch {
                return Err(PipelineError::policy_tier_excluded(
                    None,
                    effective_tier,
                    &decision.providers,
                    format!(
                        "nessun provider ammesso per il modello richiesto al sensitivity \
                         tier {effective_tier} (contenuto riservato)"
                    ),
                ));
            }
            return Err(PipelineError::blocked(
                "nessun provider configurato/risolvibile per il tier richiesto",
            ));
        }
        resolved
    };

    // Redaction pre-flight: strict mode quando il tier e' elevato (>=2) e il
    // provider scelto e' cloud. La mappa serve per la reidratazione post-flight.
    let strict = effective_tier >= 2;
    // Policy PII asimmetrica (regola G, opt-in): quando ON, la PII fornita
    // volontariamente dall'utente nel proprio messaggio (soggetto del task) non
    // viene oscurata; segreti e PII di terzi restano redatti. Default sicuro
    // (comportamento storico) se il flag e' assente o il DB non risponde.
    let skip_pii_in_user_messages = nexus_auth::get_bool_setting(
        &state.db,
        "gateway.redaction.skip_pii_in_user_messages",
    )
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    let pipeline = RedactionPipeline::new(
        runtime.presidio.clone(),
        RedactionOptions {
            strict_mode: strict,
            skip_pii_in_user_messages,
            ..Default::default()
        },
    );
    let redaction = pipeline
        .redact(req)
        .await
        .map_err(|e| PipelineError::blocked(e.to_string()))?;
    if redaction.any_redacted() {
        // Regola M: log dai flag strutturati, non dal placeholder testuale.
        tracing::info!(
            secrets = redaction.stats.secrets_found,
            pii = redaction.stats.pii_found,
            code = redaction.stats.code_anonymized,
            secret_redacted = redaction.secret_redacted(),
            pii_redacted = redaction.pii_redacted(),
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
    // `strict`: quando il provider e' pinnato (scelta utente / routing gia'
    // risolto, nessuno swap possibile) si RITENTA lo stesso modello sui
    // transitori e si attende un cooldown breve invece di fallire subito.
    let strict = req.pin_provider.is_some();
    let mut response = run_fallback(
        &resolved,
        &state.cooldown,
        &redacted_req,
        strict,
        runtime.timeouts.per_attempt,
    )
    .await?;

    // Reidratazione post-flight: ripristina gli originali nei placeholder.
    response = pipeline.rehydrate(&response, &mut map);

    // Telemetria: ledger best-effort (non blocca la risposta).
    record_usage_to_ledger(&state.db, req, &response).await;

    Ok(response)
}

/// Accoppia i nomi-provider della decisione coi provider costruiti e risolve il
/// modello reale per ciascuno. Provider non costruiti (chiave assente) o senza
/// modello risolvibile vengono esclusi (regola G: niente fallback silenzioso).
///
/// Il secondo elemento della tupla e' `true` se ALMENO un provider e' stato
/// escluso per incompatibilita' di tier dell'alias (`AliasError::TierMismatch`):
/// segnale strutturato che permette al caller di rispondere
/// `POLICY_TIER_EXCLUDED` (esclusione per sensitivity) quando la chain resta
/// vuota, invece del generico "non configurato".
fn resolve_providers(
    names: &[String],
    built: &[Arc<dyn LlmProvider>],
    aliases: &ModelAliasResolver,
    logical_model: &str,
    tier: u8,
) -> (Vec<ResolvedProvider>, bool) {
    let mut out = Vec::new();
    let mut any_tier_mismatch = false;
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
                if matches!(e, crate::model_alias_resolver::AliasError::TierMismatch { .. }) {
                    any_tier_mismatch = true;
                }
                // Provider escluso dalla chain: modello non risolvibile per quel
                // provider/tier. Solo nome+motivo nel log (regola F).
                tracing::debug!(provider = %name, reason = %e, "gateway: provider escluso dalla chain");
            }
        }
    }
    (out, any_tier_mismatch)
}

/// Gate DLP cloud per-tier sul provider PINNATO (punto unico, regola L, usato
/// dai path complete e stream). Il pin bypassa il ROUTING (`policy.decide`) ma
/// NON questo gate: se il tier effettivo vieta i provider cloud
/// (`dlp_allow_cloud_tier2/3=false`) e il pin e' cloud, la richiesta viene
/// rifiutata con `POLICY_TIER_EXCLUDED` (tier rilevato + provider ammessi)
/// invece di inviare comunque il contenuto riservato al cloud. Coi flag DLP
/// permissivi (default profilo cloud) il gate non scatta mai: bit-identico al
/// comportamento storico.
fn pin_tier_gate(
    policy: &crate::policy_engine::PolicyEngine,
    pin: &str,
    effective_tier: u8,
    feature: &str,
) -> Result<(), PipelineError> {
    use crate::policy_engine::PolicyEngine;
    if policy.cloud_blocked_for_tier(effective_tier) && PolicyEngine::is_cloud_provider(pin) {
        // Provider ammessi al tier (solo i locali, dato il gate chiuso).
        let allowed = policy.decide(effective_tier, feature, &HashMap::new()).providers;
        return Err(PipelineError::policy_tier_excluded(
            Some(pin),
            effective_tier,
            &allowed,
            format!(
                "provider \"{pin}\" escluso dalla policy per sensitivity tier \
                 {effective_tier} (contenuto riservato, cloud vietato dal gate DLP); \
                 provider ammessi: {}",
                if allowed.is_empty() {
                    "nessuno".to_string()
                } else {
                    allowed.join(", ")
                }
            ),
        ));
    }
    Ok(())
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

/// Esegue il fallback sui provider risolti: prova in ordine, con retry sullo
/// STESSO modello per gli errori transitori quando `strict` (pin: nessuno swap
/// possibile). Punto unico dello stato cooldown e della classificazione errore
/// (regola L).
///
/// `strict = true` (provider pinnato / routing gia' risolto): il modello scelto
/// NON viene mai sostituito. Su transitorio (429/5xx/timeout) si ritenta lo
/// stesso modello con backoff; su cooldown transitorio BREVE si attende il
/// residuo invece di fallire subito. Errore solo su billing/quota, errore lato
/// client (400/404/403-per-modello) o retry esauriti.
///
/// `strict = false` (chain multi-provider, path non-pin): un tentativo per
/// provider, poi passa al successivo (comportamento storico).
async fn run_fallback(
    resolved: &[ResolvedProvider],
    cooldown: &CooldownManager,
    base_req: &LlmRequest,
    strict: bool,
    per_attempt: std::time::Duration,
) -> Result<LlmResponse, PipelineError> {
    let mut failures: Vec<ProviderFailure> = Vec::new();
    let policy = cooldown.retry_policy();

    for rp in resolved {
        let name = rp.provider.name();

        if cooldown.is_in_cooldown(name) {
            let secs = cooldown.seconds_remaining(name);
            if cooldown.is_billing_cooldown(name) {
                // Billing: il provider e' inutilizzabile finche' non si ricarica.
                // Il messaggio lo segnala cosi' il chiamante applica il cooldown
                // lungo invece di riprovare a ogni iterazione.
                failures.push(ProviderFailure {
                    provider: name.to_string(),
                    class: "cooldown_billing",
                    status: None,
                    code: None,
                    message: format!("cooldown billing, {secs}s rimanenti"),
                });
                continue;
            }
            // Cooldown transitorio BREVE + strict pin (nessuno swap): attendi il
            // residuo e ritenta lo stesso modello, invece del hard-fail storico
            // (causa dell'errore "tutti i provider hanno fallito -> google
            // (cooldown 21s)"). Oltre il tetto, propaga. NB: `secs` e' troncato a
            // interi, quindi un residuo sub-secondo vale 0: attendere 0s = procedi
            // subito (il cooldown e' di fatto scaduto), non e' un caso di hard-fail.
            if strict && secs <= policy.wait_short_cooldown_cap_s {
                tracing::info!(
                    provider = name,
                    wait_s = secs,
                    "gateway: attendo cooldown transitorio breve prima di ritentare (strict pin)"
                );
                tokio::time::sleep(std::time::Duration::from_secs(secs as u64)).await;
            } else {
                failures.push(ProviderFailure {
                    provider: name.to_string(),
                    class: "cooldown",
                    status: None,
                    code: None,
                    message: format!("in cooldown, {secs}s rimanenti"),
                });
                continue;
            }
        }

        // Richiesta col modello reale risolto per questo provider.
        let mut req = base_req.clone();
        req.model = rp.model.clone();

        match complete_with_retry(
            rp.provider.as_ref(),
            &req,
            name,
            cooldown,
            &policy,
            strict,
            per_attempt,
        )
        .await
        {
            Ok(resp) => {
                // Successo reale: se il provider era in cooldown transitorio,
                // liberalo subito (ha appena risposto 200).
                cooldown.clear(name);
                return Ok(resp);
            }
            Err(f) => failures.push(f.into_provider_failure(name)),
        }
    }

    // Messaggio umano invariato (display/log); la CLASSE di ogni fallimento
    // viaggia in `details` (regola M: il motore decide dalla struttura).
    let human = failures
        .iter()
        .map(|f| format!("{} ({})", f.provider, f.message))
        .collect::<Vec<_>>()
        .join("; ");
    // Causa primaria = classe del PRIMO fallimento: col pin la chain ha un solo
    // elemento; nella chain multi-provider il primo e' il primario del tier.
    let primary_cause = failures.first().map(|f| f.class).unwrap_or("unknown");
    let details = json!({
        "primary_cause": primary_cause,
        "failures": failures.iter().map(ProviderFailure::to_json).collect::<Vec<_>>(),
    });
    Err(PipelineError::provider_with_details(
        format!("tutti i provider hanno fallito -> {human}"),
        details,
    ))
}

/// Fallimento di UN provider della chain, in forma STRUTTURATA (regola M).
/// `class` e' il vocabolario chiuso condiviso col motore agentico:
/// `billing` | `client_error` | `transient` (esito della chiamata) oppure
/// `cooldown` | `cooldown_billing` (provider saltato senza chiamarlo) oppure
/// `empty_completion` (200 senza output utile: content vuoto, zero tool-call,
/// finish_reason non terminale -> chain prosegue / failover cross-provider).
/// `status`/`code` arrivano dal [`ProviderHttpError`] quando disponibili.
struct ProviderFailure {
    provider: String,
    class: &'static str,
    status: Option<u16>,
    code: Option<String>,
    message: String,
}

impl ProviderFailure {
    fn to_json(&self) -> Value {
        json!({
            "provider": self.provider,
            "class": self.class,
            "status": self.status,
            "code": self.code,
            "message": self.message,
        })
    }
}

/// Esito d'errore di [`complete_with_retry`]: classe + segnali strutturati del
/// [`ProviderHttpError`] (se la chiamata ha raggiunto il provider). Sostituisce
/// il vecchio `Err(String)` che appiattiva la classificazione gia' calcolata.
struct CallFailure {
    class: &'static str,
    status: Option<u16>,
    code: Option<String>,
    message: String,
}

impl CallFailure {
    fn from_error(kind: ProviderErrorKind, err: &anyhow::Error) -> Self {
        let http = err.chain().find_map(|c| c.downcast_ref::<ProviderHttpError>());
        Self {
            class: match kind {
                ProviderErrorKind::Billing => "billing",
                ProviderErrorKind::ClientError => "client_error",
                ProviderErrorKind::ContextTooLong => "context_too_long",
                ProviderErrorKind::Transient => "transient",
            },
            status: http.map(|h| h.status),
            code: http.and_then(|h| h.code.clone()),
            message: err.to_string(),
        }
    }

    /// Fallimento per risposta DEGENERE (HTTP 200 senza output utile, regola M).
    /// Non e' colpa di salute/credito del provider (nessun cooldown billing o
    /// transient da marcare): e' un turno improduttivo che deve far proseguire la
    /// chain al provider successivo. `status`/`code` restano `None` (non c'e' un
    /// errore HTTP), la classe strutturata `empty_completion` porta l'informazione.
    fn empty_completion(finish_reason: &str) -> Self {
        Self {
            class: "empty_completion",
            status: None,
            code: None,
            message: format!(
                "risposta degenere: nessun testo ne' tool-call (finish_reason={finish_reason})"
            ),
        }
    }

    /// Fallimento per CAP PER-TENTATIVO scaduto: il provider non ha risposto
    /// entro la quota che gli spetta dentro il budget della richiesta. E'
    /// `transient` (il provider e' lento ORA, non rotto), quindi la chain passa
    /// oltre e il cooldown breve lo libera al primo re-probe sano. La classe e'
    /// un segnale STRUTTURATO (regola M): nessuno dovra' dedurre "timeout" dal
    /// testo del messaggio.
    fn attempt_timeout(per_attempt: std::time::Duration) -> Self {
        Self {
            class: "transient",
            status: None,
            code: Some("attempt_timeout".to_string()),
            message: format!(
                "nessuna risposta entro il cap per-tentativo ({}s)",
                per_attempt.as_secs()
            ),
        }
    }

    fn into_provider_failure(self, provider: &str) -> ProviderFailure {
        ProviderFailure {
            provider: provider.to_string(),
            class: self.class,
            status: self.status,
            code: self.code,
            message: self.message,
        }
    }
}

/// Chiama `provider.complete` con retry sullo STESSO modello per errori
/// transitori (Fase B1, strict pin). Classifica l'errore col punto unico
/// [`classify_provider_error`] (regola L):
///   - Billing   -> `mark_billing`, niente retry, errore (ricarica necessaria);
///   - ClientError history-related -> sanificazione aggressiva + 1 retry;
///   - ClientError invalid_model -> errore immediato (niente cooldown provider);
///   - ClientError altro -> errore (colpa config/modello singolo);
///   - Transient -> retry con backoff+jitter; dopo l'ultimo tentativo
///     `mark_transient` (cooldown breve, liberato dal re-probe appena sano).
///
/// `strict = false` prova una volta sola (la chain passa al provider successivo).
///
/// `per_attempt` limita OGNI singolo `provider.complete()`: senza, un provider
/// appeso consumava tutto il budget della richiesta e la chain non arrivava mai
/// al provider successivo.
async fn complete_with_retry(
    provider: &dyn LlmProvider,
    req: &LlmRequest,
    name: &str,
    cooldown: &CooldownManager,
    policy: &RetryPolicy,
    strict: bool,
    per_attempt: std::time::Duration,
) -> Result<LlmResponse, CallFailure> {
    let max_attempts = if strict { policy.max_attempts.max(1) } else { 1 };
    let mut attempt = 0u32;
    let mut history_aggressive_retry = true;
    loop {
        attempt += 1;
        let mut call_req = req.clone();
        let sanitize_mode = if history_aggressive_retry {
            SanitizeMode::Standard
        } else {
            SanitizeMode::Aggressive
        };
        let sanitize_report =
            history_sanitizer::sanitize_history(&mut call_req.messages, name, sanitize_mode);
        if sanitize_report != history_sanitizer::SanitizeReport::default() {
            tracing::debug!(
                provider = name,
                mode = ?sanitize_mode,
                stripped_reasoning = sanitize_report.stripped_reasoning,
                stripped_trailing = sanitize_report.stripped_trailing_assistant,
                orphan_tools = sanitize_report.removed_orphan_tool_results,
                synthetic_tools = sanitize_report.injected_synthetic_tool_results,
                "gateway: history sanificata per dialetto provider"
            );
        }

        let call = match tokio::time::timeout(per_attempt, provider.complete(&call_req)).await {
            Ok(r) => r,
            Err(_) => {
                // Il cap e' scaduto: nessuna risposta da classificare. Il
                // provider e' lento ORA -> cooldown breve (come gli altri
                // transient) e la chain prova il prossimo dentro il budget.
                tracing::warn!(
                    provider = name,
                    per_attempt_s = per_attempt.as_secs(),
                    attempt,
                    "gateway: cap per-tentativo scaduto -> transient, passo oltre"
                );
                let failure = CallFailure::attempt_timeout(per_attempt);
                cooldown.mark_transient(name, Some(failure.message.clone()));
                return Err(failure);
            }
        };

        match call {
            Ok(resp) => {
                // PUNTO UNICO di validazione della risposta (regola L+M): un 200
                // senza output utile (content vuoto E nessuna tool-call E
                // finish_reason non terminale, es. Gemini "length" col budget
                // consumato dal thinking) e' un fallimento STRUTTURATO, non un
                // successo. Convertirlo in `Err(empty_completion)` fa proseguire
                // la chain al provider successivo (ramo Err in run_fallback =
                // push failure + continue) e, sull'ultimo elemento, produce un
                // 500 PROVIDER_ERROR con primary_cause="empty_completion" che il
                // motore mappa a failover cross-provider. NON marcare cooldown
                // billing/transient (non e' colpa di salute/credito) e NON
                // ritentare lo stesso modello: la degenerazione da budget e'
                // deterministica.
                if resp.is_degenerate_completion() {
                    tracing::warn!(
                        provider = name,
                        finish_reason = %resp.finish_reason,
                        "gateway: risposta degenere (content vuoto, zero tool-call, \
                         finish non terminale) -> failure empty_completion, passo oltre"
                    );
                    return Err(CallFailure::empty_completion(&resp.finish_reason));
                }
                return Ok(resp);
            }
            Err(err) => {
                // Classificazione DETERMINISTICA su status/codice strutturato
                // (regola H): il testo del messaggio serve solo per log/display.
                let kind = classify_provider_error(&err);
                let http = err
                    .chain()
                    .find_map(|c| c.downcast_ref::<ProviderHttpError>());
                // `Retry-After` autoritativo dal provider (RFC 9457/7231), se c'e'.
                let retry_after = http.and_then(|h| h.retry_after_seconds);
                let failure = CallFailure::from_error(kind, &err);
                let msg = failure.message.clone();
                match kind {
                    ProviderErrorKind::Billing => {
                        cooldown.mark_billing(name, Some(msg));
                        return Err(failure);
                    }
                    ProviderErrorKind::ClientError => {
                        let code = failure.code.as_deref();
                        let status = failure.status.unwrap_or(0);
                        if history_sanitizer::is_invalid_model_error(code, status) {
                            tracing::warn!(
                                provider = name,
                                status = failure.status,
                                code = code,
                                "gateway: modello invalido/deprecato (client_error, niente cooldown provider)"
                            );
                            return Err(failure);
                        }
                        if history_aggressive_retry
                            && history_sanitizer::is_history_related_client_error(code)
                        {
                            history_aggressive_retry = false;
                            tracing::warn!(
                                provider = name,
                                status = failure.status,
                                code = code,
                                "gateway: client_error history -> sanificazione aggressiva e retry"
                            );
                            continue;
                        }
                        tracing::warn!(
                            provider = name,
                            status = failure.status,
                            code = code,
                            "gateway: il provider ha rifiutato la richiesta \
                             (errore client, niente retry/cooldown)"
                        );
                        return Err(failure);
                    }
                    ProviderErrorKind::ContextTooLong => {
                        // 413 request_too_large: ritentare lo STESSO provider e' inutile
                        // e NON va messo in cooldown (il provider e' sano, e' la
                        // richiesta a superare la sua finestra/limite). Torniamo l'errore
                        // con class "context_too_long": il motore (allows_cross_provider_failover,
                        // causa != ClientError) ripieghera' su un provider a finestra piu'
                        // grande invece di chiudere n/d.
                        tracing::warn!(
                            provider = name,
                            status = failure.status,
                            "gateway: richiesta troppo grande per il provider (413) -> niente \
                             retry/cooldown, il motore fara' failover cross-provider"
                        );
                        return Err(failure);
                    }
                    ProviderErrorKind::Transient => {
                        let cap_s = policy.wait_short_cooldown_cap_s.max(0) as u64;
                        // Se il provider chiede un'attesa piu' lunga del tetto, non
                        // bloccare la richiesta cosi' a lungo: arrenditi (cooldown
                        // breve; la riprova successiva o il re-probe recuperano).
                        if attempt >= max_attempts || retry_after.is_some_and(|s| s > cap_s) {
                            cooldown.mark_transient(name, Some(msg));
                            return Err(failure);
                        }
                        // Onora `Retry-After` (autoritativo) se presente e sotto il
                        // tetto; altrimenti backoff esponenziale+jitter calcolato.
                        let delay = match retry_after {
                            Some(s) => s.saturating_mul(1000),
                            None => policy.backoff_ms(attempt, jitter_seed()),
                        };
                        tracing::warn!(
                            provider = name,
                            attempt,
                            delay_ms = delay,
                            honored_retry_after = retry_after.is_some(),
                            "gateway: errore transitorio, retry stesso modello"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
            }
        }
    }
}

/// Seed per il jitter del backoff: nanosecondi dell'orologio di sistema. Serve
/// solo a de-sincronizzare client concorrenti (non e' crittografico); in caso di
/// errore dell'orologio ripiega su 0 (backoff senza jitter, comunque valido).
fn jitter_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
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
    match p.list_models_meta().await {
        Ok(metas) => {
            // Contratto retro-compatibile: `models` resta la lista di id;
            // `models_meta` (additivo) porta la finestra dichiarata dal provider
            // quando esposta (Mistral `max_context_length`), cosi' il catalog
            // sync scrive il valore REALE invece di un placeholder (regola G/H).
            let ids: Vec<&str> = metas.iter().map(|m| m.id.as_str()).collect();
            Json(json!({ "provider": provider, "models": ids, "models_meta": metas }))
                .into_response()
        }
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
    // DEADLINE END-TO-END (retry e chain inclusi). Senza di essa una singola
    // completion poteva correre quanto il TRASPORTO concedeva (300s) — cioe'
    // quanto l'INTERO run multi-turno che l'aveva chiesta: una chiamata lenta
    // consumava il 100% della vita del run, che moriva con `it=0`, e la colpa
    // finiva ogni volta sul modello di turno. Il budget garantisce invece
    // `min_guaranteed_turns` turni per run (punto unico: nexus_auth::llm_timeouts).
    let budget = state.runtime_snapshot().await.timeouts.request_budget;
    match tokio::time::timeout(budget, run_complete(&state, &body)).await {
        Ok(Ok(resp)) => Json(resp).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => {
            tracing::warn!(
                budget_s = budget.as_secs(),
                model = %body.model,
                feature = %body.metadata.feature,
                "gateway: budget della richiesta esaurito -> 504, il motore fara' failover"
            );
            // 504 con codice STRUTTURATO (regola M): il chiamante decide sul
            // codice, non sul testo.
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({
                    "error": "request budget exceeded",
                    "code": "request_budget_exceeded",
                    "primary_cause": "request_budget_exceeded",
                    "budget_seconds": budget.as_secs(),
                })),
            )
                .into_response()
        }
    }
}

/// `POST /v1/stream`: completion in streaming SSE (auth richiesta).
///
/// Differenza dal Node: il fallback multi-provider (`run_fallback`) non espone
/// uno stream, quindi lo streaming usa il PRIMO provider risolto non in cooldown
/// (parita'
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
    // Stessa guardia del path non-streaming (punto unico validate_logical_model):
    // un modello vuoto non deve mai aprire uno stream verso i provider.
    if let Err(e) = validate_logical_model(&body.model) {
        return e.into_response();
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
        // (classify/redaction) resta a monte; qui si salta solo il routing —
        // il gate cloud per-tier (`pin_tier_gate`) resta enforced come nel
        // path non-streaming.
        let resolved: Vec<ResolvedProvider> = if let Some(pin) = body.pin_provider.as_deref() {
            let pinned = pin_tier_gate(&runtime.policy, pin, tier, &body.metadata.feature)
                .and_then(|()| resolve_pinned_provider(pin, &runtime.providers, &body.model));
            match pinned {
                Ok(rp) => vec![rp],
                Err(e) => {
                    // Parita' col body JSON del path non-streaming: `code` (e
                    // `details` se presenti) accanto al testo, cosi' il
                    // consumer SSE decide dalla struttura (regola M).
                    let mut payload = json!({ "error": e.message, "code": e.code });
                    if let Some(d) = e.details {
                        payload["details"] = *d;
                    }
                    let _ = tx
                        .send(Ok(Event::default().data(payload.to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            }
        } else {
            let decision = runtime.policy.decide(tier, &body.metadata.feature, &HashMap::new());
            if decision.blocked {
                let code = if decision.dlp_blocked {
                    "POLICY_TIER_EXCLUDED"
                } else {
                    "TIER_BLOCKED"
                };
                let _ = tx
                    .send(Ok(Event::default().data(
                        json!({
                            "error": decision.reason.unwrap_or_default(),
                            "code": code,
                            "details": { "detected_tier": tier, "allowed_providers": decision.providers },
                        })
                        .to_string(),
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
            .0
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
                                    citations: None,
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
                let kind = classify_provider_error(&err);
                let msg = err.to_string();
                match kind {
                    ProviderErrorKind::Billing => state.cooldown.mark_billing(name, Some(msg.clone())),
                    // Colpa nostra/config o modello non abilitato: niente cooldown.
                    ProviderErrorKind::ClientError => {}
                    // 413 troppo grande per il provider: sano, niente cooldown.
                    ProviderErrorKind::ContextTooLong => {}
                    ProviderErrorKind::Transient => {
                        state.cooldown.mark_transient(name, Some(msg.clone()))
                    }
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

// ── Image generation ─────────────────────────────────────────────────────────

/// `POST /v1/images/generations` (auth richiesta): genera immagini con il
/// provider scelto. Risolve il provider (pin esplicito oppure il primo sano che
/// dichiara `supports_image_gen()`), enforce quota PRIMA della chiamata, delega a
/// `provider.generate_image`. Ritorna [`ImageGenResponse`].
///
/// Routing: volutamente SEMPLICE (il routing per-capability fine vive in
/// mcp-core, regola L): con `pin_provider` esegue quel provider; senza pin sceglie
/// il primo provider image-capable non in cooldown. Nessun fallback cross-provider
/// in questo PR.
///
/// Ledger: la `record_usage_to_ledger` esistente e' per-token (input/output) e non
/// si applica all'image-gen, il cui costo e' per-immagine (non riportato in token
/// dai provider). Per non INVENTARE costi (regola G/H: niente fallback nascosto),
/// in questo PR il ledger NON viene scritto per le immagini. TODO PR successiva:
/// estendere il billing con un costo per-immagine censito in `ai_price_catalog`.
pub async fn generate_image(
    State(state): State<AppState>,
    Json(body): Json<ImageGenRequest>,
) -> Response {
    if body.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "prompt required" })),
        )
            .into_response();
    }
    match run_generate_image(&state, &body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Pipeline image-gen: risolve provider image-capable, enforce quota, genera.
async fn run_generate_image(
    state: &AppState,
    body: &ImageGenRequest,
) -> Result<ImageGenResponse, PipelineError> {
    let providers = state.runtime_snapshot().await.providers;

    // Risoluzione provider (punto unico testabile, regola L): pin esplicito o
    // primo image-capable non in cooldown.
    let provider = select_image_provider(
        &providers,
        body.pin_provider.as_deref(),
        &state.cooldown,
    )?;
    let model = strip_model_prefix(&body.model);

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L): stima dal prompt come singolo messaggio user.
    let quota_req = image_gen_to_llm_request(body, &model);
    enforce_quota(&state.db, &quota_req, provider.name(), &model)
        .await
        .map_err(|e| {
            if let Some(q) = e.downcast_ref::<QuotaExceeded>() {
                PipelineError::quota(&q.scope, &q.reason)
            } else {
                PipelineError::provider(format!("quota check fallito: {e}"))
            }
        })?;

    // Richiesta col modello reale risolto per il provider.
    let mut req = body.clone();
    req.model = model;

    provider
        .generate_image(&req)
        .await
        .map_err(|e| PipelineError::provider(e.to_string()))
}

/// Seleziona il provider per l'image-gen (punto unico, regola L). Con `pin`:
/// ESATTAMENTE quel provider, che DEVE essere configurato e image-capable
/// (regola H: errore esplicito, niente delega a chi non genera immagini, niente
/// ripiego silenzioso). Senza pin: il PRIMO provider image-capable non in
/// cooldown. Nessun fallback cross-provider in questo PR.
fn select_image_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    if let Some(pin) = pin {
        let Some(p) = providers.iter().find(|p| p.name() == pin) else {
            return Err(PipelineError::provider(format!(
                "provider pinnato \"{pin}\" non configurato/abilitato nel gateway"
            )));
        };
        if !p.supports_image_gen() {
            return Err(PipelineError::provider(format!(
                "provider \"{pin}\" non supporta la generazione di immagini"
            )));
        }
        return Ok(p.clone());
    }
    providers
        .iter()
        .find(|p| p.supports_image_gen() && !cooldown.is_in_cooldown(p.name()))
        .cloned()
        .ok_or_else(|| {
            PipelineError::provider("nessun provider sano supporta la generazione di immagini")
        })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` a partire da
/// una [`ImageGenRequest`]: il prompt diventa l'unico messaggio user (cosi' la
/// stima char/4 di `estimate_prompt_tokens` resta coerente col punto unico
/// billing). Nessun `max_tokens` (le immagini non hanno completion in token).
fn image_gen_to_llm_request(body: &ImageGenRequest, model: &str) -> LlmRequest {
    use crate::types::{LlmMessage, MessageContent};
    LlmRequest {
        model: model.to_string(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(body.prompt.clone()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }],
        temperature: None,
        max_tokens: None,
        tools: None,
        response_format: None,
        stream: None,
        thinking: None,
        tool_choice: None,
        pin_provider: body.pin_provider.clone(),
        metadata: RequestMetadata {
            tenant_id: body.metadata.tenant_id.clone(),
            user_id: body.metadata.user_id.clone(),
            request_id: body.metadata.request_id.clone(),
            sensitivity_tier: body.metadata.sensitivity_tier,
            feature: body.metadata.feature.clone(),
        },
    }
}

// ── Video generation ─────────────────────────────────────────────────────────

/// `POST /v1/videos` (auth richiesta): genera un video con il provider scelto.
/// Risolve il provider (pin esplicito oppure il primo sano che dichiara
/// `supports_video_gen()`), enforce quota PRIMA della chiamata, delega a
/// `provider.generate_video`. Ritorna [`VideoGenResponse`].
///
/// DIFFERENZA CHIAVE rispetto a image/audio: il backend Veo e' ASYNC
/// long-running. Il poll-loop e' incapsulato DENTRO `provider.generate_video`
/// (start + poll con timeout DB-driven, regola H), quindi questo handler resta
/// sincrono per il client: ritorna solo quando il video e' pronto o al timeout.
///
/// Routing: volutamente SEMPLICE (il routing per-capability fine vive in
/// mcp-core, regola L), gemello di [`generate_image`]: con `pin_provider` esegue
/// quel provider; senza pin sceglie il primo provider video-capable non in
/// cooldown. Nessun fallback cross-provider in questo PR.
///
/// Ledger: la `record_usage_to_ledger` esistente e' per-token (input/output) e
/// non si applica al video-gen, il cui costo e' al secondo di video (non
/// riportato in token dal provider, non censito in `ai_price_catalog`). Per non
/// INVENTARE costi (regola G/H: niente fallback nascosto), in questo PR il ledger
/// NON viene scritto per i video, allineato al pattern image-gen / audio. TODO PR
/// successiva: costo per-secondo-video in `ai_price_catalog`.
pub async fn generate_video(
    State(state): State<AppState>,
    Json(body): Json<VideoGenRequest>,
) -> Response {
    if body.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "prompt required" })),
        )
            .into_response();
    }
    match run_generate_video(&state, &body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Pipeline video-gen: risolve provider video-capable, enforce quota, genera
/// (il poll-loop async vive dentro `provider.generate_video`).
async fn run_generate_video(
    state: &AppState,
    body: &VideoGenRequest,
) -> Result<VideoGenResponse, PipelineError> {
    let providers = state.runtime_snapshot().await.providers;

    // Risoluzione provider (punto unico testabile, regola L): pin esplicito o
    // primo video-capable non in cooldown.
    let provider =
        select_video_provider(&providers, body.pin_provider.as_deref(), &state.cooldown)?;
    let model = strip_model_prefix(&body.model);

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L): stima dal prompt come singolo messaggio user.
    let quota_req = video_gen_to_llm_request(body, &model);
    enforce_quota(&state.db, &quota_req, provider.name(), &model)
        .await
        .map_err(|e| {
            if let Some(q) = e.downcast_ref::<QuotaExceeded>() {
                PipelineError::quota(&q.scope, &q.reason)
            } else {
                PipelineError::provider(format!("quota check fallito: {e}"))
            }
        })?;

    // Richiesta col modello reale risolto per il provider.
    let mut req = body.clone();
    req.model = model;

    provider
        .generate_video(&req)
        .await
        .map_err(|e| PipelineError::provider(e.to_string()))
}

/// Seleziona il provider per il video-gen (punto unico, regola L). Gemello di
/// [`select_image_provider`]: con `pin` ESATTAMENTE quel provider, che DEVE
/// essere configurato e video-capable (regola H: errore esplicito, niente delega
/// a chi non genera video, niente ripiego silenzioso). Senza pin: il PRIMO
/// provider video-capable non in cooldown. Nessun fallback cross-provider qui.
fn select_video_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    if let Some(pin) = pin {
        let Some(p) = providers.iter().find(|p| p.name() == pin) else {
            return Err(PipelineError::provider(format!(
                "provider pinnato \"{pin}\" non configurato/abilitato nel gateway"
            )));
        };
        if !p.supports_video_gen() {
            return Err(PipelineError::provider(format!(
                "provider \"{pin}\" non supporta la generazione di video"
            )));
        }
        return Ok(p.clone());
    }
    providers
        .iter()
        .find(|p| p.supports_video_gen() && !cooldown.is_in_cooldown(p.name()))
        .cloned()
        .ok_or_else(|| {
            PipelineError::provider("nessun provider sano supporta la generazione di video")
        })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` da una
/// [`VideoGenRequest`]: il prompt diventa l'unico messaggio user (stima char/4
/// del punto unico billing). Gemella di [`image_gen_to_llm_request`]: niente
/// costo inventato (regola G/H), il ledger non viene scritto a valle.
fn video_gen_to_llm_request(body: &VideoGenRequest, model: &str) -> LlmRequest {
    use crate::types::{LlmMessage, MessageContent};
    LlmRequest {
        model: model.to_string(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(body.prompt.clone()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }],
        temperature: None,
        max_tokens: None,
        tools: None,
        response_format: None,
        stream: None,
        thinking: None,
        tool_choice: None,
        pin_provider: body.pin_provider.clone(),
        metadata: RequestMetadata {
            tenant_id: body.metadata.tenant_id.clone(),
            user_id: body.metadata.user_id.clone(),
            request_id: body.metadata.request_id.clone(),
            sensitivity_tier: body.metadata.sensitivity_tier,
            feature: body.metadata.feature.clone(),
        },
    }
}

// ── Audio transcription ──────────────────────────────────────────────────────

/// `POST /v1/audio/transcriptions` (auth richiesta): trascrive un audio col
/// provider scelto. Risolve il provider (pin esplicito oppure il primo sano che
/// dichiara `supports_audio_in()`), enforce quota PRIMA della chiamata, delega a
/// `provider.transcribe_audio`. Ritorna [`TranscribeResponse`].
///
/// Routing: volutamente SEMPLICE (il routing per-capability fine vive in
/// mcp-core, regola L), gemello di [`generate_image`]: con `pin_provider` esegue
/// quel provider; senza pin sceglie il primo provider audio-capable non in
/// cooldown. Nessun fallback cross-provider in questo PR.
///
/// Ledger: la `record_usage_to_ledger` esistente e' per-token (input/output) e
/// non si applica alla trascrizione, il cui costo e' al minuto/secondo di audio
/// (non riportato in token dal provider) e il testo risultante non e' noto a
/// priori. Per non INVENTARE costi (regola G/H: niente fallback nascosto), in
/// questo PR il ledger NON viene scritto per le trascrizioni, allineato al
/// pattern image-gen. TODO PR successiva: costo per-durata-audio in
/// `ai_price_catalog`.
pub async fn transcribe_audio(
    State(state): State<AppState>,
    Json(body): Json<TranscribeRequest>,
) -> Response {
    if body.audio_base64.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "audio_base64 required" })),
        )
            .into_response();
    }
    match run_transcribe_audio(&state, &body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Pipeline audio-in: risolve provider audio-capable, enforce quota, trascrive.
async fn run_transcribe_audio(
    state: &AppState,
    body: &TranscribeRequest,
) -> Result<TranscribeResponse, PipelineError> {
    let providers = state.runtime_snapshot().await.providers;

    // Risoluzione provider (punto unico testabile, regola L): pin esplicito o
    // primo audio-capable non in cooldown.
    let provider =
        select_audio_in_provider(&providers, body.pin_provider.as_deref(), &state.cooldown)?;
    let model = strip_model_prefix(&body.model);

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L). Il testo risultante non e' noto a priori e
    // l'audio non e' tokenizzabile come prompt: usiamo una stima minima (un
    // messaggio user vuoto). Coerente col pattern image-gen, che NON scrive
    // ledger: niente costo inventato (regola G/H).
    let quota_req = transcribe_to_llm_request(body, &model);
    enforce_quota(&state.db, &quota_req, provider.name(), &model)
        .await
        .map_err(|e| {
            if let Some(q) = e.downcast_ref::<QuotaExceeded>() {
                PipelineError::quota(&q.scope, &q.reason)
            } else {
                PipelineError::provider(format!("quota check fallito: {e}"))
            }
        })?;

    // Richiesta col modello reale risolto per il provider.
    let mut req = body.clone();
    req.model = model;

    provider
        .transcribe_audio(&req)
        .await
        .map_err(|e| PipelineError::provider(e.to_string()))
}

/// Seleziona il provider per la trascrizione audio (punto unico, regola L).
/// Gemello di [`select_image_provider`]: con `pin` ESATTAMENTE quel provider, che
/// DEVE essere configurato e audio-capable (regola H: errore esplicito, niente
/// delega a chi non trascrive, niente ripiego silenzioso). Senza pin: il PRIMO
/// provider audio-capable non in cooldown. Nessun fallback cross-provider qui.
fn select_audio_in_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    if let Some(pin) = pin {
        let Some(p) = providers.iter().find(|p| p.name() == pin) else {
            return Err(PipelineError::provider(format!(
                "provider pinnato \"{pin}\" non configurato/abilitato nel gateway"
            )));
        };
        if !p.supports_audio_in() {
            return Err(PipelineError::provider(format!(
                "provider \"{pin}\" non supporta la trascrizione audio"
            )));
        }
        return Ok(p.clone());
    }
    providers
        .iter()
        .find(|p| p.supports_audio_in() && !cooldown.is_in_cooldown(p.name()))
        .cloned()
        .ok_or_else(|| {
            PipelineError::provider("nessun provider sano supporta la trascrizione audio")
        })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` da una
/// [`TranscribeRequest`]: un messaggio user vuoto (l'audio non e' un prompt
/// testuale tokenizzabile e il testo risultante non e' noto a priori). Gemella di
/// [`image_gen_to_llm_request`]: stima minima, niente costo inventato (regola G/H).
fn transcribe_to_llm_request(body: &TranscribeRequest, model: &str) -> LlmRequest {
    use crate::types::{LlmMessage, MessageContent};
    LlmRequest {
        model: model.to_string(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(String::new()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }],
        temperature: None,
        max_tokens: None,
        tools: None,
        response_format: None,
        stream: None,
        thinking: None,
        tool_choice: None,
        pin_provider: body.pin_provider.clone(),
        metadata: RequestMetadata {
            tenant_id: body.metadata.tenant_id.clone(),
            user_id: body.metadata.user_id.clone(),
            request_id: body.metadata.request_id.clone(),
            sensitivity_tier: body.metadata.sensitivity_tier,
            feature: body.metadata.feature.clone(),
        },
    }
}

// ── Text-to-speech ───────────────────────────────────────────────────────────

/// `POST /v1/audio/speech` (auth richiesta): sintetizza in audio un testo col
/// provider scelto. Risolve il provider (pin esplicito oppure il primo sano che
/// dichiara `supports_audio_out()`), enforce quota PRIMA della chiamata, delega a
/// `provider.text_to_speech`. Ritorna [`TtsResponse`] (audio in base64).
///
/// Routing: volutamente SEMPLICE (il routing per-capability fine vive in
/// mcp-core, regola L), gemello di [`generate_image`]/[`transcribe_audio`]: con
/// `pin_provider` esegue quel provider; senza pin sceglie il primo provider
/// audio-out-capable non in cooldown. Nessun fallback cross-provider in questo PR.
///
/// Ledger: la `record_usage_to_ledger` esistente e' per-token (input/output) e
/// non si applica al TTS, il cui costo e' al carattere di input (non riportato in
/// token dal provider). Per non INVENTARE costi (regola G/H: niente fallback
/// nascosto), in questo PR il ledger NON viene scritto per la sintesi vocale,
/// allineato al pattern image-gen / transcribe. TODO PR successiva: costo
/// per-carattere-audio in `ai_price_catalog`.
pub async fn text_to_speech(
    State(state): State<AppState>,
    Json(body): Json<TtsRequest>,
) -> Response {
    if body.input.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "input required" })),
        )
            .into_response();
    }
    match run_text_to_speech(&state, &body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Pipeline audio-out: risolve provider audio-out-capable, enforce quota, sintetizza.
async fn run_text_to_speech(
    state: &AppState,
    body: &TtsRequest,
) -> Result<TtsResponse, PipelineError> {
    let providers = state.runtime_snapshot().await.providers;

    // Risoluzione provider (punto unico testabile, regola L): pin esplicito o
    // primo audio-out-capable non in cooldown.
    let provider =
        select_audio_out_provider(&providers, body.pin_provider.as_deref(), &state.cooldown)?;
    let model = strip_model_prefix(&body.model);

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L). A differenza di transcribe, qui l'input testuale
    // E' noto: la stima riusa il pattern char/4 della chat sui caratteri
    // dell'input. Coerente col pattern image-gen, NON scrive ledger: niente costo
    // inventato (regola G/H).
    let quota_req = tts_to_llm_request(body, &model);
    enforce_quota(&state.db, &quota_req, provider.name(), &model)
        .await
        .map_err(|e| {
            if let Some(q) = e.downcast_ref::<QuotaExceeded>() {
                PipelineError::quota(&q.scope, &q.reason)
            } else {
                PipelineError::provider(format!("quota check fallito: {e}"))
            }
        })?;

    // Richiesta col modello reale risolto per il provider.
    let mut req = body.clone();
    req.model = model;

    provider
        .text_to_speech(&req)
        .await
        .map_err(|e| PipelineError::provider(e.to_string()))
}

/// Seleziona il provider per la sintesi vocale (punto unico, regola L). Gemello di
/// [`select_audio_in_provider`]: con `pin` ESATTAMENTE quel provider, che DEVE
/// essere configurato e audio-out-capable (regola H: errore esplicito, niente
/// delega a chi non sintetizza, niente ripiego silenzioso). Senza pin: il PRIMO
/// provider audio-out-capable non in cooldown. Nessun fallback cross-provider qui.
fn select_audio_out_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    if let Some(pin) = pin {
        let Some(p) = providers.iter().find(|p| p.name() == pin) else {
            return Err(PipelineError::provider(format!(
                "provider pinnato \"{pin}\" non configurato/abilitato nel gateway"
            )));
        };
        if !p.supports_audio_out() {
            return Err(PipelineError::provider(format!(
                "provider \"{pin}\" non supporta la sintesi vocale"
            )));
        }
        return Ok(p.clone());
    }
    providers
        .iter()
        .find(|p| p.supports_audio_out() && !cooldown.is_in_cooldown(p.name()))
        .cloned()
        .ok_or_else(|| {
            PipelineError::provider("nessun provider sano supporta la sintesi vocale")
        })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` da una
/// [`TtsRequest`]: a differenza di transcribe, qui il testo di input E' noto, e
/// lo mettiamo come messaggio user cosi' la stima char/4 esistente lo conta.
/// Gemella di [`image_gen_to_llm_request`]/[`transcribe_to_llm_request`]: niente
/// costo inventato (regola G/H), il ledger non viene scritto a valle.
fn tts_to_llm_request(body: &TtsRequest, model: &str) -> LlmRequest {
    use crate::types::{LlmMessage, MessageContent};
    LlmRequest {
        model: model.to_string(),
        messages: vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(body.input.clone()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
        }],
        temperature: None,
        max_tokens: None,
        tools: None,
        response_format: None,
        stream: None,
        thinking: None,
        tool_choice: None,
        pin_provider: body.pin_provider.clone(),
        metadata: RequestMetadata {
            tenant_id: body.metadata.tenant_id.clone(),
            user_id: body.metadata.user_id.clone(),
            request_id: body.metadata.request_id.clone(),
            sensitivity_tier: body.metadata.sensitivity_tier,
            feature: body.metadata.feature.clone(),
        },
    }
}

// ── Batch API ────────────────────────────────────────────────────────────────

/// Base URL Anthropic per le chiamate batch. Allineata al provider non-batch
/// (`DEFAULT_BASE_URL`); override-abile via setting `anthropic_base_url` (regola
/// G: nessun valore di business hardcoded oltre al default ufficiale).
const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Risolve la base URL Anthropic dal DB (setting `anthropic_base_url`), con
/// fallback all'endpoint ufficiale. Punto unico della risoluzione base URL per i
/// due handler batch (regola L).
async fn anthropic_base_url(state: &AppState) -> String {
    nexus_auth::get_setting(&state.db, "anthropic_base_url")
        .await
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ANTHROPIC_DEFAULT_BASE_URL.to_string())
}

/// Risolve la chiave Anthropic dal DB, rispettando il flag `anthropic_enabled`
/// (parita' col bootstrap). `None` se il provider e' disabilitato o senza chiave.
async fn anthropic_api_key(state: &AppState) -> Option<String> {
    let enabled = nexus_auth::get_bool_setting(&state.db, "anthropic_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    nexus_auth::get_setting(&state.db, "anthropic_api_key")
        .await
        .filter(|k| !k.trim().is_empty())
}

/// `POST /v1/batch` (auth richiesta): crea un batch sul provider indicato.
/// Body: `{ provider, requests: [{ custom_id, ...LlmRequest }] }`.
/// Ritorna `{ batch_id, status }`.
///
/// ANTHROPIC: completo (Message Batches API). GOOGLE: 501 documentato (vedi
/// `create_batch_google`). Provider sconosciuto: 400.
pub async fn create_batch(State(state): State<AppState>, Json(body): Json<CreateBatchBody>) -> Response {
    if body.requests.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "requests required" })),
        )
            .into_response();
    }
    match body.provider.as_str() {
        "anthropic" => create_batch_anthropic(&state, &body).await,
        "google" => create_batch_google(),
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("provider '{other}' non supportato per batch") })),
        )
            .into_response(),
    }
}

/// Crea il batch Anthropic: serializza le richieste (punto unico
/// `build_anthropic_batch_body`), POST su `/messages/batches`, estrae l'id.
async fn create_batch_anthropic(state: &AppState, body: &CreateBatchBody) -> Response {
    let Some(api_key) = anthropic_api_key(state).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "provider anthropic non configurato/abilitato" })),
        )
            .into_response();
    };
    let base_url = anthropic_base_url(state).await;
    let payload = build_anthropic_batch_body(&body.requests);

    let mut builder = reqwest::Client::new().post(anthropic_batches_url(&base_url));
    for (k, v) in anthropic_headers(&api_key) {
        builder = builder.header(k, v);
    }
    let resp = match builder.json(&payload).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("gateway batch: submit Anthropic fallito");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("submit batch fallito: {e}") })),
            )
                .into_response();
        }
    };
    let status = resp.status();
    if !status.is_success() {
        // Regola F: il body d'errore non contiene prompt utente; lo propaghiamo.
        let text = resp.text().await.unwrap_or_default();
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("anthropic HTTP {}: {}", status.as_u16(), text) })),
        )
            .into_response();
    }
    let parsed: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("risposta submit non valida: {e}") })),
            )
                .into_response()
        }
    };
    match parse_anthropic_batch_id(&parsed) {
        Ok(batch_id) => {
            tracing::info!(
                provider = "anthropic",
                requests = body.requests.len(),
                "gateway batch: creato"
            );
            Json(CreateBatchResponse {
                batch_id,
                status: crate::batch::map_anthropic_status(
                    parsed
                        .get("processing_status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("in_progress"),
                ),
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Crea il batch Google: 501 documentato. Il flusso Vertex Batch richiede
/// `files.upload` di un JSONL + `batches.create(src=...)` + `files.download` dei
/// risultati: upload/download di file (Cloud Storage / files endpoint) non
/// riducibile a una chiamata REST pulita in questo passo. Un'implementazione a
/// meta' (solo submit senza recupero) sarebbe fragile (regola H: niente toppe),
/// quindi si ritorna 501 esplicito col motivo finche' il flusso file non e'
/// completato. L'auth Vertex (`gcp_auth::VertexAuth`) e' gia' pronta per quando
/// si chiudera' il pezzo.
fn create_batch_google() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "batch Google non ancora implementato",
            "reason": "il flusso Vertex Batch richiede files.upload (JSONL) + batches.create(src=...) + files.download dei risultati: upload/download file non riducibile a REST pulita in questo passo. Auth Vertex (gcp_auth) gia' pronta; manca il trasporto file."
        })),
    )
        .into_response()
}

/// `GET /v1/batch/{provider}/{batch_id}` (auth richiesta): stato del batch e, se
/// terminato, i risultati per `custom_id`.
/// Ritorna `{ status, request_counts, results: [{ custom_id, response|error }] }`.
pub async fn get_batch(
    State(state): State<AppState>,
    axum::extract::Path((provider, batch_id)): axum::extract::Path<(String, String)>,
) -> Response {
    match provider.as_str() {
        "anthropic" => get_batch_anthropic(&state, &batch_id).await,
        "google" => create_batch_google(), // stesso 501 documentato
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("provider '{other}' non supportato per batch") })),
        )
            .into_response(),
    }
}

/// Stato + risultati di un batch Anthropic. Retrieve dello stato; se `ended`,
/// scarica e parsa il file risultati JSONL (punto unico `parse_anthropic_results`).
async fn get_batch_anthropic(state: &AppState, batch_id: &str) -> Response {
    let Some(api_key) = anthropic_api_key(state).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "provider anthropic non configurato/abilitato" })),
        )
            .into_response();
    };
    let base_url = anthropic_base_url(state).await;
    let client = reqwest::Client::new();

    // 1) retrieve stato.
    let mut builder = client.get(anthropic_batch_url(&base_url, batch_id));
    for (k, v) in anthropic_headers(&api_key) {
        builder = builder.header(k, v);
    }
    let resp = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("retrieve batch fallito: {e}") })),
            )
                .into_response()
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("anthropic HTTP {}: {}", status.as_u16(), text) })),
        )
            .into_response();
    }
    let info_json: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("risposta retrieve non valida: {e}") })),
            )
                .into_response()
        }
    };
    let mut out: BatchStatusResponse = match parse_anthropic_status(&info_json) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    };

    // 2) se terminato, scarica i risultati (JSONL).
    if out.status.is_ended() {
        let mut rb = client.get(anthropic_results_url(&base_url, batch_id));
        for (k, v) in anthropic_headers(&api_key) {
            rb = rb.header(k, v);
        }
        match rb.send().await {
            Ok(r) if r.status().is_success() => {
                let jsonl = r.text().await.unwrap_or_default();
                out.results = parse_anthropic_results(&jsonl);
            }
            Ok(r) => {
                let code = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("results HTTP {code}: {text}") })),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("download results fallito: {e}") })),
                )
                    .into_response()
            }
        }
    }

    Json(out).into_response()
}

// ── Admin reload ─────────────────────────────────────────────────────────────

/// `POST /admin/reload`: ricarica chiavi/policy/alias dal DB e sostituisce il
/// `RuntimeState` a caldo. Richiede service token o JWT valido (l'auth e' gia'
/// applicata dal middleware globale; qui non c'e' check aggiuntivo perche'
/// `/admin/reload` non e' tra i path esenti).
pub async fn admin_reload(State(state): State<AppState>) -> Response {
    // Ricostruisce la config dai settings correnti (profilo puo' essere cambiato).
    let config = GatewayConfig::load(&state.db).await;
    // Client e timeout dal punto unico, come all'avvio: qui viveva un
    // `reqwest::Client::new()` che dopo ogni reload lasciava i provider SENZA
    // timeout, keepalive e pool_max_idle(0) — una chiamata poteva appendersi
    // senza limite. I timeout si rileggono dal DB, cosi' il reload li applica.
    let timeouts = LlmTimeouts::resolve(&state.db).await;
    let http = match build_http_client(&timeouts) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("reload failed: {e}") })),
            )
                .into_response()
        }
    };
    match build_runtime(&state.db, &http, config, timeouts).await {
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

    /// Cap per-tentativo nei test della chain: generoso di proposito, cosi' i
    /// provider finti (che rispondono all'istante) non lo toccano mai e i test
    /// restano su cio' che vogliono verificare. Il cap ha i suoi test dedicati.
    const TEST_PER_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(30);

    // ── Provider finto (no rete) ────────────────────────────────────────────
    enum Behaviour {
        Ok,
        ErrBilling,
        /// Errore lato client (400 invalid_request): non ritentabile, non da cooldown.
        ErrClient,
        /// Primo tentativo: 400 history-related; secondo tentativo: OK (sanificazione).
        ErrHistoryThenOk,
        /// Non risponde mai: simula la chiamata APPESA che ha originato il fix
        /// (misurata sul campo: 197s senza alcun log, con le figure ferme).
        Hang,
    }

    struct FakeProvider {
        name: String,
        behaviour: Behaviour,
        calls: AtomicUsize,
        /// Esito di `list_models`: `Some(Ok(..))` lista live, `Some(Err(..))`
        /// fallimento simulato, `None` => default del trait (lista vuota).
        models_result: Option<Result<Vec<String>, String>>,
        /// Se il provider dichiara `supports_image_gen()` (default `false`).
        image_capable: bool,
        /// Se il provider dichiara `supports_audio_in()` (default `false`).
        audio_capable: bool,
        /// Se il provider dichiara `supports_audio_out()` (default `false`).
        audio_out_capable: bool,
        /// Se il provider dichiara `supports_video_gen()` (default `false`).
        video_capable: bool,
        /// Numero di chiamate iniziali a `complete` che falliscono con un errore
        /// TRANSITORIO (503) prima di comportarsi secondo `behaviour`. Serve ai
        /// test del retry strict-pin. Default 0 (nessun fallimento transitorio).
        transient_fail_calls: usize,
    }

    impl FakeProvider {
        fn new(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                models_result: None,
                image_capable: false,
                audio_capable: false,
                audio_out_capable: false,
                video_capable: false,
            })
        }

        /// Variante che fallisce `fail_n` volte con errore TRANSITORIO poi risponde
        /// OK. Per i test del retry strict-pin.
        fn transient_then_ok(name: &str, fail_n: usize) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour: Behaviour::Ok,
                calls: AtomicUsize::new(0),
                transient_fail_calls: fail_n,
                models_result: None,
                image_capable: false,
                audio_capable: false,
                audio_out_capable: false,
                video_capable: false,
            })
        }

        /// Variante per i test di autodiscovery: fissa l'esito di `list_models`.
        fn with_models(name: &str, models_result: Result<Vec<String>, String>) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour: Behaviour::Ok,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                models_result: Some(models_result),
                image_capable: false,
                audio_capable: false,
                audio_out_capable: false,
                video_capable: false,
            })
        }

        /// Variante image-capable per i test di routing image-gen.
        fn image(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                models_result: None,
                image_capable: true,
                audio_capable: false,
                audio_out_capable: false,
                video_capable: false,
            })
        }

        /// Variante audio-capable per i test di routing audio-in.
        fn audio(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                models_result: None,
                image_capable: false,
                audio_capable: true,
                audio_out_capable: false,
                video_capable: false,
            })
        }

        /// Variante audio-out-capable per i test di routing text-to-speech.
        fn audio_out(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                models_result: None,
                image_capable: false,
                audio_capable: false,
                audio_out_capable: true,
                video_capable: false,
            })
        }

        /// Variante video-capable per i test di routing video-gen.
        fn video(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                models_result: None,
                image_capable: false,
                audio_capable: false,
                audio_out_capable: false,
                video_capable: true,
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
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            // Prime `transient_fail_calls` chiamate: errore transitorio (503),
            // emesso come ProviderHttpError (status certo) come i provider reali.
            if idx < self.transient_fail_calls {
                return Err(crate::providers::ProviderHttpError {
                    provider: self.name.clone(),
                    status: 503,
                    code: None,
                    retry_after_seconds: None,
                    message: "service unavailable (transient test)".into(),
                }
                .into());
            }
            if matches!(self.behaviour, Behaviour::Hang) {
                // Piu' lungo di qualunque cap usato nei test: e' il chiamante a
                // dover mollare, non il provider a farla finita.
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
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
                    citations: None,
                }),
                // Errori strutturati (status + codice), come i provider reali.
                Behaviour::ErrBilling => Err(crate::providers::ProviderHttpError {
                    provider: self.name.clone(),
                    status: 402,
                    code: Some("insufficient_quota".into()),
                    retry_after_seconds: None,
                    message: "insufficient_quota".into(),
                }
                .into()),
                Behaviour::ErrClient => Err(crate::providers::ProviderHttpError {
                    provider: self.name.clone(),
                    status: 400,
                    code: Some("invalid_request_error".into()),
                    retry_after_seconds: None,
                    message: "invalid request: bad field".into(),
                }
                .into()),
                Behaviour::Hang => unreachable!("Behaviour::Hang: il sleep precede il match"),
                Behaviour::ErrHistoryThenOk => {
                    if idx == 0 {
                        Err(crate::providers::ProviderHttpError {
                            provider: self.name.clone(),
                            status: 400,
                            code: Some("invalid_request_message_order".into()),
                            retry_after_seconds: None,
                            message: "invalid message order".into(),
                        }
                        .into())
                    } else {
                        Ok(LlmResponse {
                            content: "ok-after-sanitize".into(),
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
                            citations: None,
                        })
                    }
                }
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
        fn supports_image_gen(&self) -> bool {
            self.image_capable
        }
        async fn generate_image(
            &self,
            req: &ImageGenRequest,
        ) -> anyhow::Result<ImageGenResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Ok => Ok(ImageGenResponse {
                    images: vec![crate::types::GeneratedImage {
                        b64_json: Some("AAAA".to_string()),
                        url: None,
                        mime: None,
                    }],
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                }),
                Behaviour::ErrBilling => anyhow::bail!("HTTP 402 insufficient_quota"),
                Behaviour::Hang => unreachable!("Behaviour::Hang: usato solo da complete"),
                Behaviour::ErrClient | Behaviour::ErrHistoryThenOk => {
                    anyhow::bail!("HTTP 400 invalid_request: bad field")
                }
            }
        }
        fn supports_audio_in(&self) -> bool {
            self.audio_capable
        }
        async fn transcribe_audio(
            &self,
            req: &TranscribeRequest,
        ) -> anyhow::Result<TranscribeResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Ok => Ok(TranscribeResponse {
                    text: "ciao".to_string(),
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                }),
                Behaviour::ErrBilling => anyhow::bail!("HTTP 402 insufficient_quota"),
                Behaviour::Hang => unreachable!("Behaviour::Hang: usato solo da complete"),
                Behaviour::ErrClient | Behaviour::ErrHistoryThenOk => {
                    anyhow::bail!("HTTP 400 invalid_request: bad field")
                }
            }
        }
        fn supports_audio_out(&self) -> bool {
            self.audio_out_capable
        }
        async fn text_to_speech(&self, req: &TtsRequest) -> anyhow::Result<TtsResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Ok => Ok(TtsResponse {
                    audio_base64: "QUFBQQ==".to_string(),
                    mime: "audio/mpeg".to_string(),
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                }),
                Behaviour::ErrBilling => anyhow::bail!("HTTP 402 insufficient_quota"),
                Behaviour::Hang => unreachable!("Behaviour::Hang: usato solo da complete"),
                Behaviour::ErrClient | Behaviour::ErrHistoryThenOk => {
                    anyhow::bail!("HTTP 400 invalid_request: bad field")
                }
            }
        }
        fn supports_video_gen(&self) -> bool {
            self.video_capable
        }
        async fn generate_video(
            &self,
            req: &VideoGenRequest,
        ) -> anyhow::Result<VideoGenResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Ok => Ok(VideoGenResponse {
                    video_base64: Some("QUFBQQ==".to_string()),
                    url: None,
                    mime: "video/mp4".to_string(),
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                }),
                Behaviour::ErrBilling => anyhow::bail!("HTTP 402 insufficient_quota"),
                Behaviour::Hang => unreachable!("Behaviour::Hang: usato solo da complete"),
                Behaviour::ErrClient | Behaviour::ErrHistoryThenOk => {
                    anyhow::bail!("HTTP 400 invalid_request: bad field")
                }
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
                reasoning: None,
            }],
            temperature: None,
            max_tokens: None,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
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
        let (resolved, any_tier_mismatch) = resolve_providers(
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
        // Nessuna esclusione per tier: il flag strutturato resta false.
        assert!(!any_tier_mismatch);
    }

    #[tokio::test]
    async fn run_fallback_primo_sano_vince() {
        let p1: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p1,
            model: "gpt-x".into(),
        }];
        let cooldown = CooldownManager::new();
        let resp = run_fallback(&resolved, &cooldown, &req(), false, TEST_PER_ATTEMPT).await.unwrap();
        assert_eq!(resp.provider_used, "openai");
        assert_eq!(resp.model_used, "gpt-x");
    }

    // ── validate_logical_model: guardia d'ingresso sul modello vuoto ──────────

    #[test]
    fn modello_vuoto_rifiutato_con_400() {
        // "" e whitespace: mai propagare ai provider (incidente 2026-07-02:
        // cascata fallita su tutti i provider con "you must provide a model").
        for m in ["", "   "] {
            let err = validate_logical_model(m).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
            assert_eq!(err.code, "INVALID_REQUEST");
        }
    }

    #[test]
    fn prefisso_provider_senza_modello_rifiutato() {
        // "openai/" (segmento modello vuoto dopo lo strip): stesso bug, stessa
        // guardia. Era la forma prodotta da format!("{provider}/{model}") con
        // model vuoto nell'adapter mcp-core.
        let err = validate_logical_model("openai/").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn modelli_validi_passano() {
        for m in ["gpt-x", "openai/gpt-x", "google/gemini-2.5-pro", "a/b/c"] {
            assert!(validate_logical_model(m).is_ok(), "atteso ok per {m}");
        }
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
        let resp = run_fallback(&resolved, &cooldown, &req(), false, TEST_PER_ATTEMPT).await.unwrap();
        assert_eq!(resp.provider_used, "mistral");
        assert!(cooldown.is_in_cooldown("openai"));
    }

    #[tokio::test]
    async fn strict_retry_transitorio_poi_successo() {
        // Provider pinnato: 2 fallimenti transitori (503) poi OK. Strict pin
        // ritenta lo STESSO modello e vince; nessun cooldown residuo.
        let p = FakeProvider::transient_then_ok("google", 2);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let resp = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .unwrap();
        assert_eq!(resp.provider_used, "google");
        assert_eq!(p.calls.load(Ordering::SeqCst), 3); // 2 falliti + 1 ok
        assert!(!cooldown.is_in_cooldown("google"));
    }

    #[tokio::test]
    async fn strict_retry_esaurito_marca_transient() {
        // 5 fallimenti transitori, max 3 tentativi: si arrende e marca cooldown
        // BREVE (non billing).
        let p = FakeProvider::transient_then_ok("google", 5);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("tutti i provider hanno fallito"));
        assert_eq!(p.calls.load(Ordering::SeqCst), 3); // esattamente max_attempts
        assert!(cooldown.is_in_cooldown("google"));
        assert!(!cooldown.is_billing_cooldown("google"));
    }

    #[tokio::test]
    async fn strict_billing_nessun_retry() {
        // Billing: nessun retry (serve ricarica), cooldown lungo immediato.
        let p = FakeProvider::new("openai", Behaviour::ErrBilling);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gpt-x".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("tutti i provider hanno fallito"));
        assert_eq!(p.calls.load(Ordering::SeqCst), 1); // un solo tentativo
        assert!(cooldown.is_billing_cooldown("openai"));
    }

    #[tokio::test]
    async fn strict_client_error_nessun_retry_nessun_cooldown() {
        // 400 invalid_request: colpa nostra/config. Nessun retry, NESSUN cooldown
        // (il provider e' sano, non va penalizzato).
        let p = FakeProvider::new("google", Behaviour::ErrClient);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("tutti i provider hanno fallito"));
        // history-related client_error: 1 retry con sanificazione aggressiva, poi errore
        assert_eq!(p.calls.load(Ordering::SeqCst), 2);
        assert!(!cooldown.is_in_cooldown("google")); // niente cooldown
    }

    #[tokio::test]
    async fn history_client_error_sanitize_retry_poi_successo() {
        let p = FakeProvider::new("mistral", Behaviour::ErrHistoryThenOk);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "mistral-small-latest".into(),
        }];
        let cooldown = CooldownManager::new();
        let resp = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .unwrap();
        assert_eq!(resp.content, "ok-after-sanitize");
        assert_eq!(p.calls.load(Ordering::SeqCst), 2);
    }

    // ── Body d'errore strutturato (regola M): classe per-provider nei details ──

    #[tokio::test]
    async fn client_error_produce_details_con_classe_e_status() {
        // Il 4xx del provider NON deve sparire nel 500 aggregato: la classe
        // `client_error` + status + code viaggiano in details.failures cosi' il
        // motore riporta il motivo onesto (incidente run 48793fde: 4xx deepseek
        // travestito da "cooldown" nel nastro chat).
        let p = FakeProvider::new("deepseek", Behaviour::ErrClient);
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "deepseek-chat".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .err()
            .unwrap();
        assert_eq!(err.code, "PROVIDER_ERROR");
        let details = err.details.expect("details strutturati presenti");
        assert_eq!(details["primary_cause"], "client_error");
        let f = &details["failures"][0];
        assert_eq!(f["provider"], "deepseek");
        assert_eq!(f["class"], "client_error");
        assert_eq!(f["status"], 400);
        assert_eq!(f["code"], "invalid_request_error");
    }

    #[tokio::test]
    async fn cooldown_billing_produce_classe_dedicata() {
        // Provider saltato per cooldown billing attivo: classe "cooldown_billing"
        // (mai chiamato), distinta dal billing fresco della chiamata.
        let p: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "gpt-x".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        cooldown.mark_billing("openai", Some("insufficient_quota".into()));
        let err = run_fallback(&resolved, &cooldown, &req(), false, TEST_PER_ATTEMPT)
            .await
            .err()
            .unwrap();
        let details = err.details.expect("details presenti");
        assert_eq!(details["primary_cause"], "cooldown_billing");
        assert_eq!(details["failures"][0]["class"], "cooldown_billing");
    }

    // ── Gate DLP sul pin + esclusioni per tier (POLICY_TIER_EXCLUDED) ─────────

    /// Policy con cloud VIETATO a tier 3 (YAML), come i profili hybrid/onprem.
    fn policy_tier3_blocked() -> crate::policy_engine::PolicyEngine {
        crate::policy_engine::PolicyEngine::from_yaml_str(
            r#"
profile: test
features:
  allow_cloud_tier2: true
  allow_cloud_tier3: false
  dlp_enabled: true
routing:
  tier_0:
    primary: deepseek
  tier_3:
    primary: deepseek
"#,
        )
        .expect("policy valida")
    }

    #[test]
    fn pin_cloud_escluso_a_tier3_con_codice_dedicato() {
        let policy = policy_tier3_blocked();
        // Tier 3 + pin cloud: rifiuto STRUTTURATO, non un 500 generico.
        let err = pin_tier_gate(&policy, "deepseek", 3, "chat").unwrap_err();
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.code, "POLICY_TIER_EXCLUDED");
        let details = err.details.expect("details presenti");
        assert_eq!(details["provider"], "deepseek");
        assert_eq!(details["detected_tier"], 3);
        // Tier basso: il gate non scatta (bit-identico allo storico).
        assert!(pin_tier_gate(&policy, "deepseek", 0, "chat").is_ok());
        // Provider non-cloud: mai bloccato dal gate DLP.
        assert!(pin_tier_gate(&policy, "vllm", 3, "chat").is_ok());
    }

    #[test]
    fn resolve_providers_segnala_esclusione_per_tier() {
        // Alias con max_tier 1: a tier 2 il provider e' escluso per sensitivity
        // (TierMismatch) e il flag strutturato lo segnala, cosi' il caller
        // risponde POLICY_TIER_EXCLUDED invece di "non configurato".
        let aliases = ModelAliasResolver::from_yaml_str(
            r#"
aliases:
  coder-small:
    cloud_primary: openai/gpt-4o-mini
    min_tier: 0
    max_tier: 1
"#,
        )
        .unwrap();
        let p: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![p];
        let (resolved, any_tier_mismatch) =
            resolve_providers(&["openai".into()], &built, &aliases, "coder-small", 2);
        assert!(resolved.is_empty());
        assert!(any_tier_mismatch);
    }

    #[tokio::test]
    async fn strict_attende_cooldown_transitorio_breve_poi_ritenta() {
        // Google in cooldown transitorio breve (1s): strict pin attende e ritenta
        // lo stesso modello invece del hard-fail "-> google (cooldown 21s)".
        let p = FakeProvider::new("google", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        // 2s cosi' seconds_remaining (troncato) e' ~1: esercita un'attesa reale.
        cooldown.mark_at(
            "google",
            crate::cooldown::CooldownReason::Transient,
            None,
            chrono::Utc::now(),
            2,
        );
        let resp = run_fallback(&resolved, &cooldown, &req(), true, TEST_PER_ATTEMPT)
            .await
            .unwrap();
        assert_eq!(resp.provider_used, "google");
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
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
        let err = run_fallback(&resolved, &cooldown, &req(), false, TEST_PER_ATTEMPT).await.err().unwrap();
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

        let err = run_fallback(&resolved, &cooldown, &req(), false, TEST_PER_ATTEMPT)
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
        let err = run_fallback(&resolved, &cooldown, &req(), false, TEST_PER_ATTEMPT)
            .await
            .expect_err("provider pinnato fallito deve dare errore, non fallback");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // anthropic provato 1 volta, openai MAI (niente fallback cross-provider).
        assert_eq!(anthropic.calls.load(Ordering::SeqCst), 1);
        assert_eq!(openai.calls.load(Ordering::SeqCst), 0);
        // Il fallimento ha marcato anthropic in cooldown (billing).
        assert!(cooldown.is_in_cooldown("anthropic"));
    }

    // ── Routing image generation ─────────────────────────────────────────────

    #[test]
    fn select_image_provider_senza_pin_sceglie_primo_image_capable_sano() {
        // openai NON image-capable, google image-capable: senza pin vince google.
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let google: Arc<dyn LlmProvider> = FakeProvider::image("google", Behaviour::Ok);
        let built = vec![openai, google];
        let cooldown = CooldownManager::new();
        let p = select_image_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "google");
    }

    #[test]
    fn select_image_provider_senza_pin_salta_quello_in_cooldown() {
        let openai: Arc<dyn LlmProvider> = FakeProvider::image("openai", Behaviour::Ok);
        let google: Arc<dyn LlmProvider> = FakeProvider::image("google", Behaviour::Ok);
        let built = vec![openai, google];
        let cooldown = CooldownManager::new();
        cooldown.mark_billing("openai", Some("credit balance too low".into()));
        // openai image-capable ma in cooldown -> vince google.
        let p = select_image_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "google");
    }

    #[test]
    fn select_image_provider_senza_capable_e_errore() {
        // Nessun provider image-capable -> errore esplicito (regola H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![openai];
        let cooldown = CooldownManager::new();
        let Err(err) = select_image_provider(&built, None, &cooldown) else {
            panic!("senza provider image-capable deve fallire");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("nessun provider"));
    }

    #[test]
    fn select_image_provider_pin_non_capable_e_errore_non_fallback() {
        // Pin su openai (non image-capable) anche se google capable e' presente:
        // errore esplicito, niente ripiego su google (regola G/H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let google: Arc<dyn LlmProvider> = FakeProvider::image("google", Behaviour::Ok);
        let built = vec![openai, google];
        let cooldown = CooldownManager::new();
        let Err(err) = select_image_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non image-capable deve fallire");
        };
        assert!(err.message.contains("non supporta"));
    }

    #[test]
    fn select_image_provider_pin_non_configurato_e_errore() {
        let google: Arc<dyn LlmProvider> = FakeProvider::image("google", Behaviour::Ok);
        let built = vec![google];
        let cooldown = CooldownManager::new();
        let Err(err) = select_image_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non configurato deve fallire");
        };
        assert!(err.message.contains("openai"));
    }

    #[tokio::test]
    async fn fake_provider_genera_immagine() {
        let google = FakeProvider::image("google", Behaviour::Ok);
        let req = ImageGenRequest {
            model: "imagen-3.0".into(),
            prompt: "un gatto".into(),
            n: Some(1),
            size: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "image".into(),
            },
        };
        let out = google.generate_image(&req).await.unwrap();
        assert_eq!(out.provider_used, "google");
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].b64_json.as_deref(), Some("AAAA"));
    }

    // ── Routing audio transcription ──────────────────────────────────────────

    #[test]
    fn select_audio_provider_senza_pin_sceglie_primo_audio_capable_sano() {
        // openai NON audio-capable, mistral audio-capable: senza pin vince mistral.
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let mistral: Arc<dyn LlmProvider> = FakeProvider::audio("mistral", Behaviour::Ok);
        let built = vec![openai, mistral];
        let cooldown = CooldownManager::new();
        let p = select_audio_in_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "mistral");
    }

    #[test]
    fn select_audio_provider_senza_pin_salta_quello_in_cooldown() {
        let openai: Arc<dyn LlmProvider> = FakeProvider::audio("openai", Behaviour::Ok);
        let mistral: Arc<dyn LlmProvider> = FakeProvider::audio("mistral", Behaviour::Ok);
        let built = vec![openai, mistral];
        let cooldown = CooldownManager::new();
        cooldown.mark_billing("openai", Some("credit balance too low".into()));
        // openai audio-capable ma in cooldown -> vince mistral.
        let p = select_audio_in_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "mistral");
    }

    #[test]
    fn select_audio_provider_senza_capable_e_errore() {
        // Nessun provider audio-capable -> errore esplicito (regola H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![openai];
        let cooldown = CooldownManager::new();
        let Err(err) = select_audio_in_provider(&built, None, &cooldown) else {
            panic!("senza provider audio-capable deve fallire");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("nessun provider"));
    }

    #[test]
    fn select_audio_provider_pin_non_capable_e_errore_non_fallback() {
        // Pin su openai (non audio-capable) anche se mistral capable e' presente:
        // errore esplicito, niente ripiego su mistral (regola G/H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let mistral: Arc<dyn LlmProvider> = FakeProvider::audio("mistral", Behaviour::Ok);
        let built = vec![openai, mistral];
        let cooldown = CooldownManager::new();
        let Err(err) = select_audio_in_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non audio-capable deve fallire");
        };
        assert!(err.message.contains("non supporta"));
    }

    #[test]
    fn select_audio_provider_pin_non_configurato_e_errore() {
        let mistral: Arc<dyn LlmProvider> = FakeProvider::audio("mistral", Behaviour::Ok);
        let built = vec![mistral];
        let cooldown = CooldownManager::new();
        let Err(err) = select_audio_in_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non configurato deve fallire");
        };
        assert!(err.message.contains("openai"));
    }

    #[tokio::test]
    async fn fake_provider_trascrive_audio() {
        let openai = FakeProvider::audio("openai", Behaviour::Ok);
        let req = TranscribeRequest {
            model: "whisper-1".into(),
            audio_base64: "AAAA".into(),
            mime: Some("audio/mpeg".into()),
            language: Some("it".into()),
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "audio".into(),
            },
        };
        let out = openai.transcribe_audio(&req).await.unwrap();
        assert_eq!(out.provider_used, "openai");
        assert_eq!(out.model_used, "whisper-1");
        assert_eq!(out.text, "ciao");
    }

    // ── Routing text-to-speech ───────────────────────────────────────────────

    #[test]
    fn select_audio_out_provider_senza_pin_sceglie_primo_audio_out_capable_sano() {
        // openai NON audio-out-capable, eleven audio-out-capable: vince eleven.
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let eleven: Arc<dyn LlmProvider> = FakeProvider::audio_out("eleven", Behaviour::Ok);
        let built = vec![openai, eleven];
        let cooldown = CooldownManager::new();
        let p = select_audio_out_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "eleven");
    }

    #[test]
    fn select_audio_out_provider_senza_pin_salta_quello_in_cooldown() {
        let openai: Arc<dyn LlmProvider> = FakeProvider::audio_out("openai", Behaviour::Ok);
        let eleven: Arc<dyn LlmProvider> = FakeProvider::audio_out("eleven", Behaviour::Ok);
        let built = vec![openai, eleven];
        let cooldown = CooldownManager::new();
        cooldown.mark_billing("openai", Some("credit balance too low".into()));
        // openai audio-out-capable ma in cooldown -> vince eleven.
        let p = select_audio_out_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "eleven");
    }

    #[test]
    fn select_audio_out_provider_senza_capable_e_errore() {
        // Nessun provider audio-out-capable -> errore esplicito (regola H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![openai];
        let cooldown = CooldownManager::new();
        let Err(err) = select_audio_out_provider(&built, None, &cooldown) else {
            panic!("senza provider audio-out-capable deve fallire");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("nessun provider"));
    }

    #[test]
    fn select_audio_out_provider_pin_non_capable_e_errore_non_fallback() {
        // Pin su openai (non audio-out-capable) anche se eleven capable e' presente:
        // errore esplicito, niente ripiego su eleven (regola G/H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let eleven: Arc<dyn LlmProvider> = FakeProvider::audio_out("eleven", Behaviour::Ok);
        let built = vec![openai, eleven];
        let cooldown = CooldownManager::new();
        let Err(err) = select_audio_out_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non audio-out-capable deve fallire");
        };
        assert!(err.message.contains("non supporta"));
    }

    #[test]
    fn select_audio_out_provider_pin_non_configurato_e_errore() {
        let eleven: Arc<dyn LlmProvider> = FakeProvider::audio_out("eleven", Behaviour::Ok);
        let built = vec![eleven];
        let cooldown = CooldownManager::new();
        let Err(err) = select_audio_out_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non configurato deve fallire");
        };
        assert!(err.message.contains("openai"));
    }

    #[tokio::test]
    async fn fake_provider_sintetizza_audio() {
        let openai = FakeProvider::audio_out("openai", Behaviour::Ok);
        let req = TtsRequest {
            model: "gpt-4o-mini-tts".into(),
            input: "ciao mondo".into(),
            voice: Some("alloy".into()),
            response_format: Some("mp3".into()),
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "audio".into(),
            },
        };
        let out = openai.text_to_speech(&req).await.unwrap();
        assert_eq!(out.provider_used, "openai");
        assert_eq!(out.model_used, "gpt-4o-mini-tts");
        assert_eq!(out.mime, "audio/mpeg");
        assert!(!out.audio_base64.is_empty());
    }

    // ── Routing video-gen ────────────────────────────────────────────────────

    #[test]
    fn select_video_provider_senza_pin_sceglie_primo_video_capable_sano() {
        // openai NON video-capable, google video-capable: vince google.
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let google: Arc<dyn LlmProvider> = FakeProvider::video("google", Behaviour::Ok);
        let built = vec![openai, google];
        let cooldown = CooldownManager::new();
        let p = select_video_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "google");
    }

    #[test]
    fn select_video_provider_senza_pin_salta_quello_in_cooldown() {
        let google: Arc<dyn LlmProvider> = FakeProvider::video("google", Behaviour::Ok);
        let other: Arc<dyn LlmProvider> = FakeProvider::video("other", Behaviour::Ok);
        let built = vec![google, other];
        let cooldown = CooldownManager::new();
        cooldown.mark_billing("google", Some("credit balance too low".into()));
        // google video-capable ma in cooldown -> vince other.
        let p = select_video_provider(&built, None, &cooldown).unwrap();
        assert_eq!(p.name(), "other");
    }

    #[test]
    fn select_video_provider_senza_capable_e_errore() {
        // Nessun provider video-capable -> errore esplicito (regola H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let built = vec![openai];
        let cooldown = CooldownManager::new();
        let Err(err) = select_video_provider(&built, None, &cooldown) else {
            panic!("senza provider video-capable deve fallire");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("nessun provider"));
    }

    #[test]
    fn select_video_provider_pin_non_capable_e_errore_non_fallback() {
        // Pin su openai (non video-capable) anche se google capable e' presente:
        // errore esplicito, niente ripiego su google (regola G/H).
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let google: Arc<dyn LlmProvider> = FakeProvider::video("google", Behaviour::Ok);
        let built = vec![openai, google];
        let cooldown = CooldownManager::new();
        let Err(err) = select_video_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non video-capable deve fallire");
        };
        assert!(err.message.contains("non supporta"));
    }

    #[test]
    fn select_video_provider_pin_non_configurato_e_errore() {
        let google: Arc<dyn LlmProvider> = FakeProvider::video("google", Behaviour::Ok);
        let built = vec![google];
        let cooldown = CooldownManager::new();
        let Err(err) = select_video_provider(&built, Some("openai"), &cooldown) else {
            panic!("pin su provider non configurato deve fallire");
        };
        assert!(err.message.contains("openai"));
    }

    #[tokio::test]
    async fn fake_provider_genera_video() {
        let google = FakeProvider::video("google", Behaviour::Ok);
        let req = VideoGenRequest {
            model: "veo-3.1-generate-001".into(),
            prompt: "un drone sul mare".into(),
            duration_seconds: Some(8),
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "video".into(),
            },
        };
        let out = google.generate_video(&req).await.unwrap();
        assert_eq!(out.provider_used, "google");
        assert_eq!(out.model_used, "veo-3.1-generate-001");
        assert_eq!(out.mime, "video/mp4");
        assert!(out.video_base64.is_some());
    }

    // ── Cap per-tentativo: la regressione che ha originato il fix ────────────
    //
    // Prima non esisteva NESSUN timeout logico nel gateway (`tokio::time::timeout`
    // non compariva nel crate): una chiamata appesa correva quanto il trasporto
    // concedeva (300s), cioe' quanto l'INTERO run che l'aveva chiesta, e il run
    // moriva con zero iterazioni completate.

    /// Un provider che non risponde MAI viene mollato al cap, con un segnale
    /// strutturato (regola M): classe + codice, mai testo da interpretare.
    #[tokio::test]
    async fn il_cap_per_tentativo_molla_un_provider_appeso() {
        let p = FakeProvider::new("slow", Behaviour::Hang);
        let cooldown = CooldownManager::new();
        let policy = cooldown.retry_policy();
        let cap = std::time::Duration::from_millis(50);

        // La rete di sicurezza del test e' ESTERNA alla funzione sotto esame: se
        // il cap sparisce, questo test FALLISCE in 5s invece di restare appeso
        // (un test che si blocca inchioda la CI e non dice cosa e' rotto).
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            complete_with_retry(p.as_ref(), &req(), "slow", &cooldown, &policy, true, cap),
        )
        .await
        .expect("cap per-tentativo NON applicato: la chiamata e' rimasta appesa");

        let Err(err) = res else {
            panic!("un provider appeso non puo' produrre un successo");
        };
        assert_eq!(err.class, "transient", "lento ORA, non rotto");
        assert_eq!(err.code.as_deref(), Some("attempt_timeout"));
    }

    /// Il provider appeso non deve bloccare la chain: il successivo, sano,
    /// risponde. Prima il primo elemento consumava tutto e non si arrivava mai
    /// al secondo.
    #[tokio::test]
    async fn un_provider_appeso_non_blocca_il_resto_della_chain() {
        let hang: Arc<dyn LlmProvider> = FakeProvider::new("slow", Behaviour::Hang);
        let ok: Arc<dyn LlmProvider> = FakeProvider::new("sano", Behaviour::Ok);
        let resolved = vec![
            ResolvedProvider {
                provider: hang,
                model: "m".into(),
            },
            ResolvedProvider {
                provider: ok,
                model: "m".into(),
            },
        ];
        let cooldown = CooldownManager::new();

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_fallback(
                &resolved,
                &cooldown,
                &req(),
                false,
                std::time::Duration::from_millis(50),
            ),
        )
        .await
        .expect("il provider appeso ha bloccato la chain: cap non applicato")
        .expect("la chain deve raggiungere il provider sano");
        assert_eq!(resp.provider_used, "sano");
    }
}
