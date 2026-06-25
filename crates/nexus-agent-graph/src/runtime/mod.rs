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
    EventSink, ExecMode, LlmGateway, LlmMessage, LlmRequest, LlmResponse, LlmUsage, PortError,
    SseEvent, ToolCall, ToolExecutor, ToolOutcome,
};

#[cfg(test)]
pub mod test_doubles {
    //! Implementazioni di test delle porte (mock/stub) riusabili dai test dei
    //! nodi e dello shadow. Ritornano valori fissi e/o registrano le chiamate
    //! ricevute, cosi' i test verificano comportamento senza I/O reale.

    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::ports::{
        EventSink, ExecMode, LlmGateway, LlmRequest, LlmResponse, LlmUsage, PortError, SseEvent,
        ToolCall, ToolExecutor, ToolOutcome,
    };

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
}
