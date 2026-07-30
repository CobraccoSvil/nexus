//! Implementazione principale di Orchestrator: orchestrazione
//! del run agentico, risoluzione provider, prompt e routing.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    billing::{self, LedgerUsage},
    domain::OrchestratorAudit,
    nexus_gateway::NexusGatewayClient,
    provider_cooldown::is_provider_in_cooldown,
};

use super::*;

/// Esito di un'esecuzione LLM: `(provider, model, completion, usage, cost,
/// currency)`. Alias interno usato dai due path di esecuzione (gateway/neural)
/// estratti da `Orchestrator::run`.
type LlmExecution = (String, String, serde_json::Value, LedgerUsage, f64, String);

impl Orchestrator {
    /// Il gateway e' un parametro OBBLIGATORIO: senza non si puo' chiamare alcun
    /// LLM. Era iniettato da un `with_gateway` opzionale, chiamato solo se una
    /// probe all'avvio trovava il gateway gia' sano — e chi perdeva quella corsa
    /// restava senza gateway per sempre. Ora la firma non lo consente.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        neural: NeuralCoreClient,
        template_cache: crate::prompt_templates::TemplateCache,
        nexus_gateway: NexusGatewayClient,
        routing_matrix: crate::routing_matrix::RoutingMatrixCache,
        routing_thresholds: crate::routing_config::RoutingThresholdsCache,
        intent_capability: crate::routing_config::IntentCapabilityCache,
        slots_matrix: crate::routing_slots::SlotsRoutingMatrixCache,
    ) -> Self {
        Self {
            neural,
            template_cache,
            nexus_gateway,
            routing_matrix,
            routing_thresholds,
            intent_capability,
            slots_matrix,
        }
    }

    pub async fn neural_healthy(&self) -> bool {
        self.neural.is_healthy().await
    }

    /// Classifica l'intent restituendo solo `(intent, confidence)`.
    ///
    /// Delega a [`Self::classify_intent_full`] (punto unico, regola L): prima le
    /// due funzioni ripetevano la stessa scelta di motore, e una modifica a una
    /// sola avrebbe fatto divergere in silenzio i due chiamanti.
    async fn classify_intent_with_db_thresholds(
        &self,
        db: &PgPool,
        message: &str,
    ) -> (&'static str, f32) {
        let classified = self.classify_intent_full(db, message).await;
        (classified.intent, classified.confidence)
    }

    /// Variante "full" che ritorna `ClassifiedIntent` con candidati + flag
    /// ambiguita'. Usata da `spawn_agent_run` per decidere se chiedere
    /// disambiguazione all'utente (best practice NLU).
    ///
    /// Il motore e' UNO: `intent_classifier::classify` in-process via gateway.
    /// Non c'e' piu' una scelta rust-vs-python (`routing.classifier_engine`,
    /// mig 0458/0460) ne' un ripiego sull'endpoint brain `/classify-intent-agentic`:
    /// il brain e' stato eliminato (mig 0462/0532), quindi quel ramo non
    /// classificava piu' nulla — restituiva l'intent neutro e spegneva in
    /// silenzio il dimensionamento. Un'alternativa che non puo' funzionare non e'
    /// un fallback: e' un buco in cui cadere.
    ///
    /// Se il gateway non risponde, `classify_intent_full_rust` lo DICHIARA
    /// (`fallback_used=true` -> `classifier_resolved=false`) e i consumatori a
    /// valle sanno di non potersi fidare della classe.
    pub async fn classify_intent_full(&self, db: &PgPool, message: &str) -> ClassifiedIntent {
        let (classified, _ai) = classify_intent_full_rust(db, &self.nexus_gateway, message).await;
        classified
    }

    /// Routing slot-first (Livello 4 NLU): se il classifier ha estratto slot
    /// validi con confidence sufficiente, tenta lookup nella `nexus_routing_slots_matrix`
    /// (mig 0133). In caso di no-match o slot incompleti, ritorna `None` e il
    /// caller fa fallback al routing classico `(intent, behavior_mode)`.
    ///
    /// La soglia sopra la quale fidarsi degli slot si legge QUI, dalla cache delle
    /// `routing.*` (`slots_min_confidence`, mig 0658), non la passa il chiamante:
    /// era un `0.60` letterale nel call site, e un secondo consumatore avrebbe
    /// potuto scriverne un altro senza che nulla lo segnalasse (regola G/L). Se la
    /// cache non e' disponibile non si inventa una soglia: niente routing
    /// slot-based e si prosegue col percorso classico.
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
    ) -> Option<(String, String, &'static str)> {
        if !slots.is_complete() {
            return None;
        }
        let min_slot_confidence = match self.routing_thresholds.current_async().await {
            Ok(t) => t.slots_min_confidence,
            Err(e) => {
                tracing::warn!(
                    "route_by_slots: soglia slot non leggibile ({e}), fallback intent classico"
                );
                return None;
            }
        };
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
        let candidates = slot_routing_candidates(db, &req, slots).await?;
        let (provider, model) = pick_slot_candidate_out_of_cooldown(&candidates, &req, slots)?;
        Some((provider, model, "slots_matrix"))
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
        let gate_enabled = tool_use_gate_enabled(db).await;
        let caps = fetch_tool_capability_row(db, provider, model).await;
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
                self.apply_tool_use_fallback(db, provider, model, intent, message)
                    .await;
            }
        }
    }

    /// Ramo `NeedsFallback` del gate tool-use, estratto da
    /// [`Orchestrator::apply_tool_use_capability_gate`] per contenerne la
    /// lunghezza. Sostituzione in-place e logging invariati: se il catalog non
    /// offre nessun modello non-thinking tool-capable NON sostituisce e logga un
    /// WARN esplicito (fail visibile, regola G).
    async fn apply_tool_use_fallback(
        &self,
        db: &PgPool,
        provider: &mut String,
        model: &mut String,
        intent: &str,
        message: &str,
    ) {
        // Tier/capability dell'intent dalla cache (mig 0110), stessi
        // valori usati dal routing dinamico. Default light/chat se
        // l'intent non e' mappato (helper unico, regola L).
        let estimated_tokens = estimate_complexity(message);
        let (tier, capability) = self.intent_tier_capability(intent, estimated_tokens).await;
        // Fallback deterministico: miglior modello tool-capable del tier
        // (degradazione di tier controllata nel selettore).
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
                    "routing: {}/{} scartato per run agente (non tool-capable o thinking)                      -> fallback {}/{} (intent={}, tier={}, capability={})",
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
                    "routing: {}/{} non utilizzabile per run agente (intent={}) ma                      nessun modello non-thinking tool-capable disponibile nel catalog                      (tier={}, neppure rilassando la capability). Run proseguira' col                      modello originale — verifica ai_price_catalog (supports_tool_use,                      agentic_thinking_policy) e i provider in cooldown.",
                    provider,
                    model,
                    intent,
                    tier,
                );
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
                self.override_with_vision_model(db, provider, model, intent, message)
                    .await;
            }
        }
    }

    /// Ramo `NeedsVisionModel` del gate vision, estratto da
    /// [`Orchestrator::apply_vision_capability_gate`] per contenerne la
    /// lunghezza. Selezione (servizio unico `select_model` con capability
    /// `'vision'`), sostituzione in-place e logging sono invariati: se nessun
    /// modello vision e' disponibile NON sostituisce e logga un WARN.
    ///
    /// Il tier dell'intent arriva dalla cache (mig 0110), stesso meccanismo del
    /// gate tool-use; default light/chat se l'intent non e' mappato (helper
    /// unico, regola L): qui serve solo il tier. Run agentico (intent != "chat")
    /// -> serve un modello vision CHE sia anche tool-capable: passiamo il
    /// profilo agentico al selettore unico, cosi' non vanifichiamo il gate
    /// tool-use applicato prima.
    async fn override_with_vision_model(
        &self,
        db: &PgPool,
        provider: &mut String,
        model: &mut String,
        intent: &str,
        message: &str,
    ) {
        let estimated_tokens = estimate_complexity(message);
        let (tier, _capability) = self.intent_tier_capability(intent, estimated_tokens).await;
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
                    "routing(vision): {}/{} senza vision ma il turno ha un'immagine                      -> override {}/{} (intent={}, tier={}, tool_use={})",
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
                    "routing(vision): il turno ha un'immagine ma {}/{} non supporta la                      vision e nessun modello supports_vision=TRUE e' disponibile                      (intent={}, tier={}, tool_use={}). L'immagine resta investigabile                      via nexus_describe_image_attachment — verifica ai_price_catalog                      (supports_vision) e i provider in cooldown.",
                    provider,
                    model,
                    intent,
                    tier,
                    requires_tool_use,
                );
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
        if let Some(resolved) = self
            .resolve_user_override(db, matrix, provider_override, model_override)
            .await
        {
            return resolved;
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
            if let Some(resolved) = self
                .resolve_dynamic_catalog_provider(db, intent, estimated_tokens, model_override)
                .await
            {
                return resolved;
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

    /// Override espliciti utente (ADR 0023), estratti da
    /// [`Orchestrator::resolve_agent_provider`] per contenerne lunghezza e
    /// complessita' ciclomatica. Gestisce i quattro casi della coppia
    /// `(provider_override, model_override)`:
    ///   - `(Some, Some)` -> rispetta entrambi cosi' come sono.
    ///   - `(Some, None)` -> provider forzato, modello dal routing/default provider.
    ///   - `(None, Some)` -> un modello identifica univocamente il suo provider:
    ///     lo ricaviamo dal catalogo prezzi (`provider_for_model`). Se non
    ///     trovato, `None` -> il chiamante cade sul routing per intent.
    ///   - `(None, None)` -> `None`, routing per intent.
    async fn resolve_user_override(
        &self,
        db: &PgPool,
        matrix: &crate::routing_matrix::RoutingMatrix,
        provider_override: Option<&str>,
        model_override: Option<&str>,
    ) -> Option<(String, String)> {
        let provider_ov = provider_override.filter(|v| !v.trim().is_empty());
        let model_ov = model_override.filter(|v| !v.trim().is_empty());
        match (provider_ov, model_ov) {
            (Some(p), Some(m)) => Some((p.to_string(), m.to_string())),
            (Some(p), None) => {
                let routing = match Self::load_routing_config(db).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("load_routing_config (provider_ov): {e}");
                        RoutingConfig::default()
                    }
                };
                let model = routing.resolve_model(matrix, p, Some(p), model_override);
                Some((p.to_string(), model))
            }
            (None, Some(m)) => match provider_for_model(db, m).await {
                Some(provider) => {
                    tracing::info!(
                        "Agent routing (model_override): '{}' -> provider '{}' dal catalogo",
                        m,
                        provider
                    );
                    Some((provider, m.to_string()))
                }
                None => {
                    // Niente provider hardcoded (regola G): se il modello non
                    // e' nel catalogo, cadiamo nel routing per intent.
                    tracing::warn!(
                        "model_override '{}' non trovato nel catalogo, fallback routing per intent",
                        m
                    );
                    None
                }
            },
            (None, None) => None,
        }
    }

    /// Ramo "dinamico" di [`Orchestrator::resolve_agent_provider`]: il catalogo
    /// prezzi e' autoritativo. Estratto per contenerne lunghezza e complessita'
    /// ciclomatica; la logica e il logging sono invariati.
    ///
    /// `None` = il catalogo non ha prodotto un provider servibile (vuoto, oppure
    /// tutti in cooldown) e il chiamante cade sul ramo statico "bilanciata".
    ///
    /// Saltiamo `candidates()` e `provider_models` perche' altrimenti riordinano
    /// sempre sui provider configurati nell'admin (anthropic/openai prima) e
    /// applicano il `provider_model_<x>` override → risultato: il dinamico non
    /// sceglie mai nulla.
    async fn resolve_dynamic_catalog_provider(
        &self,
        db: &PgPool,
        intent: &str,
        estimated_tokens: u32,
        model_override: Option<&str>,
    ) -> Option<(String, String)> {
        let (base_tier, capability) = self.dynamic_tier_capability(intent, estimated_tokens).await;
        if intent == "ricerca_web" {
            if let Some(found) = resolve_web_search_model(db, &base_tier, model_override).await {
                return Some(found);
            }
        }
        // Turno agentico = intent diverso da "chat" (convenzione del progetto,
        // model_routing.rs:772). Attiva il pavimento di tier agentico nel
        // selettore dinamico (regola L: la decisione "agentico?" arriva dal
        // chiamante che conosce l'intent, non re-implementata nel selettore).
        let is_agentic_turn = intent != "chat";
        let d = route_model_from_catalog(db, &base_tier, &capability, "dinamico", is_agentic_turn)
            .await?;
        let provider = d.provider;
        if is_provider_in_cooldown(&provider) {
            tracing::warn!(
                "Agent routing: '{}' in cooldown (catalog/dinamico), skip",
                provider
            );
            return None;
        }
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
        Some((provider, model))
    }

    /// Tier/capability per il ramo dinamico, dalla cache `intent_capability`
    /// (mig 0110) invece che da un match Rust statico (rimosso). Estratta da
    /// [`Orchestrator::resolve_agent_provider`].
    ///
    /// Niente magic fallback "light" (regola G): un intent non mappato in
    /// `nexus_intent_capability` e' tipicamente un task agentico (es.
    /// `agentic_default`), e degradarlo a "light" sceglie un modello debole
    /// (mistral-small ecc.). Default sicuro medium/reasoning + WARN per
    /// accorgersene e aggiungerlo alla tabella (mig 0110/0358). NB: e'
    /// deliberatamente diverso dall'helper [`Orchestrator::intent_tier_capability`],
    /// che usa il default light/chat.
    async fn dynamic_tier_capability(
        &self,
        intent: &str,
        estimated_tokens: u32,
    ) -> (String, String) {
        let icap_arc = self.intent_capability.current_async().await.ok();
        let Some(map) = icap_arc.as_deref() else {
            tracing::warn!("intent_capability cache non disponibile, uso default medium/reasoning");
            return ("medium".to_string(), "reasoning".to_string());
        };
        match map.get(intent) {
            Some(c) => (
                c.tier_for_tokens(estimated_tokens),
                c.base_capability.clone(),
            ),
            None => {
                tracing::warn!(
                    "Agent routing: intent '{}' non in nexus_intent_capability, \
                     uso default medium/reasoning (aggiungerlo alla tabella)",
                    intent
                );
                ("medium".to_string(), "reasoning".to_string())
            }
        }
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
        log_catalog_fallback_reason(decision_provider, intent);

        // Tier/capability dell'intent dalla cache intent_capability (mig 0110):
        // stessi valori del routing dinamico. Default light/chat se non mappato
        // (helper unico, regola L).
        let (tier, cap) = self.intent_tier_capability(intent, estimated_tokens).await;

        let found = select_catalog_fallback_model(db, &tier, &cap).await;
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
        // catalog non ha nulla di sano nel tier.
        found.unwrap_or_else(|| {
            legacy_hierarchy_fallback(matrix, routing, intent, decision_provider)
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
            self.suggest_from_catalog(
                db,
                matrix,
                intent_str,
                msg_tokens_estimate,
                base_tier,
                capability,
            )
            .await
        } else {
            self.suggest_from_matrix(matrix, behavior_mode, intent_str, msg_tokens_estimate)
                .await
        }
    }

    /// Ramo "dinamico" di [`Orchestrator::resolve_suggested_model`]: il catalogo
    /// prezzi e' autoritativo. Estratto per contenerne lunghezza e complessita':
    /// se il provider scelto dal catalog e' sano lo si usa, altrimenti si cerca
    /// un'alternativa tier-aware (logging invariato).
    async fn suggest_from_catalog(
        &self,
        db: &PgPool,
        matrix: &crate::routing_matrix::RoutingMatrix,
        intent_str: &str,
        msg_tokens_estimate: u32,
        base_tier: &str,
        capability: &str,
    ) -> (Option<String>, Option<String>) {
        // Turno agentico = intent != "chat" (convenzione del progetto):
        // attiva il pavimento di tier agentico nel selettore dinamico.
        let is_agentic_turn = intent_str != "chat";
        match route_model_from_catalog(db, base_tier, capability, "dinamico", is_agentic_turn).await
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
                        d.provider,
                        d.model
                    );
                }
                self.suggest_catalog_cooldown_alternative(
                    db,
                    matrix,
                    intent_str,
                    msg_tokens_estimate,
                    base_tier,
                    capability,
                )
                .await
            }
        }
    }

    /// Alternativa tier-aware quando il ramo dinamico non ha prodotto un
    /// provider sano. Estratta da [`Orchestrator::suggest_from_catalog`].
    ///
    /// Tier-chain di degradazione graceful (PUNTO UNICO, regola L): stessa
    /// `agentic_tier_chain` del selettore principale, non piu' una copia
    /// hardcoded con degradazione diversa. Provider-agnostico: stesso tier tra
    /// tutti i provider sani, poi un gradino sotto. Ultima spiaggia: la matrice
    /// statica in modalita' "bilanciata".
    async fn suggest_catalog_cooldown_alternative(
        &self,
        db: &PgPool,
        matrix: &crate::routing_matrix::RoutingMatrix,
        intent_str: &str,
        msg_tokens_estimate: u32,
        base_tier: &str,
        capability: &str,
    ) -> (Option<String>, Option<String>) {
        // SERVIZIO UNICO (regola L): `Degrade` — stesso tier fra i provider
        // sani, poi un gradino sotto.
        //
        // NB: qui l'ORDER BY era `input_cost ASC` SENZA il tie-break
        // `is_featured DESC` di AGENTIC_COST_FIRST_ORDER — un'altra
        // micro-divergenza della stessa famiglia, e per giunta un ordine NON
        // deterministico a parita' di costo. `CostFirst` allinea al resto del
        // routing e rende la scelta stabile.
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
            let (pref, thr) = futures::executor::block_on(self.routing_helpers_for(intent_str));
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

    /// Rami NON dinamici di [`Orchestrator::resolve_suggested_model`]: "manuale"
    /// (nessun routing automatico — provider/model dalla config admin, risolti
    /// in "bilanciata") e locale (la modalita' configurata). Estratti per
    /// contenere la lunghezza del chiamante; i due messaggi di log restano
    /// distinti come in origine.
    async fn suggest_from_matrix(
        &self,
        matrix: &crate::routing_matrix::RoutingMatrix,
        behavior_mode: &str,
        intent_str: &str,
        msg_tokens_estimate: u32,
    ) -> (Option<String>, Option<String>) {
        let manual = behavior_mode == "manuale";
        let mode = if manual { "bilanciata" } else { behavior_mode };
        let (pref, thr) = self.routing_helpers_for(intent_str).await;
        let d = route_model_with_mode(
            matrix,
            intent_str,
            msg_tokens_estimate,
            mode,
            pref.as_deref(),
            &thr,
        );
        if manual {
            tracing::info!(
                "Manual routing config: intent={} tokens~{} → {}/{}",
                intent_str,
                msg_tokens_estimate,
                d.provider,
                d.model
            );
        } else {
            tracing::info!(
                "Local routing: intent={} tokens~{} mode={} → {}/{}",
                intent_str,
                msg_tokens_estimate,
                behavior_mode,
                d.provider,
                d.model
            );
        }
        (Some(d.provider.to_string()), Some(d.model.to_string()))
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
        provider_choice: &ProviderChoice,
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
        // Richiesta e coppia da prenotare nascono dal PUNTO UNICO (regola L):
        // il pin del provider forzato, il modello non prefissato e il modello
        // riallineato a quel provider sono decisi tutti li'.
        let call = build_chat_gateway_call(ChatCallSpec {
            routing,
            matrix,
            intent,
            provider_choice,
            forced_model,
            suggested_provider,
            suggested_model,
            composed_prompt,
            token_budget,
            tenant_id: &input.project_id,
            user_id: &input.user_id,
            request_id: run_id.to_string(),
        });
        // Il pin serve ancora DOPO che la richiesta e' stata consumata dalla
        // chiamata: e' cio' che rende leggibile il fallimento (nessun ripiego).
        let pin_provider = call.request.pin_provider.clone();
        // La stessa domanda che si porra' il gateway sulla richiesta che riceve,
        // posta qui sulla richiesta che PARTE, con la stessa regola (punto unico,
        // regola L). Non e' una formalita': se l'identita' c'e', un gateway che
        // poi non dichiara nulla non e' un caso legittimo, e la differenza fra i
        // due casi vale il doppio del costo della chiamata. Si misura sulla
        // richiesta VERA, non sul fatto che qui sopra girino due UUID.
        let identita_inviata = nexus_ledger::identity_from_metadata(
            &call.request.metadata.tenant_id,
            &call.request.metadata.user_id,
        )
        .is_some();
        let prompt_tokens = mcp_token::count_tokens(composed_prompt) as i32;
        let estimated_completion = (token_budget as i32 - prompt_tokens).max(0);
        let reservation = billing::reserve_usage(
            db,
            user_id,
            project_uuid,
            &call.ledger_provider,
            &call.ledger_model,
            prompt_tokens,
            estimated_completion,
            json!({"intent": intent, "profile_id": input.profile_id,
                       "via_nexus_gateway": true,
                       "corrections_count": corrections_count}),
        )
        .await
        .map_err(|e| anyhow::anyhow!("billing_rejected: {e}"))?;

        let gw_resp = match gw.complete(call.request).await {
            Ok(r) => r,
            Err(e) => {
                billing::release_usage(db, &reservation, "gateway_error", None).await;
                // La resa nasce QUI, dove si sa ancora che il provider era
                // forzato, e viaggia come errore tipizzato: il confine HTTP
                // della chat la rilegge invece di ri-derivarla da una stringa
                // gia' appiattita (`chat_messages::run` -> rendered_from_error).
                let rendered = rendered_chat_gateway_error(&e, pin_provider.as_deref());
                tracing::warn!(
                    intent = %intent,
                    // Lo stato del vincolo si DICHIARA (regola M): "auto",
                    // "preferred" o "pinned" dice subito se il fallback c'era e
                    // non e' bastato, oppure se non c'era per scelta dell'utente.
                    provider_choice = %provider_choice.label(),
                    pin_provider = ?pin_provider,
                    "chat: chiamata al gateway fallita: {}",
                    rendered.log_line()
                );
                return Err(anyhow::Error::new(rendered));
            }
        };
        // Dal punto unico: la costruzione a mano scartava i campi di cache che
        // `GwUsage` porta gia' (era il terzo percorso che li perdeva).
        let actual_usage = billing::usage_from_gateway(&gw_resp.usage);
        // Chi addebita questa chiamata lo decide il punto unico, dal segnale che
        // il gateway emette solo se ha davvero scritto la sua riga: se l'ha
        // scritta la prenotazione si rilascia (una sola riga finalizzata per
        // run), altrimenti si finalizza come sempre (nessun addebito perso).
        let dichiarazione = nexus_ledger::Declaration::dal_wire(gw_resp.ledger.clone());
        let settlement =
            billing::settle_usage(db, &reservation, run_id, &actual_usage, &dichiarazione).await?;

        // Il verdetto sulla dichiarazione, confrontata con cio' che e' stato
        // MANDATO. Il caso che qui va urlato e' il silenzio su una chiamata con
        // identita' valida: i due servizi sono processi distinti che si
        // aggiornano in momenti diversi, e un gateway di build precedente la riga
        // l'ha scritta lo stesso — cioe' questa chiamata la sta pagando due
        // volte, ed e' esattamente il difetto del 2026-07-27 che ritorna. Finora
        // non c'era ne' un WARN ne' un contatore: il ritorno del difetto sarebbe
        // stato invisibile quanto la prima volta.
        let audit = dichiarazione.audit(identita_inviata);
        if audit.sospetta() {
            tracing::warn!(
                target: "billing",
                // Regola M: si filtra sui codici, non sulla frase.
                audit = audit.code(),
                declared = dichiarazione.as_str(),
                identity_sent = identita_inviata,
                charged_by = settlement.charged_by.as_str(),
                run_id = %run_id,
                provider = %gw_resp.provider_used,
                model = %gw_resp.model_used,
                reservation_ledger_id = %reservation.ledger_id,
                "billing: dichiarazione contabile SOSPETTA dal gateway — {}",
                audit.conseguenza()
            );
        }
        // Anche nel caso normale l'esito si porta appresso: e' la riga che, di
        // fronte a un importo che non torna, dice QUALE delle due guardare —
        // quella di chi ha eseguito o la prenotazione, che se rilasciata vale
        // zero. Viaggia sulla riga di log che esiste gia' (sotto), non su una in
        // piu': `ChargedBy` e' `Copy`, e sopravvive al move di `settlement`.
        let charged_by = settlement.charged_by;
        let (cost, cur) = (settlement.total_cost, settlement.currency);
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
                charged_by = charged_by.as_str(),
                "Nexus Gateway: privacy re-route tier={} → local provider={} intent={} tokens={}",
                pr.blocked_tier,
                pr.provider,
                intent,
                actual_usage.total_tokens
            );
        } else {
            tracing::info!(
                // `charged_by` viaggia sulla riga che esiste gia': dice quale
                // delle due righe di ledger porta davvero l'addebito di questa
                // chiamata — quella del gateway o la prenotazione — e senza di
                // esso, davanti a un importo che non torna, non si sa nemmeno da
                // che parte cominciare a cercare.
                charged_by = charged_by.as_str(),
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
        // La scelta di provider arriva GIA' risolta (provider + forza del
        // vincolo) dal punto unico `ProviderChoice::resolve`, chiamato al
        // confine HTTP: qui non si deduce piu' niente dal solo nome, e la
        // normalizzazione (trim/lowercase) vive li' invece che in ogni lettore.
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
        // Memorie di progetto pertinenti alla domanda, dal punto unico
        // `prompt_memories` (regola L): lo stesso caricatore che usa il percorso
        // agentico in `spawn_agent_run`. Prima viveva qui dentro, e questo e'
        // l'unico ramo che lo raggiungeva.
        let memories = crate::prompt_memories::ProjectMemories::load(
            db,
            &crate::prompt_memories::VectorRecall::new(&self.neural),
            project_uuid,
            &input.message,
        )
        .await;
        let composed_prompt = Self::compose_prompt(
            db,
            &self.template_cache,
            &context.optimized_prompt,
            &memories,
            input.automation_mode,
            &input.attachments,
        )
        .await;

        // ── Step 4: LLM Execution ─────────────────────────────────────────────────
        // Un solo path: il Nexus Gateway (routing, DLP, rate limiting, fallback
        // automatico). Il "PATH B" storico era il brain gRPC, usato quando il
        // gateway risultava assente: il brain non esiste piu' e il gateway ora e'
        // obbligatorio per costruzione, quindi l'alternativa non c'e'.
        let (provider, model, completion, usage, total_cost, currency) = self
            .execute_via_gateway(
                db,
                &self.nexus_gateway,
                matrix,
                &input,
                &routing,
                &intent,
                &input.provider_choice,
                forced_model.as_deref(),
                suggested_provider.as_deref(),
                suggested_model.as_deref(),
                &composed_prompt,
                token_budget,
                memories.len(),
                run_id,
                user_id,
                project_uuid,
            )
            .await?;

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
        // project_id non parsabile (id legacy non-UUID) -> meta-DB (comportamento
        // storico); DB progetto non disponibile -> insert saltato con WARN
        // (l'audit e' best-effort, vedi `.ok()` sotto — niente fallback al meta).
        let orch_pool = match Uuid::parse_str(&input.project_id) {
            Ok(pid) => match crate::project_db_routes::project_data_pool_from(db, pid).await {
                Ok(pool) => Some(pool),
                Err(e) => {
                    tracing::warn!(project_id = %pid, error = %e, "orchestrator audit: DB progetto non disponibile, insert orchestrator_runs saltato");
                    None
                }
            },
            Err(_) => Some(db.clone()),
        };
        if let Some(orch_pool) = orch_pool {
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
        }

        let payload = json!({
            "run_id": run_id.to_string(),
            "intent": intent,
            "provider": provider,
            "model": model,
            "completion": completion,
            "tokens_saved": context.tokens_saved,
            "prompt_tokens": usage.tokens.prompt_tokens,
            "completion_tokens": usage.tokens.completion_tokens,
            "total_tokens": usage.total_tokens,
            "total_cost": total_cost,
            "currency": currency,
            "applied_corrections": memories.into_values(),
            "automation_mode": input.automation_mode.as_str(),
            "attachments_count": input.attachments.len(),
        });

        Ok(OrchestratorResult { payload })
    }

    async fn compose_prompt(
        db: &PgPool,
        cache: &crate::prompt_templates::TemplateCache,
        base_prompt: &str,
        memories: &crate::prompt_memories::ProjectMemories,
        automation_mode: AutomationMode,
        attachments: &[ChatAttachment],
    ) -> String {
        let tpl_key = automation_mode.prompt_instruction_template_key();
        let mode_instruction =
            crate::prompt_templates::get_template_or_default(db, cache, tpl_key).await;
        let mut sections = vec![mode_instruction];
        if let Some(block) = memories.section() {
            sections.push(block);
        }
        sections.extend(attachment_sections(attachments));
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

/// Ricerca web citata (intent `ricerca_web`): instrada verso un modello con
/// capability `web_search` (Perplexity sonar) via il ramo NON-agentico
/// (`requires_tool_use=false`), perche' i sonar hanno `supports_tool_use=false`
/// e sono esclusi dai selettori agentici. Il gateway non allega tool ai provider
/// `supports_tools=false` (garanzia difensiva in `generic.rs`), quindi il grafo
/// gira in modalita' testo e completa con le citazioni.
///
/// Estratta da [`Orchestrator::resolve_agent_provider`]: gated su intent dal
/// chiamante, quindi INERTE finche' il classifier non emette `ricerca_web` (che
/// richiede l'attivazione admin del prompt). `None` (col WARN) se nessun sonar
/// e' disponibile (modelli disabilitati / api_key assente / cooldown): il
/// chiamante cade nel routing normale.
async fn resolve_web_search_model(
    db: &PgPool,
    base_tier: &str,
    model_override: Option<&str>,
) -> Option<(String, String)> {
    // SERVIZIO UNICO (regola L). Pin su perplexity -> per I5 e'
    // `Exact{PinnedProvider}`: se perplexity non ha il tier/capability,
    // l'esito e' vuoto e il chiamante cade nel routing normale (il piano B
    // c'e' gia'), invece di degradare pur di onorare il pin.
    let found = crate::orchestrator::model_service::select_model(
        db,
        &crate::orchestrator::model_service::ModelRequest::non_agentic(base_tier)
            .capability(Some("web_search"))
            .pinned("perplexity"),
    )
    .await
    .ok()
    .map(|c| (c.provider, c.model));
    if let Some((provider, model)) = found {
        if !is_provider_in_cooldown(&provider) {
            let model = model_override
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
                .unwrap_or(model);
            tracing::info!("Agent routing (ricerca_web): -> {}/{}", provider, model);
            return Some((provider, model));
        }
    }
    tracing::warn!(
        "ricerca_web: nessun modello web_search disponibile (sonar disabilitato / provider non configurato), fallback al routing normale"
    );
    None
}

/// Candidati (provider, model) sani per una richiesta slot-routed, dal punto
/// unico tier-based `select_models_for_requirement`: ordinati per score, uno per
/// provider — la rotazione provider per disponibilita' vale quindi anche per le
/// richieste slot-routed. Estratti da [`Orchestrator::route_by_slots`]: `None`
/// (col medesimo logging) sia su errore di selezione sia su lista vuota, cosi'
/// il chiamante cade sul routing intent classico.
async fn slot_routing_candidates(
    db: &sqlx::PgPool,
    req: &crate::routing_slots::SlotRequirement,
    slots: &crate::routing_slots::ActionSlots,
) -> Option<Vec<(String, String)>> {
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
    Some(candidates)
}

/// Cooldown-awareness del routing slot-first: ritorna il primo candidato con
/// provider NON in cooldown, oppure `None` (col WARN "tutti in cooldown") se non
/// ce n'e' nessuno. Estratta da [`Orchestrator::route_by_slots`]: stessa
/// iterazione, stesso logging. Il tag di sorgente `"slots_matrix"` resta al
/// chiamante.
fn pick_slot_candidate_out_of_cooldown(
    candidates: &[(String, String)],
    req: &crate::routing_slots::SlotRequirement,
    slots: &crate::routing_slots::ActionSlots,
) -> Option<(String, String)> {
    let mut skipped: Vec<String> = Vec::new();
    for (provider, model) in candidates {
        if crate::provider_cooldown::is_provider_in_cooldown(provider) {
            skipped.push(provider.clone());
            continue;
        }
        if !skipped.is_empty() {
            tracing::info!(
                "route_by_slots: skip provider in cooldown [{}], scelto {}/{} (tier {}, pos {}/{})",
                skipped.join(","),
                provider,
                model,
                req.preferred_tier,
                skipped.len() + 1,
                candidates.len(),
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
        return Some((provider.clone(), model.clone()));
    }
    // Tutti i provider candidati sono in cooldown.
    tracing::warn!(
        "route_by_slots: TUTTI i {} provider candidati in cooldown [{}], fallback intent classico",
        candidates.len(),
        skipped.join(",")
    );
    None
}

/// WARN che motiva il fallback su catalog tier-aware: la matrice non aveva un
/// modello per l'intent (sentinella `__no_model__`) oppure il provider scelto e'
/// in cooldown. Estratto da [`Orchestrator::resolve_via_catalog_fallback`]:
/// stessi due messaggi, stessa condizione.
fn log_catalog_fallback_reason(decision_provider: &str, intent: &str) {
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
}

/// Selezione tier-aware su catalog per il fallback di routing.
///
/// Tier-chain di degradazione graceful (PUNTO UNICO, regola L):
/// `agentic_tier_chain`, la STESSA usata da `route_model_from_catalog` e
/// `select_models_tierchain`. Provider-agnostico: cerca il tier richiesto tra
/// TUTTI i provider non in cooldown (stessi criteri), e degrada di UN gradino
/// solo se quel tier e' vuoto. Prima qui c'era una tier-chain hardcoded con
/// pavimento diverso -> degradazione incoerente tra i percorsi (rimossa).
///
/// PUNTO UNICO di selezione agentica (regola L): miglior modello tool-capable
/// del tier/capability, da un provider NON in cooldown. Ordine allineato a
/// `route_model_from_catalog` (AGENTIC_COST_FIRST_ORDER): il tier gia'
/// garantisce la fascia di capacita', dentro il tier si prende il piu' ECONOMICO
/// (obiettivo costi). I modelli economici problematici sono retrocessi dalla
/// governance telemetria-aware e dagli esiti dei run, non da un flag
/// `is_featured` statico.
async fn select_catalog_fallback_model(
    db: &PgPool,
    tier: &str,
    cap: &str,
) -> Option<(String, String)> {
    // SERVIZIO UNICO (regola L): `Degrade` — il tier e' un requisito, e se e'
    // vuoto si scende lungo `agentic_tier_chain`. Provider-agnostico: cerca
    // tra TUTTI i provider non in cooldown. `CostFirst` perche' il tier gia'
    // garantisce la fascia di capacita': dentro il tier si prende il piu'
    // ECONOMICO (obiettivo costi). I modelli economici problematici li
    // retrocede la governance telemetria-aware sugli esiti reali, non un flag
    // `is_featured` statico.
    crate::orchestrator::model_service::select_model(
        db,
        &crate::orchestrator::model_service::ModelRequest::agentic(tier).capability(Some(cap)),
    )
    .await
    .ok()
    .map(|c| (c.provider, c.model))
}

/// Ultima spiaggia del fallback di routing: hierarchy classica (candidates +
/// default per-provider), raggiunta SOLO se il catalog non ha nulla di sano nel
/// tier. Se nemmeno qui c'e' un provider fuori cooldown mantiene la sentinella
/// `__no_model__`, cosi' il chiamante (`resolve_agent_provider_detailed`)
/// calcola `no_capable_provider` e ferma il run con alert invece di spacciare un
/// modello fittizio. Estratta da [`Orchestrator::resolve_via_catalog_fallback`].
fn legacy_hierarchy_fallback(
    matrix: &crate::routing_matrix::RoutingMatrix,
    routing: &RoutingConfig,
    intent: &str,
    decision_provider: &str,
) -> (String, String) {
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
}

/// Flag DB `agent.require_tool_use_capability` (default `true`) che governa il
/// gate tool-use. Estratto da [`Orchestrator::apply_tool_use_capability_gate`]:
/// stesso pattern di lettura settings usato altrove (es.
/// `agent.model_tool_failure_threshold` in `agent_run.rs`), stessa semantica dei
/// valori spenti (`false` / `0` / `no`).
async fn tool_use_gate_enabled(db: &PgPool) -> bool {
    crate::settings::get_setting(db, "agent.require_tool_use_capability")
        .await
        .ok()
        .flatten()
        .map(|v| {
            let t = v.trim().to_ascii_lowercase();
            !(t == "false" || t == "0" || t == "no")
        })
        .unwrap_or(true)
}

/// Capability del modello risolto dal catalog: `(supports_tool_use,
/// agentic_thinking_policy, is_enabled)`. `None` = modello assente (problema di
/// sync, gestito conservativamente dalla funzione pura `decide_tool_capability_gate`).
///
/// `agentic_thinking_policy`: solo `'exclude'` (reasoning-only senza function
/// calling) va scartato dagli agentici; i dual-mode (deepseek-v4, claude,
/// gemini-2.5) restano e l'adapter forza il non-thinking (ADR 0025).
/// `is_enabled`: un modello DISABILITATO (es. legacy pruned dalla mig 0320,
/// raggiunto via una config di default stale) non e' chiamabile -> va comunque
/// sostituito su un run agentico (robustezza oltre alla policy).
async fn fetch_tool_capability_row(
    db: &PgPool,
    provider: &str,
    model: &str,
) -> Option<(bool, String, bool)> {
    sqlx::query_as::<_, (bool, String, bool)>(
        "SELECT supports_tool_use, agentic_thinking_policy, is_enabled FROM ai_price_catalog \
         WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Sezioni allegati del prompt composto (prima i file testuali, poi l'elenco
/// delle immagini). Estratte da `compose_prompt`: stesso ordine, stessa
/// formattazione, e nessuna sezione quando non ci sono allegati del tipo.
fn attachment_sections(attachments: &[ChatAttachment]) -> Vec<String> {
    let mut sections = Vec::new();
    if attachments.is_empty() {
        return sections;
    }
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
    sections
}
