//! Runtime dei nodi: contesto d'esecuzione + porte I/O astratte.
//!
//! Questo modulo NON contiene il motore del grafo (quello vive in `nexus-graph`,
//! crate puro): qui c'e' solo l'INFRASTRUTTURA che i nodi concreti usano per
//! fare I/O senza accoppiarsi a mcp-core. Vedi `ports.rs` (trait astratti) e
//! `ctx.rs` (`AgentNodeCtx`, il tipo `C` di `GraphNode`).

pub mod ctx;
pub mod ports;

pub use ctx::AgentNodeCtx;
pub use ports::{
    AgentStepStore, BillingCooldownPort, ContextOffload, ContextPressure, Coordination,
    CriteriaRunner, CriterionResult, CriterionSpec, EmbeddingStore, EscalationInputs,
    EscalationPort, EventSink, ExecMode, LlmGateway, LlmMessage, LlmRequest, LlmResponse, LlmUsage,
    MetaReasonerPort, MetaStepStore, ModelUpscalePort, NextActionChoice, NextActionsDeriver,
    OffloadKind, OrchPhase, OrchestrationContext, OrchestrationMove, PlanBlock, PlanRow, PortError,
    RecoveryMove, RunControlStore, ScaleContext, ScaleMove, ScaleTier, SseEvent, StallBudgetPort,
    StallContext, SubTask, SummaryStore, SupervisorContext, TodoStore, ToolCall, ToolExecutor, ToolOutcome,
    UpscalePick, VerifierRunRecord, VerifierRunStore,
};

use async_trait::async_trait;

/// Sink eventi NO-OP: scarta ogni evento, nessun output verso l'utente.
///
/// E' un'implementazione legittima in PRODUZIONE per il run SHADOW (read-only):
/// lo shadow non deve emettere nulla sul canale SSE del frontend (l'output
/// all'utente resta quello del primario). Vive fuori da `#[cfg(test)]` perche'
/// e' un no-op reale, non un doppio di test. I test lo riusano (regola L: un
/// solo no-op sink, non duplicato in `test_doubles`).
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn emit(&self, _ev: SseEvent) {}
}

/// Meta-reasoner INERTE (kill-switch OFF): tutti e tre i metodi
/// ([`MetaReasonerPort::recover`] recovery, [`MetaReasonerPort::orchestrate`]
/// orchestrazione, [`MetaReasonerPort::assess_scale`] scala-tier) ritornano sempre
/// `Ok(None)` (nessuna mossa) in ogni modalita'.
///
/// E' un'implementazione legittima in PRODUZIONE, non un doppio di test (come
/// [`NullEventSink`]): finche' i flag `agent.stall_recovery.enabled` /
/// `agent.orchestration.enabled` sono `false` (default, regola G) mcp-core inietta
/// QUESTA porta (o l'impl concreta la degrada a `Ok(None)`), e i nodi ricadono
/// sulla gerarchia/euristica fissa (`progress_controller::decide`,
/// `is_eligible`/`should_parallelize`) — rete di sicurezza. Cosi' il wiring compila
/// e resta inerte SENZA la porta concreta (`PgMetaReasonerPort`). Vive fuori da
/// `#[cfg(test)]`: e' il fallback inerte reale, riusato anche dai test (regola L,
/// un solo no-op reasoner).
///
/// NB: `Ok(None)` e' il degrado LEGITTIMO opt-in (flag OFF / purpose NotFound), NON
/// un "magic fallback" mascherante (regola G): un DB-down / provider indisponibile
/// nell'impl CONCRETA ritorna `Err(PortError::ProviderUnavailable)`, mai `Ok(None)`.
/// Questo stub e' inerte per COSTRUZIONE (non consulta nulla), non maschera guasti.
pub struct StubMetaReasonerPort;

#[async_trait]
impl MetaReasonerPort for StubMetaReasonerPort {
    async fn recover(
        &self,
        _ctx: StallContext,
        _mode: ExecMode,
    ) -> Result<Option<RecoveryMove>, PortError> {
        Ok(None)
    }

    async fn orchestrate(
        &self,
        _ctx: OrchestrationContext,
        _mode: ExecMode,
    ) -> Result<Option<OrchestrationMove>, PortError> {
        Ok(None)
    }

    async fn assess_scale(
        &self,
        _ctx: ScaleContext,
        _mode: ExecMode,
    ) -> Result<Option<ScaleMove>, PortError> {
        Ok(None)
    }

    async fn supervise(
        &self,
        _ctx: SupervisorContext,
        _mode: ExecMode,
    ) -> Result<Option<crate::decisions::supervisor::SupervisorDecision>, PortError> {
        Ok(Some(crate::decisions::supervisor::SupervisorDecision::Continue))
    }
}

/// Budget stall CROSS-RUN INERTE: [`StallBudgetPort::consultations_in_session`]
/// ritorna sempre `Ok(0)` e [`StallBudgetPort::record_consultation`] e' un no-op.
///
/// E' un'implementazione legittima in PRODUZIONE (come [`NullEventSink`] /
/// [`StubMetaReasonerPort`]): quando l'`ExecutorNode` NON riceve una porta budget
/// concreta (`with_stall_budget` non chiamato — es. graph di scaffold, test), il
/// cap cross-run non e' disponibile e il motore ricade sul solo cap PER-RUN
/// (`extra["stall_moves_used"]`, comportamento storico). FAIL-OPEN per
/// costruzione: `Ok(0)` = budget cross-run non esaurito, non blocca mai. Vive
/// fuori da `#[cfg(test)]`: e' il fallback inerte reale, riusato anche dai test
/// (regola L, un solo no-op budget).
pub struct NullStallBudgetPort;

#[async_trait]
impl StallBudgetPort for NullStallBudgetPort {
    async fn consultations_in_session(&self, _session_id: uuid::Uuid) -> Result<i64, PortError> {
        Ok(0)
    }

    async fn record_consultation(
        &self,
        _session_id: uuid::Uuid,
        _mode: ExecMode,
    ) -> Result<(), PortError> {
        Ok(())
    }
}

#[cfg(test)]
pub mod test_doubles {
    //! Implementazioni di test delle porte (mock/stub) riusabili dai test dei
    //! nodi e dello shadow. Ritornano valori fissi e/o registrano le chiamate
    //! ricevute, cosi' i test verificano comportamento senza I/O reale.

    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::ports::{
        AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, CriterionResult,
        CriterionSpec, EmbeddingStore, EscalationInputs, EscalationPort, EventSink, ExecMode,
        LlmGateway, LlmRequest, LlmResponse, LlmUsage, MetaStepStore, ModelUpscalePort,
        NextActionChoice, NextActionsDeriver, OffloadKind, PlanRow, PortError,
        ProviderFailureCause, ProviderUnavailableInfo, RunControlStore,
        SseEvent, SummaryStore, TodoStore, ToolCall, ToolExecutor, ToolOutcome, UpscalePick,
        VerifierRunRecord, VerifierRunStore,
    };
    use crate::decisions::dag_scheduler::{Todo, TodoStatus};
    use crate::decisions::escalation::CrossProviderCandidate;

    /// Gateway LLM di test: ritorna una `LlmResponse` fissa e registra le
    /// richieste ricevute (per asserzioni sull'input passato dal nodo).
    pub struct StubLlmGateway {
        /// Risposta fissa ritornata da ogni `complete`.
        pub canned: LlmResponse,
        /// Se `Some`, `complete` ritorna un `Err`: esercita il ramo error del nodo
        /// (parita' con l'except Python). La VARIANTE e' decisa da
        /// [`Self::error_provider_unavailable`].
        pub error: Option<String>,
        /// Se `true` (e `error` e' `Some`), `complete` ritorna
        /// `Err(PortError::ProviderUnavailable(_))` invece di `PortError::Llm(_)`:
        /// esercita il ramo FALLBACK cross-provider del nodo (provider in cooldown).
        pub error_provider_unavailable: bool,
        /// Causa tipizzata quando `error_provider_unavailable` e' true. `None` ->
        /// `Unknown` (retro-compatibile).
        pub provider_unavailable_cause: Option<ProviderFailureCause>,
        /// Richieste registrate (in ordine d'arrivo).
        pub seen: Mutex<Vec<LlmRequest>>,
    }

    impl StubLlmGateway {
        /// Crea uno stub con una risposta testuale semplice.
        pub fn with_text(text: &str) -> Self {
            Self {
                canned: LlmResponse {
                    content: text.to_string(),
                    tool_calls: vec![],
                    usage: LlmUsage::default(),
                    ..Default::default()
                },
                error: None,
                error_provider_unavailable: false,
                provider_unavailable_cause: None,
                seen: Mutex::new(vec![]),
            }
        }

        /// Crea uno stub che emette UN tool_call (nome + input dati), nessun testo.
        /// Usato dai test dei nodi che forzano una tool call (es. il planner con
        /// `nexus_todo_write`).
        pub fn with_tool_call(name: &str, input: serde_json::Value) -> Self {
            Self {
                canned: LlmResponse {
                    content: String::new(),
                    tool_calls: vec![crate::state::ToolUse {
                        id: "stub-tc".to_string(),
                        name: name.to_string(),
                        input,
                        thought_signature: None,
                    }],
                    usage: LlmUsage::default(),
                    ..Default::default()
                },
                error: None,
                error_provider_unavailable: false,
                provider_unavailable_cause: None,
                seen: Mutex::new(vec![]),
            }
        }

        /// Crea uno stub il cui `complete` fallisce sempre con `PortError::Llm`
        /// (provider down/billing): per il ramo error del nodo (il run NON deve
        /// abortire con `NodeError` ma proseguire al delta con `stop_reason=error`).
        pub fn with_error(message: &str) -> Self {
            Self {
                canned: LlmResponse::default(),
                error: Some(message.to_string()),
                error_provider_unavailable: false,
                provider_unavailable_cause: None,
                seen: Mutex::new(vec![]),
            }
        }

        /// Crea uno stub il cui `complete` fallisce sempre con
        /// `PortError::ProviderUnavailable` (provider scelto in cooldown): per il
        /// ramo FALLBACK cross-provider del nodo (escalation invece di chiusura
        /// `Error`).
        pub fn with_provider_unavailable(message: &str) -> Self {
            Self {
                canned: LlmResponse::default(),
                error: Some(message.to_string()),
                error_provider_unavailable: true,
                provider_unavailable_cause: None,
                seen: Mutex::new(vec![]),
            }
        }

        /// Come [`with_provider_unavailable`](Self::with_provider_unavailable) ma con
        /// causa tipizzata esplicita (es. `ClientError` per test policy failover).
        pub fn with_provider_unavailable_cause(
            cause: ProviderFailureCause,
            message: &str,
        ) -> Self {
            Self {
                canned: LlmResponse::default(),
                error: Some(message.to_string()),
                error_provider_unavailable: true,
                provider_unavailable_cause: Some(cause),
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl LlmGateway for StubLlmGateway {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            self.seen.lock().expect("lock seen").push(req);
            if let Some(msg) = &self.error {
                if self.error_provider_unavailable {
                    let cause = self
                        .provider_unavailable_cause
                        .unwrap_or(ProviderFailureCause::Unknown);
                    return Err(PortError::ProviderUnavailable(
                        ProviderUnavailableInfo::new(cause, msg.clone()),
                    ));
                }
                return Err(PortError::Llm(msg.clone()));
            }
            Ok(self.canned.clone())
        }
    }

    /// Esecutore di tool di test: ritorna un esito fisso e registra le chiamate
    /// con la modalita' usata (per verificare che lo shadow usi `Replay`).
    pub struct StubToolExecutor {
        /// Esito fisso ritornato da ogni `execute`.
        pub canned: ToolOutcome,
        /// Chiamate registrate: (call, mode).
        pub seen: Mutex<Vec<(ToolCall, ExecMode)>>,
    }

    impl StubToolExecutor {
        /// Crea uno stub con un esito di successo dal contenuto dato.
        pub fn with_success(content: serde_json::Value) -> Self {
            Self {
                canned: ToolOutcome {
                    tool_call_id: "stub".to_string(),
                    content,
                    is_error: false,
                    ..Default::default()
                },
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl ToolExecutor for StubToolExecutor {
        async fn execute(&self, call: ToolCall, mode: ExecMode) -> Result<ToolOutcome, PortError> {
            self.seen.lock().expect("lock seen").push((call, mode));
            Ok(self.canned.clone())
        }
    }

    /// Motore criteri di test: ritorna una lista fissa di [`CriterionResult`] e
    /// registra le chiamate con la modalita' usata (per verificare che lo shadow
    /// usi `Replay`). I risultati dei criteri sono cosi' INPUT stubati: i test
    /// del `FinalGateNode` esercitano la decision machine, non l'esecuzione reale.
    pub struct StubCriteriaRunner {
        /// Risultati fissi ritornati da ogni `run`.
        pub canned: Vec<CriterionResult>,
        /// Chiamate registrate: (criteria, mode).
        pub seen: Mutex<Vec<(Vec<CriterionSpec>, ExecMode)>>,
    }

    impl StubCriteriaRunner {
        /// Crea uno stub che ritorna i risultati dati.
        pub fn with_results(results: Vec<CriterionResult>) -> Self {
            Self {
                canned: results,
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl CriteriaRunner for StubCriteriaRunner {
        async fn run(
            &self,
            criteria: Vec<CriterionSpec>,
            mode: ExecMode,
        ) -> Result<Vec<CriterionResult>, PortError> {
            self.seen.lock().expect("lock seen").push((criteria, mode));
            Ok(self.canned.clone())
        }
    }

    /// Sink eventi di test: registra ogni evento emesso (nessun I/O).
    #[derive(Default)]
    pub struct RecordingEventSink {
        /// Eventi registrati in ordine d'emissione.
        pub events: Mutex<Vec<SseEvent>>,
    }

    impl EventSink for RecordingEventSink {
        fn emit(&self, ev: SseEvent) {
            self.events.lock().expect("lock events").push(ev);
        }
    }

    /// Sink eventi no-op: ri-esportato dal PUNTO UNICO `super::NullEventSink`
    /// (regola L). Vive fuori da `#[cfg(test)]` perche' lo usa anche il run
    /// shadow in produzione; i test continuano a importarlo da qui.
    pub use super::NullEventSink;

    /// Store todo di test: mantiene una lista di [`Todo`] in memoria e applica i
    /// `mark_status` ricevuti (cosi' i test del `TodoRunnerNode` osservano gli
    /// avanzamenti: in_progress/completed/blocked/skipped/pending). Registra anche
    /// la sequenza delle `mark_status` per asserzioni su cascade-skip e retry.
    ///
    /// Gate shadow (come l'impl concreta, regola L): in [`ExecMode::Replay`] la
    /// `mark_status` e' un NO-OP (nessuna registrazione in `marks`, nessuna
    /// mutazione dei todo). Cosi' un test puo' asserire ZERO scritture in shadow
    /// verificando `marks` vuoto.
    pub struct StubTodoStore {
        /// Todo correnti (ordinati per `seq`, come `list_todos`).
        pub todos: Mutex<Vec<Todo>>,
        /// Storico delle `mark_status` REALI (mode==Real): (todo_id, nuovo_status),
        /// in ordine. Vuoto in shadow (Replay no-op).
        pub marks: Mutex<Vec<(String, TodoStatus)>>,
        /// Piano restituito da `fetch_plan` (`None` = nessun piano). Usato dai
        /// test del `PlannerNode` per esercitare il riuso piano intent/mode-aware.
        pub plan: Mutex<Option<PlanRow>>,
    }

    impl StubTodoStore {
        /// Crea uno store coi todo dati (gia' ordinati per `seq` dal chiamante).
        /// `fetch_plan` ritorna `None` (nessun piano).
        pub fn with_todos(todos: Vec<Todo>) -> Self {
            Self {
                todos: Mutex::new(todos),
                marks: Mutex::new(vec![]),
                plan: Mutex::new(None),
            }
        }

        /// Crea uno store coi todo dati + un piano esistente per `fetch_plan`
        /// (test del riuso piano del `PlannerNode`).
        pub fn with_plan(todos: Vec<Todo>, plan: Option<PlanRow>) -> Self {
            Self {
                todos: Mutex::new(todos),
                marks: Mutex::new(vec![]),
                plan: Mutex::new(plan),
            }
        }
    }

    #[async_trait]
    impl TodoStore for StubTodoStore {
        async fn list_todos(&self, _run_id: &str) -> Result<Vec<Todo>, PortError> {
            Ok(self.todos.lock().expect("lock todos").clone())
        }

        async fn fetch_plan(&self, _run_id: &str) -> Result<Option<PlanRow>, PortError> {
            Ok(self.plan.lock().expect("lock plan").clone())
        }

        async fn mark_status(
            &self,
            todo_id: &str,
            status: TodoStatus,
            mode: ExecMode,
        ) -> Result<(), PortError> {
            // Gate shadow: in Replay NON si scrive (no-op), come l'impl concreta
            // (zero side-effect sul DAG del run primario).
            if mode != ExecMode::Real {
                return Ok(());
            }
            self.marks
                .lock()
                .expect("lock marks")
                .push((todo_id.to_string(), status));
            // Applica lo status in memoria, cosi' una list_todos successiva
            // riflette l'avanzamento (come il DB reale fra una chiamata e l'altra).
            for t in self.todos.lock().expect("lock todos").iter_mut() {
                if t.id == todo_id {
                    t.status = status;
                }
            }
            Ok(())
        }
    }

    /// Store esiti verifier di test: registra i [`VerifierRunRecord`] persistiti
    /// (solo in `ExecMode::Real`). In `Replay` la `record` e' un NO-OP (nessuna
    /// registrazione), come l'impl concreta: un test puo' asserire ZERO scritture
    /// in shadow verificando `records` vuoto.
    #[derive(Default)]
    pub struct StubVerifierRunStore {
        /// Record persistiti in ordine (vuoto in shadow).
        pub records: Mutex<Vec<VerifierRunRecord>>,
    }

    #[async_trait]
    impl VerifierRunStore for StubVerifierRunStore {
        async fn record(&self, run: VerifierRunRecord, mode: ExecMode) -> Result<(), PortError> {
            // Gate shadow: in Replay NON si persiste (no-op), come l'impl concreta.
            if mode != ExecMode::Real {
                return Ok(());
            }
            self.records.lock().expect("lock records").push(run);
            Ok(())
        }
    }

    /// Controllo run di test (punto unico executor+tool_dispatch). `superseded`
    /// configurabile (default `false`); registra heartbeat e modello effettivo
    /// SOLO in `Real` (gate shadow), cosi' un test asserisce ZERO scritture in
    /// shadow verificando i vettori vuoti.
    #[derive(Default)]
    pub struct StubRunControlStore {
        /// Valore ritornato da `is_superseded`.
        pub superseded: bool,
        /// Se `true`, `is_superseded` ritorna un errore: lo stub VERIFICA il
        /// fail-open dei chiamanti (errore -> trattato come `false`, il run
        /// prosegue). Lo stub NON applica il fail-open al posto loro: ritorna
        /// l'errore cosi' i test del nodo esercitano il loro mapping.
        pub fail_is_superseded: bool,
        /// `run_id` per cui e' stato chiamato `heartbeat` (solo `Real`).
        pub heartbeats: Mutex<Vec<String>>,
        /// Modelli effettivi registrati (`Real`): (run_id, provider, model).
        pub effective_models: Mutex<Vec<(String, String, String)>>,
    }

    #[async_trait]
    impl RunControlStore for StubRunControlStore {
        async fn is_superseded(&self, _run_id: &str) -> Result<bool, PortError> {
            if self.fail_is_superseded {
                return Err(PortError::Llm("stub: is_superseded fail".to_string()));
            }
            Ok(self.superseded)
        }

        async fn heartbeat(&self, run_id: &str, mode: ExecMode) -> Result<(), PortError> {
            if mode != ExecMode::Real {
                return Ok(());
            }
            self.heartbeats
                .lock()
                .expect("lock heartbeats")
                .push(run_id.to_string());
            Ok(())
        }

        async fn set_effective_model(
            &self,
            run_id: &str,
            provider: &str,
            model: &str,
            mode: ExecMode,
        ) -> Result<(), PortError> {
            if mode != ExecMode::Real {
                return Ok(());
            }
            self.effective_models.lock().expect("lock models").push((
                run_id.to_string(),
                provider.to_string(),
                model.to_string(),
            ));
            Ok(())
        }
    }

    /// Store step di test: registra i blocchi persistiti (solo `Real`). In
    /// `Replay` la `persist_step` e' un NO-OP (nessuna registrazione): un test
    /// asserisce ZERO scritture in shadow verificando `steps` vuoto.
    #[derive(Default)]
    pub struct StubAgentStepStore {
        /// Step persistiti in ordine: (run_id, step_index = iteration*1000+idx,
        /// block, result). Vuoto in shadow.
        pub steps: Mutex<Vec<(String, i64, serde_json::Value, Option<serde_json::Value>)>>,
    }

    #[async_trait]
    impl AgentStepStore for StubAgentStepStore {
        async fn persist_step(
            &self,
            run_id: &str,
            iteration: i64,
            idx: i64,
            block: serde_json::Value,
            result: Option<serde_json::Value>,
            mode: ExecMode,
        ) -> Result<(), PortError> {
            // Gate shadow: in Replay NON si scrive (no-op), come l'impl concreta.
            if mode != ExecMode::Real {
                return Ok(());
            }
            // step_index deterministico = iteration*1000 + idx (come l'impl concreta).
            self.steps.lock().expect("lock steps").push((
                run_id.to_string(),
                iteration * 1000 + idx,
                block,
                result,
            ));
            Ok(())
        }
    }

    /// Store meta-step di test: registra i meta-step persistiti (solo `Real`).
    /// `Replay` no-op (zero scritture shadow).
    #[derive(Default)]
    pub struct StubMetaStepStore {
        /// Meta-step persistiti in ordine (JSON). Vuoto in shadow.
        pub meta_steps: Mutex<Vec<serde_json::Value>>,
    }

    #[async_trait]
    impl MetaStepStore for StubMetaStepStore {
        async fn persist_meta_step(
            &self,
            meta_step: serde_json::Value,
            mode: ExecMode,
        ) -> Result<(), PortError> {
            if mode != ExecMode::Real {
                return Ok(());
            }
            self.meta_steps
                .lock()
                .expect("lock meta_steps")
                .push(meta_step);
            Ok(())
        }
    }

    /// Offload di test: NON tocca Qdrant; ritorna un pointer fittizio
    /// deterministico e registra i payload "offloadati" (per asserire che il nodo
    /// abbia delegato l'offload). Per esercitare il DEGRADO A TRONCAMENTO dei
    /// chiamanti si puo' impostare `fail=true` (ritorna `PortError`).
    ///
    /// Gata `Real` come l'impl concreta: in `Replay` la scrittura e' NO-OP (nulla
    /// in `offloaded`) e ritorna `PortError` (il chiamante degrada a troncamento).
    /// Un test asserisce ZERO scritture in shadow verificando `offloaded` vuoto.
    #[derive(Default)]
    pub struct StubContextOffload {
        /// Se `true`, `offload_to_rag` fallisce in Real (test del degrado a troncamento).
        pub fail: bool,
        /// Payload "offloadati" in ordine. Vuoto in shadow (gate `Real`).
        pub offloaded: Mutex<Vec<serde_json::Value>>,
        /// `OffloadKind` di ogni offload, in ordine (per asserire tool_result vs chat_history).
        pub offloaded_kinds: Mutex<Vec<OffloadKind>>,
    }

    #[async_trait]
    impl ContextOffload for StubContextOffload {
        async fn offload_to_rag(
            &self,
            payload: serde_json::Value,
            kind: OffloadKind,
            _session_id: Option<String>,
            _project_id: Option<String>,
            mode: ExecMode,
        ) -> Result<String, PortError> {
            // Gate shadow: in Replay NON si scrive (no-op), come l'impl concreta.
            // Ritorna PortError cosi' il chiamante degrada a troncamento non-RAG.
            if mode != ExecMode::Real {
                return Err(PortError::Tool("shadow: offload no-op".to_string()));
            }
            if self.fail {
                return Err(PortError::Tool("stub: offload fail".to_string()));
            }
            let mut g = self.offloaded.lock().expect("lock offloaded");
            g.push(payload);
            self.offloaded_kinds
                .lock()
                .expect("lock offloaded_kinds")
                .push(kind);
            // Pointer fittizio deterministico (indice progressivo).
            Ok(format!("stub-rag-pointer-{}", g.len() - 1))
        }
    }

    /// Embedder di test: NON chiama l'ONNX bridge. In Real ritorna, per ciascun
    /// testo, il vettore associato in `vectors` per posizione (cicla se ci sono piu'
    /// testi che vettori); se `vectors` e' vuoto (DEFAULT) ritorna `PortError` cosi'
    /// i test esercitano il DEGRADO best-effort (niente continuity-trim). Gata `Real`
    /// come l'impl concreta: in `Replay` e' NO-OP (`PortError`).
    #[derive(Default)]
    pub struct StubEmbeddingStore {
        /// Vettori ritornati in Real (per posizione, ciclici). Vuoto -> `PortError`.
        pub vectors: Vec<Vec<f32>>,
        /// Testi ricevuti da `embed`, in ordine (per asserire l'input). Vuoto in shadow.
        pub embed_seen: Mutex<Vec<String>>,
    }

    impl StubEmbeddingStore {
        /// Costruttore comodo: stub che ritorna `vectors` (ciclici) in Real.
        pub fn with_vectors(vectors: Vec<Vec<f32>>) -> Self {
            StubEmbeddingStore {
                vectors,
                embed_seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl EmbeddingStore for StubEmbeddingStore {
        async fn embed(
            &self,
            texts: Vec<String>,
            mode: ExecMode,
        ) -> Result<Vec<Vec<f32>>, PortError> {
            if mode != ExecMode::Real {
                return Err(PortError::Tool("shadow: embed no-op".to_string()));
            }
            if self.vectors.is_empty() {
                return Err(PortError::Tool(
                    "stub: nessun vettore configurato".to_string(),
                ));
            }
            {
                let mut seen = self.embed_seen.lock().expect("lock embed_seen");
                for t in &texts {
                    seen.push(t.clone());
                }
            }
            let out = texts
                .iter()
                .enumerate()
                .map(|(i, _)| self.vectors[i % self.vectors.len()].clone())
                .collect();
            Ok(out)
        }
    }

    /// Summarizer di test: NON chiama alcun LLM. Se `summary` e' `Some`, ritorna
    /// quel testo (in Real) e registra l'input ricevuto in `summarize_seen` (per
    /// asserire CHE COSA viene serializzato e passato al modello). Se `summary` e'
    /// `None` (DEFAULT) ritorna `PortError`: cosi' i test che NON configurano il
    /// summary esercitano il DEGRADO best-effort (history invariata) senza dover
    /// impostare nulla.
    ///
    /// Gata `Real` come l'impl concreta: in `Replay` e' NO-OP (`PortError`, niente
    /// in `summarize_seen`).
    #[derive(Default)]
    pub struct StubSummaryStore {
        /// Riassunto fisso ritornato in Real. `None` (default) -> `PortError`
        /// (degrado best-effort: la history resta invariata).
        pub summary: Option<String>,
        /// Testi ricevuti da `summarize`, in ordine (per asserire l'input). Vuoto
        /// in shadow (gate `Real`).
        pub summarize_seen: Mutex<Vec<String>>,
    }

    impl StubSummaryStore {
        /// Costruttore comodo: stub che ritorna `summary` in Real.
        pub fn with_summary(summary: impl Into<String>) -> Self {
            StubSummaryStore {
                summary: Some(summary.into()),
                summarize_seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl SummaryStore for StubSummaryStore {
        async fn summarize(&self, text: String, mode: ExecMode) -> Result<String, PortError> {
            // Gate shadow: in Replay NON si riassume (no-op), come l'impl concreta.
            if mode != ExecMode::Real {
                return Err(PortError::Llm("shadow: summarize no-op".to_string()));
            }
            self.summarize_seen
                .lock()
                .expect("lock summarize_seen")
                .push(text);
            match &self.summary {
                Some(s) => Ok(s.clone()),
                // Nessun riassunto configurato: degrado best-effort (history invariata).
                None => Err(PortError::Llm(
                    "stub: nessun summary configurato".to_string(),
                )),
            }
        }
    }

    /// Porta escalation di test: ritorna `EscalationInputs` fissi (catena +
    /// cooldown + cross-provider configurabili) e registra le chiamate. Il
    /// default (campi vuoti) fa risolvere la selezione a `None` -> chiusura secca,
    /// cosi' i test che NON vogliono escalation usano `default()`.
    #[derive(Default)]
    pub struct StubEscalationPort {
        /// Modelli della catena intra-provider (in ordine di posizione).
        pub chain: Vec<String>,
        /// Tier applicato a OGNI entry della catena (FIX-A: verifica che il pick
        /// propaghi `current_tier`). `None` = tier non risolto dalla porta.
        pub chain_tier: Option<String>,
        /// `true` se il provider corrente e' in cooldown (salta Tier 1).
        pub provider_in_cooldown: bool,
        /// Candidato cross-provider `(provider, model)`, o `None`.
        pub cross_provider: Option<(String, String)>,
        /// Esito di `failover_provider` `(provider, model)`, o `None` (nessun
        /// provider sano -> chiusura Error). Default `None`: i test che non
        /// esercitano il failover su provider caduto non lo configurano.
        pub failover: Option<(String, String)>,
        /// Tier del modello di failover (FIX-A): il call-site lo scrive in
        /// `current_tier`. `None` = tier non risolto dall'adapter.
        pub failover_tier: Option<String>,
        /// `exclude` ricevuti dalle chiamate a `failover_provider` (per asserire
        /// che la cascata accumula i provider gia' provati).
        pub failover_seen: Mutex<Vec<Vec<String>>>,
        /// Se `true`, `escalation_inputs` ritorna un `PortError` (per i test del
        /// mapping fail-open dei chiamanti).
        pub fail: bool,
        /// Chiamate registrate: (intent, provider, model).
        pub seen: Mutex<Vec<(Option<String>, Option<String>, Option<String>)>>,
    }

    impl StubEscalationPort {
        /// Stub con una catena intra-provider data (nessun cooldown, nessun cross).
        pub fn with_chain(models: &[&str]) -> Self {
            Self {
                chain: models.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
        }

        /// Come [`with_chain`](Self::with_chain) ma con un tier per la catena
        /// (FIX-A): il pick propaghera' questo tier in `current_tier`.
        pub fn with_chain_tier(models: &[&str], tier: &str) -> Self {
            Self {
                chain: models.iter().map(|s| s.to_string()).collect(),
                chain_tier: Some(tier.to_string()),
                ..Default::default()
            }
        }

        /// Stub con SOLO un candidato cross-provider (catena vuota).
        pub fn with_cross(provider: &str, model: &str) -> Self {
            Self {
                cross_provider: Some((provider.to_string(), model.to_string())),
                ..Default::default()
            }
        }

        /// Stub con SOLO un esito di failover su provider caduto (catena vuota,
        /// nessun cross): esercita il ramo `ProviderUnavailable` dell'executor.
        pub fn with_failover(provider: &str, model: &str) -> Self {
            Self {
                failover: Some((provider.to_string(), model.to_string())),
                ..Default::default()
            }
        }

        /// Come [`with_failover`](Self::with_failover) ma col tier del modello di
        /// failover (FIX-A): il call-site lo scrive in `current_tier`.
        pub fn with_failover_tier(provider: &str, model: &str, tier: &str) -> Self {
            Self {
                failover: Some((provider.to_string(), model.to_string())),
                failover_tier: Some(tier.to_string()),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl EscalationPort for StubEscalationPort {
        async fn escalation_inputs(
            &self,
            intent: Option<&str>,
            provider: Option<&str>,
            model: Option<&str>,
            _mode: ExecMode,
        ) -> Result<EscalationInputs, PortError> {
            self.seen.lock().expect("lock seen").push((
                intent.map(str::to_string),
                provider.map(str::to_string),
                model.map(str::to_string),
            ));
            if self.fail {
                return Err(PortError::Llm("stub: escalation_inputs fail".to_string()));
            }
            // Nuovo contratto agentico: insieme UNIFICATO di candidati (intra +
            // cross) con tier + telemetria. Il provider della catena intra e' quello
            // corrente (passato); telemetria default (sano) -> il ranking del modulo
            // puro si riduce a tier + ordine d'ingresso (deterministico nei test).
            let mut candidates: Vec<crate::decisions::escalation::EscalationCandidate> = self
                .chain
                .iter()
                .map(|m| crate::decisions::escalation::EscalationCandidate {
                    provider: provider.unwrap_or("").to_string(),
                    model: m.clone(),
                    tier: self.chain_tier.clone(),
                    telemetry: crate::decisions::governance::ModelTelemetry::default(),
                })
                .collect();
            if let Some((p, m)) = self.cross_provider.as_ref() {
                candidates.push(crate::decisions::escalation::EscalationCandidate {
                    provider: p.clone(),
                    model: m.clone(),
                    tier: None,
                    telemetry: crate::decisions::governance::ModelTelemetry::default(),
                });
            }
            Ok(EscalationInputs {
                candidates,
                policy: crate::decisions::governance::GovernancePolicy::default(),
            })
        }

        async fn failover_provider(
            &self,
            _current_provider: Option<&str>,
            _current_model: Option<&str>,
            _current_tier: Option<&str>,
            exclude: &[String],
        ) -> Result<Option<CrossProviderCandidate>, PortError> {
            self.failover_seen
                .lock()
                .expect("lock failover_seen")
                .push(exclude.to_vec());
            if self.fail {
                return Err(PortError::Llm("stub: failover_provider fail".to_string()));
            }
            Ok(self.failover.as_ref().map(|(p, m)| CrossProviderCandidate {
                provider: p.clone(),
                model: m.clone(),
                tier: self.failover_tier.clone(),
            }))
        }
    }

    /// Deriver di test per le scelte di proseguimento (`next_actions`): ritorna
    /// una lista fissa di [`NextActionChoice`] (default vuota = nessun meta_step) e
    /// registra il testo ricevuto. `fail=true` esercita il ramo best-effort
    /// (errore -> il nodo NON deve propagare: tratta come nessuna scelta, ma il
    /// blocco e' gia' stato rimosso dal punto unico deterministico).
    #[derive(Default)]
    pub struct StubNextActionsDeriver {
        /// Scelte fisse ritornate da `derive` (vuoto = nessuna).
        pub choices: Vec<NextActionChoice>,
        /// Se `true`, `derive` ritorna un `PortError` (test del ramo error).
        pub fail: bool,
        /// Testi (gia' puliti) ricevuti, in ordine.
        pub seen: Mutex<Vec<String>>,
    }

    impl StubNextActionsDeriver {
        /// Stub che ritorna le scelte date (label, prompt).
        pub fn with_choices(choices: &[(&str, &str)]) -> Self {
            Self {
                choices: choices
                    .iter()
                    .map(|(l, p)| NextActionChoice {
                        label: l.to_string(),
                        prompt: p.to_string(),
                    })
                    .collect(),
                ..Default::default()
            }
        }

        /// Stub che FALLISCE sempre (per il ramo best-effort).
        pub fn failing() -> Self {
            Self {
                fail: true,
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl NextActionsDeriver for StubNextActionsDeriver {
        async fn derive(&self, cleaned_text: &str) -> Result<Vec<NextActionChoice>, PortError> {
            self.seen
                .lock()
                .expect("lock seen")
                .push(cleaned_text.to_string());
            if self.fail {
                return Err(PortError::Llm("stub: derive fail".to_string()));
            }
            Ok(self.choices.clone())
        }
    }

    /// Porta cooldown billing di test: ritorna una lista fissa di provider
    /// esauriti (default vuota = nessun fail-fast). `fail=true` esercita il
    /// fail-open dei chiamanti (errore -> trattato come nessun esausto).
    #[derive(Default)]
    pub struct StubBillingCooldownPort {
        /// Provider in cooldown billing (gia' ordinati dal chiamante).
        pub exhausted: Vec<String>,
        /// Se `true`, ritorna un `PortError` (test del fail-open del chiamante).
        pub fail: bool,
    }

    impl StubBillingCooldownPort {
        /// Stub con i provider esauriti dati.
        pub fn with_exhausted(providers: &[&str]) -> Self {
            Self {
                exhausted: providers.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl BillingCooldownPort for StubBillingCooldownPort {
        async fn billing_exhausted_providers(&self) -> Result<Vec<String>, PortError> {
            if self.fail {
                return Err(PortError::Llm("stub: billing snapshot fail".to_string()));
            }
            Ok(self.exhausted.clone())
        }
    }

    /// Porta smart-upscale di test: window fissa per il modello corrente +
    /// candidato di upscale fisso (default `None` = nessun upscale). Registra le
    /// chiamate di selezione (modello corrente + required) per le asserzioni.
    #[derive(Default)]
    pub struct StubModelUpscalePort {
        /// Window ritornata da `context_window` (0 = ignota -> niente upscale).
        pub window: i64,
        /// Candidato ritornato da `select_upscale_model` (`None` = nessuno).
        pub pick: Option<UpscalePick>,
        /// Window ritornata per il modello PROMOSSO (se `Some` e il modello
        /// richiesto e' `pick.model`): simula il catalog dove il candidato di
        /// upscale ha una finestra piu' grande del corrente.
        pub promoted_window: Option<i64>,
        /// Chiamate di selezione registrate: (current_model, required_tokens).
        pub selected: Mutex<Vec<(String, i64)>>,
        /// Candidato ritornato da `select_model_for_tier` (SCALE-CONTROLLER, PR-B3):
        /// `None` = nessun modello del tier soddisfa i vincoli (il chiamante annulla
        /// il cambio-tier). Registra le chiamate `(tier, min_context_window)` in
        /// `tier_selected` per le asserzioni.
        pub tier_pick: Option<(String, String)>,
        /// Chiamate `select_model_for_tier` registrate: (tier, min_context_window).
        pub tier_selected: Mutex<Vec<(String, i64)>>,
    }

    impl StubModelUpscalePort {
        /// Stub con una window data e un candidato di upscale `(provider, model)`.
        pub fn promoting(window: i64, provider: &str, model: &str) -> Self {
            Self {
                window,
                pick: Some(UpscalePick {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    reason: "context_overflow".to_string(),
                    tier: "heavy".to_string(),
                }),
                promoted_window: None,
                selected: Mutex::new(vec![]),
                ..Default::default()
            }
        }

        /// Stub che, per lo SCALE-CONTROLLER (PR-B3), risolve un modello del tier
        /// target `(provider, model)` (nessun upscale-token). Per il test del rientro
        /// DownscaleTo con modello disponibile.
        pub fn tier_resolving(provider: &str, model: &str) -> Self {
            Self {
                tier_pick: Some((provider.to_string(), model.to_string())),
                ..Default::default()
            }
        }

        /// Come [`Self::promoting`], ma il modello promosso ha una window propria
        /// (piu' grande): per i test dell'hard-cap post-upscale (ADR 0016 D2).
        pub fn promoting_to_window(
            window: i64,
            provider: &str,
            model: &str,
            promoted_window: i64,
        ) -> Self {
            Self {
                promoted_window: Some(promoted_window),
                ..Self::promoting(window, provider, model)
            }
        }

        /// Stub con una window data ma NESSUN candidato (l'upscale non promuove).
        pub fn no_pick(window: i64) -> Self {
            Self {
                window,
                pick: None,
                promoted_window: None,
                selected: Mutex::new(vec![]),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl ModelUpscalePort for StubModelUpscalePort {
        async fn context_window(&self, model: &str) -> Result<i64, PortError> {
            if let (Some(pw), Some(pick)) = (self.promoted_window, self.pick.as_ref()) {
                if model == pick.model {
                    return Ok(pw);
                }
            }
            Ok(self.window)
        }

        async fn select_upscale_model(
            &self,
            current_model: &str,
            required_tokens: i64,
        ) -> Result<Option<UpscalePick>, PortError> {
            self.selected
                .lock()
                .expect("lock selected")
                .push((current_model.to_string(), required_tokens));
            Ok(self.pick.clone())
        }

        async fn select_model_for_tier(
            &self,
            tier: &str,
            min_context_window: i64,
            _capability: Option<&str>,
            _exclude_providers: &[String],
            mode: ExecMode,
        ) -> Result<Option<(String, String)>, PortError> {
            // Opzione A: in Replay nessuna risoluzione (parita' con l'impl reale).
            if mode != ExecMode::Real {
                return Ok(None);
            }
            self.tier_selected
                .lock()
                .expect("lock tier_selected")
                .push((tier.to_string(), min_context_window));
            Ok(self.tier_pick.clone())
        }
    }
}
