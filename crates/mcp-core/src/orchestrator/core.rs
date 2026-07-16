//! Implementazione principale di Orchestrator: orchestrazione
//! del run agentico, risoluzione provider, prompt e routing.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    billing::{self, UsageNumbers},
    domain::OrchestratorAudit,
    nexus_gateway::{intent_to_alias, GwMessage, GwMetadata, GwRequest, NexusGatewayClient},
    provider_cooldown::is_provider_in_cooldown,
    vector_memory,
};

use super::*;

/// Esito di un'esecuzione LLM: `(provider, model, completion, usage, cost,
/// currency)`. Alias interno usato dai due path di esecuzione (gateway/neural)
/// estratti da `Orchestrator::run`.
type LlmExecution = (String, String, serde_json::Value, UsageNumbers, f64, String);

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

    /// Classifier intent che usa le soglie da DB (mig 0111) e delega al punto
    /// unico di scelta motore [`select_classifier_engine`] (regola L, flag mig
    /// 0458): `python` -> endpoint brain `/classify-intent-agentic` (path vivo
    /// INVARIATO), `rust` -> `intent_classifier::classify` in-process. Se la
    /// cache `routing_thresholds` non e' disponibile, fallback ai default.
    async fn classify_intent_with_db_thresholds(
        &self,
        db: &PgPool,
        message: &str,
    ) -> (&'static str, f32) {
        // Path RUST in-process quando il flag DB lo dice E il gateway e'
        // disponibile (senza gateway non si puo' chiamare l'LLM: si resta sul
        // path Python, comportamento stabile).
        if let (ClassifierEngine::Rust, Some(gw)) = (
            select_classifier_engine(db).await,
            self.nexus_gateway.as_ref(),
        ) {
            let (classified, _ai) = classify_intent_full_rust(db, gw, message).await;
            return (classified.intent, classified.confidence);
        }
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
    pub async fn classify_intent_full(&self, db: &PgPool, message: &str) -> ClassifiedIntent {
        // Punto unico di scelta motore (regola L, flag mig 0458): `rust` ->
        // classificatore in-process; `python` (default) -> endpoint brain. Il
        // path rust richiede il gateway; senza, si resta sul path Python.
        if let (ClassifierEngine::Rust, Some(gw)) = (
            select_classifier_engine(db).await,
            self.nexus_gateway.as_ref(),
        ) {
            let (classified, _ai) = classify_intent_full_rust(db, gw, message).await;
            return classified;
        }
        let (min_conf, timeout_s) = match self.routing_thresholds.current_async().await {
            Ok(t) => (
                t.llm_classifier_min_confidence,
                t.llm_classifier_timeout_seconds,
            ),
            Err(_) => (LLM_CLASSIFIER_MIN_CONFIDENCE_DEFAULT, 5.0),
        };

        // Solo interpretazione semantica LLM: niente piu' pre-check ne' fallback
        // keyword/deterministico. Quando l'LLM non e' disponibile, la funzione
        // ritorna l'intent di sistema neutro `agentic_default`, che attiva lato
        // agente il _LAZY_MINIMAL_TOOLKIT (l'LLM dell'agente interpreta da se').
        classify_intent_async_full_with_threshold(message, min_conf, timeout_s).await
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
    ///
    /// La slot-matrix esprime solo TIER+capability (mig 0357): la scelta del
    /// provider+modello concreto e' delegata al punto unico tier-based
    /// `select_models_for_requirement` - lo stesso scoring che governa la
    /// routing matrix per intent. Niente piu' modelli pinnati (regola G/H/L).
    /// `db` serve a leggere il catalog sano al momento della decisione.
    pub async fn route_by_slots(
        &self,
        db: &sqlx::PgPool,
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
        let req = match matrix.lookup(slots) {
            Some(r) => r,
            None => {
                tracing::debug!(
                    "route_by_slots: nessun match per ({}, {}, {}, {}) in matrix",
                    slots.action_verb,
                    slots.target_type,
                    slots.framework,
                    slots.scope,
                );
                return None;
            }
        };
        // Punto unico tier-based: candidati (provider, model) sani ordinati per
        // score, uno per provider. La rotazione provider per disponibilita'
        // vale quindi anche per le richieste slot-routed.
        let candidates = match crate::routing_matrix_auto_promoter::select_models_for_requirement(
            db,
            &req.preferred_tier,
            &req.required_capabilities,
            req.requires_tool_use,
            &req.cost_direction,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "route_by_slots: selezione tier-based fallita ({e}), fallback intent classico"
                );
                return None;
            }
        };
        if candidates.is_empty() {
            tracing::debug!(
                "route_by_slots: nessun candidato per tier {} (slots {}, {}, {}, {})",
                req.preferred_tier,
                slots.action_verb,
                slots.target_type,
                slots.framework,
                slots.scope,
            );
            return None;
        }
        // Cooldown-awareness: ritorna il primo provider NON in cooldown.
        let mut skipped: Vec<String> = Vec::new();
        for (provider, model) in &candidates {
            if crate::provider_cooldown::is_provider_in_cooldown(provider) {
                skipped.push(provider.clone());
                continue;
            }
            if !skipped.is_empty() {
                tracing::info!(
                    "route_by_slots: skip provider in cooldown [{}], scelto {}/{} (tier {}, pos {}/{})",
                    skipped.join(","), provider, model, req.preferred_tier,
                    skipped.len() + 1, candidates.len(),
                );
            } else {
                tracing::info!(
                    "route_by_slots: slots=({}, {}, {}, {}) tier={} → {}/{}",
                    slots.action_verb,
                    slots.target_type,
                    slots.framework,
                    slots.scope,
                    req.preferred_tier,
                    provider,
                    model,
                );
            }
            return Some((provider.clone(), model.clone(), "slots_matrix"));
        }
        // Tutti i provider candidati sono in cooldown.
        tracing::warn!(
            "route_by_slots: TUTTI i {} provider candidati in cooldown [{}], fallback intent classico",
            candidates.len(), skipped.join(",")
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

    /// Helper unico (regola L): risolve `(tier, capability)` per un intent dalla
    /// cache `intent_capability` (mig 0110), col default `("light", "chat")` sia
    /// quando l'intent non e' mappato sia quando la cache non e' disponibile.
    /// Consolida il pattern ripetuto nei gate capability e nei fallback catalog.
    /// NB: il ramo "dinamico" di `resolve_agent_provider` NON usa questo helper:
    /// li' il default e' `medium/reasoning` con WARN dedicato (comportamento
    /// deliberatamente diverso, lasciato invariato).
    async fn intent_tier_capability(
        &self,
        intent: &str,
        estimated_tokens: u32,
    ) -> (String, String) {
        let icap_arc = self.intent_capability.current_async().await.ok();
        match icap_arc.as_deref() {
            Some(map) => match map.get(intent) {
                Some(c) => (
                    c.tier_for_tokens(estimated_tokens),
                    c.base_capability.clone(),
                ),
                None => ("light".to_string(), "chat".to_string()),
            },
            None => ("light".to_string(), "chat".to_string()),
        }
    }

    pub async fn embed_text(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        // Embedder ONNX in-process (regola L: punto unico, niente round-trip al
        // brain Python). spawn_blocking perche' embed() e' CPU-bound sincrono.
        let bridge = crate::nexus_bridge::NexusBridge::global()
            .ok_or_else(|| anyhow::anyhow!("nexus bridge non inizializzato (embed_text)"))?;
        let text = text.to_string();
        tokio::task::spawn_blocking(move || bridge.embed_one(&text))
            .await
            .map_err(|e| anyhow::anyhow!("embed_text spawn_blocking join: {e}"))
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
        // Intent gia' classificato dal chiamante (regola L). Propagato a
        // resolve_agent_provider e usato per gate/source senza ri-classificare.
        intent_hint: Option<&str>,
        // Il TURNO corrente contiene almeno un allegato image/*. Segnale
        // strutturato (derivato dai MIME via `turn_has_image_attachment`, mai dal
        // testo del prompt) che attiva l'override media-aware sul routing: con
        // un'immagine il modello del turno deve avere supports_vision=TRUE.
        // RIPRISTINO REGRESSIONE Python->Rust (CLAUDE.md sezione I, "Smart routing
        // vision"). `false` => nessun override (routing testuale invariato).
        turn_has_image: bool,
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
                    user_override: false,
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
        // Fix latente: prima `.unwrap_or_default()` su DB error -> behavior_mode
        // silenziosamente vuoto. Ora almeno il fallimento e' visibile nei log.
        let routing_for_mode = match Self::load_routing_config(db).await {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("load_routing_config fallito (resolve): {e}; uso default");
                RoutingConfig::default()
            }
        };
        let configured_behavior_mode = behavior_mode_session
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| routing_for_mode.behavior_mode.clone());

        let (mut provider, mut model) = self
            .resolve_agent_provider(
                db,
                _project_id,
                _profile_id,
                message,
                provider_override,
                model_override,
                context_message_count,
                Some(&configured_behavior_mode),
                intent_hint,
            )
            .await;
        // Intent per gate/source: se fornito dal chiamante (brain), lo usiamo
        // (regola L, niente doppia classificazione); altrimenti classifichiamo
        // via classifier LLM (gemini-flash, cache 24h). Vedi `classify_intent_async`.
        let (intent_owned, confidence): (String, f32) = match intent_hint {
            Some(h) if !h.trim().is_empty() => (h.to_string(), 1.0),
            _ => {
                let (i, c) = self.classify_intent_with_db_thresholds(db, message).await;
                (i.to_string(), c)
            }
        };
        let intent: &str = intent_owned.as_str();

        // ── Gate di capability tool-use (ADR 0018, leva 0) ──────────────────
        // Un RUN AGENTICO (intent != "chat") non deve MAI usare un modello con
        // ai_price_catalog.supports_tool_use = false: il loop agentico forza il
        // tool_choice e un modello non tool-capable produrrebbe lo stop
        // narrativo / MALFORMED_FUNCTION_CALL. Caso reale: mistral-code-latest
        // (supports_tool_use=false) raggiungibile dal routing agentico via
        // nexus_routing_matrix. Il gate riusa i meccanismi di fallback gia'
        // esistenti (best_model_for_tier, filtrato su supports_tool_use=TRUE) —
        // nessun nome modello hardcoded (regola G).
        if intent != "chat" && !provider.is_empty() && !model.is_empty() {
            self.apply_tool_use_capability_gate(db, &mut provider, &mut model, intent, message)
                .await;
        }

        // ── Gate di capability VISION (RIPRISTINO regressione Python->Rust) ───
        // Override media-aware sul routing del TURNO (CLAUDE.md sezione I, "Smart
        // routing vision"): se il messaggio corrente allega un'immagine, il
        // modello del turno DEVE supportare la vision (supports_vision=TRUE),
        // altrimenti l'immagine viene ignorata e l'agente "vede" solo il testo.
        // Override CONDIZIONALE: se `turn_has_image == false` il gate e' un no-op
        // (zero regressione sul routing testuale). Riusa il selettore unico
        // (best_model_for_tier con capability='vision'), nessuna query vision
        // duplicata, nessun nome modello hardcoded (regola G/L). Applicato DOPO
        // il gate tool-use: per un run agentico+immagine cerchiamo un modello che
        // sia sia vision sia tool-capable.
        if turn_has_image && !provider.is_empty() && !model.is_empty() {
            self.apply_vision_capability_gate(db, &mut provider, &mut model, intent, message)
                .await;
        }

        // is_risky_task rimosso: il rischio non viene piu' dedotto via keyword.
        // Il behavior_mode resta quello configurato (solo dinamico->bilanciata).
        // La safety reale e' indipendente (study mode, isolamento progetto, tag
        // <safety_progetto>) e resta intatta.
        let effective_mode = if configured_behavior_mode == "dinamico" {
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
        // ADR 0023: un override esplicito puo' essere sul provider, sul modello,
        // o su entrambi. Anche un model_override da solo conta come "override".
        let has_provider_override = provider_override.filter(|v| !v.trim().is_empty()).is_some();
        let has_model_override = model_override.filter(|v| !v.trim().is_empty()).is_some();
        let source: &'static str = if has_provider_override || has_model_override {
            "override"
        } else if configured_behavior_mode == "dinamico" {
            // In modalita' dinamica il catalogo prezzi e' autoritativo.
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
        // a vuoto le richieste. Lista dal punto unico registry-aware (regola L):
        // i provider onboardati via registry (mig 0565+) entrano nell'alert senza
        // hardcode; fallback ai 5 noti gia' garantito da merge_provider_names.
        let api_keys = crate::environment::fetch_api_key_configured(db).await;
        let known_providers = crate::environment::provider_names_for_status(db, &api_keys).await;
        let providers_in_cooldown: Vec<String> = known_providers
            .iter()
            .filter(|p| is_provider_in_cooldown(p))
            .cloned()
            .collect();
        // Forzatura esplicita utente (ADR 0020): se l'utente ha scelto
        // provider/modello dal dropdown (non Auto), il cooldown e' deliberatamente
        // ignorato. `user_override` segnala al chiamante che la scelta e'
        // consapevole e non va bloccata anche se il provider e' in cooldown.
        let user_override = has_provider_override || has_model_override;
        // Nessun provider capable = il provider scelto e' lui stesso in
        // cooldown (succede quando tutti gli altri della hierarchy sono
        // anch'essi in cooldown e l'algoritmo riusa l'originale).
        // In modalita' AUTO il cooldown e' VINCOLANTE. Con forzatura utente
        // (user_override) NON e' mai "no_capable": l'utente decide (ADR 0020).
        let no_capable_provider = compute_no_capable_provider(
            user_override,
            is_provider_in_cooldown(&provider),
            providers_in_cooldown.len() >= known_providers.len(),
        );

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
            "intent={} confidence={:.2} mode={} source={} → {}/{}{}{}",
            intent,
            confidence,
            effective_mode,
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
            // is_risky_task rimosso: il flag resta nello schema per compatibilita'
            // ma non viene piu' valutato (nessuna interpretazione keyword).
            risky: false,
            rationale,
            source: source.to_string(),
            configured_behavior_mode,
            no_capable_provider,
            user_override,
            providers_in_cooldown,
            error: None,
        }
    }

    /// Gate di capability tool-use per i run AGENTICI (ADR 0018, leva 0).
    ///
    /// Se `(provider, model)` risolti puntano a un modello con
    /// `ai_price_catalog.supports_tool_use = false`, sostituisce in-place con il
    /// primo modello tool-capable secondo i meccanismi di fallback gia'
    /// esistenti (`best_model_for_tier`, che filtra `supports_tool_use = TRUE`
    /// ed esclude i provider in cooldown). Nessun nome modello hardcoded.
    ///
    /// Configurabile via setting `agent.require_tool_use_capability` (default
    /// `true`). Se nessun modello tool-capable e' disponibile per quell'intent,
    /// NON sostituisce ma logga un WARN esplicito (fail visibile, regola G):
    /// il run proseguira' col modello originale, ma l'incidente e' tracciato.
    async fn apply_tool_use_capability_gate(
        &self,
        db: &PgPool,
        provider: &mut String,
        model: &mut String,
        intent: &str,
        message: &str,
    ) {
        // Flag DB (default true). Stesso pattern di lettura settings usato
        // altrove (es. agent.model_tool_failure_threshold in agent_run.rs).
        let gate_enabled: bool =
            crate::settings::get_setting(db, "agent.require_tool_use_capability")
                .await
                .ok()
                .flatten()
                .map(|v| {
                    let t = v.trim().to_ascii_lowercase();
                    !(t == "false" || t == "0" || t == "no")
                })
                .unwrap_or(true);

        // Capability del modello risolto dal catalog. None = modello assente
        // (problema di sync, gestito conservativamente dalla funzione pura).
        // Leggiamo agentic_thinking_policy: solo 'exclude' (reasoning-only senza
        // function calling) va scartato dagli agentici; i dual-mode (deepseek-v4,
        // claude, gemini-2.5) restano e l'adapter forza il non-thinking (ADR 0025).
        // Leggiamo anche is_enabled: un modello DISABILITATO (es. legacy pruned
        // dalla mig 0320, raggiunto via una config di default stale) non e'
        // chiamabile -> va comunque sostituito su un run agentico (robustezza
        // oltre alla policy).
        let caps: Option<(bool, String, bool)> = sqlx::query_as::<_, (bool, String, bool)>(
            "SELECT supports_tool_use, agentic_thinking_policy, is_enabled FROM ai_price_catalog \
             WHERE provider = $1 AND model = $2 LIMIT 1",
        )
        .bind(&*provider)
        .bind(&*model)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
        let supports: Option<bool> = caps.as_ref().map(|(s, _, _)| *s);
        let policy: Option<&str> = caps.as_ref().map(|(_, p, _)| p.as_str());
        let model_disabled = matches!(caps.as_ref(), Some((_, _, false)));

        let gate_decision = if model_disabled && intent != "chat" && gate_enabled {
            ToolCapabilityGate::NeedsFallback
        } else {
            decide_tool_capability_gate(intent, gate_enabled, supports, policy)
        };
        match gate_decision {
            ToolCapabilityGate::KeepOriginal => {}
            ToolCapabilityGate::NeedsFallback => {
                // Tier/capability dell'intent dalla cache (mig 0110), stessi
                // valori usati dal routing dinamico. Default light/chat se
                // l'intent non e' mappato (helper unico, regola L).
                let estimated_tokens = estimate_complexity(message);
                let (tier, capability) =
                    self.intent_tier_capability(intent, estimated_tokens).await;

                // Fallback deterministico: miglior modello tool-capable del tier
                // (degradazione di tier controllata in best_model_for_tier).
                //
                // RILASSO CAPABILITY (fix incidente UI 2026-06-04): prima prova
                // con la capability dell'intent (es. "reasoning"); se nessun
                // modello NON-thinking tool-capable la possiede — caso reale:
                // i modelli con capability "reasoning" hanno policy thinking
                // che li esclude dal gate (mig 0317/0319) — rilassa la capability e
                // prende il miglior non-thinking tool-capable del tier. Un modello
                // non-thinking senza quel tag e' comunque MOLTO meglio di un
                // thinking model che fallirebbe il loop agentico (deepseek-v4-pro
                // -> reasoning_content 400). Senza questo rilassamento il gate
                // teneva il modello thinking originale (override/slot), vanificando
                // l'esclusione su tutti i path che forzano un modello.
                // SERVIZIO UNICO (regola L): Degrade + Agentic. Il gate segue il
                // profilo (I2) invece di dipendere da questo call site.
                use crate::orchestrator::model_service::{select_model, ModelRequest};
                let fallback = match select_model(
                    db,
                    &ModelRequest::agentic(&tier).capability(Some(&capability)),
                )
                .await
                {
                    Ok(c) => Some((c.provider, c.model)),
                    Err(_) => select_model(db, &ModelRequest::agentic(&tier))
                        .await
                        .ok()
                        .map(|c| (c.provider, c.model)),
                };
                match fallback {
                    Some((alt_provider, alt_model)) => {
                        tracing::info!(
                            "routing: {}/{} scartato per run agente (non tool-capable o thinking) \
                             -> fallback {}/{} (intent={}, tier={}, capability={})",
                            provider,
                            model,
                            alt_provider,
                            alt_model,
                            intent,
                            tier,
                            capability,
                        );
                        *provider = alt_provider;
                        *model = alt_model;
                    }
                    None => {
                        // Nessun modello non-thinking tool-capable disponibile in
                        // NESSUNA capability del tier: fail visibile. Non sostituiamo
                        // silenziosamente con qualcosa di sbagliato.
                        tracing::warn!(
                            "routing: {}/{} non utilizzabile per run agente (intent={}) ma \
                             nessun modello non-thinking tool-capable disponibile nel catalog \
                             (tier={}, neppure rilassando la capability). Run proseguira' col \
                             modello originale — verifica ai_price_catalog (supports_tool_use, \
                             agentic_thinking_policy) e i provider in cooldown.",
                            provider,
                            model,
                            intent,
                            tier,
                        );
                    }
                }
            }
        }
    }

    /// Gate di capability VISION per il routing del TURNO (RIPRISTINO regressione
    /// Python->Rust, CLAUDE.md sezione I "Smart routing vision").
    ///
    /// Se il turno allega un'immagine e `(provider, model)` risolti puntano a un
    /// modello con `ai_price_catalog.supports_vision = false` (o assente dal
    /// catalog), sostituisce in-place con il miglior modello VISION dello stesso
    /// tier dell'intent, riusando il SELETTORE UNICO `best_model_for_tier` con
    /// capability `'vision'` (che mappa su `supports_vision = TRUE`, colonna
    /// canonica — vista mig 0318/0372). Nessuna query vision duplicata, nessun
    /// nome modello hardcoded (regola G/L).
    ///
    /// Override CONDIZIONALE: il chiamante invoca questo gate SOLO quando
    /// `turn_has_image == true`; per i turni testuali il routing resta invariato.
    /// Se nessun modello vision e' disponibile (tutti in cooldown, catalog senza
    /// vision per quel tier) NON sostituisce ma logga un WARN esplicito (fail
    /// visibile, regola G): il run prosegue col modello originale (l'immagine
    /// resta investigabile via il tool `nexus_describe_image_attachment`).
    async fn apply_vision_capability_gate(
        &self,
        db: &PgPool,
        provider: &mut String,
        model: &mut String,
        intent: &str,
        message: &str,
    ) {
        // Capability vision del modello risolto. None = modello assente dal
        // catalog (per costruzione senza supports_vision=TRUE -> conservativo
        // verso la sostituzione, vedi decide_vision_capability_gate).
        let model_supports_vision: Option<bool> = sqlx::query_scalar::<_, bool>(
            "SELECT supports_vision FROM ai_price_catalog \
             WHERE provider = $1 AND model = $2 LIMIT 1",
        )
        .bind(&*provider)
        .bind(&*model)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

        // Il chiamante ha gia' verificato turn_has_image == true.
        match decide_vision_capability_gate(true, model_supports_vision) {
            VisionCapabilityGate::KeepOriginal => {}
            VisionCapabilityGate::NeedsVisionModel => {
                // Tier dell'intent dalla cache (mig 0110), stesso meccanismo del
                // gate tool-use. Default light/chat se l'intent non e' mappato
                // (helper unico, regola L): qui serve solo il tier.
                let estimated_tokens = estimate_complexity(message);
                let (tier, _capability) =
                    self.intent_tier_capability(intent, estimated_tokens).await;
                // Run agentico (intent != "chat") -> serve un modello vision CHE
                // sia anche tool-capable: passiamo requires_tool_use al selettore
                // unico, cosi' non vanifichiamo il gate tool-use applicato prima.
                let requires_tool_use = intent != "chat";
                // SERVIZIO UNICO (regola L). Il profilo decide i filtri e il gate
                // (I2); la degradazione e' esplicita: con un'immagine allegata e
                // il tier vision esaurito, meglio un modello vision di un gradino
                // sotto che un turno CIECO — che era il comportamento reale
                // (WARN + prosegue col modello senza occhi). La capability resta
                // un filtro: il ripiego VEDE sempre.
                let vreq = if requires_tool_use {
                    crate::orchestrator::model_service::ModelRequest::agentic(&tier)
                } else {
                    crate::orchestrator::model_service::ModelRequest::non_agentic(&tier)
                }
                .capability(Some("vision"));
                match crate::orchestrator::model_service::select_model(db, &vreq)
                    .await
                    .map(|c| (c.provider, c.model))
                {
                    Ok((vp, vm)) => {
                        tracing::info!(
                            "routing(vision): {}/{} senza vision ma il turno ha un'immagine \
                             -> override {}/{} (intent={}, tier={}, tool_use={})",
                            provider,
                            model,
                            vp,
                            vm,
                            intent,
                            tier,
                            requires_tool_use,
                        );
                        *provider = vp;
                        *model = vm;
                    }
                    Err(motivo) => {
                        // Nessun modello vision in NESSUN tier della catena (run
                        // agentico: nemmeno vision+tool-capable). Fail visibile:
                        // non sostituiamo con un modello non-vision. Il motivo e'
                        // TIPIZZATO (I6): `GateEmpty` dice "il worker di
                        // qualificazione e' fermo", `ChainExhausted` dice "il
                        // parco vision e' giu'" — due azioni opposte, che prima
                        // collassavano nello stesso warning.
                        tracing::warn!(
                            motivo = ?motivo,
                            "routing(vision): il turno ha un'immagine ma {}/{} non supporta la \
                             vision e nessun modello supports_vision=TRUE e' disponibile \
                             (intent={}, tier={}, tool_use={}). L'immagine resta investigabile \
                             via nexus_describe_image_attachment — verifica ai_price_catalog \
                             (supports_vision) e i provider in cooldown.",
                            provider,
                            model,
                            intent,
                            tier,
                            requires_tool_use,
                        );
                    }
                }
            }
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
        // Intent gia' classificato dal chiamante (es. brain router_node). Se
        // `Some` e non vuoto, si SALTA la classificazione LLM ridondante (regola
        // L: punto unico, niente doppia classificazione) che costa 0.7-0.9s e fa
        // sforare il timeout del client. Se `None`, mcp-core classifica.
        intent_hint: Option<&str>,
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
        // Override espliciti utente (ADR 0023). Gestiamo i quattro casi della
        // coppia (provider_override, model_override):
        //   (Some, Some) -> rispetta entrambi cosi' come sono.
        //   (None, Some) -> un modello identifica univocamente il suo provider:
        //                   lo ricaviamo dal catalogo prezzi (provider_for_model).
        //                   Se non trovato, fallback al routing per intent (sotto).
        //   (Some, None) -> provider forzato, modello dal routing/default provider.
        //   (None, None) -> routing per intent (nessun ramo qui).
        let provider_ov = provider_override.filter(|v| !v.trim().is_empty());
        let model_ov = model_override.filter(|v| !v.trim().is_empty());
        match (provider_ov, model_ov) {
            (Some(p), Some(m)) => {
                return (p.to_string(), m.to_string());
            }
            (Some(p), None) => {
                let routing = match Self::load_routing_config(db).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("load_routing_config (provider_ov): {e}");
                        RoutingConfig::default()
                    }
                };
                let model = routing.resolve_model(matrix, p, Some(p), model_override);
                return (p.to_string(), model);
            }
            (None, Some(m)) => {
                // model_override da solo: ricava il provider dal catalogo.
                match provider_for_model(db, m).await {
                    Some(provider) => {
                        tracing::info!(
                            "Agent routing (model_override): '{}' -> provider '{}' dal catalogo",
                            m,
                            provider
                        );
                        return (provider, m.to_string());
                    }
                    None => {
                        // Niente provider hardcoded (regola G): se il modello non
                        // e' nel catalogo, cadiamo nel routing per intent sotto.
                        tracing::warn!(
                            "model_override '{}' non trovato nel catalogo, fallback routing per intent",
                            m
                        );
                    }
                }
            }
            (None, None) => {}
        }

        // Routing locale — zero latenza gRPC
        // estimate_complexity usa solo le prime 200 parole per non farsi ingannare da liste dati
        let base_estimated = estimate_complexity(message);
        // Se la sessione ha già molti messaggi (continuazione di task lungo),
        // alza la stima per evitare di assegnare modelli troppo piccoli.
        // Ogni 10 messaggi = +1000 token equivalenti (cap a 6000).
        let context_bonus = ((context_message_count / 10) as u32 * 1_000).min(6_000);
        let estimated_tokens = base_estimated.saturating_add(context_bonus);
        // Punto unico classificazione (regola L): se il chiamante ha gia' l'intent
        // lo usiamo, altrimenti classifichiamo. Evita la classificazione LLM
        // ridondante e il timeout client su message non cachati.
        let intent_owned: String = match intent_hint {
            Some(h) if !h.trim().is_empty() => h.to_string(),
            _ => self
                .classify_intent_with_db_thresholds(db, message)
                .await
                .0
                .to_string(),
        };
        let intent = intent_owned.as_str();
        // La RoutingConfig admin può sovrascrivere il modello per provider.
        // Il behavior_mode effettivo: override sessione > DB globale.
        let routing = match Self::load_routing_config(db).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("load_routing_config (intent routing): {e}");
                RoutingConfig::default()
            }
        };
        let effective_behavior_mode: String = behavior_mode_override
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| routing.behavior_mode.clone());
        // Se behavior_mode == "dinamico" consulta il catalogo prezzi (come fa
        // Orchestrator::run). Altrimenti usa la matrix statica route_model_with_mode.
        // Estratte come String per uniformare i due rami (catalogo restituisce String,
        // matrix restituisce &'static str).
        // Caso speciale "dinamico": il catalogo prezzi è autoritativo.
        // Saltiamo candidates() e provider_models perché altrimenti riordinano sempre
        // sui provider configurati nell'admin (anthropic/openai prima) e applicano
        // il provider_model_<x> override → risultato: il dinamico non sceglie mai nulla.
        if effective_behavior_mode == "dinamico" {
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
                    None => {
                        // Niente magic fallback "light" (regola G): un intent non
                        // mappato in nexus_intent_capability e' tipicamente un task
                        // agentico (es. agentic_default), e degradarlo a "light"
                        // sceglie un modello debole (mistral-small ecc.). Default
                        // sicuro medium/reasoning + WARN per accorgersene e
                        // aggiungerlo alla tabella (mig 0110/0358).
                        tracing::warn!(
                            "Agent routing: intent '{}' non in nexus_intent_capability, \
                             uso default medium/reasoning (aggiungerlo alla tabella)",
                            intent
                        );
                        ("medium".to_string(), "reasoning".to_string())
                    }
                },
                None => {
                    tracing::warn!(
                        "intent_capability cache non disponibile, uso default medium/reasoning"
                    );
                    ("medium".to_string(), "reasoning".to_string())
                }
            };
            // Turno agentico = intent diverso da "chat" (convenzione del progetto,
            // model_routing.rs:772). Attiva il pavimento di tier agentico nel
            // selettore dinamico (regola L: la decisione "agentico?" arriva dal
            // chiamante che conosce l'intent, non re-implementata nel selettore).
            // Ricerca web citata (intent ricerca_web): instrada verso un modello con
            // capability web_search (Perplexity sonar) via il ramo NON-agentico
            // (requires_tool_use=false), perche' i sonar hanno supports_tool_use=false
            // e sono esclusi dai selettori agentici. Il gateway non allega tool ai
            // provider supports_tools=false (garanzia difensiva in generic.rs), quindi
            // il grafo gira in modalita' testo e completa con le citazioni. Gated su
            // intent: INERTE finche' il classifier non emette ricerca_web (che richiede
            // l'attivazione admin del prompt). Se nessun sonar e' disponibile (modelli
            // disabilitati / api_key assente) cade nel routing normale sotto.
            if intent == "ricerca_web" {
                // SERVIZIO UNICO (regola L). Pin su perplexity -> per I5 e'
                // `Exact{PinnedProvider}`: se perplexity non ha il tier/capability,
                // l'esito e' vuoto e si cade nel routing normale sotto (il piano B
                // c'e' gia'), invece di degradare pur di onorare il pin.
                if let Some((provider, model)) = crate::orchestrator::model_service::select_model(
                    db,
                    &crate::orchestrator::model_service::ModelRequest::non_agentic(&base_tier)
                        .capability(Some("web_search"))
                        .pinned("perplexity"),
                )
                .await
                .ok()
                .map(|c| (c.provider, c.model))
                {
                    if !is_provider_in_cooldown(&provider) {
                        let model = model_override
                            .filter(|v| !v.trim().is_empty())
                            .map(str::to_string)
                            .unwrap_or(model);
                        tracing::info!("Agent routing (ricerca_web): -> {}/{}", provider, model);
                        return (provider, model);
                    }
                }
                tracing::warn!(
                    "ricerca_web: nessun modello web_search disponibile (sonar disabilitato / provider non configurato), fallback al routing normale"
                );
            }
            let is_agentic_turn = intent != "chat";
            if let Some(d) =
                route_model_from_catalog(db, &base_tier, &capability, "dinamico", is_agentic_turn)
                    .await
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

        // is_risky_task rimosso: niente piu' elevazione automatica a "approfondita"
        // dedotta da verbi distruttivi via keyword. Resta solo dinamico->bilanciata.
        let effective_mode = if effective_behavior_mode == "dinamico" {
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

        // FASE 3 (Stadio 1) — shadow-compare opt-in (ADR 0030): NON cambia la
        // decisione servita; quando il flag routing.per_intent_runtime_shadow e'
        // attivo, calcola in parallelo la risoluzione tier-runtime e logga la
        // divergenza per misurare la parita' prima di abilitare il routing runtime
        // (stadi 2-3). Solo intent SENZA manual_override (i pin admin non si toccano).
        if !matrix.is_manual_override(intent, effective_mode) {
            crate::orchestrator::shadow_compare_per_intent(
                db,
                intent,
                effective_mode,
                estimated_tokens,
                &decision_provider,
                &decision_model,
            )
            .await;
        }

        // La matrice ha prodotto una decisione (intent, mode) → (provider, model).
        // E' servibile DIRETTAMENTE solo se non e' la sentinella `__no_model__` e il
        // provider NON e' in cooldown. Altrimenti (nessun match, oppure provider
        // scelto in cooldown) si consulta PRIMA il catalog tier-aware — punto unico
        // `select_agentic_model` (regola L) — e SOLO come ultima spiaggia si cade sul
        // default per-provider. Prima il ramo `__no_model__` sceglieva un provider
        // "tier-blind" (candidates + default_model_for_provider) PRIMA del catalog:
        // con anthropic+openai in cooldown finiva su google col suo default generico
        // gemini-2.5-flash (modello LIGHT), che non converge sui task di coding heavy.
        let (provider, model) = if !needs_catalog_fallback(&decision_provider) {
            let model = if let Some(m) = model_override.filter(|v| !v.trim().is_empty()) {
                m.to_string()
            } else {
                decision_model.clone()
            };
            (decision_provider, model)
        } else {
            self.resolve_via_catalog_fallback(
                db,
                matrix,
                &routing,
                intent,
                estimated_tokens,
                &decision_provider,
            )
            .await
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

    /// Fallback tier-aware su catalog quando la matrice non e' servibile
    /// direttamente (sentinella `__no_model__` o provider in cooldown). Estratto
    /// da `resolve_agent_provider` per contenerne lunghezza e complessita': la
    /// logica e il logging sono invariati.
    ///
    /// Consulta PRIMA il catalog tier-aware (`select_agentic_model`, punto unico
    /// regola L) e SOLO come ultima spiaggia cade sulla hierarchy classica
    /// (candidates + default per-provider); se nemmeno li' c'e' un provider fuori
    /// cooldown mantiene la sentinella, cosi' il chiamante ferma il run con alert.
    async fn resolve_via_catalog_fallback(
        &self,
        db: &PgPool,
        matrix: &crate::routing_matrix::RoutingMatrix,
        routing: &RoutingConfig,
        intent: &str,
        estimated_tokens: u32,
        decision_provider: &str,
    ) -> (String, String) {
        if decision_provider == "__no_model__" {
            tracing::warn!(
                "Agent routing: matrice senza modello per intent={} (provider preferito assente o in cooldown), consulto il catalog tier-aware",
                intent
            );
        } else {
            tracing::warn!(
                "Agent routing: '{}' scelto dal routing ma in cooldown, consulto il catalog tier-aware",
                decision_provider
            );
        }

        // Tier/capability dell'intent dalla cache intent_capability (mig 0110):
        // stessi valori del routing dinamico. Default light/chat se non mappato
        // (helper unico, regola L).
        let (tier, cap) = self.intent_tier_capability(intent, estimated_tokens).await;

        // SERVIZIO UNICO (regola L): `Degrade` — il tier e' un requisito, e se e'
        // vuoto si scende lungo `agentic_tier_chain`. Provider-agnostico: cerca
        // tra TUTTI i provider non in cooldown. `CostFirst` perche' il tier gia'
        // garantisce la fascia di capacita': dentro il tier si prende il piu'
        // ECONOMICO (obiettivo costi). I modelli economici problematici li
        // retrocede la governance telemetria-aware sugli esiti reali, non un flag
        // `is_featured` statico.
        let found = crate::orchestrator::model_service::select_model(
            db,
            &crate::orchestrator::model_service::ModelRequest::agentic(&tier)
                .capability(Some(&cap)),
        )
        .await
        .ok()
        .map(|c| (c.provider, c.model));
        if let Some((ref alt_provider, ref alt_model)) = found {
            tracing::info!(
                "Agent routing (fallback catalog tier-aware, selettore unico): {} → {}/{} (intent={}, tier={}, cap={})",
                decision_provider,
                alt_provider,
                alt_model,
                intent,
                tier,
                cap
            );
        }

        // ULTIMA spiaggia: hierarchy classica (candidates), raggiunta SOLO se il
        // catalog non ha nulla di sano nel tier. Se nemmeno qui c'e' un provider
        // fuori cooldown si mantiene la sentinella `__no_model__`, cosi' il
        // chiamante (resolve_agent_provider_detailed) calcoli no_capable_provider
        // e fermi il run con alert invece di spacciare un modello fittizio.
        found.unwrap_or_else(|| {
            let alt = routing
                .candidates(intent, None)
                .into_iter()
                .find(|p| !is_provider_in_cooldown(p))
                .unwrap_or_else(|| decision_provider.to_string());
            let alt_model = default_model_for_provider(matrix, &alt).to_string();
            tracing::warn!(
                "Agent routing (fallback legacy hierarchy, catalog vuoto): {} → {}/{}",
                decision_provider,
                alt,
                alt_model
            );
            (alt, alt_model)
        })
    }

    /// Suggerimento provider/model per `run` in base al behavior_mode. Estratto
    /// da `run` per contenerne lunghezza e complessita': i tre rami (dinamico
    /// col fallback tier-aware, manuale, statico) e il loro logging sono
    /// invariati. Ritorna `(Some(provider), Some(model))` (i rami producono
    /// sempre un suggerimento; l'Option resta per compatibilita' col chiamante).
    #[allow(clippy::too_many_arguments)]
    async fn resolve_suggested_model(
        &self,
        db: &PgPool,
        matrix: &crate::routing_matrix::RoutingMatrix,
        behavior_mode: &str,
        intent_str: &str,
        msg_tokens_estimate: u32,
        base_tier: &str,
        capability: &str,
    ) -> (Option<String>, Option<String>) {
        if behavior_mode == "dinamico" {
            // Turno agentico = intent != "chat" (convenzione del progetto):
            // attiva il pavimento di tier agentico nel selettore dinamico.
            let is_agentic_turn = intent_str != "chat";
            match route_model_from_catalog(db, base_tier, capability, "dinamico", is_agentic_turn)
                .await
            {
                Some(dyn_decision) if !is_provider_in_cooldown(&dyn_decision.provider) => {
                    tracing::info!(
                        "Dynamic catalog routing: intent={} tokens~{} → {}/{}",
                        intent_str,
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
                    // SERVIZIO UNICO (regola L): `Degrade` — stesso tier fra i
                    // provider sani, poi un gradino sotto.
                    //
                    // NB: qui l'ORDER BY era `input_cost ASC` SENZA il tie-break
                    // `is_featured DESC` di AGENTIC_COST_FIRST_ORDER — un'altra
                    // micro-divergenza della stessa famiglia, e per giunta un
                    // ordine NON deterministico a parita' di costo. `CostFirst`
                    // allinea al resto del routing e rende la scelta stabile.
                    let catalog_alt = crate::orchestrator::model_service::select_model(
                        db,
                        &crate::orchestrator::model_service::ModelRequest::agentic(base_tier)
                            .capability(Some(capability)),
                    )
                    .await
                    .ok()
                    .map(|c| (c.provider, c.model))
                    .map(|(p, m)| {
                        tracing::info!(
                            "Dynamic catalog routing (cooldown-fallback, selettore unico): → {}/{}",
                            p,
                            m
                        );
                        (Some(p), Some(m))
                    });

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
        } else if behavior_mode == "manuale" {
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
                intent_str,
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
                behavior_mode,
                pref.as_deref(),
                &thr,
            );
            tracing::info!(
                "Local routing: intent={} tokens~{} mode={} → {}/{}",
                intent_str,
                msg_tokens_estimate,
                behavior_mode,
                d.provider,
                d.model
            );
            (Some(d.provider.to_string()), Some(d.model.to_string()))
        }
    }

    /// Esecuzione LLM via Nexus Gateway (PATH A). Estratta da `run` per
    /// contenerne lunghezza e complessita': logica, billing e logging invariati.
    /// Ritorna `(provider, model, completion, usage, cost, currency)`.
    #[allow(clippy::too_many_arguments)]
    async fn execute_via_gateway(
        &self,
        db: &PgPool,
        gw: &NexusGatewayClient,
        matrix: &crate::routing_matrix::RoutingMatrix,
        input: &OrchestratorRequest,
        routing: &RoutingConfig,
        intent: &str,
        forced_provider: Option<&str>,
        forced_model: Option<&str>,
        suggested_provider: Option<&str>,
        suggested_model: Option<&str>,
        composed_prompt: &str,
        token_budget: u32,
        corrections_count: usize,
        run_id: Uuid,
        user_id: Uuid,
        project_uuid: Uuid,
    ) -> anyhow::Result<LlmExecution> {
        let alias = intent_to_alias(intent, &routing.behavior_mode, forced_model);
        let gw_model = if let Some(fp) = forced_provider {
            format!("{fp}/{}", forced_model.unwrap_or(&alias))
        } else {
            alias
        };
        let gw_req = GwRequest {
            model: gw_model,
            messages: vec![GwMessage {
                role: "user".to_string(),
                content: serde_json::Value::String(composed_prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                thinking_signature: None,
            }],
            max_tokens: Some(token_budget),
            temperature: None,
            metadata: GwMetadata {
                tenant_id: input.project_id.clone(),
                user_id: input.user_id.clone(),
                request_id: run_id.to_string(),
                // Claim locale di sensibilita' (punto unico dlp, regola L).
                // L'enforcement resta nel gateway, che ri-classifica e usa
                // max(claim, classificazione) + validate_tier_claim: il claim
                // onesto evita di dichiarare 'pubblico' (0) un prompt che il
                // DLP locale sa gia' essere sensibile.
                sensitivity_tier: crate::dlp::classify_sensitivity(composed_prompt) as u8,
                feature: intent.to_string(),
            },
            ..Default::default()
        };
        let prompt_tokens = mcp_token::count_tokens(composed_prompt) as i32;
        let estimated_completion = (token_budget as i32 - prompt_tokens).max(0);
        // Fallback provider/model letti da DB (matrice routing) invece che hardcoded.
        let fallback_provider: String = match matrix.lookup("chat", "bilanciata") {
            Some((p, _)) => p,
            None => "openai".to_string(),
        };
        let hint_provider_owned: String = forced_provider
            .or(suggested_provider)
            .map(String::from)
            .unwrap_or(fallback_provider);
        let fallback_model: String = default_model_for_provider(matrix, &hint_provider_owned);
        let hint_model_owned: String = forced_model
            .or(suggested_model)
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
                       "corrections_count": corrections_count}),
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
            "latency_ms": gw_resp.latency_ms, "finish_reason": gw_resp.finish_reason,
            // Fonti citate dai provider di ricerca (Perplexity): propagate nel
            // metadata per il pannello "Fonti consultate". Assente per gli altri.
            "citations": gw_resp.citations.clone()},
            "privacy_rerouted": gw_resp.privacy_rerouted.as_ref().map(|pr| json!({
                "provider": pr.provider,
                "blocked_tier": pr.blocked_tier,
                "reason": pr.reason,
            }))
        });
        if let Some(ref pr) = gw_resp.privacy_rerouted {
            tracing::warn!(
                "Nexus Gateway: privacy re-route tier={} → local provider={} intent={} tokens={}",
                pr.blocked_tier,
                pr.provider,
                intent,
                actual_usage.total_tokens
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
        Ok((
            gw_resp.provider_used,
            gw_resp.model_used,
            gw_completion,
            actual_usage,
            cost,
            cur,
        ))
    }

    /// Esecuzione LLM via Brain gRPC legacy (PATH B): itera i provider candidati,
    /// salta quelli non sani / rate-limited / falliti, e ritorna il primo che
    /// completa. Estratta da `run` per contenerne lunghezza e complessita':
    /// logica di fallback, billing e logging invariati.
    #[allow(clippy::too_many_arguments)]
    async fn execute_via_neural(
        &self,
        db: &PgPool,
        matrix: &crate::routing_matrix::RoutingMatrix,
        input: &OrchestratorRequest,
        routing: &RoutingConfig,
        intent: &str,
        forced_provider: Option<&str>,
        forced_model: Option<&str>,
        suggested_provider: Option<&str>,
        suggested_model: Option<&str>,
        composed_prompt: &str,
        prompt_corrections: &[Value],
        token_budget: u32,
        run_id: Uuid,
        user_id: Uuid,
        project_uuid: Uuid,
    ) -> anyhow::Result<LlmExecution> {
        let mut selected_provider: Option<String> = None;
        let mut selected_model: Option<String> = None;
        let mut completion: Option<serde_json::Value> = None;
        let mut usage: Option<UsageNumbers> = None;
        let mut usage_cost: Option<(f64, f64, f64, String)> = None;
        let mut skip_reasons = Vec::new();

        // In modalità dinamico la scelta del catalogo è autoritativa
        let provider_candidates = if let Some(provider) = forced_provider {
            vec![provider.to_string()]
        } else if routing.behavior_mode == "dinamico" {
            if let Some(p) = suggested_provider {
                vec![p.to_string()]
            } else {
                routing.candidates(intent, suggested_provider)
            }
        } else {
            routing.candidates(intent, suggested_provider)
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

            let model = if forced_provider == Some(provider.as_str()) {
                forced_model.map(str::to_string).unwrap_or_else(|| {
                    routing.resolve_model(matrix, &provider, Some(provider.as_str()), None)
                })
            } else if routing.behavior_mode == "dinamico"
                && suggested_provider == Some(provider.as_str())
                && suggested_model.is_some()
            {
                // In dinamico fidiamoci del catalogo: niente override da provider_model_<x>.
                // suggested_model.is_some() controllato sopra; clone+unwrap_or e' difensivo.
                suggested_model.map(str::to_string).unwrap_or_default()
            } else {
                routing.resolve_model(matrix, &provider, suggested_provider, suggested_model)
            };
            let prompt_tokens = mcp_token::count_tokens(composed_prompt) as i32;
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
                .generate_completion(&provider, &model, composed_prompt)
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
                    tracing::warn!(
                        "Provider {provider} segnala rate limit nella risposta, provo il prossimo"
                    );
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
        let completion = completion.ok_or_else(|| anyhow::anyhow!("No completion generated"))?;
        let usage = usage.unwrap_or(UsageNumbers {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        });
        let (_, _, cost, cur) = usage_cost.unwrap_or((0.0, 0.0, 0.0, "EUR".to_string()));
        Ok((provider, model, completion, usage, cost, cur))
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
            .classify_intent_with_db_thresholds(db, &input.message)
            .await;
        let intent = intent_str.to_string();
        let mut routing = Self::load_routing_config(db).await?;

        // Routing: se modalità "dinamico" usa il catalogo DB, altrimenti la matrice statica
        // Risolvi tier/capability dalla cache intent_capability (mig 0110)
        // (helper unico, regola L).
        let (base_tier, capability) = self
            .intent_tier_capability(intent_str, msg_tokens_estimate)
            .await;
        let (suggested_provider, suggested_model): (Option<String>, Option<String>) = self
            .resolve_suggested_model(
                db,
                matrix,
                &routing.behavior_mode,
                intent_str,
                msg_tokens_estimate,
                &base_tier,
                &capability,
            )
            .await;
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
        let (provider, model, completion, usage, total_cost, currency) =
            if let Some(gw) = &self.nexus_gateway {
                self.execute_via_gateway(
                    db,
                    gw,
                    matrix,
                    &input,
                    &routing,
                    &intent,
                    forced_provider.as_deref(),
                    forced_model.as_deref(),
                    suggested_provider.as_deref(),
                    suggested_model.as_deref(),
                    &composed_prompt,
                    token_budget,
                    prompt_corrections.len(),
                    run_id,
                    user_id,
                    project_uuid,
                )
                .await?
            } else {
                self.execute_via_neural(
                    db,
                    matrix,
                    &input,
                    &routing,
                    &intent,
                    forced_provider.as_deref(),
                    forced_model.as_deref(),
                    suggested_provider.as_deref(),
                    suggested_model.as_deref(),
                    &composed_prompt,
                    &prompt_corrections,
                    token_budget,
                    run_id,
                    user_id,
                    project_uuid,
                )
                .await?
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
        // orchestrator_runs e' migrata: vive nel DB del progetto (separazione DB).
        // A flag OFF / project_id non parsabile ricade sul meta-DB.
        let orch_pool = match Uuid::parse_str(&input.project_id) {
            Ok(pid) => crate::project_db_routes::project_data_pool_from(db, pid).await,
            Err(_) => db.clone(),
        };
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
        .execute(&orch_pool)
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

        Ok(OrchestratorResult { payload })
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
            // prompt_corrections e' migrata: la tabella vive nel DB del progetto
            // (separazione DB). settings/project_learning_config sopra restano su
            // meta (globale/non migrata); la ricerca Qdrant e' multi-tenant per
            // payload. A flag OFF il pool ritorna il meta-DB.
            let cpool = crate::project_db_routes::project_data_pool_from(db, project_id).await;
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
            .execute(&cpool)
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
                    'token_budget',
                    'max_token_budget',
                    -- NB: default_model, provider_model_*, <provider>_model NON sono
                    -- piu' selezionati: RoutingConfig::from_settings non li legge
                    -- (rimossi, fix __no_model__ / ADR 0030). Il default-per-provider
                    -- viene da nexus_provider_default_model (mig 0101).
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
