//! Classificazione intent: SOLO interpretazione semantica via classifier LLM
//! in-process (`intent_classifier::classify`, che parla col Nexus Gateway).
//! Niente keyword matching / promozione / fallback deterministico: quando l'LLM
//! non risponde si usa l'intent di sistema neutro `agentic_default` e lo si
//! DICHIARA (`fallback_used`), cosi' i consumatori a valle sanno che la classe
//! non e' attendibile.
//!
//! Non esiste piu' una scelta di motore: il ramo storico chiamava l'endpoint
//! brain `/classify-intent-agentic` e il brain e' stato eliminato (mig
//! 0462/0532). Il flag `routing.classifier_engine` (mig 0458/0460) e' rimosso.

use sqlx::PgPool;

use crate::nexus_gateway::NexusGatewayClient;

/// Mappa l'output del classifier (`AgenticIntent`) sulla `ClassifiedIntent`
/// attesa dai call site di routing. Gli intent fuori enum della matrix cadono
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
        classifier_resolved: !ai.fallback_used,
        complexity: ai.complexity,
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
    /// dal classifier LLM. Vuoto quando il classifier non ha risolto (fallback
    /// di sistema): il "classifier keyword fallback" che li lasciava vuoti non
    /// esiste piu' da quando l'interpretazione e' solo semantica.
    /// Quando `slots.is_complete()` E `slots.confidence >= 0.60`, il
    /// router prova prima la `nexus_routing_slots_matrix` (mig 0133), e
    /// cade sul routing classico (intent, behavior_mode) se non c'e' match.
    pub slots: crate::routing_slots::ActionSlots,
    /// Complessita' del task giudicata dal classifier LLM (`low`/`medium`/`high`).
    /// Consumata dalla DECISIONE AGENTICA di convocare consiglio/multi-provider
    /// (regola M: segnale strutturato del classificatore, non keyword-match sul
    /// testo). `medium` sul fallback neutro.
    pub complexity: String,
    /// `true` se l'interpretazione viene DAVVERO dal classifier LLM; `false` sul
    /// fallback neutro (LLM down/timeout/JSON invalido). I gate a valle usano il
    /// giudizio LLM solo se `true`, altrimenti degradano al percorso keyword.
    pub classifier_resolved: bool,
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
                intent, classifier_source, classifier_confidence,
                selected_provider, selected_model, decision_source, rationale,
                no_capable_provider, providers_in_cooldown, fallback_triggered)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        )
        .bind(&p_hash)
        .bind(estimated_tokens)
        .bind(&behavior_mode)
        .bind(&intent)
        // classifier_source e' DEDOTTO dalla soglia di confidenza, non ricevuto
        // dal classificatore: due soli valori possibili, e quello sotto soglia
        // non sa distinguere il percorso keyword dalla promozione agentica.
        .bind(if classifier_confidence >= 0.85 {
            "llm"
        } else {
            "keyword_or_promotion"
        })
        .bind(classifier_confidence)
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
        "ricerca_web" => Some("ricerca_web"),
        // Intent di sistema: non emesso dal classifier LLM, ma assegnato come
        // fallback neutro quando l'LLM non risponde (vedi
        // classify_intent_async_with_threshold). Mappato qui per coerenza se
        // mai dovesse transitare per questa funzione.
        "agentic_default" => Some("agentic_default"),
        _ => None,
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

}
