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
    AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, CriterionResult,
    CriterionSpec, EscalationInputs, EscalationPort, EventSink, ExecMode, LlmGateway, LlmMessage,
    LlmRequest, LlmResponse, LlmUsage, MetaStepStore, ModelUpscalePort, NextActionChoice,
    NextActionsDeriver, PlanRow, PortError, RunControlStore, SseEvent, TodoStore, ToolCall,
    ToolExecutor, ToolOutcome, UpscalePick, VerifierRunRecord, VerifierRunStore,
};

#[cfg(test)]
pub mod test_doubles {
    //! Implementazioni di test delle porte (mock/stub) riusabili dai test dei
    //! nodi e dello shadow. Ritornano valori fissi e/o registrano le chiamate
    //! ricevute, cosi' i test verificano comportamento senza I/O reale.

    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::ports::{
        AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, CriterionResult,
        CriterionSpec, EscalationInputs, EscalationPort, EventSink, ExecMode, LlmGateway,
        LlmRequest, LlmResponse, LlmUsage, MetaStepStore, ModelUpscalePort, NextActionChoice,
        NextActionsDeriver, PlanRow, PortError, RunControlStore, SseEvent, ToolCall, ToolExecutor,
        ToolOutcome, TodoStore, UpscalePick, VerifierRunRecord, VerifierRunStore,
    };
    use crate::decisions::dag_scheduler::{Todo, TodoStatus};
    use crate::decisions::escalation::{ChainEntry, CrossProviderCandidate};

    /// Gateway LLM di test: ritorna una `LlmResponse` fissa e registra le
    /// richieste ricevute (per asserzioni sull'input passato dal nodo).
    pub struct StubLlmGateway {
        /// Risposta fissa ritornata da ogni `complete`.
        pub canned: LlmResponse,
        /// Se `Some`, `complete` ritorna `Err(PortError::Llm(_))` (provider down/
        /// billing): esercita il ramo error del nodo (parita' con l'except Python).
        pub error: Option<String>,
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
                    }],
                    usage: LlmUsage::default(),
                    ..Default::default()
                },
                error: None,
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
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl LlmGateway for StubLlmGateway {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            self.seen.lock().expect("lock seen").push(req);
            if let Some(msg) = &self.error {
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
        async fn execute(
            &self,
            call: ToolCall,
            mode: ExecMode,
        ) -> Result<ToolOutcome, PortError> {
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

    /// Sink eventi no-op: scarta tutto. Usato nel ctx SHADOW (nessun output
    /// verso l'utente dal run shadow).
    pub struct NullEventSink;

    impl EventSink for NullEventSink {
        fn emit(&self, _ev: SseEvent) {}
    }

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
        async fn record(
            &self,
            run: VerifierRunRecord,
            mode: ExecMode,
        ) -> Result<(), PortError> {
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
    }

    #[async_trait]
    impl ContextOffload for StubContextOffload {
        async fn offload_to_rag(
            &self,
            payload: serde_json::Value,
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
            // Pointer fittizio deterministico (indice progressivo).
            Ok(format!("stub-rag-pointer-{}", g.len() - 1))
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
        /// `true` se il provider corrente e' in cooldown (salta Tier 1).
        pub provider_in_cooldown: bool,
        /// Candidato cross-provider `(provider, model)`, o `None`.
        pub cross_provider: Option<(String, String)>,
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

        /// Stub con SOLO un candidato cross-provider (catena vuota).
        pub fn with_cross(provider: &str, model: &str) -> Self {
            Self {
                cross_provider: Some((provider.to_string(), model.to_string())),
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
        ) -> Result<EscalationInputs, PortError> {
            self.seen.lock().expect("lock seen").push((
                intent.map(str::to_string),
                provider.map(str::to_string),
                model.map(str::to_string),
            ));
            if self.fail {
                return Err(PortError::Llm("stub: escalation_inputs fail".to_string()));
            }
            Ok(EscalationInputs {
                chain: self
                    .chain
                    .iter()
                    .map(|m| ChainEntry {
                        escalation_model: m.clone(),
                    })
                    .collect(),
                provider_in_cooldown: self.provider_in_cooldown,
                cross_provider: self
                    .cross_provider
                    .as_ref()
                    .map(|(p, m)| CrossProviderCandidate {
                        provider: p.clone(),
                        model: m.clone(),
                    }),
            })
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
        async fn derive(
            &self,
            cleaned_text: &str,
        ) -> Result<Vec<NextActionChoice>, PortError> {
            self.seen.lock().expect("lock seen").push(cleaned_text.to_string());
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
        /// Chiamate di selezione registrate: (current_model, required_tokens).
        pub selected: Mutex<Vec<(String, i64)>>,
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
                }),
                selected: Mutex::new(vec![]),
            }
        }

        /// Stub con una window data ma NESSUN candidato (l'upscale non promuove).
        pub fn no_pick(window: i64) -> Self {
            Self {
                window,
                pick: None,
                selected: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl ModelUpscalePort for StubModelUpscalePort {
        async fn context_window(&self, _model: &str) -> Result<i64, PortError> {
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
    }
}
