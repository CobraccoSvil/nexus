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
    CriteriaRunner, CriterionResult, CriterionSpec, EventSink, ExecMode, LlmGateway, LlmMessage,
    LlmRequest, LlmResponse, LlmUsage, PlanRow, PortError, SseEvent, TodoStore, ToolCall,
    ToolExecutor, ToolOutcome, VerifierRunRecord, VerifierRunStore,
};

#[cfg(test)]
pub mod test_doubles {
    //! Implementazioni di test delle porte (mock/stub) riusabili dai test dei
    //! nodi e dello shadow. Ritornano valori fissi e/o registrano le chiamate
    //! ricevute, cosi' i test verificano comportamento senza I/O reale.

    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::ports::{
        CriteriaRunner, CriterionResult, CriterionSpec, EventSink, ExecMode, LlmGateway, LlmRequest,
        LlmResponse, LlmUsage, PlanRow, PortError, SseEvent, ToolCall, ToolExecutor, ToolOutcome,
        TodoStore, VerifierRunRecord, VerifierRunStore,
    };
    use crate::decisions::dag_scheduler::{Todo, TodoStatus};

    /// Gateway LLM di test: ritorna una `LlmResponse` fissa e registra le
    /// richieste ricevute (per asserzioni sull'input passato dal nodo).
    pub struct StubLlmGateway {
        /// Risposta fissa ritornata da ogni `complete`.
        pub canned: LlmResponse,
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
                },
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
                },
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl LlmGateway for StubLlmGateway {
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
            self.seen.lock().expect("lock seen").push(req);
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
}
