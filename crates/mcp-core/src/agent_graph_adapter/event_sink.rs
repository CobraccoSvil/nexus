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

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{EventSink, SseEvent};

use crate::agent_types::{
    AITraceEvent, AgentMetaStep, AgentStep, AgentStepEvent, AgentStepStatus,
};

/// Accumulatore per-turno della traccia gateway (`AITraceEvent`).
///
/// Il grafo nativo (`nexus-agent-graph`) NON emette un evento `trace` strutturato
/// come il path Python: emette `MetaStep{kind:"executor_call"}` (provider/model/
/// iteration/tools_count, PRIMA dell'interrogazione), `Usage` (token del turno) e
/// poi `ToolUse`*/`EndTurn` (chiusura del turno). L'adapter RICOSTRUISCE da questi
/// segnali l'`AITraceEvent` di ogni turno (uno per interrogazione del modello, 1:1
/// col Python) e lo flusha (emit `trace` LIVE + persist su `nexus_agent_traces`)
/// alla chiusura del turno. Punto unico della persistenza: [`crate::trace_store`].
#[derive(Default, Clone)]
struct PendingTrace {
    /// Iterazione dell'executor (campo `iteration` dell'AITraceEvent).
    iteration: u32,
    /// Provider effettivo del turno (dal payload `executor_call`).
    provider: String,
    /// Modello effettivo del turno.
    model: String,
    /// Numero di tool disponibili al turno.
    tools_count: u32,
    /// Token di prompt del turno (dall'evento `Usage`).
    input_tokens: u32,
    /// Token di completion del turno.
    output_tokens: u32,
    /// Tool richiesti dal modello in questo turno (`{name, input}`), per il campo
    /// `tool_calls` dell'AITraceEvent.
    tool_calls: Vec<serde_json::Value>,
}

/// Adapter [`EventSink`] -> canale broadcast SSE della chat
/// ([`broadcast::Sender<AgentStepEvent>`], lo stesso di `state.agent_channels`).
pub struct SseEventSinkAdapter {
    /// Sender broadcast del run su cui `emit` pubblica (per-run, da `agent_channels`).
    tx: broadcast::Sender<AgentStepEvent>,
    /// Run a cui gli eventi emessi appartengono (campo `run_id` degli eventi SSE).
    run_id: Uuid,
    /// Contatore monotono per-run dello `step_index` (parita' col path Python
    /// `brain_agent_client::run_via_brain`, dove ogni `tool_use` incrementa
    /// l'indice). Senza questo ogni step usava `step_index: 0` e il frontend
    /// (upsert per `stepIndex`) sovrascriveva lo step precedente mostrandone uno
    /// solo: il progresso live spariva. `fetch_add(1)` ritorna il valore PRIMA
    /// dell'incremento, quindi il primo step ha indice 0 e i successivi crescono.
    next_step_index: AtomicI64,
    /// Mappa `tool_call_id` -> (step_index, tool_name) registrata al `ToolUse`:
    /// permette al `ToolResult` correlato di aggiornare LO STESSO step (stesso
    /// indice, stesso nome del tool) invece di crearne uno nuovo con nome vuoto
    /// e indice 0 (parita' col Python che aggiorna l'oggetto step in `Running`).
    tool_index: Mutex<HashMap<String, (i64, String)>>,
    /// Pool Postgres per la persistenza best-effort delle tracce gateway su
    /// `nexus_agent_traces` (FIX persistenza tracing nativo). `None` quando
    /// l'adapter e' costruito senza DB (es. test del solo mapping SSE): la
    /// persistenza diventa un no-op, l'emissione LIVE resta invariata.
    db: Option<PgPool>,
    /// Sessione a cui i run (e quindi le tracce) appartengono: colonna
    /// `nexus_agent_traces.session_id` (il payload AITraceEvent non la porta).
    session_id: Uuid,
    /// Traccia gateway del turno in costruzione: accumulata dagli eventi del
    /// turno (`executor_call`/`Usage`/`ToolUse`) e flushata alla chiusura
    /// (`executor_call` successivo / `EndTurn` / `Done`).
    pending_trace: Mutex<Option<PendingTrace>>,
    /// Contatore monotono per-run del `seq` delle tracce (indice progressivo nel
    /// run, parita' col `trace_seq` di `brain_agent_client::run_via_brain`).
    next_trace_seq: AtomicI64,
}

impl SseEventSinkAdapter {
    /// Costruisce l'adapter sul sender broadcast concreto del run, SENZA
    /// persistenza DB (solo canale LIVE). Costruttore di SOLO TEST del mapping
    /// SSE: in produzione l'adapter primario usa sempre [`Self::with_persistence`]
    /// (la persistenza tracce e' parte del contratto del run nativo).
    #[cfg(test)]
    pub fn new(tx: broadcast::Sender<AgentStepEvent>, run_id: Uuid) -> Self {
        Self {
            tx,
            run_id,
            next_step_index: AtomicI64::new(0),
            tool_index: Mutex::new(HashMap::new()),
            db: None,
            session_id: Uuid::nil(),
            pending_trace: Mutex::new(None),
            next_trace_seq: AtomicI64::new(0),
        }
    }

    /// Costruisce l'adapter con persistenza delle tracce gateway abilitata: oltre
    /// a emettere gli eventi LIVE, ricostruisce l'`AITraceEvent` di ogni turno e
    /// lo persiste best-effort su `nexus_agent_traces` (punto unico
    /// [`crate::trace_store::persist_trace`], regola L). `session_id` e' la
    /// sessione del run (colonna della tabella tracce).
    pub fn with_persistence(
        tx: broadcast::Sender<AgentStepEvent>,
        run_id: Uuid,
        session_id: Uuid,
        db: PgPool,
    ) -> Self {
        Self {
            tx,
            run_id,
            next_step_index: AtomicI64::new(0),
            tool_index: Mutex::new(HashMap::new()),
            db: Some(db),
            session_id,
            pending_trace: Mutex::new(None),
            next_trace_seq: AtomicI64::new(0),
        }
    }

    /// Helper interno: invia un `AgentStepEvent` best-effort (l'errore "nessun
    /// subscriber" e' atteso e ignorato, come in `run_via_brain`).
    fn send(&self, ev: AgentStepEvent) {
        let _ = self.tx.send(ev);
    }

    /// Converte l'indice monotono interno (`i64`, mai negativo) nel `u32` del
    /// campo `AgentStep::step_index`. Saturazione esplicita (niente cast silente):
    /// gli indici partono da 0 e crescono, un overflow di `u32` e' irrealistico
    /// per un singolo run ma la clamp evita comunque un troncamento ambiguo.
    fn step_index_u32(idx: i64) -> u32 {
        idx.clamp(0, u32::MAX as i64) as u32
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

    /// Apre un nuovo turno-traccia dai dati del `MetaStep{kind:"executor_call"}`
    /// (provider/model/iteration/tools_count). PRIMA flusha l'eventuale turno
    /// ancora pendente (turno precedente concluso con tool_use, che non emette
    /// `EndTurn`): cosi' ogni interrogazione del modello produce esattamente una
    /// traccia, parita' col path Python.
    fn open_trace_turn(&self, payload: &serde_json::Value, stop_reason: &str) {
        self.flush_trace_turn(stop_reason);
        let pt = PendingTrace {
            iteration: payload
                .get("iteration")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            provider: payload
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            model: payload
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            tools_count: payload
                .get("tools_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            input_tokens: 0,
            output_tokens: 0,
            tool_calls: Vec::new(),
        };
        *self.pending_trace.lock() = Some(pt);
    }

    /// Chiude il turno-traccia pendente (se esiste): costruisce l'`AITraceEvent`,
    /// lo EMETTE LIVE (evento con `trace: Some(...)` -> il trace panel del
    /// frontend lo riceve anche nel ramo nativo) e lo PERSISTE best-effort su
    /// `nexus_agent_traces` (punto unico [`crate::trace_store::persist_trace`]).
    /// `stop_reason` distingue il turno concluso (`end_turn`) da quello che ha
    /// richiesto tool (`tool_use`). No-op se non c'e' un turno pendente.
    fn flush_trace_turn(&self, stop_reason: &str) {
        let Some(pt) = self.pending_trace.lock().take() else {
            return;
        };
        let trace = AITraceEvent {
            run_id: self.run_id.to_string(),
            iteration: pt.iteration,
            provider: pt.provider,
            model: pt.model,
            // Il grafo nativo non espone via SSE i messaggi del contesto ne' il
            // testo della risposta del turno (nessun token streaming): i campi
            // restano a 0 / vuoto. La diagnostica del ramo nativo si basa su
            // provider/model/iteration/tokens/tool_calls/stop_reason.
            messages_sent: 0,
            tools_count: pt.tools_count,
            response_text: String::new(),
            tool_calls: pt.tool_calls,
            stop_reason: stop_reason.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            input_tokens: pt.input_tokens,
            output_tokens: pt.output_tokens,
            cache_read_tokens: 0,
        };

        // Persistenza best-effort (punto unico trace_store, regola L). `emit` e'
        // sincrono e infallibile: la INSERT async gira in un task staccato
        // (fire-and-forget) cosi' non blocca il nodo del grafo. Un seq monotono
        // per-run da' l'ordine cronologico nel trace panel.
        if let Some(db) = &self.db {
            let seq = self.next_trace_seq.fetch_add(1, Ordering::Relaxed);
            let seq = seq.clamp(0, i32::MAX as i64) as i32;
            if let Ok(payload) = serde_json::to_value(&trace) {
                let db = db.clone();
                let session_id = self.session_id;
                let run_id = self.run_id;
                tokio::spawn(async move {
                    crate::trace_store::persist_trace(&db, session_id, run_id, seq, &payload).await;
                });
            }
        }

        // Emissione LIVE: lo stesso evento `agent_trace` del path Python (il
        // frontend mappa `trace.is_some()` -> "agent_trace"). Live e refresh ora
        // coincidono anche nel ramo nativo.
        let mut e = self.base();
        e.trace = Some(trace);
        self.send(e);
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
                // `executor_call` apre un turno-traccia: il grafo lo emette PRIMA
                // di interrogare il modello, col provider/model/iteration/
                // tools_count del turno. Da qui l'adapter ricostruisce
                // l'AITraceEvent (nessun evento `trace` nativo). Un nuovo
                // `executor_call` chiude il turno precedente come `tool_use` (il
                // turno con tool non emette `EndTurn`).
                if kind == "executor_call" {
                    self.open_trace_turn(&payload, "tool_use");
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
                // Token del turno corrente nella traccia gateway in costruzione.
                // Nella STESSA presa del lock leggo provider/model del turno
                // (aperti dall'executor_call precedente) per attribuire i token al
                // provider corrente nel payload usage_snapshot: cosi' il frontend
                // ripartisce token/costo per provider SENZA aggregare le trace
                // (ADR 0037 arricchimento C, additivo). Segnale STRUTTURATO dalla
                // PendingTrace, mai dedotto dal testo (regola M).
                let mut provider_model: Option<(String, String)> = None;
                if let Some(pt) = self.pending_trace.lock().as_mut() {
                    pt.input_tokens = prompt_tokens.clamp(0, u32::MAX as i64) as u32;
                    pt.output_tokens = completion_tokens.clamp(0, u32::MAX as i64) as u32;
                    if !pt.provider.is_empty() || !pt.model.is_empty() {
                        provider_model = Some((pt.provider.clone(), pt.model.clone()));
                    }
                }
                let mut payload = serde_json::json!({
                    "totalTokens": total_tokens,
                    "promptTokens": prompt_tokens,
                    "completionTokens": completion_tokens,
                });
                // Campi extra additivi: presenti solo se il turno-traccia porta il
                // provider/model (executor_call aperto). Assenti = degrado pulito.
                if let (Some((provider, model)), Some(obj)) =
                    (provider_model, payload.as_object_mut())
                {
                    if !provider.is_empty() {
                        obj.insert("provider".to_string(), serde_json::Value::String(provider));
                    }
                    if !model.is_empty() {
                        obj.insert("model".to_string(), serde_json::Value::String(model));
                    }
                }
                let mut e = self.base();
                e.meta_step = Some(AgentMetaStep {
                    kind: "usage_snapshot".to_string(),
                    title: String::new(),
                    payload,
                    correlation_id: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                });
                self.send(e);
            }
            SseEvent::ToolUse { id, name, input } => {
                // Accumula la tool-call nella traccia gateway del turno corrente
                // (campo `tool_calls` dell'AITraceEvent: name + input).
                if let Some(pt) = self.pending_trace.lock().as_mut() {
                    pt.tool_calls.push(serde_json::json!({
                        "name": name,
                        "input": input,
                    }));
                }
                // Indice monotono per-run (parita' Python): ogni tool_use apre un
                // nuovo step. `fetch_add` ritorna il valore precedente; il primo
                // step ha indice 0, i successivi crescono -> il frontend non
                // sovrascrive piu' lo step (upsert per stepIndex distinto).
                let idx = self.next_step_index.fetch_add(1, Ordering::Relaxed);
                // Registra la correlazione id -> (indice, nome) per il ToolResult.
                self.tool_index
                    .lock()
                    .insert(id.clone(), (idx, name.clone()));
                let mut e = self.base();
                e.step = Some(AgentStep {
                    run_id: self.run_id.to_string(),
                    step_index: Self::step_index_u32(idx),
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
                // Recupera indice + nome dallo step Running aperto dal ToolUse
                // correlato cosi' il frontend aggiorna LO STESSO step (stesso
                // step_index, stesso tool_name) invece di crearne uno nuovo.
                // Se la correlazione manca (ToolResult orfano) apriamo comunque
                // un indice nuovo con nome vuoto per non collidere con altri step.
                let (idx, name) = self
                    .tool_index
                    .lock()
                    .get(&tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        (
                            self.next_step_index.fetch_add(1, Ordering::Relaxed),
                            String::new(),
                        )
                    });
                let mut e = self.base();
                e.step = Some(AgentStep {
                    run_id: self.run_id.to_string(),
                    step_index: Self::step_index_u32(idx),
                    tool_name: name,
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
                // Turno concluso testualmente: flusha la traccia gateway come
                // `end_turn` (emit LIVE `agent_trace` + persist nexus_agent_traces).
                self.flush_trace_turn("end_turn");
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
                // Chiusura del run: flusha l'eventuale traccia residua (turno
                // chiuso con tool_use a cui non e' seguito un altro executor_call
                // ne' un EndTurn, es. interrupt HITL).
                self.flush_trace_turn("tool_use");
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
        // Primo step: indice 0 (fetch_add ritorna il valore pre-incremento).
        assert_eq!(step.step_index, 0);
    }

    #[test]
    fn tool_use_indici_monotoni_crescenti() {
        // Regressione del bug del progresso live: step_index hardcoded a 0 ->
        // il frontend (upsert per stepIndex) sovrascriveva ogni step. Ora ogni
        // tool_use deve avere un indice distinto e crescente.
        let (sink, mut rx, _) = setup();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            sink.emit(SseEvent::ToolUse {
                id: id.to_string(),
                name: format!("tool_{i}"),
                input: json!({}),
            });
            let step = rx.try_recv().expect("evento emesso").step.expect("step");
            assert_eq!(step.step_index, i as u32, "indice atteso {i}");
        }
    }

    #[test]
    fn tool_result_riusa_indice_e_nome_del_tool_use() {
        // Parita' col path Python: il tool_result aggiorna LO STESSO step del
        // tool_use correlato (stesso step_index, stesso tool_name).
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::ToolUse {
            id: "tu_42".to_string(),
            name: "edit_file".to_string(),
            input: json!({"path": "y"}),
        });
        let use_step = rx.try_recv().expect("tool_use").step.expect("step");
        assert_eq!(use_step.step_index, 0);
        assert_eq!(use_step.status, AgentStepStatus::Running);

        sink.emit(SseEvent::ToolResult {
            tool_call_id: "tu_42".to_string(),
            content: json!("done"),
            is_error: false,
        });
        let res_step = rx.try_recv().expect("tool_result").step.expect("step");
        // Stesso indice e stesso nome del tool_use: il frontend aggiorna lo step.
        assert_eq!(res_step.step_index, use_step.step_index);
        assert_eq!(res_step.tool_name, "edit_file");
        assert_eq!(res_step.status, AgentStepStatus::Completed);
        assert_eq!(res_step.tool_result.as_deref(), Some("done"));
    }

    #[test]
    fn tool_result_orfano_usa_indice_nuovo_nome_vuoto() {
        // Nessun tool_use correlato: indice nuovo (non collide) e nome vuoto.
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::ToolResult {
            tool_call_id: "ignoto".to_string(),
            content: json!("x"),
            is_error: false,
        });
        let step = rx.try_recv().expect("evento emesso").step.expect("step");
        assert_eq!(step.step_index, 0);
        assert_eq!(step.tool_name, "");
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

    /// Drena il canale e ritorna la PRIMA traccia (`trace.is_some()`) ricevuta.
    fn next_trace(rx: &mut broadcast::Receiver<AgentStepEvent>) -> Option<AITraceEvent> {
        while let Ok(ev) = rx.try_recv() {
            if let Some(t) = ev.trace {
                return Some(t);
            }
        }
        None
    }

    #[test]
    fn executor_call_apre_turno_endturn_flusha_traccia() {
        // Turno concluso testualmente: executor_call (provider/model/iter) + Usage
        // (token) + EndTurn -> una traccia gateway end_turn ricostruita ed emessa.
        let (sink, mut rx, run_id) = setup();
        sink.emit(SseEvent::MetaStep {
            kind: "executor_call".to_string(),
            title: "Sto interrogando".to_string(),
            payload: json!({
                "provider": "anthropic",
                "model": "claude-x",
                "iteration": 2,
                "tools_count": 5,
            }),
        });
        sink.emit(SseEvent::Usage {
            prompt_tokens: 100,
            completion_tokens: 30,
            total_tokens: 130,
        });
        sink.emit(SseEvent::EndTurn);

        let trace = next_trace(&mut rx).expect("traccia emessa su EndTurn");
        assert_eq!(trace.run_id, run_id.to_string());
        assert_eq!(trace.iteration, 2);
        assert_eq!(trace.provider, "anthropic");
        assert_eq!(trace.model, "claude-x");
        assert_eq!(trace.tools_count, 5);
        assert_eq!(trace.input_tokens, 100);
        assert_eq!(trace.output_tokens, 30);
        assert_eq!(trace.stop_reason, "end_turn");
        assert!(trace.tool_calls.is_empty(), "turno testuale: nessun tool");
    }

    #[test]
    fn turno_con_tool_use_flusha_su_nuovo_executor_call() {
        // Il turno con tool NON emette EndTurn: la traccia si chiude al successivo
        // executor_call, con stop_reason tool_use e i tool_calls accumulati.
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::MetaStep {
            kind: "executor_call".to_string(),
            title: "t1".to_string(),
            payload: json!({"provider": "google", "model": "g", "iteration": 0, "tools_count": 3}),
        });
        sink.emit(SseEvent::Usage {
            prompt_tokens: 50,
            completion_tokens: 10,
            total_tokens: 60,
        });
        sink.emit(SseEvent::ToolUse {
            id: "tu1".to_string(),
            name: "read_file".to_string(),
            input: json!({"path": "x"}),
        });
        // Secondo turno: chiude il primo come tool_use.
        sink.emit(SseEvent::MetaStep {
            kind: "executor_call".to_string(),
            title: "t2".to_string(),
            payload: json!({"provider": "google", "model": "g", "iteration": 1, "tools_count": 3}),
        });

        let trace = next_trace(&mut rx).expect("traccia del primo turno");
        assert_eq!(trace.iteration, 0);
        assert_eq!(trace.stop_reason, "tool_use");
        assert_eq!(trace.tool_calls.len(), 1, "una tool-call accumulata");
        assert_eq!(trace.tool_calls[0]["name"], json!("read_file"));
    }

    #[test]
    fn done_flusha_traccia_residua() {
        // Turno con tool a cui non segue altro executor_call: Done flusha il residuo.
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::MetaStep {
            kind: "executor_call".to_string(),
            title: "t".to_string(),
            payload: json!({"provider": "mistral", "model": "m", "iteration": 0, "tools_count": 1}),
        });
        sink.emit(SseEvent::ToolUse {
            id: "tu".to_string(),
            name: "edit_file".to_string(),
            input: json!({}),
        });
        sink.emit(SseEvent::Done);

        let trace = next_trace(&mut rx).expect("traccia residua su Done");
        assert_eq!(trace.provider, "mistral");
        assert_eq!(trace.stop_reason, "tool_use");
        assert_eq!(trace.tool_calls.len(), 1);
    }

    #[test]
    fn senza_executor_call_nessuna_traccia() {
        // EndTurn senza un turno aperto (nessun executor_call) non emette traccia.
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::EndTurn);
        assert!(next_trace(&mut rx).is_none(), "nessuna traccia senza turno");
    }

    /// Drena il canale e ritorna il payload del PRIMO meta_step col `kind` dato.
    fn next_meta_step_payload(
        rx: &mut broadcast::Receiver<AgentStepEvent>,
        kind: &str,
    ) -> Option<serde_json::Value> {
        while let Ok(ev) = rx.try_recv() {
            if let Some(ms) = ev.meta_step {
                if ms.kind == kind {
                    return Some(ms.payload);
                }
            }
        }
        None
    }

    #[test]
    fn usage_snapshot_arricchito_con_provider_model_del_turno() {
        // ADR 0037 arricchimento C: dopo un executor_call(provider,model) il
        // meta_step usage_snapshot deve portare provider/model del turno corrente
        // (letti dalla PendingTrace, segnale strutturato), oltre ai token.
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::MetaStep {
            kind: "executor_call".to_string(),
            title: "Sto interrogando".to_string(),
            payload: json!({
                "provider": "anthropic",
                "model": "claude-x",
                "iteration": 0,
                "tools_count": 4,
            }),
        });
        sink.emit(SseEvent::Usage {
            prompt_tokens: 120,
            completion_tokens: 40,
            total_tokens: 160,
        });

        let payload =
            next_meta_step_payload(&mut rx, "usage_snapshot").expect("usage_snapshot emesso");
        // Campi token invariati (retro-compatibilita').
        assert_eq!(payload["totalTokens"], json!(160));
        assert_eq!(payload["promptTokens"], json!(120));
        assert_eq!(payload["completionTokens"], json!(40));
        // Campi extra additivi dal turno-traccia corrente.
        assert_eq!(payload["provider"], json!("anthropic"));
        assert_eq!(payload["model"], json!("claude-x"));
    }

    #[test]
    fn usage_snapshot_senza_turno_non_ha_provider_model() {
        // Additivo e a degrado pulito: senza executor_call (nessuna PendingTrace)
        // il payload usage_snapshot NON deve inventare provider/model.
        let (sink, mut rx, _) = setup();
        sink.emit(SseEvent::Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        });

        let payload =
            next_meta_step_payload(&mut rx, "usage_snapshot").expect("usage_snapshot emesso");
        assert_eq!(payload["totalTokens"], json!(15));
        assert!(
            payload.get("provider").is_none(),
            "senza turno-traccia niente provider"
        );
        assert!(
            payload.get("model").is_none(),
            "senza turno-traccia niente model"
        );
    }
}
