//! Adapter del trait [`nexus_agent_graph::runtime::ports::EventSink`].
//!
//! Implementa `EventSink::emit` (sincrono, infallibile, best-effort) pubblicando
//! l'evento sul canale SSE concreto della chat: il `broadcast::Sender<AgentStepEvent>`
//! registrato in `state.agent_channels` per il run (lo STESSO canale che
//! `brain_agent_client::run_via_brain` usa per ritrasmettere gli eventi del brain,
//! regola L: nessuna seconda forma di evento). Nessun gate `mode`: lo shadow usa il
//! sink no-op iniettato nel ctx (`NullEventSink`), l'unica fonte di verita' verso
//! l'utente resta il run primario. Il run a cui gli eventi appartengono e' fissato
//! alla costruzione.
//!
//! NOTA (correzione vs scaffold F1): l'handle e' il `broadcast::Sender<AgentStepEvent>`,
//! non `ProjectChannels`. La chat NON viaggia su `nexus_events::ProjectChannels`
//! (canale di progetto, topic agent/services/...) ma sul broadcast per-run di
//! `agent_channels` (il flusso `run_via_brain` emette li'); l'adapter delega allo
//! stesso canale per parita' 1:1 con il motore Python.

use tokio::sync::broadcast;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{EventSink, SseEvent};

use crate::agent_types::{AgentMetaStep, AgentStep, AgentStepEvent, AgentStepStatus};

/// Adapter [`EventSink`] -> canale broadcast SSE della chat
/// ([`broadcast::Sender<AgentStepEvent>`], lo stesso di `state.agent_channels`).
pub struct SseEventSinkAdapter {
    /// Sender broadcast del run su cui `emit` pubblica (per-run, da `agent_channels`).
    tx: broadcast::Sender<AgentStepEvent>,
    /// Run a cui gli eventi emessi appartengono (campo `run_id` degli eventi SSE).
    run_id: Uuid,
}

impl SseEventSinkAdapter {
    /// Costruisce l'adapter sul sender broadcast concreto del run.
    pub fn new(tx: broadcast::Sender<AgentStepEvent>, run_id: Uuid) -> Self {
        Self { tx, run_id }
    }

    /// Helper interno: invia un `AgentStepEvent` best-effort (l'errore "nessun
    /// subscriber" e' atteso e ignorato, come in `run_via_brain`).
    fn send(&self, ev: AgentStepEvent) {
        let _ = self.tx.send(ev);
    }

    /// Scheletro evento con `run_id` valorizzato e tutti i campi opzionali a `None`
    /// (un solo posto definisce il default, i mapping sotto sovrascrivono solo i
    /// campi rilevanti — regola L).
    fn base(&self) -> AgentStepEvent {
        AgentStepEvent {
            run_id: self.run_id.to_string(),
            step: None,
            trace: None,
            is_final: false,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        }
    }
}

impl EventSink for SseEventSinkAdapter {
    /// Traduce un [`SseEvent`] del grafo nell'[`AgentStepEvent`] del canale chat,
    /// 1:1 con i mapping di `brain_agent_client::run_via_brain`:
    /// - `ThinkingDelta`  -> `thinking_delta` (blocco "Ragionamento")
    /// - `MetaStep`       -> `meta_step` (plan/routing/clarify/fallback/usage_snapshot/...)
    /// - `Usage`          -> `meta_step` kind=`usage_snapshot` (barra contesto/TokenUsageBar)
    /// - `ToolUse`        -> `step` (AgentStep, status `Running`)
    /// - `ToolResult`     -> `step` (AgentStep aggiornato, status `Completed`/`Failed`)
    /// - `EndTurn`        -> evento `meta_step` kind=`end_turn` informativo (NON `is_final`:
    ///   la chiusura definitiva del run e' del finalizzatore con `is_final=true`)
    /// - `Done`           -> `is_final=true` (terminatore dello stream)
    ///
    /// `Done`/`EndTurn`: il run nativo emette `Done` come terminatore esplicito; nel
    /// flusso Python `is_final` e' messo dal finalizzatore di `agent_run.rs`. Qui
    /// `Done` mappa `is_final=true` cosi' il sink resta self-contained se usato senza
    /// finalizzatore esterno; un eventuale doppio `is_final` e' idempotente lato UI.
    fn emit(&self, ev: SseEvent) {
        match ev {
            SseEvent::ThinkingDelta { delta } => {
                if delta.is_empty() {
                    return;
                }
                let mut e = self.base();
                e.thinking_delta = Some(delta);
                self.send(e);
            }
            SseEvent::MetaStep {
                kind,
                title,
                payload,
            } => {
                if kind.is_empty() {
                    return;
                }
                let mut e = self.base();
                e.meta_step = Some(AgentMetaStep {
                    kind,
                    title,
                    payload,
                    correlation_id: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                self.send(e);
            }
            SseEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => {
                let mut e = self.base();
                e.meta_step = Some(AgentMetaStep {
                    kind: "usage_snapshot".to_string(),
                    title: String::new(),
                    payload: serde_json::json!({
                        "totalTokens": total_tokens,
                        "promptTokens": prompt_tokens,
                        "completionTokens": completion_tokens,
                    }),
                    correlation_id: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                self.send(e);
            }
            SseEvent::ToolUse { id, name, input } => {
                let mut e = self.base();
                e.step = Some(AgentStep {
                    run_id: self.run_id.to_string(),
                    // step_index non disponibile a livello evento: usiamo 0; il
                    // valore autoritativo per la UI e' il run_id + ordine d'arrivo,
                    // e l'id tool e' propagato nel payload (correlazione tool_result).
                    step_index: 0,
                    tool_name: name,
                    tool_input: serde_json::json!({ "id": id, "input": input }),
                    tool_result: None,
                    status: AgentStepStatus::Running,
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                self.send(e);
            }
            SseEvent::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let mut e = self.base();
                e.step = Some(AgentStep {
                    run_id: self.run_id.to_string(),
                    step_index: 0,
                    // Per il tool_result il nome non e' rilevante in UI (lo step
                    // Running precedente porta gia' nome/input); riportiamo il
                    // tool_call_id per la correlazione lato frontend.
                    tool_name: String::new(),
                    tool_input: serde_json::json!({ "id": tool_call_id }),
                    tool_result: Some(match &content {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }),
                    status: if is_error {
                        AgentStepStatus::Failed
                    } else {
                        AgentStepStatus::Completed
                    },
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                self.send(e);
            }
            SseEvent::EndTurn => {
                let mut e = self.base();
                e.meta_step = Some(AgentMetaStep {
                    kind: "end_turn".to_string(),
                    title: String::new(),
                    payload: serde_json::json!({}),
                    correlation_id: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                self.send(e);
            }
            SseEvent::Done => {
                let mut e = self.base();
                e.is_final = true;
                self.send(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (SseEventSinkAdapter, broadcast::Receiver<AgentStepEvent>, Uuid) {
        let (tx, rx) = broadcast::channel::<AgentStepEvent>(16);
        let run_id = Uuid::new_v4();
        (SseEventSinkAdapter::new(tx, run_id), rx, run_id)
    }

    #[test]
    fn thinking_delta_mappa_su_campo_thinking() {
        let (sink, mut rx, run_id) = setup();
        sink.emit(SseEvent::ThinkingDelta {
            delta: "ragiono".to_string(),
        });
        let ev = rx.try_recv().expect("evento emesso");
        assert_eq!(ev.run_id, run_id.to_string());
        assert_eq!(ev.thinking_delta.as_deref(), Some("ragiono"));
        assert!(ev.step.is_none() && ev.meta_step.is_none() && !ev.is_final);
    }

    #[test]
    fn thinking_delta_vuoto_non_emette() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::ThinkingDelta {
            delta: String::new(),
        });
        assert!(rx.try_recv().is_err(), "delta vuoto non deve emettere");
    }

    #[test]
    fn usage_mappa_su_meta_step_usage_snapshot() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });
        let ev = rx.try_recv().expect("evento emesso");
        let ms = ev.meta_step.expect("meta_step presente");
        assert_eq!(ms.kind, "usage_snapshot");
        assert_eq!(ms.payload["totalTokens"], json!(15));
        assert_eq!(ms.payload["promptTokens"], json!(10));
        assert_eq!(ms.payload["completionTokens"], json!(5));
    }

    #[test]
    fn tool_use_mappa_su_step_running() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::ToolUse {
            id: "tu_1".to_string(),
            name: "read_file".to_string(),
            input: json!({"path": "x"}),
        });
        let ev = rx.try_recv().expect("evento emesso");
        let step = ev.step.expect("step presente");
        assert_eq!(step.tool_name, "read_file");
        assert_eq!(step.status, AgentStepStatus::Running);
        assert_eq!(step.tool_input["id"], json!("tu_1"));
        assert_eq!(step.tool_input["input"], json!({"path": "x"}));
    }

    #[test]
    fn tool_result_errore_mappa_su_step_failed() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::ToolResult {
            tool_call_id: "tu_1".to_string(),
            content: json!("boom"),
            is_error: true,
        });
        let ev = rx.try_recv().expect("evento emesso");
        let step = ev.step.expect("step presente");
        assert_eq!(step.status, AgentStepStatus::Failed);
        assert_eq!(step.tool_result.as_deref(), Some("boom"));
        assert_eq!(step.tool_input["id"], json!("tu_1"));
    }

    #[test]
    fn tool_result_ok_mappa_su_step_completed() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::ToolResult {
            tool_call_id: "tu_2".to_string(),
            content: json!("ok"),
            is_error: false,
        });
        let ev = rx.try_recv().expect("evento emesso");
        assert_eq!(ev.step.expect("step").status, AgentStepStatus::Completed);
    }

    #[test]
    fn meta_step_senza_kind_non_emette() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::MetaStep {
            kind: String::new(),
            title: "x".to_string(),
            payload: json!({}),
        });
        assert!(rx.try_recv().is_err(), "kind vuoto non deve emettere");
    }

    #[test]
    fn done_mappa_su_is_final() {
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::Done);
        let ev = rx.try_recv().expect("evento emesso");
        assert!(ev.is_final, "Done deve marcare is_final");
    }

    #[test]
    fn emit_senza_subscriber_non_panica() {
        // Best-effort: niente receiver -> emit non deve fallire/panicare.
        let (tx, rx) = broadcast::channel::<AgentStepEvent>(4);
        drop(rx);
        let sink = SseEventSinkAdapter::new(tx, Uuid::new_v4());
        sink.emit(SseEvent::EndTurn); // nessun panic atteso
    }
}
