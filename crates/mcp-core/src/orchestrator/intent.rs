//! Classificazione intent: SOLO interpretazione semantica via classifier LLM
//! (endpoint brain `/classify-intent-agentic`). Niente piu' keyword matching /
//! promozione / fallback deterministico: quando l'LLM non risponde si usa
//! l'intent di sistema neutro `agentic_default`.

use sqlx::PgPool;
use std::sync::atomic::Ordering;

use crate::nexus_gateway::NexusGatewayClient;

use super::*;

/// Chiave DB (regola G/L): motore di classificazione intent. Valori ammessi
/// `'python'` (default, endpoint brain `/classify-intent-agentic`) e `'rust'`
/// (in-process `crate::intent_classifier::classify`). Punto unico di scelta:
/// [`select_classifier_engine`]. Migrazione 0458.
const KEY_CLASSIFIER_ENGINE: &str = "routing.classifier_engine";

/// Motore di classificazione intent selezionato dal DB (flag mig 0458). Verso
/// la rimozione della dipendenza HTTP `/classify-intent-agentic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassifierEngine {
    /// Path STORICO: chiamata HTTP all'endpoint brain `/classify-intent-agentic`.
    Python,
    /// Path NATIVO: `crate::intent_classifier::classify` in-process.
    Rust,
}

/// Punto unico (regola L): legge `routing.classifier_engine` dal DB e decide il
/// motore. Default conservativo `Python` (motore stabile) per chiave assente o
/// valore ignoto — niente magic-fallback nascosto, il degrado e' loggato.
pub(crate) async fn select_classifier_engine(db: &PgPool) -> ClassifierEngine {
    match nexus_auth::get_setting(db, KEY_CLASSIFIER_ENGINE).await {
        Some(v) => match v.trim().to_lowercase().as_str() {
            "rust" => ClassifierEngine::Rust,
            "python" => ClassifierEngine::Python,
            other => {
                tracing::warn!(
                    value = %other,
                    "{KEY_CLASSIFIER_ENGINE}: valore non riconosciuto -> motore stabile (python)"
                );
                ClassifierEngine::Python
            }
        },
        None => ClassifierEngine::Python,
    }
}

/// Mappa l'output del classifier RUST (`AgenticIntent`) sulla stessa
/// `ClassifiedIntent` prodotta dal path Python, cosi' i call site di routing non
/// distinguono il motore (regola L). Gli intent fuori enum della matrix cadono
/// sul neutro `agentic_default` (stesso contratto di `intent_str_to_static`).
fn classified_from_rust(ai: crate::intent_classifier::AgenticIntent) -> ClassifiedIntent {
    let intent_static = intent_str_to_static(&ai.intent).unwrap_or("agentic_default");
    let candidates: Vec<IntentCandidate> = if ai.candidates.is_empty() {
        vec![IntentCandidate {
            intent: intent_static.to_string(),
            confidence: ai.confidence,
        }]
    } else {
        ai.candidates
            .into_iter()
            .map(|c| IntentCandidate {
                intent: c.intent,
                confidence: c.confidence,
            })
            .collect()
    };
    ClassifiedIntent {
        intent: intent_static,
        confidence: ai.confidence,
        candidates,
        // Il fallback neutro del classifier rust NON e' incertezza (scelta di
        // sistema): non forziamo disambiguazione su un fallback.
        is_ambiguous: ai.is_ambiguous && !ai.fallback_used,
        slots: ai.slots,
    }
}

/// Classificazione FULL via motore RUST in-process (`intent_classifier::classify`).
/// Ritorna sia la `ClassifiedIntent` (per i call site di routing, parita' col
/// path Python) sia l'`AgenticIntent` grezzo, che porta i dati COMPLETI del
/// turno (`requires_tools`/`agentic_score`/`authorizes_changes`) necessari alla
/// derivazione fedele di `action_oriented`/`report_only` nello shadow (Tappa 1b
/// punto B). `None` per `agentic` quando il path e' Python (dati non disponibili
/// in-process: il brain riclassifica per conto suo).
pub(crate) async fn classify_intent_full_rust(
    db: &PgPool,
    gateway: &NexusGatewayClient,
    message: &str,
) -> (ClassifiedIntent, crate::intent_classifier::AgenticIntent) {
    let ai = crate::intent_classifier::classify(db, gateway, message).await;
    (classified_from_rust(ai.clone()), ai)
}

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

#[cfg(test)]
mod tests_classifier_engine {
    use super::*;
    use crate::intent_classifier::{AgenticIntent, IntentCandidate as RustCandidate};

    fn agentic(intent: &str, is_ambiguous: bool, fallback_used: bool) -> AgenticIntent {
        AgenticIntent {
            intent: intent.to_string(),
            agentic_score: 0.7,
            requires_tools: true,
            complexity: "medium".to_string(),
            confidence: 0.88,
            model_used: "x".to_string(),
            cached: false,
            fallback_used,
            authorizes_changes: true,
            candidates: vec![RustCandidate {
                intent: intent.to_string(),
                confidence: 0.88,
            }],
            is_ambiguous,
            slots: crate::routing_slots::ActionSlots::default(),
        }
    }

    #[test]
    fn classified_from_rust_mappa_intent_e_candidati() {
        let c = classified_from_rust(agentic("fix", false, false));
        assert_eq!(c.intent, "fix");
        assert!((c.confidence - 0.88).abs() < 1e-6);
        assert_eq!(c.candidates.len(), 1);
        assert_eq!(c.candidates[0].intent, "fix");
        assert!(!c.is_ambiguous);
    }

    #[test]
    fn classified_from_rust_intent_fuori_enum_cade_su_neutro() {
        let c = classified_from_rust(agentic("banana", false, false));
        assert_eq!(c.intent, "agentic_default");
    }

    #[test]
    fn classified_from_rust_fallback_non_e_ambiguo() {
        // is_ambiguous=true MA fallback_used=true (scelta di sistema): non si
        // forza disambiguazione su un fallback neutro.
        let c = classified_from_rust(agentic("agentic_default", true, true));
        assert!(!c.is_ambiguous, "fallback di sistema -> non ambiguo");
    }

    /// Tabella settings minimale (gli `#[sqlx::test]` del crate creano lo schema
    /// a mano: le migrazioni non sono applicate automaticamente).
    async fn create_settings_table(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS settings ( \
                 key      TEXT PRIMARY KEY, \
                 value    TEXT NOT NULL, \
                 category TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create table settings");
    }

    #[sqlx::test]
    async fn select_engine_default_python_se_setting_assente(pool: sqlx::PgPool) {
        create_settings_table(&pool).await;
        // Nessuna riga settings -> default conservativo python.
        assert_eq!(select_classifier_engine(&pool).await, ClassifierEngine::Python);
    }

    #[sqlx::test]
    async fn select_engine_legge_rust_e_python_dal_db(pool: sqlx::PgPool) {
        create_settings_table(&pool).await;
        sqlx::query(
            "INSERT INTO settings (key, value, category) \
             VALUES ('routing.classifier_engine', 'rust', 'routing')",
        )
        .execute(&pool)
        .await
        .expect("insert setting rust");
        assert_eq!(select_classifier_engine(&pool).await, ClassifierEngine::Rust);

        sqlx::query(
            "UPDATE settings SET value = 'python' WHERE key = 'routing.classifier_engine'",
        )
        .execute(&pool)
        .await
        .expect("update setting python");
        assert_eq!(
            select_classifier_engine(&pool).await,
            ClassifierEngine::Python
        );

        // Valore ignoto -> degrado conservativo python.
        sqlx::query(
            "UPDATE settings SET value = 'boh' WHERE key = 'routing.classifier_engine'",
        )
        .execute(&pool)
        .await
        .expect("update setting ignoto");
        assert_eq!(
            select_classifier_engine(&pool).await,
            ClassifierEngine::Python
        );
    }
}
