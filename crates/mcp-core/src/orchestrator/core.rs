//! Implementazione principale di Orchestrator: orchestrazione
//! del run agentico, risoluzione provider, prompt e routing.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use uuid::Uuid;

use mcp_proto::neural::{
    neural_core_service_client::NeuralCoreServiceClient, ClassifyIntentRequest, EmbedTextRequest,
    GenerateAgentTurnRequest, GenerateCompletionRequest, RouteModelRequest,
};

use crate::{
    billing::{self, UsageNumbers},
    domain::OrchestratorAudit,
    nexus_gateway::{intent_to_alias, GwMessage, GwMetadata, GwRequest, NexusGatewayClient},
    provider_cooldown::{is_provider_in_cooldown, put_provider_in_cooldown},
    vector_memory,
};

use super::*;

impl Orchestrator {
    pub fn new(
        neural: NeuralCoreClient,
        template_cache: crate::prompt_templates::TemplateCache,
        routing_matrix: crate::routing_matrix::RoutingMatrixCache,
        routing_thresholds: crate::routing_config::RoutingThresholdsCache,
        intent_capability: crate::routing_config::IntentCapabilityCache,
        slots_matrix: crate::routing_slots::SlotsRoutingMatrixCache,
    ) -> Self {
        Self {
            neural,
            template_cache,
            nexus_gateway: None,
            routing_matrix,
            routing_thresholds,
            intent_capability,
            slots_matrix,
        }
    }

    pub fn with_gateway(mut self, gw: NexusGatewayClient) -> Self {
        self.nexus_gateway = Some(gw);
        self
    }

    pub async fn neural_healthy(&self) -> bool {
        self.neural.is_healthy().await
    }

    /// Classifier intent che usa le soglie da DB (mig 0111). Sostituisce le
    /// chiamate a `classify_intent_async(message)` nei call site di routing.
    /// Se la cache `routing_thresholds` non e' disponibile, fallback ai default.
    async fn classify_intent_with_db_thresholds(&self, message: &str) -> (&'static str, f32) {
        let (min_conf, timeout_s) = match self.routing_thresholds.current_async().await {
            Ok(t) => (
                t.llm_classifier_min_confidence,
                t.llm_classifier_timeout_seconds,
            ),
            Err(_) => (LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT, 5.0),
        };
        classify_intent_async_with_threshold(message, min_conf, timeout_s).await
    }

    /// Variante "full" che ritorna `ClassifiedIntent` con candidati + flag
    /// ambiguita'. Usata da `spawn_agent_run` per decidere se chiedere
    /// disambiguazione all'utente (best practice NLU).
    pub async fn classify_intent_full(&self, message: &str) -> ClassifiedIntent {
        let (min_conf, timeout_s, det_high, det_min) =
            match self.routing_thresholds.current_async().await {
                Ok(t) => (
                    t.llm_classifier_min_confidence,
                    t.llm_classifier_timeout_seconds,
                    t.intent_deterministic_high,
                    t.intent_deterministic_min,
                ),
                Err(_) => (
                    LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT,
                    5.0,
                    INTENT_DETERMINISTIC_HIGH_DEFAULT,
                    INTENT_DETERMINISTIC_MIN_DEFAULT,
                ),
            };

        // Classificatore deterministico (keyword/pattern). Calcolato una volta
        // e riusato sia come pre-check sia come fallback se l'LLM fallisce.
        let deterministic = deterministic_intent_fallback(message);

        // (a) PRE-CHECK: se il deterministico e' confidente >= soglia alta,
        // saltiamo del tutto l'LLM. E' piu' veloce e, soprattutto, robusto:
        // un task agentico evidente ("Crea l'applicazione descritta nel file")
        // viene instradato al path agent anche se il classifier LLM e' down.
        if let Some((det_intent, det_conf)) = deterministic {
            if det_conf >= det_high {
                tracing::info!(
                    "classify_intent: deterministic match intent={} conf={:.2} (pre-check, LLM saltato)",
                    det_intent, det_conf
                );
                return ClassifiedIntent {
                    intent: det_intent,
                    confidence: det_conf,
                    candidates: vec![IntentCandidate {
                        intent: det_intent.to_string(),
                        confidence: det_conf,
                    }],
                    is_ambiguous: false,
                    slots: crate::routing_slots::ActionSlots::default(),
                };
            }
        }

        // (b) Path normale: prova l'LLM. Se ritorna un risultato valido lo usa.
        let llm_result =
            classify_intent_async_full_with_threshold(message, min_conf, timeout_s).await;

        // L'LLM e' considerato "non utile" quando ricade su `chat` con
        // confidence sotto la soglia minima del deterministico: in quel caso
        // un eventuale match deterministico (anche a confidence media) e' piu'
        // affidabile per non perdere il path agent. Questo copre sia il caso
        // di vero fallimento HTTP (gia' degradato a keyword internamente) sia
        // il caso di LLM che risponde "chat" su un task chiaramente agentico.
        let llm_degraded_to_chat = llm_result.intent == "chat";
        if llm_degraded_to_chat {
            if let Some((det_intent, det_conf)) = deterministic {
                if det_intent != "chat" && det_conf >= det_min {
                    tracing::warn!(
                        "classify_intent: LLM ha prodotto chat (conf={:.2}), uso fallback deterministico intent={} conf={:.2}",
                        llm_result.confidence, det_intent, det_conf
                    );
                    return ClassifiedIntent {
                        intent: det_intent,
                        confidence: det_conf,
                        candidates: vec![IntentCandidate {
                            intent: det_intent.to_string(),
                            confidence: det_conf,
                        }],
                        is_ambiguous: false,
                        slots: crate::routing_slots::ActionSlots::default(),
                    };
                }
            }
        }

        llm_result
    }

    /// Routing slot-first (Livello 4 NLU): se il classifier ha estratto slot
    /// validi con confidence sufficiente, tenta lookup nella `nexus_routing_slots_matrix`
    /// (mig 0133). In caso di no-match o slot incompleti, ritorna `None` e il
    /// caller fa fallback al routing classico `(intent, behavior_mode)`.
    ///
    /// `min_slot_confidence`: soglia sopra la quale fidarsi degli slot.
    /// Tipicamente 0.60 — sotto questa soglia il classifier "non e' sicuro"
    /// di action_verb/scope e meglio cadere sul routing classico testato.
    ///
    /// Ritorna `Some((provider, model, rationale))` dove rationale spiega
    /// la decisione (utile per audit telemetria + UI debug).
    pub async fn route_by_slots(
        &self,
        slots: &crate::routing_slots::ActionSlots,
        min_slot_confidence: f32,
    ) -> Option<(String, String, &'static str)> {
        if !slots.is_complete() {
            return None;
        }
        if !slots.meets_confidence(min_slot_confidence) {
            tracing::debug!(
                "route_by_slots: confidence {:.2} < soglia {:.2}, fallback intent classico",
                slots.confidence,
                min_slot_confidence
            );
            return None;
        }
        let matrix = self.slots_matrix.current_async().await?;
        // Cooldown-awareness: scorri la chain di candidati (priority DESC) e
        // ritorna il primo provider NON in cooldown. Se tutti i provider della
        // chain matrice slots sono in cooldown, ritorna None → fallback al
        // routing classico (che ha la propria cooldown chain).
        let chain = matrix.lookup_chain(slots);
        if chain.is_empty() {
            tracing::debug!(
                "route_by_slots: nessun match per ({}, {}, {}, {}) in matrix",
                slots.action_verb,
                slots.target_type,
                slots.framework,
                slots.scope,
            );
            return None;
        }
        let mut skipped: Vec<String> = Vec::new();
        for (provider, model) in &chain {
            if crate::provider_cooldown::is_provider_in_cooldown(provider) {
                skipped.push(provider.clone());
                continue;
            }
            if !skipped.is_empty() {
                tracing::info!(
                    "route_by_slots: skip provider in cooldown [{}], scelto {}/{} (chain pos {}/{})",
                    skipped.join(","), provider, model,
                    skipped.len() + 1, chain.len(),
                );
            } else {
                tracing::info!(
                    "route_by_slots: slots=({}, {}, {}, {}) → {}/{}",
                    slots.action_verb,
                    slots.target_type,
                    slots.framework,
                    slots.scope,
                    provider,
                    model,
                );
            }
            return Some((provider.clone(), model.clone(), "slots_matrix"));
        }
        // Tutti i provider della chain matrice sono in cooldown.
        tracing::warn!(
            "route_by_slots: TUTTI i {} provider della chain in cooldown [{}], fallback intent classico",
            chain.len(), skipped.join(",")
        );
        None
    }

    /// Helper unico: estrae preferred_provider per intent (da nexus_intent_capability,
    /// mig 0110) + TokenThresholds (da settings.routing.*, mig 0111). Usato dai
    /// call site di `route_model_with_mode` per evitare di duplicare il pattern
    /// "leggi cache → estrai → passa".
    async fn routing_helpers_for(&self, intent: &str) -> (Option<String>, TokenThresholds) {
        let preferred = match self.intent_capability.current_async().await {
            Ok(map) => map.preferred_provider_for(intent).map(String::from),
            Err(_) => None,
        };
        let thresholds = match self.routing_thresholds.current_async().await {
            Ok(t) => TokenThresholds::from_routing_thresholds(&t),
            Err(_) => TokenThresholds::defaults(),
        };
        (preferred, thresholds)
    }

    pub async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.neural.embed_text("", text).await
    }

    /// Versione "detailed" di [`resolve_agent_provider`] che restituisce
    /// anche l'intent classificato, la modalita' effettiva e una stringa
    /// di rationale leggibile. Esposta tramite `/api/internal/routing/decide`
    /// in modo che il brain Python possa consultare il routing Rust senza
    /// duplicare la matrice in `service.py::_ROUTING_MATRIX`.
    pub async fn resolve_agent_provider_detailed(
        &self,
        db: &PgPool,
        _project_id: &str,
        _profile_id: &str,
        message: &str,
        provider_override: Option<&str>,
        model_override: Option<&str>,
        context_message_count: usize,
        // Modalita' scelta per la singola sessione (es. dal dropdown chat).
        // Se `Some`, sovrascrive `nexus_behavior_mode` DB solo per questa chiamata.
        behavior_mode_session: Option<&str>,
    ) -> RoutingResolveResult {
        // Snapshot della routing matrix DB (cache 60s, lock-free clone Arc).
        // Se la matrice non e' caricata (DB down all'avvio), ritorniamo
        // immediatamente un risultato di errore — niente fallback hardcoded.
        let matrix_arc = match self.routing_matrix.current() {
            Ok(m) => m,
            Err(e) => {
                return RoutingResolveResult {
                    provider: String::new(),
                    model: String::new(),
                    intent: "unknown".to_string(),
                    mode: "unknown".to_string(),
                    risky: false,
                    rationale: format!("routing_matrix non disponibile: {e}"),
                    source: "error".to_string(),
                    configured_behavior_mode: "unknown".to_string(),
                    no_capable_provider: true,
                    providers_in_cooldown: vec![],
                    error: Some(format!(
                        "Configurazione routing mancante: {e}. \
                         Applica le migrazioni 0101 e 0102 e popola le tabelle \
                         nexus_routing_matrix / nexus_provider_default_model / nexus_purpose_model."
                    )),
                };
            }
        };
        let matrix = &*matrix_arc;
        // Risolve il behavior_mode effettivo: sessione > DB globale.
        // Caricato prima di resolve_agent_provider per passarlo coerentemente.
        let routing_for_mode = Self::load_routing_config(db).await.unwrap_or_default();
        let configured_behavior_mode = behavior_mode_session
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| routing_for_mode.behavior_mode.clone());

        let (provider, model) = self
            .resolve_agent_provider(
                db,
                _project_id,
                _profile_id,
                message,
                provider_override,
                model_override,
                context_message_count,
                Some(&configured_behavior_mode),
            )
            .await;
        // Riclassifica via classifier LLM (gemini-flash, cache 24h) con fallback
        // keyword + promozione agentic. Vedi `classify_intent_async`.
        let (intent, confidence) = self.classify_intent_with_db_thresholds(message).await;
        let risky = is_risky_task(message);
        let effective_mode = if risky && configured_behavior_mode != "approfondita" {
            "approfondita".to_string()
        } else if configured_behavior_mode == "dinamico" {
            "bilanciata".to_string()
        } else {
            configured_behavior_mode.clone()
        };

        // Deduci la sorgente della decisione confrontando con i percorsi noti.
        // Non e' una telemetria diretta del flusso (richiederebbe restituire
        // il dato dalla resolve_agent_provider) ma una ricostruzione coerente:
        //   - se l'utente ha forzato un override, source = "override"
        //   - se behavior_mode e' "dinamico" e il task NON e' rischioso e il
        //     model non corrisponde a quello della matrix statica, source = "catalog"
        //   - altrimenti source = "matrix"
        let source: &'static str = if provider_override.filter(|v| !v.trim().is_empty()).is_some() {
            "override"
        } else if configured_behavior_mode == "dinamico" && !risky {
            // In modalita' dinamica non rischiosa il catalogo prezzi e' autoritativo.
            // Verifichiamo: se il modello scelto NON e' quello della matrix per
            // (intent, "bilanciata"), allora il catalogo lo ha sovrascritto.
            let (pref, thr) = self.routing_helpers_for(intent).await;
            let matrix_default =
                route_model_with_mode(matrix, intent, 1500, "bilanciata", pref.as_deref(), &thr);
            if matrix_default.model != model {
                "catalog"
            } else {
                "matrix"
            }
        } else {
            "matrix"
        };

        // Calcola lista provider in cooldown: ci serve sia per il flag
        // `no_capable_provider` sia per mostrare al frontend un alert
        // "questi provider non sono disponibili" piuttosto che far girare
        // a vuoto le richieste.
        let known_providers = ["anthropic", "openai", "deepseek", "google", "mistral"];
        let providers_in_cooldown: Vec<String> = known_providers
            .iter()
            .filter(|p| is_provider_in_cooldown(p))
            .map(|p| p.to_string())
            .collect();
        // Nessun provider capable = il provider scelto e' lui stesso in
        // cooldown (succede quando tutti gli altri della hierarchy sono
        // anch'essi in cooldown e l'algoritmo riusa l'originale).
        let no_capable_provider = is_provider_in_cooldown(&provider)
            || providers_in_cooldown.len() >= known_providers.len();

        let cooldown_note = if no_capable_provider {
            " ⚠ NESSUN PROVIDER DISPONIBILE — fermarsi e avvertire utente"
        } else if !providers_in_cooldown.is_empty() {
            // Indica all'utente quali provider non sono al momento usabili.
            // Es. "[cooldown:anthropic,openai]"
            ""
        } else {
            ""
        };

        let rationale = format!(
            "intent={} confidence={:.2} mode={}{} source={} → {}/{}{}{}",
            intent,
            confidence,
            effective_mode,
            if risky { " [risky→approfondita]" } else { "" },
            source,
            provider,
            model,
            if !providers_in_cooldown.is_empty() {
                format!(" [cooldown:{}]", providers_in_cooldown.join(","))
            } else {
                String::new()
            },
            cooldown_note,
        );

        // Telemetria fire-and-forget: INSERT in nexus_routing_decisions (mig 0112).
        // Non blocchiamo il path caldo. Errore di insert -> WARN log, decisione
        // di routing comunque restituita.
        // Stima token a partire dal message — necessaria per la telemetria
        // (campo estimated_tokens). resolve_agent_provider_detailed non la
        // calcola altrove: la stima e' veloce (count parole * 2).
        let est_tokens = estimate_complexity(message) as i32;
        spawn_routing_decision_insert(
            db.clone(),
            message,
            est_tokens,
            &configured_behavior_mode,
            intent,
            confidence,
            &provider,
            &model,
            source,
            &rationale,
            no_capable_provider,
            &providers_in_cooldown,
        );

        RoutingResolveResult {
            provider,
            model,
            intent: intent.to_string(),
            mode: effective_mode,
            risky,
            rationale,
            source: source.to_string(),
            configured_behavior_mode,
            no_capable_provider,
            providers_in_cooldown,
            error: None,
        }
    }

    /// Risolve il provider/model ottimale per l'agente basandosi sull'intent del messaggio.
    /// Replica la logica di routing della chat normale: classify_intent → route_model → candidates.
    /// Fallback a (default_provider, default_model) se Neural Core non è disponibile.
    pub async fn resolve_agent_provider(
        &self,
        db: &PgPool,
        _project_id: &str,
        _profile_id: &str,
        message: &str,
        provider_override: Option<&str>,
        model_override: Option<&str>,
        context_message_count: usize,
        // Override del behavior_mode per questa singola chiamata (sessione utente).
        // Se `Some`, sostituisce `routing.behavior_mode` letto dal DB.
        behavior_mode_override: Option<&str>,
    ) -> (String, String) {
        // Snapshot della routing matrix DB (cache 60s, await sul lock se busy).
        // Se la matrice non e' caricata (caso impossibile dopo init() che ha
        // retry-loop + panic), ritorniamo placeholder vuoti — il caller
        // resolve_agent_provider_detailed gia' gestisce questo errore prima.
        let matrix_arc = match self.routing_matrix.current_async().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("resolve_agent_provider: matrix non disponibile: {e}");
                return (String::new(), String::new());
            }
        };
        let matrix = &*matrix_arc;
        // Se l'utente ha forzato un provider specifico, lo rispettiamo.
        // Se ha forzato anche il modello, lo usiamo direttamente senza
        // passare per resolve_model (che applicherebbe override admin).
        if let Some(p) = provider_override.filter(|v| !v.trim().is_empty()) {
            if let Some(m) = model_override.filter(|v| !v.trim().is_empty()) {
                return (p.to_string(), m.to_string());
            }
            let routing = Self::load_routing_config(db).await.unwrap_or_default();
            let model = routing.resolve_model(matrix, p, Some(p), model_override);
            return (p.to_string(), model);
        }

        // Routing locale — zero latenza gRPC
        // estimate_complexity usa solo le prime 200 parole per non farsi ingannare da liste dati
        let base_estimated = estimate_complexity(message);
        // Se la sessione ha già molti messaggi (continuazione di task lungo),
        // alza la stima per evitare di assegnare modelli troppo piccoli.
        // Ogni 10 messaggi = +1000 token equivalenti (cap a 6000).
        let context_bonus = ((context_message_count / 10) as u32 * 1_000).min(6_000);
        let estimated_tokens = base_estimated.saturating_add(context_bonus);
        let (intent, _confidence) = self.classify_intent_with_db_thresholds(message).await;
        // La RoutingConfig admin può sovrascrivere il modello per provider.
        // Il behavior_mode effettivo: override sessione > DB globale.
        let routing = Self::load_routing_config(db).await.unwrap_or_default();
        let effective_behavior_mode: String = behavior_mode_override
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| routing.behavior_mode.clone());
        // Task rischioso ha PRIORITA' assoluta: salta il ramo dinamico (catalogo
        // prezzi sceglie modelli "light" per costo, ma per task distruttivi
        // serve un modello capable). L'override mode -> approfondita applicato
        // PRIMA del ramo dinamico forza la matrix statica.
        let risky_pre = is_risky_task(message);
        // Se behavior_mode == "dinamico" E il task NON e' rischioso, consulta il
        // catalogo prezzi (come fa Orchestrator::run).
        // Altrimenti usa la matrix statica route_model_with_mode.
        // Estratte come String per uniformare i due rami (catalogo restituisce String,
        // matrix restituisce &'static str).
        // Caso speciale "dinamico": il catalogo prezzi è autoritativo.
        // Saltiamo candidates() e provider_models perché altrimenti riordinano sempre
        // sui provider configurati nell'admin (anthropic/openai prima) e applicano
        // il provider_model_<x> override → risultato: il dinamico non sceglie mai nulla.
        if effective_behavior_mode == "dinamico" && !risky_pre {
            // Risolvi tier/capability dalla cache intent_capability (mig 0110)
            // invece dal match Rust statico (rimosso). Se intent non mappato,
            // default light/chat (caso tipico di intent legacy non in seed).
            let icap_arc = self.intent_capability.current_async().await.ok();
            let (base_tier, capability) = match icap_arc.as_deref() {
                Some(map) => match map.get(intent) {
                    Some(c) => (
                        c.tier_for_tokens(estimated_tokens),
                        c.base_capability.clone(),
                    ),
                    None => ("light".to_string(), "chat".to_string()),
                },
                None => {
                    tracing::warn!(
                        "intent_capability cache non disponibile, uso defaults light/chat"
                    );
                    ("light".to_string(), "chat".to_string())
                }
            };
            if let Some(d) = route_model_from_catalog(db, &base_tier, &capability, "dinamico").await
            {
                let provider = d.provider;
                if !is_provider_in_cooldown(&provider) {
                    let model = model_override
                        .filter(|v| !v.trim().is_empty())
                        .map(str::to_string)
                        .unwrap_or(d.model);
                    tracing::info!(
                        "Agent routing (dinamico/catalog): intent={} tokens~{} → {}/{}",
                        intent,
                        estimated_tokens,
                        provider,
                        model
                    );
                    return (provider, model);
                } else {
                    tracing::warn!(
                        "Agent routing: '{}' in cooldown (catalog/dinamico), skip",
                        provider
                    );
                }
            }
            // Catalogo vuoto → cade nel ramo statico bilanciata sotto
        }

        // Override "task rischioso": se il messaggio contiene verbi distruttivi
        // (rm -rf, drop table, docker prune, force push, ecc.), promuoviamo
        // automaticamente il behavior_mode a "approfondita". Motivazione:
        // i modelli leggeri (mistral-small, gpt-4.1-nano) tendono a interpretare
        // liberamente le richieste distruttive (es. "elimina file Docker" ->
        // ricrea i file). Per task ad alto impatto serve un modello capable.
        let effective_mode = if is_risky_task(message) && effective_behavior_mode != "approfondita"
        {
            tracing::info!(
                "Agent routing: task rischioso rilevato (mode {} -> approfondita)",
                effective_behavior_mode
            );
            "approfondita"
        } else if effective_behavior_mode == "dinamico" {
            "bilanciata"
        } else {
            effective_behavior_mode.as_str()
        };

        let (pref_provider, thresholds) = self.routing_helpers_for(intent).await;
        let d = route_model_with_mode(
            matrix,
            intent,
            estimated_tokens,
            effective_mode,
            pref_provider.as_deref(),
            &thresholds,
        );
        let decision_provider = d.provider.to_string();
        let decision_model = d.model.to_string();

        // La matrice gestisce già cooldown e fallback internamente.
        // Usa direttamente provider+model dalla matrice: la decisione
        // (intent, mode) → (provider, model) è specifica e non deve
        // essere sovrascritta dai default generici provider_model_*.
        // Solo se la matrice non ha trovato un provider disponibile
        // (__no_model__), cade sui candidati + default admin.
        let (provider, model) = if decision_provider != "__no_model__" {
            let model = if let Some(m) = model_override.filter(|v| !v.trim().is_empty()) {
                m.to_string()
            } else {
                decision_model.clone()
            };
            (decision_provider, model)
        } else {
            let provider = routing
                .candidates(intent, Some(decision_provider.as_str()))
                .into_iter()
                .find(|p| !is_provider_in_cooldown(p))
                .unwrap_or_else(|| decision_provider.clone());
            let model = model_override
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
                .or_else(|| routing.provider_models.get(&provider).cloned())
                .unwrap_or_else(|| default_model_for_provider(matrix, &provider).to_string());
            (provider, model)
        };

        // Se il provider scelto e' in cooldown (rate-limit recente nel processo), trova alternativa.
        // Il fallback rispetta tier/capability: per task critici (heavy/medium) non degrada
        // silenziosamente a un default generico che potrebbe essere un modello inadeguato.
        let (provider, model) =
            if is_provider_in_cooldown(&provider) {
                tracing::warn!(
                    "Agent routing: '{}' scelto dal routing ma in cooldown, cerco alternativa",
                    provider
                );

                // Risolvi tier/capability dalla cache (stessi valori usati sopra nel routing)
                let icap_arc = self.intent_capability.current_async().await.ok();
                let (tier, cap) = match icap_arc.as_deref() {
                    Some(map) => match map.get(intent) {
                        Some(c) => (
                            c.tier_for_tokens(estimated_tokens),
                            c.base_capability.clone(),
                        ),
                        None => ("light".to_string(), "chat".to_string()),
                    },
                    None => ("light".to_string(), "chat".to_string()),
                };

                // Strategia: cerca nel catalogo un modello dello stesso tier (o un
                // livello sotto) da un provider NON in cooldown. Mantiene la qualita'
                // richiesta per il task — non degrada a default generico.
                let tiers_to_try: Vec<&str> = match tier.as_str() {
                    "heavy" => vec!["heavy", "medium"],
                    "medium" => vec!["medium"],
                    _ => vec!["light"],
                };

                let mut found = None;
                for try_tier in &tiers_to_try {
                    let rows: Vec<(String, String)> = sqlx::query_as(
                        r#"SELECT provider, model FROM ai_price_catalog
                       WHERE is_enabled = TRUE
                         AND performance_tier = $1
                         AND capabilities @> $2::jsonb
                         AND supports_tool_use = TRUE
                       ORDER BY input_cost_per_million_tokens ASC
                       LIMIT 10"#,
                    )
                    .bind(try_tier)
                    .bind(format!("[\"{cap}\"]"))
                    .fetch_all(db)
                    .await
                    .unwrap_or_default();

                    for (alt_provider, alt_model) in &rows {
                        if !is_provider_in_cooldown(alt_provider) {
                            tracing::info!(
                            "Agent routing (cooldown-fallback tier-aware): {} → {}/{} (tier={})",
                            provider, alt_provider, alt_model, try_tier
                        );
                            found = Some((alt_provider.clone(), alt_model.clone()));
                            break;
                        }
                    }
                    if found.is_some() {
                        break;
                    }
                }

                // Ultimo resort: hierarchy classica (se il catalogo non ha nulla)
                found.unwrap_or_else(|| {
                    let hierarchy_str: Option<String> =
                        futures::executor::block_on(async {
                            sqlx::query_scalar(
                        "SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1"
                    ).fetch_optional(db).await.ok().flatten()
                        });
                    let hier: Vec<String> = hierarchy_str
                        .as_deref()
                        .unwrap_or(&provider)
                        .split(',')
                        .map(|s| s.trim().to_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect();
                    let alt = hier
                        .iter()
                        .find(|p| !is_provider_in_cooldown(p))
                        .cloned()
                        .unwrap_or_else(|| provider.clone());
                    let alt_model = default_model_for_provider(matrix, &alt).to_string();
                    tracing::warn!(
                        "Agent routing (cooldown-fallback legacy): {} → {}/{}",
                        provider,
                        alt,
                        alt_model
                    );
                    (alt, alt_model)
                })
            } else {
                (provider, model)
            };

        tracing::info!(
            "Agent routing (local): intent={} tokens~{} → {}/{}",
            intent,
            estimated_tokens,
            provider,
            model
        );

        (provider, model)
    }

    pub async fn run(
        &self,
        db: &PgPool,
        input: OrchestratorRequest,
    ) -> anyhow::Result<OrchestratorResult> {
        let user_id =
            Uuid::parse_str(&input.user_id).map_err(|_| anyhow::anyhow!("invalid_user_id"))?;
        let project_uuid = Uuid::parse_str(&input.project_id)
            .map_err(|_| anyhow::anyhow!("invalid_project_id"))?;
        let run_id = Uuid::new_v4();

        // Snapshot della routing matrix DB (cache 60s, await sul lock).
        // Se la matrice non e' caricata, ritorniamo errore esplicito invece
        // di un fallback nascosto.
        let matrix_arc = self.routing_matrix.current_async().await.map_err(|e| {
            anyhow::anyhow!(
                "routing_matrix non disponibile: {e}. Verifica DB e migrazioni 0101/0102."
            )
        })?;
        let matrix = &*matrix_arc;

        // Step 1 + 2: Routing locale — zero gRPC, zero latenza aggiuntiva
        // Usa estimate_complexity per non farsi ingannare da messaggi con liste dati lunghe
        let msg_tokens_estimate = estimate_complexity(&input.message);
        let (intent_str, _confidence) = self
            .classify_intent_with_db_thresholds(&input.message)
            .await;
        let intent = intent_str.to_string();
        let mut routing = Self::load_routing_config(db).await?;

        // Routing: se modalità "dinamico" usa il catalogo DB, altrimenti la matrice statica
        // Risolvi tier/capability dalla cache intent_capability (mig 0110).
        let icap_arc = self.intent_capability.current_async().await.ok();
        let (base_tier, capability) = match icap_arc.as_deref() {
            Some(map) => match map.get(intent_str) {
                Some(c) => (
                    c.tier_for_tokens(msg_tokens_estimate),
                    c.base_capability.clone(),
                ),
                None => ("light".to_string(), "chat".to_string()),
            },
            None => ("light".to_string(), "chat".to_string()),
        };
        let (suggested_provider, suggested_model): (Option<String>, Option<String>) = if routing
            .behavior_mode
            == "dinamico"
        {
            match route_model_from_catalog(db, &base_tier, &capability, "dinamico").await {
                Some(dyn_decision) if !is_provider_in_cooldown(&dyn_decision.provider) => {
                    tracing::info!(
                        "Dynamic catalog routing: intent={} tokens~{} → {}/{}",
                        intent,
                        msg_tokens_estimate,
                        dyn_decision.provider,
                        dyn_decision.model
                    );
                    (
                        Some(dyn_decision.provider.to_string()),
                        Some(dyn_decision.model.to_string()),
                    )
                }
                other => {
                    if let Some(ref d) = other {
                        tracing::warn!(
                            "Dynamic catalog routing: {}/{} in cooldown, cerco alternativa tier-aware",
                            d.provider, d.model
                        );
                    }
                    // Cerca nel catalogo un modello dello stesso tier (o inferiore)
                    // da un provider NON in cooldown
                    let tiers_to_try: Vec<&str> = match base_tier.as_str() {
                        "heavy" => vec!["heavy", "medium"],
                        "medium" => vec!["medium", "light"],
                        _ => vec!["light"],
                    };
                    let mut catalog_alt = None;
                    for try_tier in &tiers_to_try {
                        let rows: Vec<(String, String)> = sqlx::query_as(
                            r#"SELECT provider, model FROM ai_price_catalog
                               WHERE is_enabled = TRUE
                                 AND performance_tier = $1
                                 AND capabilities @> $2::jsonb
                                 AND supports_tool_use = TRUE
                               ORDER BY input_cost_per_million_tokens ASC
                               LIMIT 10"#,
                        )
                        .bind(try_tier)
                        .bind(format!("[\"{capability}\"]"))
                        .fetch_all(db)
                        .await
                        .unwrap_or_default();

                        for (alt_p, alt_m) in &rows {
                            if !is_provider_in_cooldown(alt_p) {
                                tracing::info!(
                                    "Dynamic catalog routing (cooldown-fallback tier-aware): → {}/{} (tier={})",
                                    alt_p, alt_m, try_tier
                                );
                                catalog_alt = Some((Some(alt_p.clone()), Some(alt_m.clone())));
                                break;
                            }
                        }
                        if catalog_alt.is_some() {
                            break;
                        }
                    }

                    catalog_alt.unwrap_or_else(|| {
                        let (pref, thr) =
                            futures::executor::block_on(self.routing_helpers_for(intent_str));
                        let d = route_model_with_mode(
                            matrix,
                            intent_str,
                            msg_tokens_estimate,
                            "bilanciata",
                            pref.as_deref(),
                            &thr,
                        );
                        tracing::info!(
                            "Dynamic routing fallback to bilanciata: {}/{}",
                            d.provider,
                            d.model
                        );
                        (Some(d.provider.to_string()), Some(d.model.to_string()))
                    })
                }
            }
        } else if routing.behavior_mode == "manuale" {
            // Manuale: nessun routing automatico — usa provider/model da config admin
            let (pref, thr) = self.routing_helpers_for(intent_str).await;
            let d = route_model_with_mode(
                matrix,
                intent_str,
                msg_tokens_estimate,
                "bilanciata",
                pref.as_deref(),
                &thr,
            );
            tracing::info!(
                "Manual routing config: intent={} tokens~{} → {}/{}",
                intent,
                msg_tokens_estimate,
                d.provider,
                d.model
            );
            (Some(d.provider.to_string()), Some(d.model.to_string()))
        } else {
            let (pref, thr) = self.routing_helpers_for(intent_str).await;
            let d = route_model_with_mode(
                matrix,
                intent_str,
                msg_tokens_estimate,
                &routing.behavior_mode,
                pref.as_deref(),
                &thr,
            );
            tracing::info!(
                "Local routing: intent={} tokens~{} mode={} → {}/{}",
                intent,
                msg_tokens_estimate,
                routing.behavior_mode,
                d.provider,
                d.model
            );
            (Some(d.provider.to_string()), Some(d.model.to_string()))
        };
        if let Some(project_chain) =
            Self::load_project_intent_chain(db, project_uuid, &intent).await?
        {
            routing
                .intent_provider_hierarchy
                .insert(intent.clone(), project_chain);
        }
        let forced_provider = input
            .provider_override
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_lowercase());
        let forced_model = input
            .model_override
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let token_budget = routing.resolve_token_budget(Some(msg_tokens_estimate.max(4096)));

        // Step 3: Build optimized prompt
        let context = mcp_token::optimize_context(&input.message, token_budget as usize);
        if context.tokens_saved > 0 {
            tracing::warn!(
                "CONTEXT_DROP: messaggio utente ridotto da {} → {} token (risparmiati: {}) per budget={}",
                context.original_tokens,
                context.optimized_tokens,
                context.tokens_saved,
                token_budget,
            );
        }
        let prompt_corrections = self
            .load_prompt_corrections(db, project_uuid, &input.message)
            .await
            .unwrap_or_default();
        let composed_prompt = Self::compose_prompt(
            db,
            &self.template_cache,
            &context.optimized_prompt,
            &prompt_corrections,
            input.automation_mode,
            &input.attachments,
        )
        .await;

        // ── Step 4: LLM Execution ─────────────────────────────────────────────────
        // PATH A: Nexus Gateway (routing, DLP, rate limiting, fallback automatico)
        // PATH B: Brain gRPC diretto (legacy, usato se il gateway non è disponibile)
        let (provider, model, completion, usage, total_cost, currency) = if let Some(gw) =
            &self.nexus_gateway
        {
            let alias = intent_to_alias(&intent, &routing.behavior_mode, forced_model.as_deref());
            let gw_model = if let Some(fp) = &forced_provider {
                format!("{fp}/{}", forced_model.as_deref().unwrap_or(&alias))
            } else {
                alias
            };
            let gw_req = GwRequest {
                model: gw_model,
                messages: vec![GwMessage {
                    role: "user".to_string(),
                    content: composed_prompt.clone(),
                }],
                max_tokens: Some(token_budget),
                temperature: None,
                tools: None,
                metadata: GwMetadata {
                    tenant_id: input.project_id.clone(),
                    user_id: input.user_id.clone(),
                    request_id: run_id.to_string(),
                    sensitivity_tier: 0,
                    feature: intent.clone(),
                },
            };
            let prompt_tokens = mcp_token::count_tokens(&composed_prompt) as i32;
            let estimated_completion = (token_budget as i32 - prompt_tokens).max(0);
            // Fallback provider/model letti da DB (matrice routing) invece che hardcoded.
            let fallback_provider: String = match matrix.lookup("chat", "bilanciata") {
                Some((p, _)) => p,
                None => "openai".to_string(),
            };
            let hint_provider_owned: String = forced_provider
                .as_deref()
                .or(suggested_provider.as_deref())
                .map(String::from)
                .unwrap_or(fallback_provider);
            let fallback_model: String = default_model_for_provider(matrix, &hint_provider_owned);
            let hint_model_owned: String = forced_model
                .as_deref()
                .or(suggested_model.as_deref())
                .map(String::from)
                .unwrap_or(fallback_model);
            let hint_provider = hint_provider_owned.as_str();
            let hint_model = hint_model_owned.as_str();
            let reservation = billing::reserve_usage(
                db,
                user_id,
                project_uuid,
                hint_provider,
                hint_model,
                prompt_tokens,
                estimated_completion,
                json!({"intent": intent, "profile_id": input.profile_id,
                           "via_nexus_gateway": true,
                           "corrections_count": prompt_corrections.len()}),
            )
            .await
            .map_err(|e| anyhow::anyhow!("billing_rejected: {e}"))?;

            let gw_resp = match gw.complete(gw_req).await {
                Ok(r) => r,
                Err(e) => {
                    billing::release_usage(db, &reservation, "gateway_error").await;
                    anyhow::bail!("Nexus Gateway failed for intent '{intent}': {e}");
                }
            };
            let actual_usage = UsageNumbers {
                prompt_tokens: gw_resp.usage.input_tokens as i32,
                completion_tokens: gw_resp.usage.output_tokens as i32,
                total_tokens: (gw_resp.usage.input_tokens + gw_resp.usage.output_tokens) as i32,
            };
            let (_, _, cost, cur) =
                billing::finalize_usage(db, &reservation, run_id, &actual_usage).await?;
            let gw_completion = json!({"content": gw_resp.content, "metadata": {
                "provider": gw_resp.provider_used, "model": gw_resp.model_used,
                "latency_ms": gw_resp.latency_ms, "finish_reason": gw_resp.finish_reason},
                "privacy_rerouted": gw_resp.privacy_rerouted.as_ref().map(|pr| json!({
                    "provider": pr.provider,
                    "blocked_tier": pr.blocked_tier,
                    "reason": pr.reason,
                }))
            });
            if let Some(ref pr) = gw_resp.privacy_rerouted {
                tracing::warn!(
                        "Nexus Gateway: privacy re-route tier={} → local provider={} intent={} tokens={}",
                        pr.blocked_tier, pr.provider, intent, actual_usage.total_tokens
                    );
            } else {
                tracing::info!(
                    "Nexus Gateway: intent={} provider={} model={} tokens={}",
                    intent,
                    gw_resp.provider_used,
                    gw_resp.model_used,
                    actual_usage.total_tokens
                );
            }
            (
                gw_resp.provider_used,
                gw_resp.model_used,
                gw_completion,
                actual_usage,
                cost,
                cur,
            )
        } else {
            // PATH B: Brain gRPC legacy
            let mut selected_provider: Option<String> = None;
            let mut selected_model: Option<String> = None;
            let mut completion: Option<serde_json::Value> = None;
            let mut usage: Option<UsageNumbers> = None;
            let mut usage_cost: Option<(f64, f64, f64, String)> = None;
            let mut skip_reasons = Vec::new();

            // In modalità dinamico la scelta del catalogo è autoritativa
            let provider_candidates = if let Some(provider) = forced_provider.as_ref() {
                vec![provider.clone()]
            } else if routing.behavior_mode == "dinamico" {
                if let Some(p) = suggested_provider.as_ref() {
                    vec![p.clone()]
                } else {
                    routing.candidates(&intent, suggested_provider.as_deref())
                }
            } else {
                routing.candidates(&intent, suggested_provider.as_deref())
            };
            for provider in provider_candidates {
                let health = match self.neural.provider_health(&provider).await {
                    Ok(health) => health,
                    Err(error) => {
                        skip_reasons.push(format!("{provider}:health_check_failed:{error}"));
                        continue;
                    }
                };

                let status = health["status"].as_str().unwrap_or("unknown");
                if !matches!(status, "ready" | "ok") {
                    let reason = health["reason"]
                        .as_str()
                        .or_else(|| health["skipReasons"].get(0).and_then(Value::as_str))
                        .unwrap_or(status);
                    skip_reasons.push(format!("{provider}:skipped:{reason}"));
                    continue;
                }

                let model = if forced_provider.as_deref() == Some(provider.as_str()) {
                    forced_model.clone().unwrap_or_else(|| {
                        routing.resolve_model(matrix, &provider, Some(provider.as_str()), None)
                    })
                } else if routing.behavior_mode == "dinamico"
                    && suggested_provider.as_deref() == Some(provider.as_str())
                    && suggested_model.is_some()
                {
                    // In dinamico fidiamoci del catalogo: niente override da provider_model_<x>.
                    // suggested_model.is_some() controllato sopra; clone+unwrap_or e' difensivo.
                    suggested_model.clone().unwrap_or_default()
                } else {
                    routing.resolve_model(
                        matrix,
                        &provider,
                        suggested_provider.as_deref(),
                        suggested_model.as_deref(),
                    )
                };
                let prompt_tokens = mcp_token::count_tokens(&composed_prompt) as i32;
                let estimated_completion_tokens = token_budget as i32 - prompt_tokens;
                let reservation = match billing::reserve_usage(
                    db,
                    user_id,
                    project_uuid,
                    &provider,
                    &model,
                    prompt_tokens,
                    estimated_completion_tokens.max(0),
                    json!({
                        "intent": intent,
                        "profile_id": input.profile_id,
                        "corrections_count": prompt_corrections.len(),
                        "request_message_id": input.request_message_id,
                        "automation_mode": input.automation_mode.as_str(),
                        "provider_override": forced_provider,
                        "model_override": forced_model,
                        "attachments_count": input.attachments.len(),
                    }),
                )
                .await
                {
                    Ok(reservation) => reservation,
                    Err(error) => {
                        skip_reasons.push(format!("{provider}:billing_rejected:{error}"));
                        continue;
                    }
                };

                let provider_completion = match self
                    .neural
                    .generate_completion(&provider, &model, &composed_prompt)
                    .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        billing::release_usage(db, &reservation, "provider_error").await;
                        let error_msg = error.to_string();
                        // Distingui rate limit da altri errori
                        if error_msg.contains("429")
                            || error_msg.to_lowercase().contains("rate_limit")
                            || error_msg.to_lowercase().contains("quota")
                            || error_msg.to_lowercase().contains("too_many_requests")
                        {
                            skip_reasons.push(format!("{provider}:rate_limited:{error_msg}"));
                            tracing::warn!(
                                "Provider {provider} è rate-limited, provo il prossimo candidato"
                            );
                        } else {
                            skip_reasons.push(format!("{provider}:execution_error:{error_msg}"));
                        }
                        continue;
                    }
                };

                if completion_has_error(&provider_completion) {
                    billing::release_usage(db, &reservation, "provider_failed").await;
                    let error = provider_completion["metadata"]["error"]
                        .as_str()
                        .unwrap_or("generation_failed");
                    // Distingui rate limit da altri errori anche nella risposta
                    if error.contains("429")
                        || error.to_lowercase().contains("rate_limit")
                        || error.to_lowercase().contains("quota")
                        || error.to_lowercase().contains("too_many_requests")
                    {
                        skip_reasons.push(format!("{provider}:rate_limited:{error}"));
                        tracing::warn!("Provider {provider} segnala rate limit nella risposta, provo il prossimo");
                    } else {
                        skip_reasons.push(format!("{provider}:failed:{error}"));
                    }
                    continue;
                }

                let usage_numbers = billing::extract_usage_numbers(
                    &provider_completion,
                    prompt_tokens,
                    estimated_completion_tokens,
                );
                let finalized_cost =
                    billing::finalize_usage(db, &reservation, run_id, &usage_numbers).await?;

                selected_provider = Some(provider);
                selected_model = Some(model);
                completion = Some(provider_completion);
                usage = Some(usage_numbers);
                usage_cost = Some(finalized_cost);
                break;
            }

            let provider = selected_provider.ok_or_else(|| {
                anyhow::anyhow!(
                    "No AI provider available for intent '{intent}'. Skip reasons: {}",
                    skip_reasons.join(", ")
                )
            })?;
            let model = selected_model
                .unwrap_or_else(|| default_model_for_provider(matrix, &provider).to_string());
            let completion =
                completion.ok_or_else(|| anyhow::anyhow!("No completion generated"))?;
            let usage = usage.unwrap_or(UsageNumbers {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });
            let (_, _, cost, cur) = usage_cost.unwrap_or((0.0, 0.0, 0.0, "EUR".to_string()));
            (provider, model, completion, usage, cost, cur)
        };

        // Step 5: Build audit record
        let audit = OrchestratorAudit {
            project_id: input.project_id.clone(),
            profile_id: input.profile_id.clone(),
            intent: intent.clone(),
            provider: provider.clone(),
            model: model.clone(),
            token_budget,
            tokens_saved: context.tokens_saved as u32,
            resources: input.active_files.clone(),
            guardrail_result: "allowed".to_string(),
        };

        // Step 6: Persist audit to database
        let audit_json = serde_json::to_value(&audit)?;
        let session_uuid = input
            .session_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok());
        let profile_uuid = Uuid::parse_str(&input.profile_id).ok();
        sqlx::query(
            r#"
            INSERT INTO orchestrator_runs (id, project_id, user_id, session_id, profile_id, status, audit_json)
            VALUES ($1, $2::uuid, $3, $4, $5, 'completed', $6)
            "#,
        )
        .bind(run_id)
        .bind(&input.project_id)
        .bind(user_id)
        .bind(session_uuid)
        .bind(profile_uuid)
        .bind(&audit_json)
        .execute(db)
        .await
        .ok(); // Non-fatal: log but don't fail the request

        let payload = json!({
            "run_id": run_id.to_string(),
            "intent": intent,
            "provider": provider,
            "model": model,
            "completion": completion,
            "tokens_saved": context.tokens_saved,
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens,
            "total_cost": total_cost,
            "currency": currency,
            "applied_corrections": prompt_corrections,
            "automation_mode": input.automation_mode.as_str(),
            "attachments_count": input.attachments.len(),
        });

        Ok(OrchestratorResult { payload, audit })
    }

    async fn load_prompt_corrections(
        &self,
        db: &PgPool,
        project_id: Uuid,
        query: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let globally_enabled = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'learning_prompt_corrections_enabled'",
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|value| value.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(true);
        if !globally_enabled {
            return Ok(Vec::new());
        }

        let project_enabled = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT prompt_corrections_enabled
            FROM project_learning_config
            WHERE project_id = $1
            "#,
        )
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(true);
        if !project_enabled {
            return Ok(Vec::new());
        }

        let embedding = match self.neural.embed_text("", query).await {
            Ok(vector) => vector,
            Err(error) => {
                tracing::warn!("Unable to embed query for prompt corrections: {error}");
                return Ok(Vec::new());
            }
        };

        let hits =
            match vector_memory::search_prompt_correction_points(db, &embedding, project_id, 5)
                .await
            {
                Ok(hits) => hits,
                Err(error) => {
                    tracing::warn!("Unable to search prompt corrections: {error}");
                    return Ok(Vec::new());
                }
            };

        let mut corrections = Vec::new();
        let mut correction_ids = Vec::<Uuid>::new();
        for hit in hits {
            if hit.score < 0.78 {
                continue;
            }
            let correction_id = hit
                .payload
                .get("correction_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok());
            let text = hit
                .payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            if let Some(correction_id) = correction_id {
                correction_ids.push(correction_id);
            }
            corrections.push(json!({
                "id": correction_id.map(|value| value.to_string()).unwrap_or_default(),
                "text": text,
                "score": hit.score,
                "intent": hit.payload.get("intent").and_then(Value::as_str).unwrap_or("chat"),
                "pointId": hit.point_id,
            }));
        }

        if !correction_ids.is_empty() {
            let _ = sqlx::query(
                r#"
                UPDATE prompt_corrections
                SET retrieved_count = retrieved_count + 1,
                    last_retrieved_at = NOW(),
                    updated_at = NOW()
                WHERE id = ANY($1)
                "#,
            )
            .bind(&correction_ids)
            .execute(db)
            .await;
        }

        Ok(corrections)
    }

    async fn compose_prompt(
        db: &PgPool,
        cache: &crate::prompt_templates::TemplateCache,
        base_prompt: &str,
        corrections: &[Value],
        automation_mode: AutomationMode,
        attachments: &[ChatAttachment],
    ) -> String {
        let tpl_key = automation_mode.prompt_instruction_template_key();
        let mode_instruction =
            crate::prompt_templates::get_template_or_default(db, cache, tpl_key).await;
        let mut sections = vec![mode_instruction];

        if !corrections.is_empty() {
            let mut block = String::from("Correzioni note (da rispettare se pertinenti):\n");
            for correction in corrections {
                if let Some(text) = correction.get("text").and_then(Value::as_str) {
                    block.push_str("- ");
                    block.push_str(text.trim());
                    block.push('\n');
                }
            }
            sections.push(block.trim().to_string());
        }

        if !attachments.is_empty() {
            let text_attachments: Vec<_> = attachments
                .iter()
                .filter(|a| !a.text_content.is_empty())
                .collect();
            let image_attachments: Vec<_> = attachments
                .iter()
                .filter(|a| a.base64_content.is_some())
                .collect();
            if !text_attachments.is_empty() {
                let mut block = String::from("Allegati utente per questo messaggio:\n");
                for attachment in &text_attachments {
                    block.push_str(&format!(
                        "\n### File: {} ({}, {} bytes)\n{}\n",
                        attachment.name,
                        attachment.mime_type,
                        attachment.size_bytes,
                        attachment.text_content
                    ));
                }
                sections.push(block.trim().to_string());
            }
            if !image_attachments.is_empty() {
                let names: Vec<_> = image_attachments.iter().map(|a| a.name.as_str()).collect();
                sections.push(format!("L'utente ha allegato {} immagine/i: {}. Le immagini sono incluse come content block nel messaggio.", names.len(), names.join(", ")));
            }
        }

        sections.push(base_prompt.to_string());
        sections.join("\n\n")
    }

    async fn load_project_intent_chain(
        db: &PgPool,
        project_id: Uuid,
        intent: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let key = format!("project_{}_routing_{}_providers", project_id, intent);
        let value = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = $1")
            .bind(&key)
            .fetch_optional(db)
            .await?;
        Ok(parse_provider_list(value.as_deref()))
    }

    async fn load_routing_config(db: &PgPool) -> anyhow::Result<RoutingConfig> {
        let settings = sqlx::query_as::<_, SettingValueRow>(
            r#"
            SELECT key, value
            FROM settings
            WHERE category = 'routing'
               OR key IN (
                    'provider_hierarchy',
                    'provider_priority',
                    'provider_order',
                    'fallback_order',
                    'default_provider',
                    'default_model',
                    'token_budget',
                    'max_token_budget',
                    'provider_model_openai',
                    'provider_model_anthropic',
                    'provider_model_google',
                    'openai_model',
                    'anthropic_model',
                    'google_model',
                    'routing_fix_providers',
                    'routing_refactor_providers',
                    'routing_test_providers',
                    'routing_docs_providers',
                    'routing_architecture_providers',
                    'routing_chat_providers'
               )
            "#,
        )
        .fetch_all(db)
        .await?;

        Ok(RoutingConfig::from_settings(&settings))
    }
}
