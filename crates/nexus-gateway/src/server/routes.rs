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
use crate::cooldown::{CooldownManager, PortataCooldown, RetryPolicy};
use crate::history_sanitizer::{self, SanitizeMode};
use crate::model_alias_resolver::{strip_provider_prefix, ModelAliasResolver};
use crate::provider::LlmProvider;
use crate::providers::{ProviderErrorKind, ProviderHttpError};
use crate::tassonomia_errori::{CausaErrore, VerdettoErrore, VocabolarioErrori};
use crate::redaction::pipeline::{RedactionOptions, RedactionPipeline, RedactionResult};
use crate::redaction::sensitivity_classifier::SensitivityClassifier;
use crate::types::{
    CountTokensResponse, ImageGenRequest, ImageGenResponse, LlmMessage, LlmRequest, LlmResponse,
    RequestMetadata, TranscribeRequest, TranscribeResponse, TtsRequest, TtsResponse,
    VideoGenRequest, VideoGenResponse,
};

use super::billing::{
    enforce_quota, record_and_declare, record_discarded_attempts, record_media_usage_to_ledger,
    record_usage_to_ledger, MediaKind, MediaUsage, QuotaEstimate, QuotaExceeded, TentativoScartato,
};
use nexus_pricing::UsageUnit;
use nexus_types::error_presentation::{render_user_error, ErrorDomain, ErrorFacts};
// Vocabolario del blocco `details` (regola L): le classi e i nomi dei campi
// vivono nel crate da cui dipendono ENTRAMBI i lati del confine, cosi' un
// rename rompe la compilazione di chi legge invece di lasciare il trasporto
// muto con tutti i test verdi (regola O).
use nexus_types::provider_failure::{chiave, classe, portata};
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
///
/// `facts`: gli stessi segnali, nella forma che il PUNTO UNICO di presentazione
/// (`nexus_types::error_presentation`) sa tradurre in una frase. Nasce QUI e non
/// a valle perche' qui provider, modello, status e codice del provider esistono
/// ancora: dopo il confine HTTP resta solo `message`, che per contratto porta i
/// body grezzi ("mistral HTTP 429: {\"error\":{...}}") — ed e' il testo che
/// l'utente si e' visto in chat finche' nessuno ha reso leggibile l'errore alla
/// fonte.
#[derive(Debug)]
struct PipelineError {
    status: StatusCode,
    code: String,
    message: String,
    // Boxed: tiene la Err-variant dei Result della pipeline sotto la soglia
    // clippy `result_large_err` (il details e' raro, il Result e' ovunque).
    details: Option<Box<Value>>,
    // Boxed per la stessa ragione di `details`: ErrorFacts e' largo e questo
    // Result attraversa l'intera pipeline.
    facts: Box<ErrorFacts>,
}

/// I fatti di un errore APPLICATIVO del gateway (guardie, quota, risoluzione
/// provider): dominio Gateway, codice del gateway, e la frase scritta a mano dal
/// codice come `upstream_message`.
///
/// La distinzione con [`provider_facts_from_error`] non e' formale: li' il testo
/// e' il body di un fornitore esterno (mai una frase), qui e' una riga scritta da
/// noi per un umano, quindi puo' essere concatenata al messaggio.
fn gateway_facts(code: &str, message: &str) -> Box<ErrorFacts> {
    Box::new(
        ErrorFacts::opaque(ErrorDomain::Gateway, message)
            .with_code(code)
            .with_upstream(message),
    )
}

impl PipelineError {
    fn blocked(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: StatusCode::FORBIDDEN,
            code: "TIER_BLOCKED".to_string(),
            facts: gateway_facts("TIER_BLOCKED", &message),
            message,
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
        let message = message.into();
        Self {
            status: StatusCode::FORBIDDEN,
            code: "POLICY_TIER_EXCLUDED".to_string(),
            facts: gateway_facts("POLICY_TIER_EXCLUDED", &message),
            message,
            details: Some(Box::new(json!({
                "provider": provider,
                "detected_tier": detected_tier,
                "allowed_providers": allowed,
            }))),
        }
    }
    fn quota(scope: &str, reason: &str) -> Self {
        let message = format!("quota_exceeded:{scope}:{reason}");
        Self {
            status: StatusCode::FORBIDDEN,
            code: "QUOTA_EXCEEDED".to_string(),
            // `message` e' un identificatore macchina (`quota_exceeded:scope:reason`),
            // non una frase: come upstream verrebbe letto da un umano come gergo.
            // Il motivo leggibile e' il solo `reason`.
            facts: Box::new(
                ErrorFacts::opaque(ErrorDomain::Gateway, &message)
                    .with_code("QUOTA_EXCEEDED")
                    .with_upstream(reason),
            ),
            message,
            details: None,
        }
    }
    /// Errore applicativo del gateway sul percorso dei provider (quota check
    /// fallito, nessun provider capace, alias non risolvibile): il testo e'
    /// scritto da noi, non e' il body di un fornitore.
    fn provider(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROVIDER_ERROR".to_string(),
            facts: gateway_facts("PROVIDER_ERROR", &message),
            message,
            details: None,
        }
    }
    /// Variante di [`Self::provider`] col dettaglio strutturato dei fallimenti
    /// per-provider (`details.failures` + `details.primary_cause`) e i FATTI del
    /// fallimento primario, che sono l'unica cosa da cui puo' nascere una frase
    /// che nomini il fornitore e il motivo.
    fn provider_with_details(message: impl Into<String>, details: Value, facts: ErrorFacts) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROVIDER_ERROR".to_string(),
            message: message.into(),
            details: Some(Box::new(details)),
            facts: Box::new(facts),
        }
    }
    /// Fallimento di una chiamata a UN provider quando l'errore tipizzato e'
    /// ancora nella catena (media-gen, stream SSE): i fatti si estraggono dal
    /// [`ProviderHttpError`], non dal suo `Display`.
    fn provider_call_failed(
        vocabolario: &VocabolarioErrori,
        provider: &str,
        model: &str,
        err: &anyhow::Error,
    ) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "PROVIDER_ERROR".to_string(),
            message: err.to_string(),
            details: None,
            facts: Box::new(provider_facts_from_error(
                vocabolario,
                err,
                provider,
                Some(model),
            )),
        }
    }
    fn invalid_request(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_REQUEST".to_string(),
            facts: gateway_facts("INVALID_REQUEST", &message),
            message,
            details: None,
        }
    }
    /// Budget END-TO-END della richiesta esaurito (504). Il codice sul wire e'
    /// minuscolo per ragioni storiche: e' quello su cui il motore agentico
    /// decide gia' oggi, e cambiarlo romperebbe il failover.
    fn request_budget_exceeded() -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            code: "request_budget_exceeded".to_string(),
            message: "request budget exceeded".to_string(),
            details: None,
            facts: Box::new(
                ErrorFacts::opaque(ErrorDomain::Gateway, "request budget exceeded")
                    .with_code("request_budget_exceeded")
                    .with_status(StatusCode::GATEWAY_TIMEOUT.as_u16()),
            ),
        }
    }

    /// PUNTO UNICO (regola L) del corpo d'errore del gateway.
    ///
    /// `error`/`code`/`details` restano INVARIATI: li leggono gia' mcp-core
    /// (`nexus_gateway.rs`), l'adapter del motore e il neural client. Le tre
    /// chiavi additive portano la RESA: `user_message` e' la frase,
    /// `user_code` l'identificatore su cui il frontend sceglie icona e azione
    /// (mai il testo, regola M), `user_detail` il tecnico integrale.
    ///
    /// Si chiama `user_detail` e non `detail` perche' `details` esiste gia' su
    /// questo stesso oggetto con significato opposto: due chiavi a un carattere
    /// di distanza sono la trappola che qui ha gia' prodotto il bug dei costi a
    /// $0.00.
    fn to_body(&self) -> Value {
        let mut body = json!({ "error": self.message, "code": self.code });
        // Il NOME delle tre chiavi vive nel punto unico insieme al suo lettore
        // (`RenderedError::from_wire`, usato da mcp-core): scriverle qui a mano
        // le renderebbe rinominabili da un solo lato.
        render_user_error(&self.facts).write_into(&mut body);
        if let Some(details) = &self.details {
            body["details"] = (**details).clone();
        }
        body
    }
}

/// Corpo del 504 di budget esaurito: il punto unico ([`PipelineError::to_body`])
/// piu' le due chiavi TOP-LEVEL storiche che i chiamanti cercano gia'
/// (`primary_cause` per il failover del motore, `budget_seconds` per la
/// diagnosi). Funzione e non codice inline nel handler perche' il handler
/// richiede un `AppState` con DB: cosi' il test attraversa il produttore vero
/// invece di ricomporre il body a modo suo (regola O).
fn request_budget_exceeded_body(budget_seconds: u64) -> (StatusCode, Value) {
    let err = PipelineError::request_budget_exceeded();
    let mut body = err.to_body();
    body["primary_cause"] = json!("request_budget_exceeded");
    body["budget_seconds"] = json!(budget_seconds);
    (err.status, body)
}

/// I fatti STRUTTURATI di un fallimento provider a partire dall'errore anyhow
/// che lo trasporta (punto unico nel gateway, regola L+M): status e codice dal
/// [`ProviderHttpError`] della catena, classe dal classificatore, `error.message`
/// del provider come frase upstream, body integrale in `detail`.
fn provider_facts_from_error(
    vocabolario: &VocabolarioErrori,
    err: &anyhow::Error,
    provider: &str,
    model: Option<&str>,
) -> ErrorFacts {
    let http = err.chain().find_map(|c| c.downcast_ref::<ProviderHttpError>());
    // `verdetto_muto`: la RESA di un fallimento gia' classificato non deve
    // registrare una seconda volta il codice ignoto, o lo stesso errore
    // conterebbe due occorrenze - una per la decisione, una per il modo in cui
    // la si racconta.
    let class = vocabolario.verdetto_muto(err).classe.as_wire();
    ErrorFacts {
        domain: ErrorDomain::Provider,
        http_status: http.map(|h| h.status),
        code: http.and_then(|h| h.code.clone()),
        class: Some(class.to_string()),
        provider: Some(provider.to_string()),
        model: model.map(str::to_string),
        transport: None,
        // `error.message` del contratto d'errore del provider: e' l'unico pezzo
        // di testo che sia davvero una frase (dice COSA e' invalido). Il body
        // integrale resta in `detail`.
        upstream_message: http.and_then(|h| h.structured_message()),
        detail: err.to_string(),
    }
}

/// Il fallimento di UNA chiamata provider, col verdetto del catalogo dei codici.
///
/// Esiste per non ripetere in ogni ramo media-gen il percorso fino al
/// vocabolario: quattro `map_err` identici che nominano lo stesso campo dello
/// stesso stato sono la stessa riga scritta quattro volte.
fn errore_provider(
    state: &AppState,
    provider: &str,
    model: &str,
    e: &anyhow::Error,
) -> PipelineError {
    PipelineError::provider_call_failed(&state.vocabolario_errori, provider, model, e)
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
        (self.status, Json(self.to_body())).into_response()
    }
}

/// Riferimento a un provider concreto piu' il modello reale gia' risolto per la
/// richiesta corrente.
struct ResolvedProvider {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

/// Margine sottratto al budget per la DEADLINE INTERNA della pipeline: la
/// chain deve arrendersi PRIMA che il wrapper esterno (`timeout(budget, ...)`)
/// spari il 504 anonimo, cosi' il chiamante riceve l'ULTIMO errore STRUTTURATO
/// del provider (classe/status/codice) e puo' fare failover mirato (regola M).
/// Non e' configurazione di comportamento: e' il tempo di risposta/serializzazione
/// riservato alla pipeline stessa.
const BUDGET_RESPONSE_MARGIN: std::time::Duration = std::time::Duration::from_secs(2);

/// I timeout DI QUESTA richiesta: quelli per-processo, ri-derivati sul run che
/// il chiamante dichiara (`run_timeout_secs`).
///
/// I budget del gateway nascono dal run — `budget = run / min_turns` — ma erano
/// derivati una volta sola all'avvio dal default globale (300s), mentre il run
/// vero e' per figura: `review` ne ha 240. Il gateway prometteva quindi turni
/// che il cronometro del chiamante non poteva mantenere.
///
/// Il doppio `min` e' il vincolo: puo' solo STRINGERE. Un chiamante non deve
/// poter allungare i propri budget oltre quelli configurati dichiarando un run
/// lunghissimo — il tetto di trasporto e' comunque congelato nel client HTTP
/// all'avvio (`client_http_timeout`), e sforarlo trasformerebbe un
/// `attempt_timeout` strutturato in un errore di trasporto opaco (regola M).
///
/// L'UNICA eccezione allo stringere e' la richiesta DIFFERIBILE, e non e' un
/// allentamento del vincolo: e' l'altro ramo dello stesso criterio. I budget
/// ordinari nascono dalla domanda «quanti turni deve poter completare il run
/// che contiene questa chiamata», e una richiesta differibile non appartiene a
/// nessun run — un titolo di conversazione, un riassunto, una nota. I suoi
/// numeri sono propri (`gateway.flex.*`, mig 0729) e li risolve il punto unico
/// `LlmTimeouts::with_flex`, che li ha gia' limitati al tetto di trasporto.
///
/// Il RUN vince dove e' dichiarato: chi manda `run_timeout_secs` sta dicendo
/// che quella chiamata sta dentro un run vivo, e il differibile non puo'
/// allungargliela — sarebbe la scorciatoia con cui un chiamante ottiene budget
/// piu' larghi mettendo un flag.
fn request_timeouts(
    base: &LlmTimeouts,
    run_timeout_secs: Option<u64>,
    deferrable: bool,
) -> LlmTimeouts {
    let per_run = base.for_run(run_timeout_secs);
    let stretto = LlmTimeouts {
        request_budget: per_run.request_budget.min(base.request_budget),
        per_attempt: per_run.per_attempt.min(base.per_attempt),
        ..per_run
    };
    // `run_secs_utile` e' lo stesso criterio con cui `for_run` decide se
    // ri-derivare (regola L): «un run e' dichiarato» non puo' avere due
    // definizioni, o esisterebbe un valore — lo zero, che nel DB significa
    // "non impostato" — per cui una funzione ri-deriva e l'altra no.
    if deferrable && nexus_auth::llm_timeouts::run_secs_utile(run_timeout_secs).is_none() {
        return stretto.per_flex();
    }
    stretto
}

#[cfg(test)]
mod test_request_timeouts {
    use super::*;

    fn base() -> LlmTimeouts {
        // I valori LIVE: run 300 (default globale), complete 120, 4 turni.
        LlmTimeouts::derive(300, 120, 300, 4)
    }

    /// Come li chiedeva ogni chiamante prima che esistesse la corsia
    /// differibile: e' l'argomento che il default del contratto produce.
    fn ordinaria(base: &LlmTimeouts, run: Option<u64>) -> LlmTimeouts {
        request_timeouts(base, run, false)
    }

    /// Il caso che ha motivato il lavoro: la figura `review` vive 240s, non 300.
    /// I suoi turni valgono 60s, non 75.
    #[test]
    fn un_run_piu_corto_stringe_il_budget() {
        let t = ordinaria(&base(), Some(240));
        assert_eq!(t.request_budget, std::time::Duration::from_secs(60));
        assert!(
            t.request_budget.as_secs() * t.min_guaranteed_turns <= 240,
            "i turni promessi devono starci nel run vero"
        );
    }

    /// L'INVARIANTE del doppio `min`: un chiamante non deve poter allungare i
    /// budget del gateway dichiarando un run lunghissimo. Il tetto di trasporto
    /// e' congelato nel client HTTP all'avvio: sforarlo trasformerebbe un
    /// `attempt_timeout` strutturato in un errore di trasporto opaco (regola M).
    ///
    /// Mutazione che rende rosso: togliere i due `.min(base...)`.
    #[test]
    fn nessun_run_dichiarato_puo_allungare_i_budget() {
        let b = base();
        for run in [600_u64, 3_600, 86_400, u64::MAX] {
            let t = ordinaria(&b, Some(run));
            assert!(
                t.request_budget <= b.request_budget,
                "run {run}: il budget e' cresciuto ({:?} > {:?})",
                t.request_budget,
                b.request_budget
            );
            assert!(t.per_attempt <= b.per_attempt, "run {run}: per_attempt cresciuto");
        }
    }

    /// Chiamante che non dichiara nulla (client vecchio): comportamento storico.
    #[test]
    fn senza_dichiarazione_i_budget_sono_quelli_di_sempre() {
        let b = base();
        assert_eq!(ordinaria(&b, None).request_budget, b.request_budget);
        assert_eq!(ordinaria(&b, Some(0)).request_budget, b.request_budget);
    }

    /// Il meccanismo resta SPENTO finche' i budget differibili non sono
    /// dichiarati: `deferrable: true` su timeout senza `with_flex` non cambia
    /// un numero.
    /// E' lo stato in cui il gateway parte se le chiavi `gateway.flex.*` non
    /// esistono (regola G: nessun default nascosto nel codice del gateway).
    #[test]
    fn senza_budget_dichiarati_il_differibile_non_cambia_niente() {
        let b = base();
        assert_eq!(request_timeouts(&b, None, true), ordinaria(&b, None));
    }

    /// Il caso del lotto: nessun run dichiarato + `deferrable` -> i budget sono
    /// quelli differibili, non quelli ordinari. Il `per_attempt` resta tagliato
    /// al tetto di trasporto (300s coi valori LIVE): e' il punto unico
    /// `with_flex` a deciderlo, non questa funzione.
    ///
    /// MUTAZIONE: togliere il ramo `per_flex()` -> il budget torna 75s, rosso.
    #[test]
    fn una_richiesta_differibile_ottiene_i_propri_budget() {
        let b = base().with_flex(900, 900);
        let t = request_timeouts(&b, None, true);
        assert_eq!(t.request_budget, std::time::Duration::from_secs(900));
        assert_eq!(t.per_attempt, b.client_http_timeout());
        // La stessa base senza il flag resta quella di ieri: il differibile non
        // e' un innalzamento globale.
        assert_eq!(ordinaria(&b, None).request_budget, b.request_budget);
    }

    /// IL vincolo che impedisce la scorciatoia: chi dichiara un run sta dentro
    /// un run vivo, e un flag non puo' allungarglielo. Senza, `deferrable: true`
    /// sarebbe il modo di ottenere 900s ovunque.
    ///
    /// MUTAZIONE: togliere la condizione `run_secs_utile(...).is_none()` -> il
    /// budget di un run da 240s diventa 900, rosso.
    #[test]
    fn il_run_dichiarato_vince_sul_differibile() {
        let b = base().with_flex(900, 900);
        for run in [60_u64, 240, 300, 600] {
            let t = request_timeouts(&b, Some(run), true);
            assert_eq!(
                t.request_budget,
                ordinaria(&b, Some(run)).request_budget,
                "run {run}: il differibile ha scavalcato il run dichiarato"
            );
        }
        // Zero nel DB significa "non impostato": e' un run NON dichiarato, e li'
        // il differibile vale. Stessa lettura di `for_run` (punto unico).
        assert_eq!(
            request_timeouts(&b, Some(0), true).request_budget,
            std::time::Duration::from_secs(900)
        );
    }
}

/// Esegue la pipeline completa di completion (classify -> route -> fallback ->
/// rehydrate -> ledger). Ritorna la risposta o un errore tradotto in HTTP.
///
/// `timeouts` arriva dal chiamante GIA' risolto e non viene riletto qui: prima
/// c'erano due `runtime_snapshot()` distinti, uno per il wrapper esterno e uno
/// per la deadline interna. Ri-derivandoli separatamente sul run della richiesta
/// avrebbero potuto divergere, e il wrapper esterno sarebbe scaduto PRIMA della
/// deadline interna: la pipeline verrebbe troncata senza mai produrre l'errore
/// strutturato su cui il motore fa failover, e il chiamante riceverebbe il 504
/// anonimo -- esattamente il difetto che `BUDGET_RESPONSE_MARGIN` esiste per
/// evitare. Un solo calcolo, passato per parametro.
async fn run_complete(
    state: &AppState,
    req: &LlmRequest,
    timeouts: LlmTimeouts,
) -> Result<LlmResponse, PipelineError> {
    validate_logical_model(&req.model)?;
    let runtime = state.runtime_snapshot().await;
    // DEADLINE della richiesta (incidente figure 2026-07-14): le ATTESE della
    // chain (cooldown-in-testa, backoff, Retry-After) non erano confrontate col
    // budget e potevano dormire OLTRE la deadline esterna: il chiamante moriva
    // di timeout senza mai ricevere un errore strutturato su cui failovare.
    let deadline = tokio::time::Instant::now()
        + timeouts
            .request_budget
            .saturating_sub(BUDGET_RESPONSE_MARGIN)
            .max(std::time::Duration::from_secs(1));

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
    // La redazione passa dal punto unico condiviso col conteggio token
    // (`redigi_richiesta`): le opzioni le decide il tier effettivo, e due copie
    // divergerebbero al primo ritocco della soglia (regola L).
    let (pipeline, redaction) = redigi_richiesta(state, &runtime, req, effective_tier).await?;
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
    enforce_quota(
        &state.db,
        req,
        preview.provider.name(),
        &preview.model,
        QuotaEstimate::Testuale,
    )
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
    // Tentativi CONSUMATI e scartati lungo la chain (degeneri, cap scaduti):
    // si registrano nel ledger QUALUNQUE sia l'esito finale, perche' sono
    // spesa a prescindere — per questo l'errore si propaga solo DOPO la
    // scrittura (mig 0701).
    let mut scarti: Vec<TentativoScartato> = Vec::new();
    let esito_chain = run_fallback(
        &resolved,
        &state.cooldown,
        &state.vocabolario_errori,
        &redacted_req,
        strict,
        timeouts.per_attempt,
        deadline,
        &mut scarti,
    )
    .await;
    record_discarded_attempts(&state.db, req, &scarti).await;
    let mut response = esito_chain?;

    // Reidratazione post-flight: ripristina gli originali nei placeholder.
    response = pipeline.rehydrate(&response, &mut map);

    // Ledger best-effort (non blocca la risposta). Cio' che si e' fatto della
    // contabilita' viene DICHIARATO sulla risposta: non e' telemetria, e' il
    // segnale su cui il chiamante decide di non addebitare una seconda volta la
    // stessa chiamata (regola M). Prima il gateway scriveva la sua riga in
    // silenzio e mcp-core, che non poteva saperlo, ne finalizzava una propria:
    // due righe finalizzate e costo raddoppiato per una sola chiamata.
    //
    // Scrittura e dichiarazione sono UNA chiamata sola (regola L): erano due
    // righe qui dentro, dove nessun test poteva arrivare, e questa era la piu'
    // importante delle due.
    record_and_declare(&state.db, req, &mut response).await;

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
    // Un nome fisico NUDO non dice a chi appartenga (vedi `Attribuzione`): vale
    // per il primo provider utilizzabile della catena — quello che il routing ha
    // scelto — e non per i successivi. `resolve` da solo non puo' saperlo,
    // perche' vede un provider per volta e per un nome nudo risponde "usalo
    // as-is": corretto quando il chiamante ha pinnato il fornitore, sbagliato
    // quando dietro c'e' una cascata. Propagarlo ha mandato `open-mistral-nemo`
    // a DeepSeek (HTTP 400 "you passed open-mistral-nemo", misurato il
    // 30/07/2026): il gateway l'ha classificato irrimediabile e non ha ritentato,
    // ma la richiesta era gia' persa e il chiamante ha ricevuto 500.
    let nome_nudo = matches!(
        aliases.attribuzione(logical_model),
        crate::model_alias_resolver::Attribuzione::Nuda
    );
    for name in names {
        let Some(provider) = built.iter().find(|p| p.name() == name) else {
            // Provider deciso dalla policy ma non costruito (es. chiave mancante).
            continue;
        };
        if nome_nudo && !out.is_empty() {
            tracing::debug!(
                provider = %name, model = %logical_model,
                "gateway: nome fisico non attribuibile, non propagato oltre il primo provider"
            );
            continue;
        }
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
        // Strip del prefisso SOLO se e' davvero il provider di destinazione: la
        // slash in `openai/gpt-oss-120b` (groq) o `z-ai/glm-5.2` (openrouter)
        // e' parte del NOME, non un separatore nostro.
        model: strip_provider_prefix(logical_model, pin),
    })
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
#[allow(clippy::too_many_arguments)]
async fn run_fallback(
    resolved: &[ResolvedProvider],
    cooldown: &CooldownManager,
    vocabolario: &VocabolarioErrori,
    base_req: &LlmRequest,
    strict: bool,
    per_attempt: std::time::Duration,
    deadline: tokio::time::Instant,
    scarti: &mut Vec<TentativoScartato>,
) -> Result<LlmResponse, PipelineError> {
    let mut failures: Vec<ProviderFailure> = Vec::new();
    let policy = cooldown.retry_policy();

    for rp in resolved {
        let name = rp.provider.name();

        // La domanda e' sulla COPPIA: qui il modello risolto lo conosciamo, e un
        // tetto raggiunto su un altro modello di questo fornitore non dice nulla
        // su quello che stiamo per chiamare (difetto misurato il 13/08/2026).
        if cooldown.is_model_in_cooldown(name, &rp.model) {
            let secs = cooldown.seconds_remaining_for_model(name, &rp.model);
            if cooldown.is_billing_cooldown(name) {
                // Billing: il provider e' inutilizzabile finche' non si ricarica.
                // Il messaggio lo segnala cosi' il chiamante applica il cooldown
                // lungo invece di riprovare a ogni iterazione.
                failures.push(ProviderFailure {
                    provider: name.to_string(),
                    model: Some(rp.model.clone()),
                    class: classe::COOLDOWN_BILLING,
                    status: None,
                    code: None,
                    message: format!("cooldown billing, {secs}s rimanenti"),
                    upstream: None,
                    retry_after_seconds: attesa_dichiarabile(secs),
                    // Il credito e' dell'account: fuori e' il fornitore, non la
                    // coppia (e a valle `Credito` non guarda la portata).
                    cooldown_scope: Some(portata::PROVIDER),
                });
                continue;
            }
            // Cooldown transitorio BREVE + strict pin (nessuno swap): attendi il
            // residuo e ritenta lo stesso modello, invece del hard-fail storico
            // (causa dell'errore "tutti i provider hanno fallito -> google
            // (cooldown 21s)"). Oltre il tetto, propaga. NB: `secs` e' troncato a
            // interi, quindi un residuo sub-secondo vale 0: attendere 0s = procedi
            // subito (il cooldown e' di fatto scaduto), non e' un caso di hard-fail.
            // L'attesa deve STARE nel budget della richiesta (incidente figure
            // 2026-07-14): dormire oltre la deadline consegna al chiamante un 504
            // anonimo (o un timeout client) invece del fallimento strutturato su
            // cui failovare subito.
            let residual = deadline.saturating_duration_since(tokio::time::Instant::now());
            if strict
                && secs <= policy.wait_short_cooldown_cap_s
                && std::time::Duration::from_secs(secs as u64) <= residual
            {
                tracing::info!(
                    provider = name,
                    wait_s = secs,
                    "gateway: attendo cooldown transitorio breve prima di ritentare (strict pin)"
                );
                tokio::time::sleep(std::time::Duration::from_secs(secs as u64)).await;
            } else {
                failures.push(ProviderFailure {
                    provider: name.to_string(),
                    model: Some(rp.model.clone()),
                    class: classe::COOLDOWN,
                    status: None,
                    code: None,
                    message: format!("in cooldown, {secs}s rimanenti"),
                    upstream: None,
                    retry_after_seconds: attesa_dichiarabile(secs),
                    // Chiediamo al registro CHI e' escluso: il fornitore, o solo
                    // questa coppia. Dedurlo dal fatto che il modello sia
                    // valorizzato darebbe sempre "modello", che e' il difetto
                    // simmetrico a quello che stiamo chiudendo.
                    cooldown_scope: cooldown
                        .portata_attiva(name, &rp.model)
                        .map(PortataCooldown::wire),
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
            vocabolario,
            &policy,
            strict,
            per_attempt,
            deadline,
            scarti,
        )
        .await
        {
            Ok(resp) => {
                // Successo reale: se questa coppia (o il fornitore) era in
                // cooldown transitorio, liberala subito — ha appena risposto 200.
                cooldown.clear_model(name, &rp.model);
                return Ok(resp);
            }
            // Il RESIDUO si legge dal registro che lo ha appena scritto, non si
            // ricalcola dal ramo che ha fallito: qualunque strada abbia marcato
            // il cooldown (billing, transient dopo l'ultimo retry, Retry-After
            // onorato), il numero descrive lo stato vero adesso. Un ramo che
            // domani marcasse in un modo nuovo lo dichiarerebbe da se'.
            Err(f) => {
                let attesa = cooldown
                    .is_model_in_cooldown(name, &rp.model)
                    .then(|| cooldown.seconds_remaining_for_model(name, &rp.model))
                    .and_then(attesa_dichiarabile);
                // Anche la PORTATA si legge dal registro che ha appena scritto,
                // per la stessa ragione del residuo: qualunque ramo abbia messo
                // il cooldown, qui si dichiara cio' che vale adesso.
                let portata = attesa
                    .and_then(|_| cooldown.portata_attiva(name, &rp.model))
                    .map(PortataCooldown::wire);
                failures.push(f.into_provider_failure(name, &rp.model, attesa, portata))
            }
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
    // Lo STATUS verso il chiamante riflette la classe (regola M al confine):
    // se OGNI provider ha rifiutato con un errore client deterministico, la
    // stessa richiesta non andra' mai bene e ritentarla e' inutile -> 400.
    // Prima era sempre 500: un 400 del provider usciva dal gateway come
    // "errore server, riprovabile", e i chiamanti fuori dal motore agentico
    // (worker, wiki, probe: decidono su `is_success`/5xx, non leggono
    // `details`) erano autorizzati a insistere su richieste gia' condannate
    // (misurato il 20/07: burst di 400 mistral rilanciati come 500).
    // Il motore agentico non cambia: decide su `code` + `primary_cause`
    // (classify_gateway_error), non sullo status. Ogni altra composizione
    // (transiente, cooldown, billing, mista) resta 500: ritentare puo' avere
    // senso, e il contratto coi client esistenti non si muove.
    let all_deterministic =
        !failures.is_empty() && failures.iter().all(|f| f.class == classe::CLIENT_ERROR);
    // La FRASE nasce dal PRIMO fallimento, con dominio Provider: e' l'unico
    // punto in cui provider, modello, status e codice esistono ancora. A valle
    // resterebbe solo l'aggregato "tutti i provider hanno fallito -> mistral
    // (mistral HTTP 429: {...})", da cui nessuno puo' piu' ricavare CHI ha
    // fallito e PERCHE' senza una regex sulla prosa (regola M).
    let message = format!("tutti i provider hanno fallito -> {human}");
    let facts = match failures.first() {
        Some(f) => f.facts(&message),
        // Chain vuota (nessun provider risolto): niente da nominare.
        None => ErrorFacts::opaque(ErrorDomain::Gateway, &message).with_code("PROVIDER_ERROR"),
    };
    if all_deterministic {
        return Err(PipelineError {
            status: StatusCode::BAD_REQUEST,
            code: "PROVIDER_ERROR".to_string(),
            message,
            details: Some(Box::new(details)),
            facts: Box::new(facts),
        });
    }
    Err(PipelineError::provider_with_details(message, details, facts))
}

/// Il residuo di un cooldown in forma DICHIARABILE al chiamante.
///
/// `seconds_remaining` tronca agli interi, quindi un residuo sub-secondo vale
/// `0` — e `0` non e' un'attesa: e' un cooldown di fatto scaduto. Dichiararlo
/// come attesa farebbe registrare al consumatore un'esclusione che non esiste.
/// I negativi (residuo gia' passato) cadono nello stesso caso.
fn attesa_dichiarabile(secs: i64) -> Option<u64> {
    u64::try_from(secs).ok().filter(|s| *s > 0)
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
    /// Modello REALE tentato su questo provider (l'alias e' gia' risolto). Senza,
    /// il messaggio puo' dire "mistral ha rifiutato" ma non QUALE modello, che e'
    /// la prima cosa da cambiare quando un modello viene deprecato.
    model: Option<String>,
    class: &'static str,
    status: Option<u16>,
    code: Option<String>,
    message: String,
    /// `error.message` del provider, quando il body lo espone: una frase, non il
    /// body. Alimenta la resa; NON entra in `to_json` (i `details` restano il
    /// canale delle decisioni macchina, regola M).
    upstream: Option<String>,
    /// Secondi che restano da attendere su questo fornitore, quando il registro
    /// dei cooldown ne ha uno attivo.
    ///
    /// E' un CAMPO e non un numero dentro `message` (regola Q): il residuo era
    /// gia' noto e viaggiava dentro `"in cooldown, {secs}s rimanenti"`, dove
    /// l'unico modo di leggerlo era una regex sulla prosa — cioe' esattamente
    /// cio' che la regola M vieta a chi decide. Il consumatore e' mcp-core, che
    /// da qui allinea il PROPRIO registro e smette di convocare un fornitore
    /// che questo gateway rifiutera' comunque.
    ///
    /// `None` = nessun cooldown attivo su quel nome, oppure residuo non
    /// dichiarabile. Non e' «zero»: chi legge non deve poter confondere «non
    /// c'e' attesa» con «l'attesa non e' stata misurata».
    retry_after_seconds: Option<u64>,
    /// CHI e' escluso per quei secondi: `provider` o `model` (vocabolario
    /// [`portata`]). `None` = nessuna attesa da dichiarare.
    ///
    /// Distinto da [`Self::model`], che dice quale modello si stava per
    /// chiamare ed e' valorizzato anche quando fuori e' l'intero fornitore:
    /// senza questo campo, mcp-core dovrebbe indovinare la portata da un campo
    /// che risponde a un'altra domanda.
    cooldown_scope: Option<&'static str>,
}

impl ProviderFailure {
    fn to_json(&self) -> Value {
        json!({
            chiave::PROVIDER: self.provider,
            chiave::MODELLO: self.model,
            chiave::CLASSE: self.class,
            "status": self.status,
            "code": self.code,
            "message": self.message,
            chiave::ATTESA_S: self.retry_after_seconds,
            chiave::PORTATA: self.cooldown_scope,
        })
    }

    /// I fatti da cui nasce la frase per l'utente. `detail` arriva da fuori: e'
    /// il testo tecnico AGGREGATO di tutta la cascata, non solo di questo
    /// fallimento — chi debugga vuole sapere anche cosa hanno fatto gli altri.
    fn facts(&self, detail: &str) -> ErrorFacts {
        let mut facts = ErrorFacts {
            domain: ErrorDomain::Provider,
            http_status: self.status,
            code: self.code.clone(),
            class: Some(self.class.to_string()),
            provider: Some(self.provider.clone()),
            model: self.model.clone(),
            transport: None,
            upstream_message: None,
            detail: detail.to_string(),
        };
        if let Some(u) = &self.upstream {
            facts = facts.with_upstream(u.clone());
        }
        facts
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
    /// `error.message` del body del provider (`structured_message`). Esisteva
    /// gia' come segnale, ma finiva in UN solo campo di tracing: la frase che
    /// dice COSA e' invalido moriva nei log mentre all'utente arrivava il body
    /// grezzo. Ora viaggia fino alla resa.
    upstream: Option<String>,
}

impl CallFailure {
    fn from_error(kind: ProviderErrorKind, err: &anyhow::Error) -> Self {
        let http = err.chain().find_map(|c| c.downcast_ref::<ProviderHttpError>());
        Self {
            // UNA sola traduzione classe -> wire, in `nexus-types` accanto al
            // vocabolario che mcp-core legge: questo `match` ne era la seconda
            // copia, a 700 righe da quella di `provider_facts_from_error`.
            class: kind.as_wire(),
            status: http.map(|h| h.status),
            code: http.and_then(|h| h.code.clone()),
            message: err.to_string(),
            upstream: http.and_then(|h| h.structured_message()),
        }
    }

    /// Fallimento per risposta DEGENERE (HTTP 200 senza output utile, regola M).
    /// Non e' colpa di salute/credito del provider (nessun cooldown billing o
    /// transient da marcare): e' un turno improduttivo che deve far proseguire la
    /// chain al provider successivo. `status`/`code` restano `None` (non c'e' un
    /// errore HTTP), la classe strutturata `empty_completion` porta l'informazione.
    fn empty_completion(finish_reason: &str) -> Self {
        Self {
            class: classe::EMPTY_COMPLETION,
            status: None,
            code: None,
            message: format!(
                "risposta degenere: nessun testo ne' tool-call (finish_reason={finish_reason})"
            ),
            upstream: None,
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
            class: classe::TRANSIENT,
            status: None,
            code: Some("attempt_timeout".to_string()),
            message: format!(
                "nessuna risposta entro il cap per-tentativo ({}s)",
                per_attempt.as_secs()
            ),
            upstream: None,
        }
    }

    fn into_provider_failure(
        self,
        provider: &str,
        model: &str,
        retry_after_seconds: Option<u64>,
        cooldown_scope: Option<&'static str>,
    ) -> ProviderFailure {
        ProviderFailure {
            provider: provider.to_string(),
            model: Some(model.to_string()),
            class: self.class,
            status: self.status,
            code: self.code,
            message: self.message,
            upstream: self.upstream,
            retry_after_seconds,
            cooldown_scope,
        }
    }
}

/// Chiama `provider.complete` con retry sullo STESSO modello per errori
/// transitori (Fase B1, strict pin). Classifica l'errore col punto unico
/// [`classify_provider_error`] (regola L):
///   - Billing   -> `mark_billing`, niente retry, errore (ricarica necessaria);
///   - ClientError history-related -> 1 retry con sanificazione aggressiva, ma
///     SOLO se quella sanificazione cambia davvero la richiesta;
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
#[allow(clippy::too_many_arguments)]
async fn complete_with_retry(
    provider: &dyn LlmProvider,
    req: &LlmRequest,
    name: &str,
    cooldown: &CooldownManager,
    vocabolario: &VocabolarioErrori,
    policy: &RetryPolicy,
    strict: bool,
    per_attempt: std::time::Duration,
    deadline: tokio::time::Instant,
    scarti: &mut Vec<TentativoScartato>,
) -> Result<LlmResponse, CallFailure> {
    let max_attempts = if strict { policy.max_attempts.max(1) } else { 1 };
    let mut attempt = 0u32;
    // "Il retry aggressivo e' ANCORA DISPONIBILE", non "lo sto facendo": si parte
    // in Standard e si passa ad Aggressive solo dopo un client_error di formato.
    // Il nome precedente (`history_aggressive_retry`) diceva il contrario di cio'
    // che il valore significa, e faceva leggere il ramo come invertito.
    let mut retry_aggressivo_disponibile = true;
    loop {
        attempt += 1;
        // Cap EFFETTIVO del tentativo = min(cap per-tentativo, budget residuo
        // della richiesta): un tentativo che non puo' completarsi entro la
        // deadline non va nemmeno avviato — meglio l'errore strutturato subito
        // (il chiamante failova) che il 504 anonimo del wrapper esterno.
        let residual = deadline.saturating_duration_since(tokio::time::Instant::now());
        if residual < std::time::Duration::from_millis(500) {
            // Budget esaurito senza aver potuto tentare: NESSUN cooldown (il
            // provider non ha colpe), solo il fallimento strutturato.
            return Err(CallFailure::attempt_timeout(residual));
        }
        let attempt_cap = per_attempt.min(residual);
        let sanitize_mode = if retry_aggressivo_disponibile {
            SanitizeMode::Standard
        } else {
            SanitizeMode::Aggressive
        };
        let mut call_req = req.clone();
        let (messages, sanitize_report) =
            history_sanitizer::sanitized_for_attempt(&req.messages, name, sanitize_mode);
        call_req.messages = messages;
        if sanitize_report != history_sanitizer::SanitizeReport::default() {
            // A `info`, non `debug`: quando un 400 di formato arriva in chat,
            // questa e' la sola riga che dice cosa e' stato tolto dalla history
            // e in quale modalita'. A `debug` non compariva nei log di esercizio,
            // e "il sanitizer non ha toccato niente" era indistinguibile da "ha
            // tolto un campo obbligatorio e non lo vediamo" (regola O).
            tracing::info!(
                provider = name,
                mode = ?sanitize_mode,
                attempt,
                stripped_reasoning = sanitize_report.stripped_reasoning,
                stripped_thinking_signature = sanitize_report.stripped_thinking_signature,
                stripped_thought_signature = sanitize_report.stripped_thought_signature,
                stripped_trailing = sanitize_report.stripped_trailing_assistant,
                orphan_tools = sanitize_report.removed_orphan_tool_results,
                synthetic_tools = sanitize_report.injected_synthetic_tool_results,
                "gateway: history sanificata per dialetto provider"
            );
        }

        let call = match tokio::time::timeout(attempt_cap, provider.complete(&call_req)).await {
            Ok(r) => r,
            Err(_) => {
                // Il cap e' scaduto: nessuna risposta da classificare. Il
                // provider e' lento ORA -> cooldown breve (come gli altri
                // transient) e la chain prova il prossimo dentro il budget.
                tracing::warn!(
                    provider = name,
                    attempt_cap_s = attempt_cap.as_secs(),
                    attempt,
                    "gateway: cap per-tentativo scaduto -> transient, passo oltre"
                );
                // Tentativo AVVIATO e mai risposto: il provider puo' aver
                // generato (e fatturato) comunque. Nessun usage osservato ->
                // riga a zero con la causa dichiarata (mig 0701). Il budget
                // esaurito PRIMA di tentare (sopra) invece non scarta nulla:
                // nessuna chiamata e' partita.
                scarti.push(TentativoScartato::timeout(name, &req.model));
                let failure = CallFailure::attempt_timeout(attempt_cap);
                // Nessuna risposta affatto: e' l'endpoint a non aver risposto,
                // quindi la portata e' il fornitore (nessun tetto e' stato
                // dichiarato da nessuno).
                cooldown.mark_transient(name, None, Some(failure.message.clone()));
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
                    // L'inference e' avvenuta e il provider l'ha fatturata:
                    // l'usage REALE dal wire va nel ledger come 'discarded'
                    // (mig 0701), o questa spesa non la vede nessuna query.
                    scarti.push(TentativoScartato::degenere(name, &resp.model_used, resp.usage));
                    return Err(CallFailure::empty_completion(&resp.finish_reason));
                }
                return Ok(resp);
            }
            Err(err) => {
                // Classificazione STRUTTURALE (regole H+M): decide il primo
                // candidato RICONOSCIUTO dal catalogo dei codici (mig 0705), non
                // il primo campo presente confrontato per sottostringa. Il testo
                // del messaggio serve solo per log/display.
                //
                // E' `verdetto` e non `verdetto_muto`: qui l'errore viene
                // classificato per DECIDERE, ed e' l'unico punto in cui un
                // codice mai visto puo' essere scoperto - il log non basta,
                // MISURATO: `code=` esce solo dal ramo ClientError e delle 4439
                // chiamate sbagliate non e' rimasta una riga.
                let verdetto: VerdettoErrore = vocabolario.verdetto(&err);
                let kind = verdetto.classe;
                let http = err
                    .chain()
                    .find_map(|c| c.downcast_ref::<ProviderHttpError>());
                // `Retry-After` autoritativo dal provider (RFC 9457/7231), se c'e'.
                let retry_after = http.and_then(|h| h.retry_after_seconds);
                let failure = CallFailure::from_error(kind, &err);
                let msg = failure.message.clone();
                // GUARDIA sulla CAUSA, prima della classe: vedi
                // `corsia_differibile_esaurita` per il perche'.
                if corsia_differibile_esaurita(name, verdetto.causa, &failure) {
                    return Err(failure);
                }
                match kind {
                    ProviderErrorKind::Billing => {
                        cooldown.mark_billing(name, Some(msg));
                        return Err(failure);
                    }
                    ProviderErrorKind::ClientError => {
                        let code = failure.code.as_deref();
                        let status = failure.status.unwrap_or(0);
                        if history_sanitizer::is_invalid_model_error(verdetto.causa, status) {
                            tracing::warn!(
                                provider = name,
                                status = failure.status,
                                code = code,
                                "gateway: modello invalido/deprecato (client_error, niente cooldown provider)"
                            );
                            return Err(failure);
                        }
                        if retry_aggressivo_disponibile
                            && history_sanitizer::is_history_related_client_error(verdetto.causa)
                        {
                            retry_aggressivo_disponibile = false;
                            if !history_sanitizer::retry_changes_history(
                                &req.messages,
                                &call_req.messages,
                                name,
                                SanitizeMode::Aggressive,
                            ) {
                                tracing::warn!(
                                    provider = name,
                                    status = failure.status,
                                    code = code,
                                    dettaglio = %failure.message,
                                    retry_saltato = "sanificazione_aggressiva_senza_effetto",
                                    "gateway: client_error history, ma la sanificazione \
                                     aggressiva lascia la richiesta IDENTICA -> niente retry \
                                     (stessa richiesta, stesso rifiuto)"
                                );
                                return Err(failure);
                            }
                            tracing::warn!(
                                provider = name,
                                status = failure.status,
                                code = code,
                                dettaglio = %failure.message,
                                "gateway: client_error history -> sanificazione aggressiva e retry"
                            );
                            continue;
                        }
                        tracing::warn!(
                            provider = name,
                            status = failure.status,
                            code = code,
                            // Il `code` dei provider OpenAI-compat e' spesso il generico
                            // `invalid_request_error`: cosa sia davvero invalido sta solo
                            // nel messaggio. Senza, diagnosticare un 400 e' congetturare.
                            // Resta display/diagnosi: le decisioni leggono status e code.
                            dettaglio = %failure.message,
                            "gateway: il provider ha rifiutato la richiesta \
                             (errore client, niente retry/cooldown)"
                        );
                        return Err(failure);
                    }
                    // Due cause, una conseguenza: la richiesta non sta in QUESTO
                    // fornitore — per finestra (413) o per capienza del credito
                    // residuo (402 di ammissione, misurato il 13/08/2026 con
                    // 62.186 token disponibili). In entrambi i casi il fornitore
                    // e' sano: niente retry (la stessa richiesta prende lo stesso
                    // rifiuto), niente cooldown, e il motore ripiega
                    // cross-provider perche' la causa non e' `ClientError`.
                    // QUALE delle due sia lo dice il campo `class`, non due
                    // prose diverse (regola Q).
                    ProviderErrorKind::ContextTooLong
                    | ProviderErrorKind::RequestExceedsCredit => {
                        tracing::warn!(
                            provider = name,
                            status = failure.status,
                            class = failure.class,
                            "gateway: richiesta non accettata da questo provider -> niente \
                             retry/cooldown, il motore fara' failover cross-provider"
                        );
                        return Err(failure);
                    }
                    ProviderErrorKind::Transient => {
                        let cap_s = policy.wait_short_cooldown_cap_s.max(0) as u64;
                        // Onora `Retry-After` (autoritativo) se presente e sotto il
                        // tetto; altrimenti backoff esponenziale+jitter calcolato.
                        let delay = match retry_after {
                            Some(s) => s.saturating_mul(1000),
                            None => policy.backoff_ms(attempt, jitter_seed()),
                        };
                        let residual =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        // Arrenditi se: tentativi esauriti, il provider chiede
                        // un'attesa oltre il tetto, o l'attesa NON sta nel budget
                        // residuo della richiesta (incidente figure 2026-07-14:
                        // dormire oltre la deadline nega al chiamante l'errore
                        // strutturato su cui failovare, e il client muore di
                        // timeout mentre il gateway lavora per nessuno).
                        if attempt >= max_attempts
                            || retry_after.is_some_and(|s| s > cap_s)
                            || std::time::Duration::from_millis(delay) > residual
                        {
                            // Il cooldown onora il `Retry-After` del provider: se ha
                            // detto quando tornera' a servire, ripresentarsi prima
                            // significa riprendere lo stesso errore (regola M).
                            marca_transitorio(cooldown, name, &req.model, &failure, msg, retry_after);
                            return Err(failure);
                        }
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

/// La chiamata si ferma qui perche' la corsia DIFFERIBILE non ha capacita'?
///
/// Guarda la CAUSA e non la classe (regole M+Q). Il tier differibile senza
/// capacita' arriva come 429 — la classe di un tetto di frequenza — e i due
/// rimedi si escludono: attendere e ritentare spende il tempo che il
/// differibile aveva (il fornitore chiede 300s, misurato), e un cooldown
/// toglierebbe dalla selezione un fornitore SANO, che al tier standard avrebbe
/// servito subito. Il rimedio, lo scrive il fornitore stesso nel messaggio, e'
/// cambiare corsia.
///
/// Nel caso normale non si arriva qui: il driver openai consuma quel rifiuto da
/// se', rimandando la richiesta senza il campo. Questa e' la difesa per cio'
/// che il driver non governa — un `service_tier` PINNATO dal chiamante, che non
/// scavalca di proposito, e un endpoint compat che lo emetta per conto suo.
///
/// Vale la causa e non la classe perche' il vocabolario di wire e' grossolano
/// (`transient`) e questa distinzione la usa il solo gateway: allargare
/// `ClasseErrore` per un caso che mcp-core non consuma sarebbe attrito senza
/// consumatori.
fn corsia_differibile_esaurita(
    provider: &str,
    causa: Option<CausaErrore>,
    failure: &CallFailure,
) -> bool {
    if causa != Some(CausaErrore::FlexCapacity) {
        return false;
    }
    tracing::warn!(
        provider,
        status = failure.status,
        class = failure.class,
        "gateway: la corsia differibile non ha capacita' -> niente retry/cooldown, \
         il motore fara' failover cross-provider"
    );
    true
}

/// Registra il cooldown di un fallimento transitorio i cui retry sono esauriti,
/// con la PORTATA che la causa dichiara.
///
/// Vive qui e non dentro il ciclo perche' li' si e' gia' a sei livelli di
/// indentazione, e perche' e' una decisione con un nome: un tetto di frequenza
/// e' del MODELLO — groq risponde «Rate limit reached for model
/// `openai/gpt-oss-20b` ... TPD Limit 200000» — mentre un guasto di trasporto e'
/// del fornitore. Escludere il fornitore intero per il tetto di un suo modello
/// toglie dalla selezione anche quelli che hanno quota propria (13/08/2026).
fn marca_transitorio(
    cooldown: &CooldownManager,
    provider: &str,
    model: &str,
    failure: &CallFailure,
    msg: String,
    retry_after: Option<u64>,
) {
    let escluso =
        PortataCooldown::da_segnale(failure.status, failure.code.as_deref()).modello(model);
    cooldown.mark_transient_after(provider, escluso, Some(msg), retry_after);
}

/// Il `Retry-After` che il provider ha dichiarato, se l'errore lo porta.
///
/// PUNTO UNICO (regola L) dell'estrazione: il segnale e' TIPIZZATO dentro
/// [`ProviderHttpError`] (che lo ha gia' letto dall'header, `parse_retry_after`) e si
/// prende per downcast — mai ri-parsato dal testo del messaggio (regola M).
fn retry_after_of(err: &anyhow::Error) -> Option<u64> {
    err.chain()
        .find_map(|c| c.downcast_ref::<ProviderHttpError>())?
        .retry_after_seconds
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
            // Il 502 e' corretto (il chiamante ha chiesto QUESTO provider), ma
            // senza status/codice il log non distingue una chiave scaduta da un
            // timeout o da un endpoint sbagliato: nei log del 26/07 c'e' un
            // "list_models singolo fallita provider=perplexity" che non dice
            // niente a chi deve ripararlo. I segnali strutturati esistono gia'
            // sull'errore (regola M): vanno emessi, non ri-dedotti dal testo.
            let http = e
                .chain()
                .find_map(|c| c.downcast_ref::<ProviderHttpError>());
            tracing::warn!(
                provider = %provider,
                status = http.map(|h| h.status),
                code = http.and_then(|h| h.code.as_deref()),
                dettaglio = %http
                    .and_then(|h| h.structured_message())
                    .unwrap_or_else(|| e.to_string()),
                "gateway: list_models singolo fallita"
            );
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
        // Delega al corpo d'errore unico: anche una guardia d'ingresso deve
        // uscire con la stessa forma del resto della pipeline, o il frontend
        // trova `user_message` a volte si' e a volte no.
        return PipelineError::invalid_request("messages required").into_response();
    }
    // DEADLINE END-TO-END (retry e chain inclusi). Senza di essa una singola
    // completion poteva correre quanto il TRASPORTO concedeva (300s) — cioe'
    // quanto l'INTERO run multi-turno che l'aveva chiesta: una chiamata lenta
    // consumava il 100% della vita del run, che moriva con `it=0`, e la colpa
    // finiva ogni volta sul modello di turno. Il budget garantisce invece
    // `min_guaranteed_turns` turni per run (punto unico: nexus_auth::llm_timeouts).
    // I timeout di QUESTA richiesta, dal run che il chiamante dichiara. Calcolati
    // UNA volta e passati a `run_complete`: wrapper esterno e deadline interna
    // devono venire dallo stesso numero, o il primo scade prima della seconda.
    let base = state.runtime_snapshot().await.timeouts;
    let timeouts = request_timeouts(&base, body.run_timeout_secs, body.deferrable);
    let budget = timeouts.request_budget;
    match tokio::time::timeout(budget, run_complete(&state, &body, timeouts)).await {
        Ok(Ok(resp)) => Json(resp).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => {
            tracing::warn!(
                budget_s = budget.as_secs(),
                // Da quale run e' nato il budget: senza, un 504 con budget 60s
                // sembra una configurazione sbagliata invece di una figura corta.
                run_timeout_s = body.run_timeout_secs.unwrap_or(0),
                model = %body.model,
                feature = %body.metadata.feature,
                "gateway: budget della richiesta esaurito -> 504, il motore fara' failover"
            );
            let (status, body) = request_budget_exceeded_body(budget.as_secs());
            (status, Json(body)).into_response()
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
        return PipelineError::invalid_request("messages required").into_response();
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
                    // Parita' col body JSON del path non-streaming: stesso punto
                    // unico, quindi `code`, `details` E la resa leggibile. Qui
                    // il body era ricostruito a mano ed e' rimasto indietro ogni
                    // volta che il non-streaming e' cambiato.
                    let _ = tx
                        .send(Ok(Event::default().data(e.to_body().to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            }
        } else {
            let decision = runtime.policy.decide(tier, &body.metadata.feature, &HashMap::new());
            if decision.blocked {
                let reason = decision.reason.clone().unwrap_or_default();
                let mut err = if decision.dlp_blocked {
                    PipelineError::policy_tier_excluded(None, tier, &decision.providers, reason)
                } else {
                    PipelineError::blocked(reason)
                };
                // `TIER_BLOCKED` non porta details dal suo costruttore, ma il
                // path SSE li ha sempre emessi: il consumer legge detected_tier
                // e allowed_providers.
                err.details.get_or_insert_with(|| {
                    Box::new(
                        json!({ "detected_tier": tier, "allowed_providers": decision.providers }),
                    )
                });
                let _ = tx
                    .send(Ok(Event::default().data(err.to_body().to_string())))
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
            .find(|rp| !state.cooldown.is_model_in_cooldown(rp.provider.name(), &rp.model))
        else {
            let _ = tx
                .send(Ok(Event::default()
                    .data(json!({ "error": "nessun provider disponibile" }).to_string())))
                .await;
            let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
            return;
        };

        // Quota guardrail (non blocca lo stream se passa).
        if let Err(e) = enforce_quota(
            &state.db,
            &body,
            rp.provider.name(),
            &rp.model,
            QuotaEstimate::Testuale,
        )
        .await
        {
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
                            // L'esito non viene dichiarato a nessuno: lo streaming
                            // non ha un chiamante che prenoti, quindi qui non
                            // esiste il rischio di doppio addebito che il campo
                            // `ledger` della risposta non-streaming previene.
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
                                    ledger: None,
                                };
                                let _ = record_usage_to_ledger(&state.db, &body, &resp).await;
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
                let kind = state.vocabolario_errori.verdetto(&err).classe;
                let msg = err.to_string();
                match kind {
                    ProviderErrorKind::Billing => state.cooldown.mark_billing(name, Some(msg.clone())),
                    // Colpa nostra/config o modello non abilitato: niente cooldown.
                    ProviderErrorKind::ClientError => {}
                    // 413 troppo grande per il provider: sano, niente cooldown.
                    ProviderErrorKind::ContextTooLong => {}
                    // 402 di ammissione: ha credito, e' la richiesta a non
                    // starci. Sano, niente cooldown.
                    ProviderErrorKind::RequestExceedsCredit => {}
                    // Anche sullo streaming il cooldown onora il `Retry-After`: il
                    // segnale e' lo stesso del path non-streaming, e una durata
                    // diversa a seconda del canale sarebbe due risposte alla stessa
                    // domanda (regola L).
                    // La portata segue la causa come nel path non-streaming: un
                    // tetto del modello non spegne il fornitore (regola L: un
                    // criterio solo, non uno per canale).
                    ProviderErrorKind::Transient => {
                        let http = err
                            .chain()
                            .find_map(|c| c.downcast_ref::<ProviderHttpError>());
                        let portata = PortataCooldown::da_segnale(
                            http.map(|h| h.status),
                            http.and_then(|h| h.code.as_deref()),
                        );
                        state.cooldown.mark_transient_after(
                            name,
                            portata.modello(&req.model),
                            Some(msg.clone()),
                            retry_after_of(&err),
                        )
                    }
                }
                // Stesso corpo del path non-streaming: qui usciva il solo
                // `err.to_string()`, cioe' il body grezzo del provider — la
                // sorgente esatta dei blob letti in chat, sul canale che nessuno
                // guardava perche' lo streaming e' l'eccezione.
                let _ = tx
                    .send(Ok(Event::default().data(
                        PipelineError::provider_call_failed(
                            &state.vocabolario_errori,
                            name,
                            &req.model,
                            &err,
                        )
                        .to_body()
                        .to_string(),
                    )))
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
/// Ledger: scritto da `record_media_usage_to_ledger` con la quantita' di immagini
/// prodotte (`usage_kind='image'`, `quantity_unit='image'`, contate sulla
/// risposta). Il costo si applica se `ai_price_catalog_unit` ha un prezzo
/// per-immagine per questo (provider, model); finche' non ce l'ha la riga porta
/// costo 0 con `details.price_state='not_in_catalog'` — dichiarato, non dedotto.
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
    // Il prefisso si toglie solo se e' il provider: per groq/openrouter la
    // slash e' parte del nome del modello (regola L: unica funzione).
    let model = strip_provider_prefix(&body.model, provider.name());

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L): stima dal prompt come singolo messaggio user.
    let quota_req = image_gen_to_llm_request(body, &model);
    enforce_quota(
        &state.db,
        &quota_req,
        provider.name(),
        &model,
        QuotaEstimate::NonTestuale,
    )
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

    let resp = provider
        .generate_image(&req)
        .await
        // I fatti dal ProviderHttpError, non il suo Display: qui usciva il body
        // grezzo del provider come unico testo disponibile.
        .map_err(|e| errore_provider(state, provider.name(), &req.model, &e))?;

    // Consumo: le immagini prodotte si CONTANO sulla risposta, quindi la
    // quantita' e' misurata, non stimata.
    record_media_usage_to_ledger(
        &state.db,
        &quota_req,
        &resp.provider_used,
        &resp.model_used,
        MediaUsage::misurata(MediaKind::Image, UsageUnit::Image, resp.images.len() as f64),
    )
    .await;

    Ok(resp)
}

/// PUNTO UNICO (regola L) di «quale provider serve QUESTA capability».
///
/// Le cinque domande — immagini, video, trascrizione, sintesi vocale, conteggio
/// token — hanno la stessa risposta e differiscono in due cose sole: il
/// predicato che dichiara la capability e il NOME con cui la si nomina
/// all'utente. Vivevano in cinque funzioni identiche, e la quinta (il conteggio
/// token) e' cio' che ha reso la duplicazione un problema invece di un fatto:
/// una regola che vale per tutte — per esempio «un provider in cooldown di
/// credito non conta nemmeno come pinnato» — andrebbe scritta cinque volte, e
/// basterebbe dimenticarne una perche' due percorsi vicini si comportino in
/// modo diverso senza che nulla fallisca.
///
/// La disciplina e' quella che le cinque copie gia' avevano, e non cambia: col
/// `pin` ESATTAMENTE quel provider, che deve essere configurato E capace
/// (regola H: errore esplicito, niente delega a chi non sa fare la cosa, niente
/// ripiego silenzioso); senza pin, il PRIMO capace non in cooldown. Nessun
/// fallback cross-provider.
fn seleziona_provider_capace(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
    capacita: &str,
    capace: impl Fn(&dyn LlmProvider) -> bool,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    if let Some(pin) = pin {
        let Some(p) = providers.iter().find(|p| p.name() == pin) else {
            return Err(PipelineError::provider(format!(
                "provider pinnato \"{pin}\" non configurato/abilitato nel gateway"
            )));
        };
        if !capace(p.as_ref()) {
            return Err(PipelineError::provider(format!(
                "provider \"{pin}\" non supporta {capacita}"
            )));
        }
        return Ok(p.clone());
    }
    providers
        .iter()
        .find(|p| capace(p.as_ref()) && !cooldown.is_in_cooldown(p.name()))
        .cloned()
        .ok_or_else(|| {
            PipelineError::provider(format!("nessun provider sano supporta {capacita}"))
        })
}

/// Seleziona il provider per l'image-gen: delega al punto unico
/// [`seleziona_provider_capace`].
fn select_image_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    seleziona_provider_capace(
        providers,
        pin,
        cooldown,
        "la generazione di immagini",
        |p| p.supports_image_gen(),
    )
}

/// PUNTO UNICO (regola L) della [`LlmRequest`] sintetica dei quattro handler
/// media (image/video/transcribe/tts): il testo diventa l'unico messaggio user
/// (cosi' la stima char/4 di `estimate_prompt_tokens` resta coerente col punto
/// unico billing), pin e metadata passano verbatim, ogni altro campo del
/// contratto resta assente. Prima erano quattro literal gemelli, e ogni
/// estensione del contratto li faceva riscrivere tutti.
fn media_to_llm_request(
    model: &str,
    testo: String,
    pin_provider: Option<String>,
    metadata: RequestMetadata,
) -> LlmRequest {
    use crate::types::{LlmMessage, MessageContent};
    let mut r = LlmRequest::minimal(
        model,
        vec![LlmMessage {
            role: "user".to_string(),
            content: MessageContent::Text(testo),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: None,
            is_error: None,
        }],
        metadata,
    );
    r.pin_provider = pin_provider;
    r
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` a partire da
/// una [`ImageGenRequest`]: il prompt diventa l'unico messaggio user. Nessun
/// `max_tokens` (le immagini non hanno completion in token).
fn image_gen_to_llm_request(body: &ImageGenRequest, model: &str) -> LlmRequest {
    media_to_llm_request(
        model,
        body.prompt.clone(),
        body.pin_provider.clone(),
        body.metadata.clone(),
    )
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
/// Ledger: scritto da `record_media_usage_to_ledger` (`usage_kind='video'`,
/// `quantity_unit='second'`). La durata PRODOTTA non e' riportata dal provider,
/// quindi si registra quella richiesta con `quantity_source='request'`; se il
/// chiamante non la indica (default lato provider) la quantita' resta NULL con
/// `quantity_source='none'`, invece di un numero inventato.
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
    // Il prefisso si toglie solo se e' il provider: per groq/openrouter la
    // slash e' parte del nome del modello (regola L: unica funzione).
    let model = strip_provider_prefix(&body.model, provider.name());

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L): stima dal prompt come singolo messaggio user.
    let quota_req = video_gen_to_llm_request(body, &model);
    enforce_quota(
        &state.db,
        &quota_req,
        provider.name(),
        &model,
        QuotaEstimate::NonTestuale,
    )
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

    let resp = provider
        .generate_video(&req)
        .await
        .map_err(|e| errore_provider(state, provider.name(), &req.model, &e))?;

    // Consumo: la risposta NON riporta la durata prodotta, quindi al massimo si
    // registra quella richiesta. Se il chiamante non l'ha indicata (il default e'
    // lato provider) la quantita' resta ignota: meglio una riga senza numero che
    // un numero inventato.
    record_media_usage_to_ledger(
        &state.db,
        &quota_req,
        &resp.provider_used,
        &resp.model_used,
        match body.duration_seconds {
            Some(s) => MediaUsage::da_richiesta(MediaKind::Video, UsageUnit::Second, s as f64),
            None => MediaUsage::non_quantificabile(MediaKind::Video, UsageUnit::Second),
        },
    )
    .await;

    Ok(resp)
}

/// Seleziona il provider per il video-gen: delega al punto unico
/// [`seleziona_provider_capace`].
fn select_video_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    seleziona_provider_capace(providers, pin, cooldown, "la generazione di video", |p| {
        p.supports_video_gen()
    })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` da una
/// [`VideoGenRequest`]: il prompt diventa l'unico messaggio user (stima char/4
/// del punto unico billing). Gemella di [`image_gen_to_llm_request`]: niente
/// costo inventato (regola G/H), il ledger non viene scritto a valle.
fn video_gen_to_llm_request(body: &VideoGenRequest, model: &str) -> LlmRequest {
    media_to_llm_request(
        model,
        body.prompt.clone(),
        body.pin_provider.clone(),
        body.metadata.clone(),
    )
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
/// Ledger: scritto da `record_media_usage_to_ledger` (`usage_kind='audio_in'`).
/// La durata dell'audio non e' nota a nessuna delle due parti — la richiesta
/// porta i byte codificati e la risposta gira con `response_format=json`, che non
/// include `duration` — quindi la riga registra CHI ha consumato e con quale
/// modello, con `quantity` NULL e `quantity_source='none'`. Registrare il fatto
/// senza il numero e' piu' onesto che non registrarlo affatto.
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
    // Il prefisso si toglie solo se e' il provider: per groq/openrouter la
    // slash e' parte del nome del modello (regola L: unica funzione).
    let model = strip_provider_prefix(&body.model, provider.name());

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L). Il testo risultante non e' noto a priori e
    // l'audio non e' tokenizzabile come prompt: usiamo una stima minima (un
    // messaggio user vuoto). Coerente col pattern image-gen, che NON scrive
    // ledger: niente costo inventato (regola G/H).
    let quota_req = transcribe_to_llm_request(body, &model);
    enforce_quota(
        &state.db,
        &quota_req,
        provider.name(),
        &model,
        QuotaEstimate::NonTestuale,
    )
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

    let resp = provider
        .transcribe_audio(&req)
        .await
        .map_err(|e| errore_provider(state, provider.name(), &req.model, &e))?;

    // Consumo: la trascrizione si paga al secondo di audio, ma la durata non e'
    // nota da nessuna delle due parti — la richiesta porta i byte codificati (da
    // cui i secondi non si ricavano senza decodificare il contenitore) e la
    // risposta gira con `response_format=json`, che non include `duration`.
    // La riga si scrive comunque: chi ha consumato, con quale modello e quando
    // sono fatti utili anche senza il numero.
    record_media_usage_to_ledger(
        &state.db,
        &quota_req,
        &resp.provider_used,
        &resp.model_used,
        MediaUsage::non_quantificabile(MediaKind::AudioIn, UsageUnit::Second),
    )
    .await;

    Ok(resp)
}

/// Seleziona il provider per la trascrizione audio: delega al punto unico
/// [`seleziona_provider_capace`].
fn select_audio_in_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    seleziona_provider_capace(providers, pin, cooldown, "la trascrizione audio", |p| {
        p.supports_audio_in()
    })
}

// ── Conteggio token (gratuito) ───────────────────────────────────────────────

/// `POST /v1/count_tokens`: quanti token d'ingresso vale questa richiesta,
/// secondo il tokenizzatore del FORNITORE.
///
/// Non e' una stima nostra e non la sostituisce: le stime pre-invio restano
/// quelle che sono (BPE locale nel motore, char/4 nel gateway). Questo e' il
/// numero che il fornitore usera' davvero, ed e' l'unico con cui misurare la
/// deriva delle altre.
///
/// NESSUNA scrittura di ledger e nessun `enforce_quota`: su anthropic
/// l'endpoint e' gratuito, e una riga a costo zero sarebbe indistinguibile da
/// una chiamata fatturata che non abbiamo registrato. Auth invariata: passa dal
/// solito middleware, come ogni altra rotta `/v1`.
pub async fn count_tokens(State(state): State<AppState>, Json(body): Json<LlmRequest>) -> Response {
    if body.messages.is_empty() {
        return PipelineError::invalid_request("messages required").into_response();
    }
    match run_count_tokens(&state, &body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn run_count_tokens(
    state: &AppState,
    body: &LlmRequest,
) -> Result<CountTokensResponse, PipelineError> {
    validate_logical_model(&body.model)?;
    let runtime = state.runtime_snapshot().await;
    let provider = select_count_tokens_provider(
        &runtime.providers,
        body.pin_provider.as_deref(),
        &state.cooldown,
    )?;

    let messaggi_redatti = messaggi_dietro_la_dlp(state, &runtime, body, provider.name()).await?;

    // Il prefisso si toglie solo se e' il provider: per groq/openrouter la
    // slash e' parte del nome del modello (regola L: unica funzione).
    let mut req = body.clone();
    req.messages = messaggi_redatti;
    req.model = strip_provider_prefix(&body.model, provider.name());
    provider
        .count_tokens(&req)
        .await
        .map_err(|e| PipelineError::provider(format!("conteggio token fallito: {e}")))
}

/// LA PIPELINE DLP VALE ANCHE PER IL CONTEGGIO, e questa funzione e' il punto in
/// cui lo si vede.
///
/// `/v1/count_tokens` manda al fornitore la STESSA `LlmRequest` di
/// `/v1/complete` — messaggi, system e tool per intero — quindi senza questa
/// sequenza sarebbe una seconda via d'uscita del contenuto che salta
/// classificazione, gate di tier e redazione: segreti e PII che la pipeline
/// toglie dalla completion partirebbero verbatim dal conteggio (review
/// avversaria fase 4, bloccante). Il numero contato sul testo REDATTO e' anche
/// quello giusto: e' il testo che verra' davvero spedito.
///
/// Ritorna i messaggi da spedire; blocca la richiesta dove la policy blocca.
///
/// Il gate di tier sul provider e' l'unica parte non condivisa con la
/// completion: la' vive dentro il ramo del pin, qui il provider e' gia' scelto
/// e vale sempre. La redazione invece e' la STESSA (regola L: due copie
/// divergerebbero, e la copia dimenticata sarebbe quella che lascia passare).
async fn messaggi_dietro_la_dlp(
    state: &AppState,
    runtime: &super::RuntimeState,
    body: &LlmRequest,
    provider_name: &str,
) -> Result<Vec<LlmMessage>, PipelineError> {
    let classifier = SensitivityClassifier::new(runtime.presidio.clone());
    let classification = classifier.classify(&body.messages).await;
    let effective_tier = classification.tier.max(body.metadata.sensitivity_tier);
    runtime
        .policy
        .validate_tier_claim(body.metadata.sensitivity_tier, effective_tier);
    pin_tier_gate(
        &runtime.policy,
        provider_name,
        effective_tier,
        &body.metadata.feature,
    )?;
    let (_, redaction) = redigi_richiesta(state, runtime, body, effective_tier).await?;
    Ok(redaction.messages)
}

/// La redazione con le opzioni che il tier effettivo impone: punto unico dei due
/// percorsi che spediscono una `LlmRequest` a un fornitore (`/v1/complete` e
/// `/v1/count_tokens`). Lo `strict_mode` nasce dal tier, la politica PII
/// asimmetrica dal DB — decise QUI, una volta sola.
async fn redigi_richiesta(
    state: &AppState,
    runtime: &super::RuntimeState,
    req: &LlmRequest,
    effective_tier: u8,
) -> Result<(RedactionPipeline, RedactionResult), PipelineError> {
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
            strict_mode: effective_tier >= 2,
            skip_pii_in_user_messages,
            ..Default::default()
        },
    );
    let redaction = pipeline
        .redact(req)
        .await
        .map_err(|e| PipelineError::blocked(e.to_string()))?;
    // La pipeline torna al chiamante perche' la completion la riusa per la
    // re-idratazione della risposta (`rehydrate`): costruirne una seconda con
    // altre opzioni de-redigerebbe con una mappa diversa da quella che ha
    // redatto.
    Ok((pipeline, redaction))
}

/// Seleziona il provider che sa contare i token: delega al punto unico
/// [`seleziona_provider_capace`].
///
/// Il ripiego cross-provider, che quel punto unico gia' esclude, qui sarebbe
/// peggio che altrove: un conteggio fatto col tokenizzatore di un fornitore
/// diverso da quello che servira' la richiesta e' un numero SBAGLIATO che sembra
/// giusto.
fn select_count_tokens_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    seleziona_provider_capace(providers, pin, cooldown, "il conteggio dei token", |p| {
        p.supports_count_tokens()
    })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` da una
/// [`TranscribeRequest`]: un messaggio user vuoto (l'audio non e' un prompt
/// testuale tokenizzabile e il testo risultante non e' noto a priori). Gemella di
/// [`image_gen_to_llm_request`]: stima minima, niente costo inventato (regola G/H).
fn transcribe_to_llm_request(body: &TranscribeRequest, model: &str) -> LlmRequest {
    media_to_llm_request(
        model,
        String::new(),
        body.pin_provider.clone(),
        body.metadata.clone(),
    )
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
/// Ledger: scritto da `record_media_usage_to_ledger` (`usage_kind='audio_out'`,
/// `quantity_unit='character'`). Qui la quantita' e' esatta: il testo di input lo
/// conosciamo, e si contano i CARATTERI, non i byte.
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
    // Il prefisso si toglie solo se e' il provider: per groq/openrouter la
    // slash e' parte del nome del modello (regola L: unica funzione).
    let model = strip_provider_prefix(&body.model, provider.name());

    // Quota guardrail PRIMA della chiamata (riusa la fn parametrica su
    // provider+model, regola L). A differenza di transcribe, qui l'input testuale
    // E' noto: la stima riusa il pattern char/4 della chat sui caratteri
    // dell'input. Coerente col pattern image-gen, NON scrive ledger: niente costo
    // inventato (regola G/H).
    let quota_req = tts_to_llm_request(body, &model);
    enforce_quota(
        &state.db,
        &quota_req,
        provider.name(),
        &model,
        QuotaEstimate::NonTestuale,
    )
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

    let resp = provider
        .text_to_speech(&req)
        .await
        .map_err(|e| errore_provider(state, provider.name(), &req.model, &e))?;

    // Consumo: il TTS si paga al carattere di input, e l'input lo conosciamo
    // esattamente. `chars()` e non `len()`: i byte UTF-8 non sono caratteri, e
    // fatturare un testo accentato piu' di uno ASCII sarebbe un errore silenzioso.
    record_media_usage_to_ledger(
        &state.db,
        &quota_req,
        &resp.provider_used,
        &resp.model_used,
        MediaUsage::da_richiesta(
            MediaKind::AudioOut,
            UsageUnit::Character,
            body.input.chars().count() as f64,
        ),
    )
    .await;

    Ok(resp)
}

/// Seleziona il provider per la sintesi vocale: delega al punto unico
/// [`seleziona_provider_capace`].
fn select_audio_out_provider(
    providers: &[Arc<dyn LlmProvider>],
    pin: Option<&str>,
    cooldown: &CooldownManager,
) -> Result<Arc<dyn LlmProvider>, PipelineError> {
    seleziona_provider_capace(providers, pin, cooldown, "la sintesi vocale", |p| {
        p.supports_audio_out()
    })
}

/// Costruisce una [`LlmRequest`] di sola STIMA per `enforce_quota` da una
/// [`TtsRequest`]: a differenza di transcribe, qui il testo di input E' noto, e
/// lo mettiamo come messaggio user cosi' la stima char/4 esistente lo conta.
/// Gemella di [`image_gen_to_llm_request`]/[`transcribe_to_llm_request`]: niente
/// costo inventato (regola G/H), il ledger non viene scritto a valle.
fn tts_to_llm_request(body: &TtsRequest, model: &str) -> LlmRequest {
    media_to_llm_request(
        model,
        body.input.clone(),
        body.pin_provider.clone(),
        body.metadata.clone(),
    )
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

    /// Deadline LONTANA per i test che non esercitano il budget: il vincolo
    /// non deve mai scattare (il budget ha i suoi test dedicati).
    fn far_deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + std::time::Duration::from_secs(600)
    }

    /// Le righe del catalogo (mig 0705) che i corpi d'errore di QUESTI test
    /// producono. Non e' un catalogo di comodo: `il_vocabolario_di_prova_e_un_
    /// sottoinsieme_del_seed_vero` lo confronta con la migrazione REALE, quindi
    /// una divergenza rosseggia invece di far misurare a questi test un sistema
    /// che non esiste (regola O).
    ///
    /// I test di questo modulo misurano il FLUSSO (retry, cooldown, corpo
    /// d'errore); il catalogo ha i suoi test in `tassonomia_errori`.
    const RIGHE_DI_PROVA: [(&str, &str, Option<i16>, Option<&str>); 6] = [
        ("*", "insufficient_quota", None, Some("credit_exhausted")),
        ("*", "rate_limit_exceeded", None, Some("rate_limit")),
        ("*", "invalid_request_error", None, Some("malformed_request")),
        ("groq", "tokens", None, None),
        // La corsia differibile piena (mig 0729): senza,
        // `la_corsia_differibile_piena_non_apre_cooldown` misurerebbe un
        // catalogo che in produzione non esiste, e il 429 cadrebbe su Transient
        // proprio come nel difetto che la guardia evita.
        ("openai", "flex_unavailable", Some(429), Some("flex_capacity")),
        // Il valore sintetico del quirk openrouter (mig 0709): senza,
        // `un_402_di_ammissione_non_mette_il_fornitore_in_cooldown` misurerebbe
        // un catalogo che in produzione non esiste.
        (
            "openrouter",
            "request_exceeds_credit",
            Some(402),
            Some("request_exceeds_credit"),
        ),
    ];

    fn vocabolario_di_prova() -> VocabolarioErrori {
        VocabolarioErrori::con_mappa(crate::tassonomia_errori::Mappa::da_righe(RIGHE_DI_PROVA))
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_vocabolario_di_prova_e_un_sottoinsieme_del_seed_vero(pool: sqlx::PgPool) {
        use crate::tassonomia_errori::{CausaErrore, Dichiarazione};
        let vero = crate::tassonomia_errori::carica_mappa_per_test(&pool)
            .await
            .expect("catalogo dalla migrazione 0705");
        for (provider, valore, status, causa) in RIGHE_DI_PROVA {
            let atteso = match causa {
                Some(c) => Dichiarazione::Causa(
                    CausaErrore::dal_db(c).expect("causa del vocabolario"),
                ),
                None => Dichiarazione::Ambiguo,
            };
            assert_eq!(
                vero.dichiarazione(provider, valore, status.unwrap_or(0) as u16),
                atteso,
                "({provider}, {valore}) e' cambiato nel seed 0705: questi test \
                 starebbero misurando un catalogo che in produzione non esiste"
            );
        }
    }

    // ── Provider finto (no rete) ────────────────────────────────────────────
    enum Behaviour {
        Ok,
        ErrBilling,
        /// Errore lato client (400 invalid_request): non ritentabile, non da cooldown.
        ErrClient,
        /// 400 history-related finche' la richiesta porta ancora un
        /// `thinking_signature`, OK quando la sanificazione l'ha tolto.
        ///
        /// L'esito dipende da CIO' CHE ARRIVA, come nel provider vero: un fake
        /// che rispondesse "ok al secondo giro" qualunque cosa riceva
        /// fossilizzerebbe l'assunto sotto esame (che il retry mandi qualcosa di
        /// diverso) e resterebbe verde anche se il gateway rispedisse due volte
        /// la stessa identica richiesta — che e' il difetto misurato (regola O).
        ErrHistoryUntilSanitized,
        /// Non risponde mai: simula la chiamata APPESA che ha originato il fix
        /// (misurata sul campo: 197s senza alcun log, con le figure ferme).
        Hang,
        /// 429 con `Retry-After`: il tetto token di UN modello, nella forma
        /// misurata su groq il 13/08/2026 (TPD Limit 200000, 23m44,3s).
        ErrRateLimit,
        /// 402 di OpenRouter: la prenotazione supera il credito RESIDUO. Il body
        /// e' quello reale del 13/08/2026 (residuo 62.186 token), con
        /// `error.code` NUMERICO — la forma che non produce un codice
        /// strutturato e cadeva sulla tabella per status.
        ErrOltreIlCredito,
        /// Risposta 200 DEGENERE: content vuoto, zero tool-call, finish
        /// "length", ma con un usage REALE — la forma della degenerazione da
        /// budget (Gemini col tetto consumato dal thinking). Il provider l'ha
        /// fatturata: e' il caso che la mig 0701 mette a ledger.
        Degenerate,
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
        /// Se `Some(s)`: OGNI chiamata fallisce 429 con `Retry-After: s` (il caso
        /// Vertex RESOURCE_EXHAUSTED dell'incidente figure 2026-07-14). Serve ai
        /// test del budget della richiesta.
        transient_retry_after: Option<u64>,
    }

    impl FakeProvider {
        fn new(name: &str, behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                transient_retry_after: None,
                models_result: None,
                image_capable: false,
                audio_capable: false,
                audio_out_capable: false,
                video_capable: false,
            })
        }

        /// Variante che risponde SEMPRE 429 con `Retry-After` esplicito: il caso
        /// Vertex RESOURCE_EXHAUSTED dell'incidente figure 2026-07-14 (quota
        /// esaurita, il provider chiede un'attesa che non sta nel budget).
        fn always_rate_limited(name: &str, retry_after_s: u64) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                behaviour: Behaviour::Ok,
                calls: AtomicUsize::new(0),
                transient_fail_calls: 0,
                transient_retry_after: Some(retry_after_s),
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
                transient_retry_after: None,
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
                transient_retry_after: None,
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
                transient_retry_after: None,
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
                transient_retry_after: None,
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
                transient_retry_after: None,
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
                transient_retry_after: None,
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
            // Rate-limit permanente con Retry-After (429 quota): come Vertex
            // RESOURCE_EXHAUSTED. Ha precedenza su tutto: la quota non "torna".
            if let Some(secs) = self.transient_retry_after {
                return Err(crate::providers::ProviderHttpError::from_response(
                    &self.name,
                    429,
                    r#"{"error":{"message":"resource exhausted (quota test)","type":"tokens","code":"rate_limit_exceeded"}}"#
                        .to_string(),
                )
                .with_retry_after(Some(secs))
                .into());
            }
            // Prime `transient_fail_calls` chiamate: errore transitorio (503),
            // emesso come ProviderHttpError (status certo) come i provider reali.
            if idx < self.transient_fail_calls {
                return Err(crate::providers::ProviderHttpError::from_response(
                    &self.name,
                    503,
                    r#"{"error":{"message":"service unavailable (transient test)"}}"#.to_string(),
                )
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
                        reasoning_tokens: None,
                        declared_cost_usd: None,
                        upstream_cost_usd: None,
                    },
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                    finish_reason: "stop".into(),
                    privacy_rerouted: None,
                    reasoning: None,
                    thinking_signature: None,
                    citations: None,
                    ledger: None,
                }),
                // Errori strutturati (status + codice), come i provider reali.
                Behaviour::ErrBilling => Err(crate::providers::ProviderHttpError::from_response(
                    &self.name,
                    402,
                    r#"{"error":{"message":"insufficient_quota","code":"insufficient_quota"}}"#
                        .to_string(),
                )
                .into()),
                // Passa dal PRODUTTORE (`from_response`) e non da una struct
                // costruita a mano: e' li' che i CANDIDATI vengono osservati e
                // il quirk applicato, cioe' il tratto che decide la classe.
                Behaviour::ErrOltreIlCredito => Err(crate::providers::ProviderHttpError::from_response(
                    &self.name,
                    402,
                    r#"{"error":{"message":"This request requires more credits, or fewer max_tokens. You requested up to 65536 tokens, but can only afford 62186.","code":402,"metadata":{"limit_source":"openrouter_credits"}}}"#
                        .to_string(),
                )
                .into()),
                Behaviour::ErrRateLimit => Err(crate::providers::ProviderHttpError::from_response(
                    &self.name,
                    429,
                    r#"{"error":{"message":"Rate limit reached for model `openai/gpt-oss-20b` on tokens per day (TPD): Limit 200000, Used 199788","code":"rate_limit_exceeded","type":"tokens"}}"#
                        .to_string(),
                )
                .with_retry_after(Some(1424))
                .into()),
                Behaviour::ErrClient => Err(crate::providers::ProviderHttpError::from_response(
                    &self.name,
                    400,
                    r#"{"error":{"message":"invalid request: bad field","code":"invalid_request_error"}}"#
                        .to_string(),
                )
                .into()),
                Behaviour::Hang => unreachable!("Behaviour::Hang: il sleep precede il match"),
                Behaviour::Degenerate => Ok(LlmResponse {
                    content: String::new(),
                    tool_calls: None,
                    usage: LlmUsage {
                        input_tokens: 1_000,
                        output_tokens: 5,
                        cache_read_tokens: Some(600),
                        cache_creation_tokens: None,
                        reasoning_tokens: None,
                        declared_cost_usd: None,
                        upstream_cost_usd: None,
                    },
                    model_used: req.model.clone(),
                    provider_used: self.name.clone(),
                    latency_ms: 0,
                    finish_reason: "length".into(),
                    privacy_rerouted: None,
                    reasoning: None,
                    thinking_signature: None,
                    citations: None,
                    ledger: None,
                }),
                Behaviour::ErrHistoryUntilSanitized => {
                    // Corpo VERBATIM di Anthropic quando la history porta una
                    // firma di thinking che il turno non ammette: il codice
                    // strutturato nasce da `from_response`, non e' dettato qui.
                    if req.messages.iter().any(|m| m.thinking_signature.is_some()) {
                        Err(ProviderHttpError::from_response(
                            &self.name,
                            400,
                            r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1: Expected `thinking` or `redacted_thinking`, but found `text`"}}"#
                                .to_string(),
                        )
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
                                reasoning_tokens: None,
                                declared_cost_usd: None,
                                upstream_cost_usd: None,
                            },
                            model_used: req.model.clone(),
                            provider_used: self.name.clone(),
                            latency_ms: 0,
                            finish_reason: "stop".into(),
                            privacy_rerouted: None,
                            reasoning: None,
                            thinking_signature: None,
                            citations: None,
                            ledger: None,
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
                Behaviour::Hang
                | Behaviour::Degenerate
                | Behaviour::ErrRateLimit
                | Behaviour::ErrOltreIlCredito => {
                    unreachable!("Behaviour usato solo da complete")
                }
                Behaviour::ErrClient | Behaviour::ErrHistoryUntilSanitized => {
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
                Behaviour::Hang
                | Behaviour::Degenerate
                | Behaviour::ErrRateLimit
                | Behaviour::ErrOltreIlCredito => {
                    unreachable!("Behaviour usato solo da complete")
                }
                Behaviour::ErrClient | Behaviour::ErrHistoryUntilSanitized => {
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
                Behaviour::Hang
                | Behaviour::Degenerate
                | Behaviour::ErrRateLimit
                | Behaviour::ErrOltreIlCredito => {
                    unreachable!("Behaviour usato solo da complete")
                }
                Behaviour::ErrClient | Behaviour::ErrHistoryUntilSanitized => {
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
                Behaviour::Hang
                | Behaviour::Degenerate
                | Behaviour::ErrRateLimit
                | Behaviour::ErrOltreIlCredito => {
                    unreachable!("Behaviour usato solo da complete")
                }
                Behaviour::ErrClient | Behaviour::ErrHistoryUntilSanitized => {
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
                is_error: None,
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
            run_timeout_secs: None,
            service_tier: None,
            seed: None,
            stop: None,
            user: None,
            parallel_tool_calls: None,
            deferrable: false,
            effort: None,
        }
    }

    fn aliases() -> ModelAliasResolver {
        // Modello diretto stesso provider -> passthrough (strip prefisso).
        ModelAliasResolver::from_yaml_str("aliases: {}").unwrap()
    }

    /// IL DIFETTO: un nome fisico NUDO veniva propagato a tutta la catena, e il
    /// fornitore successivo riceveva il modello di un altro. Misurato il
    /// 30/07/2026: DeepSeek ha risposto 400 "The supported API model names are
    /// deepseek-v4-pro or deepseek-v4-flash, but you passed open-mistral-nemo",
    /// il gateway l'ha giustamente classificato irrimediabile e il chiamante ha
    /// ricevuto 500. La catena e' costruita col PRODUTTORE vero
    /// (`resolve_providers`), non con una lista scritta a mano (regola O).
    #[test]
    fn un_nome_fisico_nudo_non_viaggia_oltre_il_primo_provider() {
        let mistral: Arc<dyn LlmProvider> = FakeProvider::new("mistral", Behaviour::Ok);
        let deepseek: Arc<dyn LlmProvider> = FakeProvider::new("deepseek", Behaviour::Ok);
        let built = vec![mistral, deepseek];
        let (resolved, _) = resolve_providers(
            &["mistral".into(), "deepseek".into()],
            &built,
            &aliases(),
            "open-mistral-nemo",
            0,
        );
        assert_eq!(
            resolved.len(),
            1,
            "un nome fisico nudo vale per il provider a cui e' indirizzato: \
             propagarlo manda il modello di un fornitore a un altro"
        );
        assert_eq!(resolved[0].provider.name(), "mistral");
        assert_eq!(resolved[0].model, "open-mistral-nemo");
    }

    /// Controprova: un nome col PREFISSO attraversa la catena, perche' ogni
    /// provider sa se sia suo (stesso provider -> strip; altro -> fallback o
    /// esclusione). Senza questo, il fix sopra avrebbe accorciato ogni catena.
    #[test]
    fn un_nome_col_prefisso_resta_valutabile_da_tutta_la_catena() {
        let openai: Arc<dyn LlmProvider> = FakeProvider::new("openai", Behaviour::Ok);
        let deepseek: Arc<dyn LlmProvider> = FakeProvider::new("deepseek", Behaviour::Ok);
        let built = vec![openai, deepseek];
        let (resolved, _) = resolve_providers(
            &["openai".into(), "deepseek".into()],
            &built,
            &aliases(),
            "openai/gpt-x",
            0,
        );
        // deepseek esce per assenza di fallback cross-provider, non per il taglio
        // del nome nudo: il primo resta e la valutazione del secondo e' avvenuta.
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].provider.name(), "openai");
        assert_eq!(resolved[0].model, "gpt-x");
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
        let resp = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new()).await.unwrap();
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
        let resp = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new()).await.unwrap();
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
        let resp = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("tutti i provider hanno fallito"));
        // Su questa history (un solo messaggio user) la sanificazione aggressiva
        // non ha NIENTE da togliere: il retry manderebbe la stessa richiesta e
        // otterrebbe lo stesso 400. Un solo tentativo.
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
        assert!(!cooldown.is_in_cooldown("google")); // niente cooldown
    }

    /// History che la sanificazione aggressiva CAMBIA davvero: un assistant con
    /// `thinking_signature`, che la modalita' Standard conserva su Anthropic (il
    /// dialetto la richiede nei turni con tool) e che solo Aggressive rimuove.
    fn req_con_firma_thinking() -> LlmRequest {
        let mut r = req();
        r.messages.push(LlmMessage {
            role: "assistant".into(),
            content: MessageContent::Text("penso".into()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: Some("sig-di-un-altro-turno".into()),
            reasoning: None,
            is_error: None,
        });
        r
    }

    #[tokio::test]
    async fn history_client_error_sanitize_retry_poi_successo() {
        // Il retry aggressivo resta quando serve: qui la firma di thinking c'e',
        // Aggressive la toglie, la richiesta cambia e il provider accetta.
        let p = FakeProvider::new("anthropic", Behaviour::ErrHistoryUntilSanitized);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "claude-x".into(),
        }];
        let cooldown = CooldownManager::new();
        let resp = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req_con_firma_thinking(),
            true,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(resp.content, "ok-after-sanitize");
        assert_eq!(p.calls.load(Ordering::SeqCst), 2);
    }

    /// Corpo VERBATIM del 400 DeepSeek misurato il 2026-07-26 sui log del
    /// gateway: e' da qui che nascono status e codice strutturato, non da un
    /// `ProviderHttpError` compilato a mano nel test (regola O).
    const BODY_400_DEEPSEEK: &str = r#"{"error":{"message":"The reasoning_content in the thinking mode must be passed back to the API","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#;

    /// History in thinking mode DeepSeek: un assistant che porta il proprio
    /// `reasoning`. Il sanitizer lo CONSERVA in entrambe le modalita' (DeepSeek
    /// lo esige) e non c'e' nessuna tool-call pendente da riconciliare: e' il
    /// caso in cui Aggressive non ha nulla da fare.
    fn req_deepseek_con_reasoning() -> LlmRequest {
        let mut r = req();
        r.messages.push(LlmMessage {
            role: "assistant".into(),
            content: MessageContent::Text("ecco".into()),
            tool_call_id: None,
            tool_calls: None,
            name: None,
            thinking_signature: None,
            reasoning: Some("catena di pensiero deepseek".into()),
            is_error: None,
        });
        r
    }

    #[tokio::test]
    async fn history_client_error_senza_effetto_della_sanificazione_non_ritenta() {
        // IL DIFETTO MISURATO (log gateway 2026-07-26, 13:29:04.314 -> .809): su
        // un 400 di formato il gateway annunciava "sanificazione aggressiva e
        // retry", la sanificazione non toglieva NIENTE, e mezzo secondo dopo la
        // stessa identica richiesta tornava rifiutata. Una chiamata pagata a
        // vuoto a ogni ciclo (5 occorrenze in 20 minuti su deepseek e anthropic).
        //
        // Un 400 e' deterministico: a input uguale, esito uguale. Se la
        // sanificazione aggressiva non cambia la richiesta, il retry non puo'
        // avere un esito diverso e non va speso.
        let richiesta = req_deepseek_con_reasoning();

        // PREMESSA DICHIARATA (regola O): su questa history le due modalita'
        // coincidono davvero. Se un domani Aggressive iniziasse a toccarla, il
        // test deve dirlo qui invece di fallire piu' sotto per un motivo oscuro.
        let (spedita_al_primo_tentativo, _) = history_sanitizer::sanitized_for_attempt(
            &richiesta.messages,
            "deepseek",
            SanitizeMode::Standard,
        );
        assert!(
            !history_sanitizer::retry_changes_history(
                &richiesta.messages,
                &spedita_al_primo_tentativo,
                "deepseek",
                SanitizeMode::Aggressive,
            ),
            "premessa del test decaduta: su questa history Aggressive ora cambia qualcosa"
        );

        let p = RealBodyProvider::new("deepseek", 400, BODY_400_DEEPSEEK);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "deepseek-chat".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &richiesta,
            true,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();

        // LA CONSEGUENZA: una sola chiamata al provider. Mutazione: rimettendo il
        // retry incondizionato (il `continue` senza confronto) qui si legge 2.
        assert_eq!(
            p.calls.load(Ordering::SeqCst),
            1,
            "seconda chiamata al provider con una richiesta identica alla prima"
        );
        // Il rifiuto resta quello onesto del provider, non degrada in altro.
        let details = err.details.expect("details presenti");
        assert_eq!(details["failures"][0]["class"], "client_error");
        assert_eq!(details["failures"][0]["status"], 400);
        assert!(!cooldown.is_in_cooldown("deepseek")); // il provider e' sano
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.code, "PROVIDER_ERROR");
        // Cascata TUTTA deterministica -> lo status verso il chiamante e' 400,
        // non 500: ritentare la stessa richiesta e' inutile e i chiamanti fuori
        // dal motore (decidono su 4xx/5xx, non leggono details) non devono piu'
        // essere invitati a insistere (burst mistral del 20/07, otto 400
        // rilanciati come 500).
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        let details = err.details.expect("details strutturati presenti");
        assert_eq!(details["primary_cause"], "client_error");
        let f = &details["failures"][0];
        assert_eq!(f["provider"], "deepseek");
        assert_eq!(f["class"], "client_error");
        assert_eq!(f["status"], 400);
        assert_eq!(f["code"], "invalid_request_error");
    }

    // ── Resa dell'errore al confine HTTP (le tre chiavi user_*) ──────────────

    /// Provider che fallisce con un body d'errore REALE, costruito
    /// dall'estrattore di PRODUZIONE ([`ProviderHttpError::from_response`]).
    ///
    /// Il [`FakeProvider`] scrive `code` a mano nella struct: comodo, ma un test
    /// che parte da li' fissa l'assunto che vuole verificare (regola O). Qui il
    /// codice strutturato e la frase upstream nascono dal body, come sul campo:
    /// se domani l'estrattore smettesse di trovare `error.code`, il test se ne
    /// accorge invece di restare verde.
    struct RealBodyProvider {
        name: String,
        status: u16,
        body: String,
        calls: AtomicUsize,
    }

    impl RealBodyProvider {
        fn new(name: &str, status: u16, body: &str) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                status,
                body: body.to_string(),
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for RealBodyProvider {
        fn name(&self) -> &str {
            &self.name
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn supports_streaming(&self) -> bool {
            false
        }
        fn max_context_tokens(&self) -> u32 {
            1000
        }
        fn tier_compatibility(&self) -> &[u8] {
            &[0, 1, 2]
        }
        async fn complete(&self, _req: &LlmRequest) -> anyhow::Result<LlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(
                ProviderHttpError::from_response(&self.name, self.status, self.body.clone())
                    .into(),
            )
        }
        async fn stream(&self, _req: &LlmRequest) -> anyhow::Result<crate::provider::ChunkStream> {
            anyhow::bail!("non usato")
        }
        async fn healthcheck(&self) -> bool {
            true
        }
    }

    /// Body 429 REALE di un provider OpenAI-compat (Mistral), con `error.code`
    /// macchina e `error.message` leggibile accanto a campi di contorno.
    const BODY_429: &str = r#"{"error":{"message":"Requests rate limit exceeded","type":"invalid_request_error","code":"rate_limit_exceeded","param":null},"request_id":"1f0c9e"}"#;

    #[tokio::test]
    async fn il_corpo_derrore_porta_la_frase_e_conserva_il_contratto_storico() {
        // LA CONSEGUENZA: dal body grezzo del provider, lungo la catena vera
        // (complete -> CallFailure -> ProviderFailure -> PipelineError -> JSON),
        // deve uscire un corpo in cui `user_message` e' una FRASE che nomina
        // provider, modello e motivo — e in cui il blob e' confinato in
        // `user_detail`. Prima esisteva solo `error`, che il blob lo portava
        // dentro.
        let p = RealBodyProvider::new("mistral", 429, BODY_429);
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "mistral-small-latest".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();
        let body = err.to_body();

        let user = body["user_message"].as_str().expect("user_message presente");
        assert!(
            !user.contains('{') && !user.contains("request_id"),
            "il blob e' rientrato nella frase: {user}"
        );
        assert!(
            user.contains("mistral") && user.contains("mistral-small-latest"),
            "la frase non nomina provider e modello: {user}"
        );
        // `error.message` del provider: esisteva gia' come segnale
        // (`structured_message`) ma finiva solo in un campo di tracing.
        assert!(
            user.contains("Requests rate limit exceeded"),
            "la frase del provider si perde per strada: {user}"
        );
        // Il codice macchina del provider, non lo status: e' su questo che il
        // frontend sceglie icona e azione (mai sul testo, regola M).
        assert_eq!(body["user_code"], "provider_rate_limited");
        let detail = body["user_detail"].as_str().expect("user_detail presente");
        assert!(
            detail.contains("429") && detail.contains("request_id"),
            "il tecnico integrale non deve perdersi: {detail}"
        );

        // Contratto storico INVARIATO: `error`, `code` e `details` sono letti da
        // mcp-core (nexus_gateway.rs), dall'adapter del motore e dal neural client.
        assert_eq!(body["code"], "PROVIDER_ERROR");
        assert!(body["error"]
            .as_str()
            .unwrap()
            .starts_with("tutti i provider hanno fallito"));
        assert_eq!(body["details"]["primary_cause"], "transient");
        assert_eq!(body["details"]["failures"][0]["status"], 429);
        // Il modello tentato viaggia anche nei details: senza, un modello
        // deprecato resta invisibile a chi legge la struttura.
        assert_eq!(
            body["details"]["failures"][0]["model"],
            "mistral-small-latest"
        );
    }

    #[tokio::test]
    async fn ogni_corpo_derrore_del_gateway_ha_le_tre_chiavi() {
        // L'INVARIANTE del confine: se anche UNA superficie risponde senza
        // `user_message`, il frontend torna a doversela cavare col testo tecnico
        // — cioe' a classificare per sottostringa. Qui si fissa che non accada,
        // guardia d'ingresso e 504 di budget inclusi.
        let p = RealBodyProvider::new("mistral", 429, BODY_429);
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let cascata = run_fallback(
            &[ResolvedProvider {
                provider: p,
                model: "mistral-small-latest".into(),
            }],
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();

        let (_, budget_body) = request_budget_exceeded_body(240);
        let corpi = [
            PipelineError::invalid_request("messages required").to_body(),
            PipelineError::quota("project", "monthly_limit").to_body(),
            PipelineError::blocked("contenuto riservato").to_body(),
            PipelineError::policy_tier_excluded(Some("openai"), 3, &["ollama".into()], "escluso")
                .to_body(),
            PipelineError::provider("nessun provider sano supporta la trascrizione audio")
                .to_body(),
            cascata.to_body(),
            budget_body.clone(),
        ];
        for body in corpi {
            let user = body["user_message"].as_str().unwrap_or_default();
            assert!(!user.is_empty(), "corpo senza user_message: {body}");
            assert!(
                !user.contains('{'),
                "una struttura e' finita nella frase: {user}"
            );
            assert!(
                !body["user_code"].as_str().unwrap_or_default().is_empty(),
                "corpo senza user_code: {body}"
            );
            assert!(body["error"].is_string(), "`error` storico perso: {body}");
        }

        // Il 504 conserva le due chiavi TOP-LEVEL su cui il motore agentico
        // decide gia' il failover: aggiungere la resa non doveva spostarle.
        assert_eq!(budget_body["code"], "request_budget_exceeded");
        assert_eq!(budget_body["primary_cause"], "request_budget_exceeded");
        assert_eq!(budget_body["budget_seconds"], 240);
        assert_eq!(budget_body["user_code"], "gateway_timeout");
    }

    #[test]
    fn il_fallimento_di_una_singola_chiamata_provider_nomina_chi_e_perche() {
        // Path media/SSE: nessuna cascata, un solo provider. I fatti si estraggono
        // dal ProviderHttpError ancora nella catena anyhow — non dal suo Display,
        // che e' il blob.
        let err: anyhow::Error =
            ProviderHttpError::from_response("openai", 402, r#"{"error":{"message":"Your credit balance is too low","type":"insufficient_quota"}}"#.to_string())
                .into();
        let body = PipelineError::provider_call_failed(
            &vocabolario_di_prova(),
            "openai",
            "gpt-image-1",
            &err,
        ).to_body();
        assert_eq!(body["user_code"], "provider_quota");
        let user = body["user_message"].as_str().unwrap();
        assert!(
            user.contains("openai") && user.contains("gpt-image-1") && !user.contains('{'),
            "frase inutilizzabile: {user}"
        );
        assert!(user.contains("credit balance"), "la frase del provider si perde: {user}");
    }

    #[tokio::test]
    async fn cascata_mista_resta_500_riprovabile() {
        // CONTRO-CASO del 400 deterministico: basta UN fallimento non-client
        // nella cascata (qui billing) e l'aggregato resta 500 -- un retry o un
        // failover piu' tardi possono avere senso, e il contratto storico coi
        // client non si muove. Mutazione: se la condizione all_deterministic
        // degenerasse in "il primo e' client_error", questo test rosseggia.
        let a = FakeProvider::new("deepseek", Behaviour::ErrClient);
        let b: Arc<dyn LlmProvider> = FakeProvider::new("mistral", Behaviour::ErrBilling);
        let resolved = vec![
            ResolvedProvider {
                provider: a,
                model: "deepseek-chat".into(),
            },
            ResolvedProvider {
                provider: b,
                model: "mistral-small-latest".into(),
            },
        ];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.code, "PROVIDER_ERROR");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        let details = err.details.expect("details presenti");
        // La classe per-provider resta integra nei details (regola M).
        assert_eq!(details["failures"][0]["class"], "client_error");
        assert_eq!(details["failures"][1]["class"], "billing");
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
            .await
            .err()
            .unwrap();
        let details = err.details.expect("details presenti");
        assert_eq!(details["primary_cause"], "cooldown_billing");
        assert_eq!(details["failures"][0]["class"], "cooldown_billing");
    }

    /// IL PONTE COL CONSUMATORE (regola O): il json che `run_fallback` compone
    /// davvero viene letto dalla STESSA funzione che mcp-core usera' per
    /// allineare il proprio registro dei cooldown. Un test che costruisse a
    /// mano il blocco `details` proverebbe la propria imitazione, ed e'
    /// esattamente il modo in cui questo confine e' rimasto muto finora: il
    /// residuo esisteva, ma viveva dentro la prosa del messaggio.
    ///
    /// MUTAZIONI che lo fanno rosseggiare, entrambe col difetto reale:
    ///   - `retry_after_seconds` non popolato nel ramo di skip -> `Nessuna`,
    ///     cioe' mcp-core continua a convocare un fornitore che questo gateway
    ///     sta gia' rifiutando;
    ///   - classe di credito trattata come attesa -> `Attesa`, cioe' il credito
    ///     tornerebbe disponibile allo scadere del timer di un altro processo
    ///     invece che quando il probe accerta che c'e'.
    #[tokio::test]
    async fn il_residuo_del_cooldown_arriva_al_consumatore_come_campo() {
        use nexus_types::provider_failure::EsclusioneDichiarata;

        // (a) Attesa: cooldown transitorio attivo, fornitore saltato.
        let p: Arc<dyn LlmProvider> = FakeProvider::new("groq", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "openai/gpt-oss-120b".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.mark_transient_after("groq", None, Some("connessione".into()), Some(1800));
        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();
        let details = err.details.expect("details presenti");
        match EsclusioneDichiarata::dal_blocco_details(Some(&details)) {
            EsclusioneDichiarata::Attesa {
                provider,
                model,
                secondi,
            } => {
                assert_eq!(provider, "groq");
                assert_eq!(
                    model, None,
                    "l'endpoint non risponde: fuori e' il fornitore, e il modello che \
                     viaggia nel blocco dice solo quale si stava per chiamare"
                );
                assert!(
                    (1700..=1800).contains(&secondi),
                    "il residuo dichiarato deve essere quello del registro che lo ha scritto, \
                     non un valore ricalcolato: {secondi}"
                );
            }
            altro => panic!("atteso Attesa con il residuo dichiarato, avuto {altro:?}"),
        }

        // (b) Credito: la durata NON viaggia — a liberarlo e' chi lo verifica.
        let q: Arc<dyn LlmProvider> = FakeProvider::new("anthropic", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: q,
            model: "claude-opus-4-8".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.mark_billing("anthropic", Some("credit balance too low".into()));
        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();
        let details = err.details.expect("details presenti");
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&details)),
            EsclusioneDichiarata::Credito {
                provider: "anthropic".into()
            }
        );

        // (c) IL CASO groq DEL 13/08/2026: il tetto e' del MODELLO, e la portata
        // deve arrivare fino al registro di mcp-core — altrimenti il gateway
        // esclude una coppia e il suo consumatore esclude un fornitore intero,
        // che e' il difetto di partenza spostato di un processo.
        let r: Arc<dyn LlmProvider> = FakeProvider::new("groq", Behaviour::Ok);
        let modello = "openai/gpt-oss-20b";
        let resolved = vec![ResolvedProvider {
            provider: r,
            model: modello.into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.mark_transient_after("groq", Some(modello), Some("429".into()), Some(1424));
        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();
        let details = err.details.expect("details presenti");
        match EsclusioneDichiarata::dal_blocco_details(Some(&details)) {
            EsclusioneDichiarata::Attesa {
                provider,
                model,
                secondi,
            } => {
                assert_eq!(provider, "groq");
                assert_eq!(
                    model.as_deref(),
                    Some(modello),
                    "il tetto e' di questo modello: gli altri modelli groq devono restare \
                     convocabili anche a valle"
                );
                assert!((1300..=1424).contains(&secondi), "residuo: {secondi}");
            }
            altro => panic!("attesa un'esclusione di modello, avuto {altro:?}"),
        }
    }

    /// L'INNESTO, non il criterio (regola O): dal 429 che il fornitore
    /// restituisce fino a cio' che resta scritto nel registro. I test di
    /// `PortataCooldown` provano che il criterio sa rispondere; questo prova che
    /// qualcuno gliela pone, ed e' il tratto in cui il difetto e' vissuto —
    /// `run_fallback` chiamava `mark_transient_after` col solo nome del
    /// fornitore, quindi nessuna portata veniva mai scelta.
    ///
    /// MISURATO il 13/08/2026: groq risponde «Rate limit reached for model
    /// `openai/gpt-oss-20b` ... TPD Limit 200000, Used 199788 ... try again in
    /// 23m44.3s» e groq spariva INTERO per 24 minuti.
    ///
    /// MUTAZIONE: passare `None` invece di `portata.modello(&req.model)` nel
    /// ramo `Transient` di `complete_with_retry` -> il secondo assert cade col
    /// difetto reale (fornitore escluso), e con lui l'ultimo.
    #[tokio::test]
    async fn un_429_di_modello_lascia_il_fornitore_disponibile() {
        let p: Arc<dyn LlmProvider> = FakeProvider::new("groq", Behaviour::ErrRateLimit);
        let modello = "openai/gpt-oss-20b";
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: modello.into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();

        let _ = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await;

        assert!(
            cooldown.is_model_in_cooldown("groq", modello),
            "il modello che ha esaurito il proprio tetto deve restare fuori"
        );
        assert!(
            !cooldown.is_in_cooldown("groq"),
            "il FORNITORE non ha nulla che non va: escluderlo toglie dalla selezione \
             anche i suoi modelli con quota propria"
        );
        assert!(
            !cooldown.is_model_in_cooldown("groq", "llama-3.3-70b"),
            "un altro modello groq resta convocabile"
        );
        assert!(
            cooldown.seconds_remaining_for_model("groq", modello) > 1300,
            "il Retry-After dichiarato (1424s) resta onorato: e' il fix del 10/08 \
             che questo cambiamento non deve disfare"
        );
    }

    /// IL CASO MISURATO il 13/08/2026: OpenRouter rifiuta l'AMMISSIONE perche' la
    /// prenotazione (65536 token, il suo default quando non dichiariamo un tetto)
    /// supera il credito residuo — che c'era: 62.186 token, e sulle 129 righe
    /// registrate arriva a 64.811. Trattato come credito esaurito, quel rifiuto
    /// toglieva il fornitore per SEI ORE.
    ///
    /// Attraversa `run_fallback` e guarda la CONSEGUENZA (il registro, il wire),
    /// non il nome della classe: il difetto stava tutto in cio' che accadeva
    /// dopo la classificazione.
    ///
    /// MUTAZIONE: togliere il ramo openrouter da `normalizza_codice_provider`
    /// -> si ricade su Billing, e cadono il primo e il terzo assert con il
    /// difetto reale (fornitore in cooldown di credito, esclusione propagata).
    #[tokio::test]
    async fn un_402_di_ammissione_non_mette_il_fornitore_in_cooldown() {
        use nexus_types::provider_failure::EsclusioneDichiarata;

        let p: Arc<dyn LlmProvider> = FakeProvider::new("openrouter", Behaviour::ErrOltreIlCredito);
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "qwen/qwen3-235b-a22b-2507".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();

        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .expect_err("il fornitore ha rifiutato");

        assert!(
            !cooldown.is_in_cooldown("openrouter"),
            "il fornitore ha credito e sta servendo: sei ore di cooldown sono il \
             rimedio di un altro problema"
        );
        assert!(!cooldown.is_billing_cooldown("openrouter"));

        let details = err.details.expect("details presenti");
        assert_eq!(
            details["failures"][0]["class"], "request_exceeds_credit",
            "la classe deve dire di che rifiuto si tratta: `billing` manderebbe \
             il consumatore a escludere il fornitore"
        );
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&details)),
            EsclusioneDichiarata::Nessuna,
            "nulla da propagare al registro di mcp-core: il fornitore e' sano"
        );
    }

    /// Body VERBATIM misurato il 17/08/2026 (gpt-5.2-pro, service_tier=flex).
    /// Arriva con `Retry-After: 300`, che e' precisamente cio' che il ramo
    /// Transient onorerebbe: il cooldown durerebbe cinque minuti.
    const BODY_429_CORSIA_PIENA: &str = r#"{"error":{"message":"Flex tier does not have sufficient resources available to fulfill your request. You can try again later in case more resources are available, or change service_tier=default.","type":"resource_unavailable","param":null,"code":"flex_unavailable"}}"#;

    /// LA GUARDIA sulla causa: un 429 della corsia differibile non ritenta e non
    /// mette in cooldown, perche' il fornitore e' sano e al tier standard
    /// avrebbe risposto subito.
    ///
    /// Il caso normale non arriva qui — il driver openai ripiega da se' — ma un
    /// `service_tier` PINNATO dal chiamante ci arriva di proposito, e senza
    /// questa guardia toglierebbe dalla selezione un modello che sta servendo.
    ///
    /// Attraversa `run_fallback` e il corpo REALE, e guarda la CONSEGUENZA (il
    /// registro dei cooldown, il numero di chiamate, il wire), non il nome della
    /// classe: la classe di wire resta `transient` di proposito.
    ///
    /// MUTAZIONE: togliere la guardia `verdetto.causa == FlexCapacity` da
    /// `complete_with_retry` -> il ramo Transient ritenta e marca cooldown,
    /// rossi il primo e il secondo assert col difetto reale.
    #[tokio::test]
    async fn la_corsia_differibile_piena_non_apre_cooldown() {
        let p = RealBodyProvider::new("openai", 429, BODY_429_CORSIA_PIENA);
        let contatore = p.clone();
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "gpt-5.2-pro".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();

        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            // `strict = true`: coi retry ABILITATI, cioe' nella condizione in
            // cui il ramo Transient ritenterebbe davvero. Con `false` il test
            // resterebbe verde anche senza la guardia.
            true,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .expect_err("il fornitore ha rifiutato la corsia");

        assert!(
            !cooldown.is_in_cooldown("openai"),
            "il fornitore serve al tier standard: escluderlo toglierebbe dalla \
             selezione anche le richieste ordinarie"
        );
        assert!(
            !cooldown.is_model_in_cooldown("openai", "gpt-5.2-pro"),
            "e nemmeno la coppia: la corsia e' piena, il modello no"
        );
        assert_eq!(
            contatore.calls.load(Ordering::SeqCst),
            1,
            "nessun retry: ripresentarsi nella stessa corsia prende lo stesso \
             rifiuto, e il Retry-After di 300s mangerebbe il budget"
        );

        let details = err.details.expect("details presenti");
        assert_eq!(details["failures"][0]["status"], 429);
    }

    /// Il fornitore SANO che fallisce per una causa deterministica non produce
    /// alcuna esclusione: e' la meta' del criterio che protegge dal danno
    /// opposto — escludere chi non ha nulla che non va, per un errore che
    /// riguarda la richiesta e non lui.
    #[tokio::test]
    async fn un_errore_di_richiesta_non_esclude_il_fornitore() {
        use nexus_types::provider_failure::EsclusioneDichiarata;

        let p: Arc<dyn LlmProvider> = FakeProvider::new("deepseek", Behaviour::ErrClient);
        let resolved = vec![ResolvedProvider {
            provider: p,
            model: "deepseek-chat".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let err = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut Vec::new(),
        )
        .await
        .err()
        .unwrap();
        let details = err.details.expect("details presenti");
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&details)),
            EsclusioneDichiarata::Nessuna,
            "client_error: il fornitore e' sano, escluderlo sarebbe un danno"
        );
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
        let resp = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new()).await.err().unwrap();
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

        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
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
        let err = run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), false, TEST_PER_ATTEMPT, far_deadline(), &mut Vec::new())
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
            complete_with_retry(
                p.as_ref(),
                &req(),
                "slow",
                &cooldown,
                &vocabolario_di_prova(),
                &policy,
                true,
                cap,
                far_deadline(),
                &mut Vec::new(),
            ),
        )
        .await
        .expect("cap per-tentativo NON applicato: la chiamata e' rimasta appesa");

        let Err(err) = res else {
            panic!("un provider appeso non puo' produrre un successo");
        };
        assert_eq!(err.class, "transient", "lento ORA, non rotto");
        assert_eq!(err.code.as_deref(), Some("attempt_timeout"));
    }

    // ── Tentativi consumati e scartati (mig 0701) ───────────────────────────

    /// La risposta DEGENERE e' spesa fatturata: PRIMA della failure il
    /// tentativo entra negli scarti con l'usage REALE del wire e il modello
    /// RISOLTO per quel provider, mentre la chain prosegue e vince il sano.
    ///
    /// MUTAZIONE: togliendo il `scarti.push` dal ramo degenere, `scarti` resta
    /// vuoto e il test dice quale spesa e' tornata invisibile.
    #[tokio::test]
    async fn la_degenere_entra_negli_scarti_con_lusage_reale() {
        let hollow: Arc<dyn LlmProvider> = FakeProvider::new("hollow", Behaviour::Degenerate);
        let sano: Arc<dyn LlmProvider> = FakeProvider::new("sano", Behaviour::Ok);
        let resolved = vec![
            ResolvedProvider {
                provider: hollow,
                model: "m-hollow".into(),
            },
            ResolvedProvider {
                provider: sano,
                model: "m-sano".into(),
            },
        ];
        let cooldown = CooldownManager::new();
        let mut scarti = Vec::new();
        let resp = run_fallback(
            &resolved,
            &cooldown,
            &vocabolario_di_prova(),
            &req(),
            false,
            TEST_PER_ATTEMPT,
            far_deadline(),
            &mut scarti,
        )
        .await
        .expect("la chain deve superare la degenere e chiudere col provider sano");
        assert_eq!(resp.provider_used, "sano");

        assert_eq!(scarti.len(), 1, "un solo tentativo consumato e scartato");
        let s = &scarti[0];
        assert_eq!(s.provider, "hollow");
        assert_eq!(s.model, "m-hollow", "il modello RISOLTO, non quello logico");
        assert!(matches!(
            s.reason,
            nexus_ledger::DiscardReason::DegenerateHollow
        ));
        let usage = s.usage.expect("la degenere ha un usage osservato dal wire");
        assert_eq!(usage.input_tokens, 1_000);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, Some(600));
    }

    /// Il cap per-tentativo scaduto DOPO l'avvio e' un tentativo consumato:
    /// entra negli scarti SENZA usage (nessuna risposta osservata). Il budget
    /// esaurito PRIMA di tentare invece non scarta nulla: nessuna chiamata e'
    /// partita.
    #[tokio::test]
    async fn il_cap_scaduto_entra_negli_scarti_senza_usage() {
        let p = FakeProvider::new("slow", Behaviour::Hang);
        let cooldown = CooldownManager::new();
        let policy = cooldown.retry_policy();
        let mut scarti = Vec::new();

        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            complete_with_retry(
                p.as_ref(),
                &req(),
                "slow",
                &cooldown,
                &vocabolario_di_prova(),
                &policy,
                true,
                std::time::Duration::from_millis(50),
                far_deadline(),
                &mut scarti,
            ),
        )
        .await
        .expect("cap per-tentativo NON applicato");
        assert!(res.is_err());

        assert_eq!(scarti.len(), 1);
        let s = &scarti[0];
        assert_eq!(s.provider, "slow");
        assert!(matches!(
            s.reason,
            nexus_ledger::DiscardReason::AttemptTimeout
        ));
        assert!(
            s.usage.is_none(),
            "nessuna risposta osservata: la riga resta a zero per dichiarazione"
        );

        // Budget esaurito PRIMA di tentare: deadline gia' passata -> nessuno
        // scarto nuovo (nessuna chiamata avviata).
        let mut scarti2 = Vec::new();
        let res2 = complete_with_retry(
            p.as_ref(),
            &req(),
            "slow",
            &cooldown,
            &vocabolario_di_prova(),
            &policy,
            true,
            std::time::Duration::from_millis(50),
            tokio::time::Instant::now(),
            &mut scarti2,
        )
        .await;
        assert!(res2.is_err());
        assert!(
            scarti2.is_empty(),
            "un tentativo mai avviato non e' spesa: non si registra"
        );
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
                &vocabolario_di_prova(),
                &req(),
                false,
                std::time::Duration::from_millis(50),
                far_deadline(),
                &mut Vec::new(),
            ),
        )
        .await
        .expect("il provider appeso ha bloccato la chain: cap non applicato")
        .expect("la chain deve raggiungere il provider sano");
        assert_eq!(resp.provider_used, "sano");
    }

    // ── Budget della richiesta: le attese della chain non possono sforarlo ────
    // (incidente figure 2026-07-14: Vertex 429 RESOURCE_EXHAUSTED con Retry-After
    // dentro il tetto ma oltre il budget -> il gateway dormiva, il client moriva
    // di timeout senza MAI ricevere l'errore strutturato su cui failovare.)

    #[tokio::test]
    async fn retry_after_dentro_il_tetto_ma_oltre_il_budget_fallisce_subito() {
        // 429 con Retry-After=40s: sotto il tetto (45s) quindi il SOLO check
        // storico avrebbe dormito 40s. Budget residuo 3s -> deve arrendersi al
        // primo tentativo con l'errore strutturato del provider.
        let p = FakeProvider::always_rate_limited("google", 40);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        // Rete di sicurezza FUORI dalla funzione sotto esame: se il fix viene
        // neutralizzato il test fallisce NETTO qui (niente sleep di 40s).
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, deadline, &mut Vec::new()),
        )
        .await
        .expect("il gateway ha dormito oltre il budget della richiesta")
        .err()
        .unwrap();
        assert!(err.message.contains("tutti i provider hanno fallito"));
        assert_eq!(
            p.calls.load(Ordering::SeqCst),
            1,
            "un solo tentativo: l'attesa di 40s non sta nel budget di 3s"
        );
        // Il cooldown c'e' ed e' sulla COPPIA: un 429 e' un tetto del modello
        // (portata introdotta il 13/08/2026 col caso groq). La domanda giusta
        // qui e' quella che porra' chi sta per instradare questa richiesta.
        assert!(cooldown.is_model_in_cooldown("google", "gemini"));
    }

    #[tokio::test]
    async fn budget_esaurito_niente_tentativo_e_niente_cooldown() {
        // Deadline gia' scaduta: nessun tentativo va nemmeno avviato e il
        // provider NON va punito con un cooldown (non ha colpe).
        let p = FakeProvider::new("google", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        let deadline = tokio::time::Instant::now();
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, deadline, &mut Vec::new()),
        )
        .await
        .expect("budget esaurito: la chain deve rispondere subito")
        .err()
        .unwrap();
        assert!(err.message.contains("tutti i provider hanno fallito"));
        assert_eq!(p.calls.load(Ordering::SeqCst), 0, "nessuna chiamata al provider");
        assert!(
            !cooldown.is_in_cooldown("google"),
            "budget nostro esaurito != colpa del provider"
        );
    }

    #[tokio::test]
    async fn cooldown_breve_strict_oltre_il_budget_non_attende() {
        // Provider in cooldown transitorio (residuo ~30s, sotto il tetto 45s):
        // il SOLO check storico avrebbe dormito il residuo. Budget 2s -> deve
        // propagare subito la failure "cooldown" senza attendere.
        let p = FakeProvider::new("google", Behaviour::Ok);
        let resolved = vec![ResolvedProvider {
            provider: p.clone(),
            model: "gemini".into(),
        }];
        let cooldown = CooldownManager::new();
        cooldown.set_fast_for_test();
        cooldown.mark_transient("google", None, Some("test".into()));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_fallback(&resolved, &cooldown, &vocabolario_di_prova(), &req(), true, TEST_PER_ATTEMPT, deadline, &mut Vec::new()),
        )
        .await
        .expect("il gateway ha atteso un cooldown oltre il budget della richiesta")
        .err()
        .unwrap();
        assert!(err.message.contains("cooldown"));
        assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    }
}
