//! Classificazione intent: SOLO interpretazione semantica via classifier LLM
//! (endpoint brain `/classify-intent-agentic`). Niente piu' keyword matching /
//! promozione / fallback deterministico: quando l'LLM non risponde si usa
//! l'intent di sistema neutro `agentic_default`.

use sqlx::PgPool;
use std::sync::atomic::Ordering;



use super::*;

// Le funzioni di interpretazione keyword-based dell'intent sono state RIMOSSE
// (classify_intent_local, is_risky_task, is_agentic_request,
// is_test_failure_resolution, classify_intent_with_agentic_promotion,
// deterministic_intent_fallback): l'interpretazione del testo e' ora SOLO
// semantica (classifier LLM). Quando l'LLM non risponde si usa l'intent
// neutro `agentic_default` (vedi classify_intent_async_with_threshold), che
// attiva lato agente il _LAZY_MINIMAL_TOOLKIT.

/// Risultato del classifier LLM (Fase 2). Specchia il JSON dell'endpoint
/// `POST /classify-intent-agentic` esposto da `brain/grpc_server/main.py`.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct AgenticIntentResponse {
    intent: String,
    agentic_score: f32,
    #[expect(
        dead_code,
        reason = "campo obbligatorio del contratto JSON di POST /classify-intent-agentic: senza serde(default) la deserializzazione valida la risposta del brain"
    )]
    requires_tools: bool,
    #[expect(
        dead_code,
        reason = "campo obbligatorio del contratto JSON di POST /classify-intent-agentic: senza serde(default) la deserializzazione valida la risposta del brain"
    )]
    complexity: String,
    confidence: f32,
    #[expect(
        dead_code,
        reason = "campo obbligatorio del contratto JSON di POST /classify-intent-agentic: senza serde(default) la deserializzazione valida la risposta del brain"
    )]
    model_used: String,
    #[serde(default)]
    cached: bool,
    #[serde(default)]
    fallback_used: bool,
    /// Top 3 candidati sortati per confidence DESC. Sempre contiene almeno
    /// `intent` come primo elemento.
    #[serde(default)]
    candidates: Vec<IntentCandidate>,
    /// True se il classifier ritiene la decisione ambigua (confidence < 0.70
    /// oppure margine sul secondo candidato < 0.15). Quando true il caller
    /// dovrebbe chiedere disambiguazione all'utente invece di indovinare.
    #[serde(default)]
    is_ambiguous: bool,
    /// Slot canonici per routing slot-based (Livello 4 NLU, mig 0133).
    /// Se `slots.is_complete()` E `slots.confidence >= soglia`, il caller
    /// usa `nexus_routing_slots_matrix` come fonte primaria di routing
    /// (piu' specifica della classica (intent, behavior_mode)).
    #[serde(default)]
    slots: crate::routing_slots::ActionSlots,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct IntentCandidate {
    pub intent: String,
    pub confidence: f32,
}

/// Risultato esteso di classificazione: oltre a (intent, confidence) include
/// candidati alternativi, flag di ambiguita' E slot canonici per supportare
/// disambiguazione + routing slot-based (best practice NLU: Rasa/Dialogflow/LUIS).
#[derive(Debug, Clone)]
pub struct ClassifiedIntent {
    pub intent: &'static str,
    pub confidence: f32,
    pub candidates: Vec<IntentCandidate>,
    pub is_ambiguous: bool,
    /// Slot canonici (action_verb, target_type, framework, scope) estratti
    /// dal classifier LLM. Vuoto se il classifier keyword fallback e' stato
    /// usato. Quando `slots.is_complete()` E `slots.confidence >= 0.60`, il
    /// router prova prima la `nexus_routing_slots_matrix` (mig 0133), e
    /// cade sul routing classico (intent, behavior_mode) se non c'e' match.
    pub slots: crate::routing_slots::ActionSlots,
}

/// Soglia di confidence default sotto la quale ignoriamo la classificazione LLM
/// e usiamo il fallback keyword. Override DB: `settings.routing.llm_classifier_min_confidence`
/// caricato in `RoutingThresholds`. Questa costante e' usata solo dal path che
/// non passa per `Orchestrator` (es. test isolati).
pub(crate) const LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT: f32 = 0.60;

// ── Telemetria routing (mig 0112) ───────────────────────────────────────────

/// Calcola sha256(message[:1000]) per la telemetria. Non e' PII e ci permette
/// di fare GROUP BY prompt_hash sulla tabella nexus_routing_decisions per
/// vedere prompt ricorrenti / drift del classifier.
pub(crate) fn prompt_hash(message: &str) -> String {
    use sha2::{Digest, Sha256};
    let head: String = message.chars().take(1000).collect();
    let mut hasher = Sha256::new();
    hasher.update(head.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Fire-and-forget INSERT in `nexus_routing_decisions`. Spawna un task tokio
/// per non aggiungere latenza al path caldo. Eventuali errori sono loggati WARN.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_routing_decision_insert(
    db: PgPool,
    message: &str,
    estimated_tokens: i32,
    behavior_mode: &str,
    intent: &str,
    classifier_confidence: f32,
    selected_provider: &str,
    selected_model: &str,
    decision_source: &str,
    rationale: &str,
    no_capable_provider: bool,
    providers_in_cooldown: &[String],
) {
    let p_hash = prompt_hash(message);
    let behavior_mode = behavior_mode.to_string();
    let intent = intent.to_string();
    let selected_provider = selected_provider.to_string();
    let selected_model = selected_model.to_string();
    let decision_source = decision_source.to_string();
    let rationale = rationale.to_string();
    let cooldown: Vec<String> = providers_in_cooldown.to_vec();

    tokio::spawn(async move {
        let res = sqlx::query(
            r#"INSERT INTO nexus_routing_decisions
               (prompt_hash, estimated_tokens, behavior_mode,
                intent, classifier_source, classifier_confidence, classifier_cached,
                selected_provider, selected_model, decision_source, rationale,
                no_capable_provider, providers_in_cooldown, fallback_triggered)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
        )
        .bind(&p_hash)
        .bind(estimated_tokens)
        .bind(&behavior_mode)
        .bind(&intent)
        // classifier_source: per ora derivato (LLM se confidence > soglia,
        // altrimenti keyword/promotion). Fase 4 separera' i flussi esplicitamente.
        .bind(if classifier_confidence >= 0.85 {
            "llm"
        } else {
            "keyword_or_promotion"
        })
        .bind(classifier_confidence)
        .bind::<Option<bool>>(None) // classifier_cached: non noto a questo livello
        .bind(&selected_provider)
        .bind(&selected_model)
        .bind(&decision_source)
        .bind(&rationale)
        .bind(no_capable_provider)
        .bind(&cooldown)
        .bind(no_capable_provider) // fallback_triggered = no_capable_provider
        .execute(&db)
        .await;
        if let Err(e) = res {
            tracing::warn!("routing telemetry insert failed: {e}");
        }
    });
}

/// Mappa stringa di intent dal classifier LLM al `&'static str` usato dalla
/// matrice di routing. Solo intent ammessi ritornano `Some`; valori sconosciuti
/// fanno `None` cosi' il caller cade sul fallback keyword.
pub(crate) fn intent_str_to_static(intent: &str) -> Option<&'static str> {
    match intent {
        "chat" => Some("chat"),
        "debug" => Some("debug"),
        "fix" => Some("fix"),
        "refactor" => Some("refactor"),
        "test" => Some("test"),
        "docs" => Some("docs"),
        "architecture" => Some("architecture"),
        "file_ops" => Some("file_ops"),
        "system_admin" => Some("system_admin"),
        "code_read" => Some("code_read"),
        // Intent di sistema: non emesso dal classifier LLM, ma assegnato come
        // fallback neutro quando l'LLM non risponde (vedi
        // classify_intent_async_with_threshold). Mappato qui per coerenza se
        // mai dovesse transitare per questa funzione.
        "agentic_default" => Some("agentic_default"),
        _ => None,
    }
}

/// Classifier asincrono: prova prima il classifier LLM (gemini-flash via brain
/// REST `/classify-intent-agentic`), poi cade su keyword + promozione agentic.
///
/// **Feature flag**: env var `NEXUS_LLM_CLASSIFIER_ENABLED` (default: `true`).
/// Settarla a `false` disabilita la chiamata HTTP e usa solo le keyword
/// (utile per smoke test o se il brain e' down).
///
/// **Timeout**: 3 secondi per la chiamata HTTP. In caso di timeout/errore, il
/// classifier LLM e' cache-first quindi una richiesta precedente identica
/// risponde in <50ms; ma se la cache e' fredda accettiamo il keyword fallback
/// per non bloccare il routing.
///
/// **Trust criteria**: usiamo il risultato LLM solo se:
///   1. La risposta e' arrivata entro il timeout
///   2. `confidence >= LLM_CLASSIFIER_MIN_CONFIDENCE` (default 0.60)
///   3. `fallback_used == false` (il brain stesso non ha fallato)
///   4. L'intent ritornato e' tra quelli noti alla matrix
pub(crate) async fn classify_intent_async_with_threshold(
    message: &str,
    min_confidence: f32,
    timeout_seconds: f32,
) -> (&'static str, f32) {
    // Priorita': env var override > AtomicBool inizializzato dal DB in main.rs.
    let llm_enabled = match std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").as_deref() {
        Ok(v) => !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => LLM_CLASSIFIER_ENABLED.load(Ordering::Relaxed),
    };

    // Niente interpretazione keyword: se non otteniamo una classificazione
    // semantica dall'LLM, ritorniamo l'intent di sistema neutro `agentic_default`.
    // Attiva lato agente il _LAZY_MINIMAL_TOOLKIT (discovery + lettura) + modelli
    // tool-robust, cosi' e' l'LLM dell'agente a interpretare e agire da se'.
    const NEUTRAL: (&str, f32) = ("agentic_default", 0.5);

    if !llm_enabled || message.trim().is_empty() {
        return NEUTRAL;
    }

    let brain_url =
        std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let url = format!(
        "{}/classify-intent-agentic",
        brain_url.trim_end_matches('/')
    );

    // Timeout configurabile via routing.llm_classifier_timeout_seconds (mig 0111).
    // Il classifier Python ha cache TTL 24h, request ripetuta risponde in <50ms.
    let timeout_dur = std::time::Duration::from_millis((timeout_seconds * 1000.0) as u64);
    let http = match reqwest::Client::builder().timeout(timeout_dur).build() {
        Ok(c) => c,
        Err(_) => return NEUTRAL,
    };

    let body = serde_json::json!({ "message": message });
    let resp = match http.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(
                "classifier LLM: HTTP {} — fallback agentic_default",
                r.status()
            );
            return NEUTRAL;
        }
        Err(e) => {
            tracing::debug!("classifier LLM: rete fallita ({e}) — fallback agentic_default");
            return NEUTRAL;
        }
    };

    let parsed: AgenticIntentResponse = match resp.json().await {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("classifier LLM: JSON malformato ({e}) — fallback agentic_default");
            return NEUTRAL;
        }
    };

    if parsed.fallback_used || parsed.confidence < min_confidence {
        tracing::debug!(
            "classifier LLM: scarso (fallback={}, conf={}, threshold={}) — agentic_default",
            parsed.fallback_used,
            parsed.confidence,
            min_confidence
        );
        return NEUTRAL;
    }

    let intent_static = match intent_str_to_static(&parsed.intent) {
        Some(s) => s,
        None => {
            tracing::warn!(
                "classifier LLM: intent sconosciuto '{}' — agentic_default",
                parsed.intent
            );
            return NEUTRAL;
        }
    };

    tracing::info!(
        "classifier LLM: intent={} agentic_score={:.2} confidence={:.2} cached={}",
        intent_static,
        parsed.agentic_score,
        parsed.confidence,
        parsed.cached
    );

    (intent_static, parsed.confidence)
}

/// Variante "full" che ritorna `ClassifiedIntent` (con candidati e flag
/// ambiguita') invece del solo `(intent, confidence)`.
///
/// Best practice NLU: quando `is_ambiguous=true` il caller deve chiedere
/// disambiguazione all'utente prima di scegliere un provider/modello.
///
/// Stesso flusso di `classify_intent_async_with_threshold` ma propaga
/// i campi aggiuntivi del classifier LLM (`candidates`, `is_ambiguous`).
pub(crate) async fn classify_intent_async_full_with_threshold(
    message: &str,
    min_confidence: f32,
    timeout_seconds: f32,
) -> ClassifiedIntent {
    let llm_enabled = match std::env::var("NEXUS_LLM_CLASSIFIER_ENABLED").as_deref() {
        Ok(v) => !matches!(
            v.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => LLM_CLASSIFIER_ENABLED.load(Ordering::Relaxed),
    };

    // Helper: ClassifiedIntent "secco" per l'intent di sistema neutro
    // `agentic_default` (un solo candidato, niente ambiguita'). Usato quando NON
    // otteniamo una classificazione semantica dall'LLM (down/timeout/JSON/brain
    // in fallback). L'agente parte col _LAZY_MINIMAL_TOOLKIT e interpreta da se'.
    let neutral_full = || -> ClassifiedIntent {
        ClassifiedIntent {
            intent: "agentic_default",
            confidence: 0.5,
            candidates: vec![IntentCandidate {
                intent: "agentic_default".to_string(),
                confidence: 0.5,
            }],
            is_ambiguous: false,
            slots: crate::routing_slots::ActionSlots::default(),
        }
    };

    if !llm_enabled || message.trim().is_empty() {
        return neutral_full();
    }

    let brain_url =
        std::env::var("BRAIN_REST_URL").unwrap_or_else(|_| "http://127.0.0.1:8001".to_string());
    let url = format!(
        "{}/classify-intent-agentic",
        brain_url.trim_end_matches('/')
    );

    let timeout_dur = std::time::Duration::from_millis((timeout_seconds * 1000.0) as u64);
    let http = match reqwest::Client::builder().timeout(timeout_dur).build() {
        Ok(c) => c,
        Err(_) => return neutral_full(),
    };

    let body = serde_json::json!({ "message": message });
    let resp = match http.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return neutral_full(),
    };

    let parsed: AgenticIntentResponse = match resp.json().await {
        Ok(p) => p,
        Err(_) => return neutral_full(),
    };

    // Il brain stesso non e' riuscito a classificare con l'LLM: niente keyword,
    // si va sul neutro di sistema.
    if parsed.fallback_used {
        return neutral_full();
    }

    let intent_static = match intent_str_to_static(&parsed.intent) {
        Some(s) => s,
        None => return neutral_full(),
    };

    // Confidence sotto soglia: l'LLM HA interpretato ma e' incerto. Conserviamo
    // intent + candidati e segnaliamo is_ambiguous=true cosi' l'agente chiede
    // disambiguazione (ADR ambiguity), invece di degradare a keyword.
    if parsed.confidence < min_confidence {
        let candidates = if parsed.candidates.is_empty() {
            vec![IntentCandidate {
                intent: intent_static.to_string(),
                confidence: parsed.confidence,
            }]
        } else {
            parsed.candidates
        };
        return ClassifiedIntent {
            intent: intent_static,
            confidence: parsed.confidence,
            candidates,
            is_ambiguous: true,
            slots: parsed.slots,
        };
    }

    ClassifiedIntent {
        intent: intent_static,
        confidence: parsed.confidence,
        candidates: parsed.candidates,
        is_ambiguous: parsed.is_ambiguous,
        slots: parsed.slots,
    }
}
