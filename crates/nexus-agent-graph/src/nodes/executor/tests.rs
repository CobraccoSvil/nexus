//! Test del nodo `ExecutorNode` con porte mockate. Esercitano la decision
//! machine del SINGOLO turno (gate testa, ordine nudge, risoluzione provider,
//! costruzione delta), con LLM/store stubati. L'I/O (LLM/heartbeat/meta_step) e'
//! sostituito da stub che ritornano valori fissi/registrano le chiamate.

use std::sync::Arc;

use nexus_graph::node::GraphNode;
use nexus_graph::GraphState as _;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::*;
use crate::routing::config::RoutingConfig;
use crate::runtime::ports::{LlmResponse, LlmUsage, SseEvent};
use crate::runtime::test_doubles::{
    NullEventSink, RecordingEventSink, StubAgentStepStore, StubBillingCooldownPort,
    StubEscalationPort, StubLlmGateway, StubMetaStepStore, StubModelUpscalePort,
    StubNextActionsDeriver, StubRunControlStore, StubSummaryStore, StubToolExecutor,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{ContentBlock, MessageContent, ToolUse};

fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
    let mut s = base;
    s.merge(delta);
    s
}

/// Config con provider/model risolti a monte (regola G) e progress_controller ON.
fn cfg_resolved() -> ExecutorConfig {
    ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        progress_controller_enabled: true,
        ..ExecutorConfig::default()
    }
}

/// Ctx con un gateway LLM dato e shadow configurabile.
fn ctx_with(llm: Arc<dyn crate::runtime::ports::LlmGateway>, shadow: bool) -> AgentNodeCtx {
    ctx_with_emit(llm, shadow, Arc::new(NullEventSink))
}

/// Come [`ctx_with`] ma con un [`EventSink`] iniettabile (per asserire gli emit
/// SSE del nodo): i test passano un `RecordingEventSink` e leggono `events`.
fn ctx_with_emit(
    llm: Arc<dyn crate::runtime::ports::LlmGateway>,
    shadow: bool,
    emit: Arc<dyn crate::runtime::ports::EventSink>,
) -> AgentNodeCtx {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://test:test@127.0.0.1:1/test")
        .expect("connect_lazy");
    AgentNodeCtx {
        db: pool,
        llm,
        tools: Arc::new(StubToolExecutor::with_success(json!("{}"))),
        emit,
        cfg: RoutingConfig::default(),
        cancel: CancellationToken::new(),
        run_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        thread_id: Uuid::new_v4(),
        shadow,
    }
}

/// Nodo con porte stub configurabili; ritorna anche gli store per le asserzioni.
/// Escalation disabilitata di default (porta vuota -> selezione `None` -> chiusura
/// secca come prima): i test che vogliono l'escalation usano [`node_esc`].
fn node(
    cfg: ExecutorConfig,
    rc: Arc<StubRunControlStore>,
) -> (
    ExecutorNode,
    Arc<StubMetaStepStore>,
    Arc<StubAgentStepStore>,
) {
    node_esc(cfg, rc, Arc::new(StubEscalationPort::default()))
}

/// Come [`node`] ma con una porta escalation configurabile (catena/cross/cooldown).
/// Le porte next_actions/billing/upscale sono i default no-op (rami POST/billing/
/// upscale inerti): i test che le esercitano usano [`node_ports`].
fn node_esc(
    cfg: ExecutorConfig,
    rc: Arc<StubRunControlStore>,
    esc: Arc<StubEscalationPort>,
) -> (
    ExecutorNode,
    Arc<StubMetaStepStore>,
    Arc<StubAgentStepStore>,
) {
    let meta = Arc::new(StubMetaStepStore::default());
    let steps = Arc::new(StubAgentStepStore::default());
    let n = ExecutorNode::new(
        cfg,
        rc,
        meta.clone(),
        steps.clone(),
        esc,
        Arc::new(StubNextActionsDeriver::default()),
        Arc::new(StubBillingCooldownPort::default()),
        Arc::new(StubModelUpscalePort::default()),
        // Summarizer di default: nessun summary -> degrado (history invariata).
        Arc::new(StubSummaryStore::default()),
    );
    (n, meta, steps)
}

/// Nodo con un [`StubSummaryStore`] CONFIGURABILE (rolling-summary). Ritorna anche
/// lo stub per asserire l'input serializzato passato al summarizer.
fn node_summary(
    cfg: ExecutorConfig,
    rc: Arc<StubRunControlStore>,
    summary: Arc<StubSummaryStore>,
) -> ExecutorNode {
    let meta = Arc::new(StubMetaStepStore::default());
    let steps = Arc::new(StubAgentStepStore::default());
    ExecutorNode::new(
        cfg,
        rc,
        meta,
        steps,
        Arc::new(StubEscalationPort::default()),
        Arc::new(StubNextActionsDeriver::default()),
        Arc::new(StubBillingCooldownPort::default()),
        Arc::new(StubModelUpscalePort::default()),
        summary,
    )
}

/// Nodo con le porte POST/billing/upscale CONFIGURABILI (per i 4 rami PR-J2).
/// Escalation = default vuoto. Ritorna anche `meta` per asserire i meta_step.
fn node_ports(
    cfg: ExecutorConfig,
    rc: Arc<StubRunControlStore>,
    next_actions: Arc<StubNextActionsDeriver>,
    billing: Arc<StubBillingCooldownPort>,
    upscale: Arc<StubModelUpscalePort>,
) -> (ExecutorNode, Arc<StubMetaStepStore>) {
    let meta = Arc::new(StubMetaStepStore::default());
    let steps = Arc::new(StubAgentStepStore::default());
    let n = ExecutorNode::new(
        cfg,
        rc,
        meta.clone(),
        steps,
        Arc::new(StubEscalationPort::default()),
        next_actions,
        billing,
        upscale,
        Arc::new(StubSummaryStore::default()),
    );
    (n, meta)
}

fn human(text: &str) -> Message {
    Message::Human {
        content: MessageContent::text(text),
    }
}

fn ai_tool(name: &str, input: Value) -> Message {
    Message::Ai {
        content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: "c1".into(),
            name: name.into(),
            input,
        }]),
        tool_calls: vec![],
        reasoning: None,
    }
}

fn tool_msg_err(text: &str) -> Message {
    Message::Tool {
        tool_call_id: "c1".into(),
        content: MessageContent::text(text),
    }
}

/// LLM stub che emette una tool call (happy path: il modello chiede un tool).
fn llm_tool_call(name: &str, input: Value) -> Arc<StubLlmGateway> {
    Arc::new(StubLlmGateway::with_tool_call(name, input))
}

#[tokio::test]
async fn superseded_early_return() {
    let rc = Arc::new(StubRunControlStore {
        superseded: true,
        ..Default::default()
    });
    let (n, _meta, _steps) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("ciao")],
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::Superseded));
    // LLM NON chiamato (uscita cooperativa prima del modello).
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn declared_done_ripetuto_chiude() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea x")],
        declared_outcome: Some(json!({"outcome": "done", "summary": "fatto tutto"})),
        declared_done_count: Some(3),
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.result.as_deref(), Some("fatto tutto"));
    assert_eq!(out.iterations, Some(6));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn g1_cap_raggiunto_ferma_senza_modello() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    // Re-entry G1: prev end_turn, iter>=1, action_oriented, no pending, no error.
    // current_count 2 + 1 = 3 = cap -> G1CapReached (escalation = TODO -> cap secco).
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("avvia il servizio")],
        action_oriented: Some(true),
        stop_reason: Some(StopReason::EndTurn),
        iterations: Some(4),
        g1_reroute_count: Some(2),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1CapReached));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert_eq!(out.g1_reroute_count, Some(3));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nudge_esplorazione_a_soglia_iniettato() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    // Risposta TESTUALE (no tool): cosi' exploration_counter_update lascia il flag
    // invariato (turno testuale) e l'iniezione del nudge resta osservabile in
    // output. Con una call produttiva il flag verrebbe RESETTATO a false (reset
    // coordinato 1:1 col Python), che e' un comportamento gia' coperto altrove.
    let llm = Arc::new(StubLlmGateway::with_text("Procedo con la risposta."));
    let ctx = ctx_with(llm.clone(), false);
    // exploration_count == soglia (6), nudge non ancora inviato -> nudge iniettato.
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi tutto")],
        consecutive_exploration_calls: Some(6),
        exploration_nudge_sent: Some(false),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Turno testuale -> il flag iniettato (true) sopravvive (no reset produttivo).
    assert_eq!(out.exploration_nudge_sent, Some(true));
    assert!(!llm.seen.lock().unwrap().is_empty());
    // Il prompt LLM contiene il nudge anti-esplorazione (ultimo user message).
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    let has_nudge = req.messages.iter().any(|m| {
        m.role == "user"
            && matches!(&m.content, Value::String(s) if s.contains("NON esplorare oltre"))
    });
    assert!(has_nudge, "il nudge anti-esplorazione deve essere nel prompt");
}

#[tokio::test]
async fn nudge_comando_fallito_a_tre() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = llm_tool_call("read_file", json!({"path": "x"}));
    let ctx = ctx_with(llm.clone(), false);
    // Stesso comando fallito 3 volte (run_command + tool_msg errore).
    let mut messages = vec![human("builda")];
    for _ in 0..3 {
        messages.push(ai_tool("run_command", json!({"command": "npm run build"})));
        messages.push(tool_msg_err("error: build failed"));
    }
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.repeated_cmd_nudge_sent, Some(true));
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    let has_cmd_nudge = req.messages.iter().any(|m| {
        matches!(&m.content, Value::String(s) if s.contains("[LOOP RILEVATO]") && s.contains("npm run build"))
    });
    assert!(has_cmd_nudge, "il nudge anti-loop-comando deve essere nel prompt");
}

#[tokio::test]
async fn repeated_action_abort_chiude() {
    let rc = Arc::new(StubRunControlStore::default());
    // progress_controller ON, soglia repeated_action 2, asse gia' guidato E
    // gia' diagnosticato -> ABORT (niente escalation candidate). Per un write
    // FALLITO l'ABORT scatta solo DOPO che l'estratto e' stato sfruttato (GUIDE
    // + FORCE_DIAGNOSE gia' emessi): qui simuliamo entrambi gia' passati.
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    // write_file ripetuto 2 volte SENZA mai riuscire (stallo, stesso contenuto).
    let messages = vec![
        human("scrivi"),
        ai_tool("write_file", json!({"path": "b.rs"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("permission denied".into()),
                is_error: true,
                exit_code: None,
            }]),
        },
        ai_tool("write_file", json!({"path": "b.rs"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("permission denied".into()),
                is_error: true,
                exit_code: None,
            }]),
        },
    ];
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::LoopAbort));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert!(out.result.as_deref().unwrap().contains("ESITO: non completato"));
    // LLM non chiamato (abort prima del modello).
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn repeated_action_edit_fallito_diagnose_prima_di_abort() {
    // Causa radice del falso-stallo: un write_file/edit_file FALLITO, asse GIA'
    // guidato ma NON ancora diagnosticato, NON deve abortire: deve passare per
    // FORCE_DIAGNOSE iniettando il nudge SPECIFICO ("copia l'old_string esatto")
    // e RICHIAMARE l'LLM, cosi' l'agente ha la chance di correggere prima di
    // chiudere a 0 file modificati.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("correggo"));
    let ctx = ctx_with(llm.clone(), false);
    let messages = vec![
        human("modifica il file"),
        ai_tool(
            "edit_file",
            json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
        ),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("old_string non trovato".into()),
                is_error: true,
                exit_code: None,
            }]),
        },
        ai_tool(
            "edit_file",
            json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
        ),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("old_string non trovato".into()),
                is_error: true,
                exit_code: None,
            }]),
        },
    ];
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state.clone(), delta);
    // NON deve abortire.
    assert_ne!(out.stop_reason, Some(StopReason::LoopAbort));
    // L'asse e' stato segnato come diagnosticato (FORCE_DIAGNOSE).
    assert!(out
        .progress_diagnosed_axes
        .as_deref()
        .unwrap_or_default()
        .contains(&"repeated_action".to_string()));
    // L'LLM e' stato chiamato (la diagnosi prosegue, non chiude).
    let req = llm.seen.lock().unwrap().last().cloned().expect("llm chiamato");
    // Il prompt contiene il nudge SPECIFICO edit-fallito.
    let has_specific_nudge = req.messages.iter().any(|m| {
        matches!(&m.content, Value::String(s) if s.contains("old_string ESATTO"))
    });
    assert!(has_specific_nudge, "atteso il nudge specifico edit-fallito nel prompt");
}

#[tokio::test]
async fn happy_path_tool_use_produce_pending() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, meta, _s) = node(cfg_resolved(), rc.clone());
    // Risposta con tool_use + stop_reason tool_use.
    let canned = LlmResponse {
        content: String::new(),
        tool_calls: vec![ToolUse {
            id: "tc1".into(),
            name: "write_file".into(),
            input: json!({"path": "a.rs"}),
        }],
        usage: LlmUsage::default(),
        stop_reason: Some("tool_use".into()),
        ..Default::default()
    };
    let llm = Arc::new(StubLlmGateway {
        canned,
        error: None,
        error_provider_unavailable: false,
        seen: std::sync::Mutex::new(vec![]),
    });
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi a.rs")],
        action_oriented: Some(true),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        token_budget: Some(400),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(out.iterations, Some(1));
    let pending = out.pending_tool_uses.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].get("name").and_then(Value::as_str), Some("write_file"));
    assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
    // signature registrata per il loop-detection.
    assert_eq!(out.recent_tool_signatures.unwrap().len(), 1);
    // heartbeat + meta_step persistiti in Real.
    assert!(!rc.heartbeats.lock().unwrap().is_empty());
    assert!(!meta.meta_steps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn happy_path_end_turn_testuale() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("Ecco la risposta finale."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // stop_reason None dallo stub -> default end_turn.
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.result.as_deref(), Some("Ecco la risposta finale."));
    assert!(out.pending_tool_uses.unwrap().is_empty());
}

/// DEBITO 3 (SSE primario): quando il modello chiede un tool, l'executor emette
/// `SseEvent::ToolUse` con id/name/input. Nessun `EndTurn` (il turno non e' chiuso).
#[tokio::test]
async fn executor_emette_tool_use_su_pending() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let canned = LlmResponse {
        content: String::new(),
        tool_calls: vec![ToolUse {
            id: "tc1".into(),
            name: "write_file".into(),
            input: json!({"path": "a.rs"}),
        }],
        usage: LlmUsage::default(),
        stop_reason: Some("tool_use".into()),
        ..Default::default()
    };
    let llm = Arc::new(StubLlmGateway {
        canned,
        error: None,
        error_provider_unavailable: false,
        seen: std::sync::Mutex::new(vec![]),
    });
    let sink = Arc::new(RecordingEventSink::default());
    let ctx = ctx_with_emit(llm, false, sink.clone());
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi a.rs")],
        action_oriented: Some(true),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        token_budget: Some(400),
        ..Default::default()
    };
    let _ = n.run(&state, &ctx).await.expect("run");
    let events = sink.events.lock().expect("lock");
    // Almeno un ToolUse col tool richiesto; nessun EndTurn (turno con tool).
    assert!(
        events.iter().any(|e| matches!(
            e,
            SseEvent::ToolUse { name, id, .. } if name == "write_file" && id == "tc1"
        )),
        "atteso SseEvent::ToolUse(write_file), eventi: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, SseEvent::EndTurn)),
        "nessun EndTurn quando il turno richiede un tool"
    );
}

/// DEBITO 3 (SSE primario): turno conversazionale concluso (end_turn senza tool)
/// -> l'executor emette `SseEvent::EndTurn`. NON emette `Done` (il terminatore
/// is_final e' del finalizzatore, non di un nodo intermedio).
#[tokio::test]
async fn executor_emette_end_turn_su_chiusura_testuale() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("Ecco la risposta finale."));
    let sink = Arc::new(RecordingEventSink::default());
    let ctx = ctx_with_emit(llm, false, sink.clone());
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let _ = n.run(&state, &ctx).await.expect("run");
    let events = sink.events.lock().expect("lock");
    assert!(
        events.iter().any(|e| matches!(e, SseEvent::EndTurn)),
        "atteso SseEvent::EndTurn sul turno concluso, eventi: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, SseEvent::Done)),
        "il terminatore Done non e' emesso da un nodo (lo emette il finalizzatore)"
    );
    assert!(
        !events.iter().any(|e| matches!(e, SseEvent::ToolUse { .. })),
        "nessun ToolUse su un turno solo-testo"
    );
}

/// DEBITO 3 (shadow intatto): in shadow l'EventSink iniettato nel ctx e' il no-op
/// (NullEventSink), quindi un `RecordingEventSink` collegato al ramo Real NON
/// verrebbe usato. Qui verifichiamo la PROPRIETA' a livello di nodo: con
/// `NullEventSink` (il sink dello shadow) nessun emit e' osservabile.
#[tokio::test]
async fn shadow_sink_noop_non_emette() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("risposta"));
    // shadow=true + NullEventSink: e' la combinazione che build_native_engine
    // costruisce per il run shadow. Un emit qui e' un no-op by-construction.
    let ctx = ctx_with(llm, true);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        action_oriented: Some(false),
        ..Default::default()
    };
    // Non deve panicare: gli emit cadono nel no-op. (La garanzia che lo shadow
    // riceva NullEventSink e' nel punto unico build_native_engine.)
    let _ = n.run(&state, &ctx).await.expect("run shadow");
}

#[tokio::test]
async fn no_provider_sentinella_node_error() {
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        routing_provider: "__no_capable_provider__".to_string(),
        routing_model: "__no_capable_provider__".to_string(),
        ..ExecutorConfig::default()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("fai qualcosa")],
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let err = n.run(&state, &ctx).await.expect_err("deve fallire (no provider)");
    assert!(matches!(err, NodeError::Failed { .. }));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retry_senza_forcing_su_errore() {
    // Stub LLM che alla PRIMA chiamata (con forcing) ritorna stop_reason=error,
    // alla seconda (senza forcing) ritorna una risposta valida.
    struct RetryLlm {
        calls: std::sync::Mutex<Vec<crate::runtime::ports::LlmRequest>>,
    }
    #[async_trait::async_trait]
    impl crate::runtime::ports::LlmGateway for RetryLlm {
        async fn complete(
            &self,
            req: crate::runtime::ports::LlmRequest,
        ) -> Result<LlmResponse, crate::runtime::ports::PortError> {
            let n = {
                let mut g = self.calls.lock().unwrap();
                g.push(req.clone());
                g.len()
            };
            if n == 1 {
                // Forcing fallito.
                Ok(LlmResponse {
                    stop_reason: Some("error".into()),
                    ..Default::default()
                })
            } else {
                Ok(LlmResponse {
                    content: "ok dopo retry".into(),
                    stop_reason: Some("end_turn".into()),
                    ..Default::default()
                })
            }
        }
    }
    let rc = Arc::new(StubRunControlStore::default());
    // Forcing attivo: enabled + supporto stile + action_oriented + iter<=max.
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        tool_choice_forcing_enabled: true,
        tool_choice_style: Some("anthropic_any".to_string()),
        ..ExecutorConfig::default()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(RetryLlm {
        calls: std::sync::Mutex::new(vec![]),
    });
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi")],
        action_oriented: Some(true),
        // Forcing "early action" TRANSITORIO (NON-convergenza, regola H): scatta solo
        // quando il turno PRECEDENTE non ha agito (BUG-e). iter=1 + prev end_turn (non
        // ToolUse) + tool disponibili + action_oriented = segnale strutturale -> forcing
        // ON sulla primaria.
        iterations: Some(1),
        stop_reason: Some(StopReason::EndTurn),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Due chiamate: la prima con forcing=Some(true), la seconda con Some(false).
    let calls = llm.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].force_tool_choice, Some(true));
    assert_eq!(calls[1].force_tool_choice, Some(false));
    assert_eq!(out.result.as_deref(), Some("ok dopo retry"));
}

#[tokio::test]
async fn forcing_early_action_non_scatta_se_turno_precedente_ha_agito() {
    // FIX NON-convergenza (regola H): il forcing "early action" e' TRANSITORIO. Se il
    // turno PRECEDENTE ha gia' agito (stop_reason ToolUse), il forcing NON scatta:
    // il modello PUO' chiudere con una risposta testuale quando il task e' fatto.
    // Prima il forcing scattava a OGNI iterazione iniziale (iter<=max) -> il modello
    // era costretto a chiamare un tool anche su un task read-only gia' soddisfatto,
    // generando il loop list_files/read_file -> escalation -> overflow.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        tool_choice_forcing_enabled: true,
        tool_choice_style: Some("anthropic_any".to_string()),
        ..ExecutorConfig::default()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("Ecco lo stack in 2 righe."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("elenca i file e dimmi lo stack")],
        action_oriented: Some(true),
        // Turno precedente HA agito (tool_use): il task e' gia' esplorato.
        stop_reason: Some(StopReason::ToolUse),
        iterations: Some(1),
        tools_json: Some(vec![json!({"name": "list_files"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _ = apply(state, delta);
    // Il forcing NON e' applicato: il modello e' libero di chiudere con testo.
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        req.force_tool_choice, None,
        "turno precedente che ha agito -> niente forcing early -> chiusura testuale possibile"
    );
}

#[tokio::test]
async fn forcing_early_action_non_scatta_al_primo_turno() {
    // Al PRIMO turno (iter=0, nessun precedente) il forcing early NON scatta: un task
    // banale puo' essere risposto direttamente. La forza-azione resta affidata ai
    // nudge del progress_controller / al G1 anti-descrittivo sulle iterazioni dove
    // il modello descrive senza agire (BUG-e reale), non al primo giro.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        tool_choice_forcing_enabled: true,
        tool_choice_style: Some("anthropic_any".to_string()),
        ..ExecutorConfig::default()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("Risposta diretta."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("domanda semplice")],
        action_oriented: Some(true),
        iterations: Some(0),
        tools_json: Some(vec![json!({"name": "list_files"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _ = apply(state, delta);
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        req.force_tool_choice, None,
        "primo turno -> niente forcing early (nessun BUG-e: il turno precedente non esiste)"
    );
}

#[tokio::test]
async fn forcing_early_action_scatta_se_turno_precedente_non_ha_agito() {
    // BUG-e PRESERVATO (regola H): se il turno precedente NON ha agito pur dovendo
    // (prev end_turn + tool disponibili + action_oriented), il forcing early scatta
    // ancora -> il modello e' obbligato ad agire invece di descrivere. E' la
    // condizione che f2daab6 ripristinava; qui resta intatta, solo resa transitoria.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        tool_choice_forcing_enabled: true,
        tool_choice_style: Some("anthropic_any".to_string()),
        ..ExecutorConfig::default()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = llm_tool_call("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi il file")],
        action_oriented: Some(true),
        stop_reason: Some(StopReason::EndTurn), // turno precedente: descrive, non agisce
        iterations: Some(1),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _ = apply(state, delta);
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        req.force_tool_choice, Some(true),
        "BUG-e preservato: turno precedente che non ha agito -> forcing early ON"
    );
}

#[tokio::test]
async fn lettura_ripetuta_identica_informativa_guida_a_concludere() {
    // FIX #2 loop-control (regola H): una LETTURA ripetuta identica (stesso path)
    // oltre soglia (repeated_action_threshold=2, default) scatta come repeated_action
    // di SOLA LETTURA. Su un turno INFORMATIVO (action_oriented=false) il
    // progress_controller inietta un nudge "concludi con testo" SENZA forzare un'altra
    // tool call, ben prima del cap esplorazione 2x (=12) e dell'escalation. Cosi' il
    // loop read-only non arriva a 14 iterazioni.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // progress_controller ON
    let llm = Arc::new(StubLlmGateway::with_text("Concludo a parole."));
    let ctx = ctx_with(llm.clone(), false);
    // read_file stesso path 2 volte, entrambe RIUSCITE (la rilettura riuscita
    // ripetuta E' lo stallo da fermare per i read-only).
    let messages = vec![
        human("dimmi cosa contiene il file"),
        ai_tool("read_file", json!({"path": "src/main.rs"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("fn main() {}".into()),
                is_error: false,
                exit_code: None,
            }]),
        },
        ai_tool("read_file", json!({"path": "src/main.rs"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c2".into(),
                content: Value::String("fn main() {}".into()),
                is_error: false,
                exit_code: None,
            }]),
        },
    ];
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        tools_json: Some(vec![json!({"name": "read_file"})]),
        // Turno informativo: il nudge deve guidare a concludere con testo.
        action_oriented: Some(false),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _ = apply(state, delta);
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    // Nudge "concludi con testo" iniettato.
    let has_concludi = req.messages.iter().any(|m| {
        matches!(&m.content, Value::String(s) if s.contains("Rispondi ORA a parole"))
    });
    assert!(has_concludi, "atteso nudge 'concludi con testo' per la lettura ripetuta informativa");
    // NON forza il tool (force_tool_choice None): il modello puo' chiudere con testo.
    assert_eq!(
        req.force_tool_choice, None,
        "lettura ripetuta informativa -> niente force-action (non un altro read-only)"
    );
}

#[tokio::test]
async fn lettura_ripetuta_identica_action_oriented_orienta_all_edit() {
    // Raffinamento (regola H) del FIX #2: su un turno ACTION-ORIENTED (task di fix,
    // es. "correggi la porta hardcoded in vite.config.ts") la lettura ripetuta NON
    // deve guidare a "rispondere a parole" (sarebbe una RINUNCIA a 0 file modificati):
    // deve orientare all'EDIT, mantenendo l'anti-loop (no ri-lettura identica) e
    // forzando la tool call cosi' l'agente APPLICA la correzione.
    let rc = Arc::new(StubRunControlStore::default());
    // Stile provider che SUPPORTA il forcing (come i test forcing-early): cosi'
    // force_action_hard del progress_controller si traduce in force_tool_choice.
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        progress_controller_enabled: true,
        tool_choice_style: Some("anthropic_any".to_string()),
        ..ExecutorConfig::default()
    };
    let (n, _m, _s) = node(cfg, rc); // progress_controller ON
    let llm = Arc::new(StubLlmGateway::with_text("Applico l'edit."));
    let ctx = ctx_with(llm.clone(), false);
    let messages = vec![
        human("correggi la porta hardcoded 35198 in vite.config.ts: usa la porta allocata"),
        ai_tool("read_file", json!({"path": "vite.config.ts"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("server: { port: 35198 }".into()),
                is_error: false,
                exit_code: None,
            }]),
        },
        ai_tool("read_file", json!({"path": "vite.config.ts"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c2".into(),
                content: Value::String("server: { port: 35198 }".into()),
                is_error: false,
                exit_code: None,
            }]),
        },
    ];
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        tools_json: Some(vec![json!({"name": "read_file"}), json!({"name": "edit_file"})]),
        // Turno di modifica: il nudge deve orientare all'edit, non alla resa.
        action_oriented: Some(true),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _ = apply(state, delta);
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    // Nudge orientato all'AZIONE iniettato, NON quello "rispondi a parole".
    let has_edit_nudge = req.messages.iter().any(|m| {
        matches!(&m.content, Value::String(s) if s.contains("ESEGUI l'azione"))
    });
    assert!(has_edit_nudge, "atteso nudge orientato all'azione per la lettura ripetuta su un fix");
    let has_concludi = req.messages.iter().any(|m| {
        matches!(&m.content, Value::String(s) if s.contains("Rispondi ORA a parole"))
    });
    assert!(!has_concludi, "su un fix il nudge NON deve guidare a rispondere a parole");
    // Forza la tool call: l'agente DEVE applicare l'edit, non rinunciare.
    assert_eq!(
        req.force_tool_choice, Some(true),
        "lettura ripetuta su un fix -> force-action verso l'edit"
    );
}

#[tokio::test]
async fn shadow_no_scritture() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, meta, _s) = node(cfg_resolved(), rc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("risposta"));
    let ctx = ctx_with(llm.clone(), true); // shadow=true -> Replay
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("fai")],
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let _ = n.run(&state, &ctx).await.expect("run");
    // In Replay: heartbeat e meta_step sono no-op (zero scritture).
    assert!(rc.heartbeats.lock().unwrap().is_empty());
    assert!(meta.meta_steps.lock().unwrap().is_empty());
}

#[tokio::test]
async fn signature_loop_chiude() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    // Lo stub emette sempre lo STESSO tool con lo STESSO input -> signature
    // ripetuta. recent gia' con 2 occorrenze della stessa signature: la nuova
    // (terza) scatta il loop.
    let same_input = json!({"path": "x"});
    let sig = build_signature("read_file", &same_input);
    let llm = llm_tool_call("read_file", same_input.clone());
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi")],
        recent_tool_signatures: Some(vec![sig.clone(), "altro|abc".into(), sig.clone()]),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::LoopDetected));
    assert!(out.pending_tool_uses.unwrap().is_empty());
    assert!(out.result.as_deref().unwrap().contains("[LOOP RILEVATO]"));
}

#[tokio::test]
async fn errore_gateway_persiste_contatori() {
    // Il gateway LLM fallisce (provider down/billing). Parita' col ramo `except`
    // Python (py:3104-3107 -> return UNIFICATO py:3457-3513): il run NON aborta
    // con NodeError, prosegue al delta con stop_reason=error e PERSISTE TUTTI i
    // contatori mutati nel turno (g1_reroute_count, recent_tool_signatures, ...).
    // Setup: re-entry G1 autentica (prev=end_turn, iter=1, no pending,
    // action_oriented) -> g1_reroute_count 0->1 DEVE finire nel delta anche se la
    // chiamata LLM e' fallita. recent_tool_signatures preesistente: invariato
    // (nessun nuovo pending nel turno errato).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_error("billing_error: credito esaurito"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi il file")],
        // Re-entry G1: turno precedente chiuso end_turn senza agire.
        stop_reason: Some(StopReason::EndTurn),
        iterations: Some(1),
        action_oriented: Some(true),
        g1_reroute_count: Some(0),
        recent_tool_signatures: Some(vec!["read_file|deadbeef".into()]),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run NON deve abortire su errore gateway");
    let out = apply(state, delta);
    // Esito error coerente col Python (stop_reason=error, result onesto).
    assert_eq!(out.stop_reason, Some(StopReason::Error));
    assert!(out.result.as_deref().unwrap().contains("[Errore provider"));
    assert!(out.pending_tool_uses.unwrap().is_empty());
    // iterations incrementato come nel ramo Ok (iters_in+1).
    assert_eq!(out.iterations, Some(2));
    // CONTATORE G1 persistito: la re-entry e' stata contata PRIMA della chiamata
    // LLM e DEVE sopravvivere al ramo error (era il bug di parita': il vecchio
    // early-return lo perdeva).
    assert_eq!(out.g1_reroute_count, Some(1));
    // recent_tool_signatures persistito invariato (nessun nuovo pending: la coda
    // resta quella preesistente, == updated_signatures con new_signatures vuoto).
    assert_eq!(
        out.recent_tool_signatures.as_deref(),
        Some(&["read_file|deadbeef".to_string()][..])
    );
    // auto_escalations emesso nel delta (FIX 3): invariato a 0 (escalation TODO).
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(0));
    // UNA sola chiamata LLM: nessun retry-senza-forcing sul ramo error gateway.
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provider_cooldown_fallback_cross_provider_invece_di_error() {
    // FAILOVER cross-provider (regola H + L): il provider scelto e' caduto e il
    // gateway ritorna in modo STRUTTURATO PortError::ProviderUnavailable. L'executor
    // NON deve chiudere con StopReason::Error: deve RIPIEGARE sul provider sano
    // delegando al punto unico del routing (failover_provider) -> sticky promosso +
    // StopReason::G1Escalated (il self-loop rientra col provider sano).
    let rc = Arc::new(StubRunControlStore::default());
    // Esito di failover configurato (il routing avrebbe scelto questo provider sano
    // escludendo quello caduto).
    let esc = Arc::new(StubEscalationPort::with_failover("mistral", "mistral-large-2411"));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_provider_unavailable(
        "Nexus Gateway 500: {\"error\":\"tutti i provider hanno fallito -> anthropic \
(in cooldown, 42s rimanenti)\",\"code\":\"PROVIDER_ERROR\"}",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi il file")],
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-x".into()),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run NON deve abortire");
    let out = apply(state, delta);
    // Failover cross-provider, NON chiusura Error.
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_provider.as_deref(), Some("mistral"));
    assert_eq!(out.sticky_model.as_deref(), Some("mistral-large-2411"));
    assert_eq!(out.g1_reroute_count, Some(0));
    assert_eq!(out.action_nudge_count, Some(0));
    assert!(out.pending_tool_uses.unwrap().is_empty());
    // auto_escalations incrementato (0 -> 1): gate < 3 rispettato.
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(1));
    // failover_tried accumula sia il provider CADUTO sia quello SCELTO, cosi' un
    // eventuale secondo salto li esclude entrambi (cascata).
    let tried: Vec<String> = out
        .extra
        .get("failover_tried")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert_eq!(tried, vec!["anthropic".to_string(), "mistral".to_string()]);
    // UNA sola chiamata LLM (quella fallita): la ri-esecuzione avviene nel self-loop
    // successivo del grafo, non dentro questo turno.
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    // failover_provider e' stato interrogato escludendo il provider caduto.
    let seen = esc.failover_seen.lock().unwrap();
    assert_eq!(seen.last().unwrap(), &vec!["anthropic".to_string()]);
}

#[tokio::test]
async fn provider_cooldown_senza_candidato_chiude_error() {
    // Contro-prova: provider in cooldown (ProviderUnavailable) ma NESSUN candidato
    // cross-provider (porta vuota) -> nessun fallback possibile, l'executor chiude
    // con StopReason::Error come prima (comportamento esistente preservato).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // porta escalation vuota
    let llm = Arc::new(StubLlmGateway::with_provider_unavailable(
        "Nexus Gateway 500: {\"error\":\"tutti i provider hanno fallito -> anthropic \
(in cooldown, 42s rimanenti)\",\"code\":\"PROVIDER_ERROR\"}",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi il file")],
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-x".into()),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run NON deve abortire");
    let out = apply(state, delta);
    // Nessun candidato -> chiusura Error (fallback graceful), sticky invariato.
    assert_eq!(out.stop_reason, Some(StopReason::Error));
    assert!(out.result.as_deref().unwrap().contains("[Errore provider"));
    assert!(out.sticky_provider.is_none());
    assert!(out.pending_tool_uses.unwrap().is_empty());
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn failover_cascata_accumula_provider_gia_provati() {
    // CASCATA (regola L): un salto di failover precedente ha gia' registrato un
    // provider in `failover_tried`. Al nuovo ProviderUnavailable l'executor deve
    // escludere SIA i gia' provati SIA il provider corrente caduto, cosi' la
    // selezione del routing sceglie sempre un provider DIVERSO (non insiste sullo
    // stesso): e' cio' che il vecchio `loop_fallback_default` (candidato fisso) non
    // faceva, costringendo l'utente a ri-lanciare.
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_failover("google", "gemini-2.5-pro"));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_provider_unavailable(
        "Nexus Gateway 500: {\"error\":\"tutti i provider hanno fallito -> mistral\",\
\"code\":\"PROVIDER_ERROR\"}",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    // Stato: un salto precedente ha gia' provato deepseek; auto_escalations=1 (< 3).
    extra.insert("failover_tried".into(), json!(["deepseek"]));
    extra.insert("auto_escalations".into(), json!(1));
    // Il provider corrente del turno e' quello risolto da cfg_resolved()
    // (routing_provider="anthropic"), NON state.provider_used: e' "anthropic" il
    // provider che cade qui.
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("scrivi il file")],
        tools_json: Some(vec![json!({"name": "write_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run NON deve abortire");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_provider.as_deref(), Some("google"));
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(2));
    // failover_provider interrogato escludendo i gia' provati (deepseek) PIU' il
    // provider corrente caduto (anthropic) — in quest'ordine.
    let seen = esc.failover_seen.lock().unwrap();
    assert_eq!(
        seen.last().unwrap(),
        &vec!["deepseek".to_string(), "anthropic".to_string()]
    );
    // failover_tried ora include anche google (lo scelto), per il prossimo giro.
    let tried: Vec<String> = out
        .extra
        .get("failover_tried")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert_eq!(
        tried,
        vec!["deepseek".to_string(), "anthropic".to_string(), "google".to_string()]
    );
}

#[tokio::test]
async fn signature_loop_escalation_riesegue() {
    // Signature-loop CON escalation disponibile: la catena intra-provider promuove
    // il modello e RI-ESEGUE il turno. La 2a risposta (testuale end_turn) sostituisce
    // i pending -> NON loop_detected, auto_escalations 0->1, provider_used = promosso.
    struct TwoPhaseLlm {
        same_input: Value,
        calls: std::sync::Mutex<Vec<crate::runtime::ports::LlmRequest>>,
    }
    #[async_trait::async_trait]
    impl crate::runtime::ports::LlmGateway for TwoPhaseLlm {
        async fn complete(
            &self,
            req: crate::runtime::ports::LlmRequest,
        ) -> Result<LlmResponse, crate::runtime::ports::PortError> {
            let n = {
                let mut g = self.calls.lock().unwrap();
                g.push(req.clone());
                g.len()
            };
            if n == 1 {
                // Turno primario: ripete lo stesso tool -> loop.
                Ok(LlmResponse {
                    tool_calls: vec![ToolUse {
                        id: "tc1".into(),
                        name: "read_file".into(),
                        input: self.same_input.clone(),
                    }],
                    stop_reason: Some("tool_use".into()),
                    ..Default::default()
                })
            } else {
                // Turno escalato (modello promosso): chiude con testo.
                Ok(LlmResponse {
                    content: "Risolto col modello promosso.".into(),
                    stop_reason: Some("end_turn".into()),
                    ..Default::default()
                })
            }
        }
    }
    let rc = Arc::new(StubRunControlStore::default());
    // Catena intra-provider: anthropic/claude-x -> claude-piu-capace.
    let esc = Arc::new(StubEscalationPort::with_chain(&["claude-piu-capace"]));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let same_input = json!({"path": "x"});
    let sig = build_signature("read_file", &same_input);
    let llm = Arc::new(TwoPhaseLlm {
        same_input: same_input.clone(),
        calls: std::sync::Mutex::new(vec![]),
    });
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi")],
        recent_tool_signatures: Some(vec![sig.clone(), "altro|abc".into(), sig.clone()]),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Due chiamate LLM: primaria (loop) + escalata.
    assert_eq!(llm.calls.lock().unwrap().len(), 2);
    // NON loop_detected: la 2a risposta chiude end_turn.
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.result.as_deref(), Some("Risolto col modello promosso."));
    assert!(out.pending_tool_uses.unwrap().is_empty());
    // auto_escalations incrementato.
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(1));
    // provider/model promossi nel delta (provider_used = richiesto escalato).
    assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
    assert_eq!(out.model_used.as_deref(), Some("claude-piu-capace"));
    // La porta escalation e' stata interrogata col modello corrente.
    let seen = esc.seen.lock().unwrap();
    assert_eq!(seen.last().unwrap().2.as_deref(), Some("claude-x"));
}

#[tokio::test]
async fn signature_loop_escalation_non_forza_tool_choice() {
    // Parita' col Python (py:3241-3248): nel ramo signature-loop la ri-chiamata
    // LLM escalata NON deve ereditare il `force_tool_choice` della PRIMARIA. Se la
    // primaria forza il tool (`Some(true)`, frequente proprio nei loop perche'
    // should_force_tool_choice scatta su action_oriented + tools + iter bassa),
    // l'escalata deve passare `None` (-> `auto`): forzare il tool contraddirebbe
    // l'anti_loop_hint ("cambia strategia, riassumi lo stato"), che ammette anche
    // una risposta testuale.
    struct TwoPhaseLlm {
        same_input: Value,
        calls: std::sync::Mutex<Vec<crate::runtime::ports::LlmRequest>>,
    }
    #[async_trait::async_trait]
    impl crate::runtime::ports::LlmGateway for TwoPhaseLlm {
        async fn complete(
            &self,
            req: crate::runtime::ports::LlmRequest,
        ) -> Result<LlmResponse, crate::runtime::ports::PortError> {
            let n = {
                let mut g = self.calls.lock().unwrap();
                g.push(req.clone());
                g.len()
            };
            if n == 1 {
                // Turno primario: ripete lo stesso tool -> loop.
                Ok(LlmResponse {
                    tool_calls: vec![ToolUse {
                        id: "tc1".into(),
                        name: "read_file".into(),
                        input: self.same_input.clone(),
                    }],
                    stop_reason: Some("tool_use".into()),
                    ..Default::default()
                })
            } else {
                // Turno escalato: chiude con testo (l'hint permette la risposta).
                Ok(LlmResponse {
                    content: "Stato riassunto, cambio strategia.".into(),
                    stop_reason: Some("end_turn".into()),
                    ..Default::default()
                })
            }
        }
    }
    let rc = Arc::new(StubRunControlStore::default());
    // Forcing attivo sulla PRIMARIA: enabled + stile che supporta il forcing.
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        progress_controller_enabled: true,
        tool_choice_forcing_enabled: true,
        tool_choice_style: Some("anthropic_any".to_string()),
        ..ExecutorConfig::default()
    };
    // Catena intra-provider disponibile -> l'escalation parte.
    let esc = Arc::new(StubEscalationPort::with_chain(&["claude-piu-capace"]));
    let (n, _m, _s) = node_esc(cfg, rc, esc);
    let same_input = json!({"path": "x"});
    let sig = build_signature("read_file", &same_input);
    let llm = Arc::new(TwoPhaseLlm {
        same_input: same_input.clone(),
        calls: std::sync::Mutex::new(vec![]),
    });
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi")],
        // Forcing "early action" TRANSITORIO (NON-convergenza, regola H): scatta solo
        // se il turno PRECEDENTE non ha agito (BUG-e). iter=1 + prev end_turn (non
        // ToolUse) + tool + action_oriented -> should_force_tool_choice scatta sulla 1a.
        action_oriented: Some(true),
        iterations: Some(1),
        stop_reason: Some(StopReason::EndTurn),
        recent_tool_signatures: Some(vec![sig.clone(), "altro|abc".into(), sig.clone()]),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let _ = n.run(&state, &ctx).await.expect("run");
    let calls = llm.calls.lock().unwrap();
    // Due chiamate: primaria (loop) + escalata.
    assert_eq!(calls.len(), 2);
    // La PRIMARIA forza il tool (action_oriented + tools + iter<=max + stile ok).
    assert_eq!(calls[0].force_tool_choice, Some(true));
    // L'ESCALATA NON eredita il forcing: passa None (parita' col Python).
    assert_eq!(calls[1].force_tool_choice, None);
}

#[tokio::test]
async fn signature_loop_senza_escalation_chiude_secco() {
    // Signature-loop SENZA escalation (porta vuota -> selezione None): chiude secco
    // loop_detected, NON ri-esegue, auto_escalations invariato.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // porta escalation vuota (default)
    let same_input = json!({"path": "x"});
    let sig = build_signature("read_file", &same_input);
    let llm = llm_tool_call("read_file", same_input.clone());
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi")],
        recent_tool_signatures: Some(vec![sig.clone(), "altro|abc".into(), sig.clone()]),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // UNA sola chiamata LLM (nessuna ri-esecuzione).
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    assert_eq!(out.stop_reason, Some(StopReason::LoopDetected));
    assert!(out.result.as_deref().unwrap().contains("[LOOP RILEVATO]"));
    assert!(out.pending_tool_uses.unwrap().is_empty());
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(0));
}

#[tokio::test]
async fn signature_loop_cap_escalations_chiude_secco() {
    // Anche con catena disponibile, se auto_escalations >= 3 il cap impedisce
    // l'escalation -> chiude secco loop_detected.
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_chain(&["claude-piu-capace"]));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc);
    let same_input = json!({"path": "x"});
    let sig = build_signature("read_file", &same_input);
    let llm = llm_tool_call("read_file", same_input.clone());
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert("auto_escalations".into(), json!(3));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi")],
        recent_tool_signatures: Some(vec![sig.clone(), "altro|abc".into(), sig.clone()]),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    assert_eq!(out.stop_reason, Some(StopReason::LoopDetected));
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(3));
}

#[tokio::test]
async fn g1_cap_escalation_promuove_sticky() {
    // G1 cap CON escalation disponibile (cross-provider): NON chiude secco; scrive
    // sticky al modello promosso, g1_escalated, auto_escalations+1, reroute=0, e un
    // nudge "ESEGUI subito". L'LLM NON viene chiamato (self-loop rientra).
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_cross("google", "gemini-2.5-pro"));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    // Re-entry G1: current_count 2 + 1 = 3 = cap. provider_used corrente noto.
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("avvia il servizio")],
        action_oriented: Some(true),
        stop_reason: Some(StopReason::EndTurn),
        iterations: Some(4),
        g1_reroute_count: Some(2),
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-x".into()),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_provider.as_deref(), Some("google"));
    assert_eq!(out.sticky_model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(out.g1_reroute_count, Some(0));
    assert_eq!(out.action_nudge_count, Some(0));
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(1));
    // LLM NON chiamato (escalation G1 = sticky + nudge, niente turno).
    assert!(llm.seen.lock().unwrap().is_empty());
    // La porta e' stata interrogata col modello corrente (provider_used).
    assert_eq!(esc.seen.lock().unwrap().last().unwrap().1.as_deref(), Some("anthropic"));
}

#[tokio::test]
async fn g1_cap_senza_escalation_chiude_secco() {
    // G1 cap SENZA escalation (porta vuota): cap secco g1_cap_reached come prima.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // porta vuota
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("avvia il servizio")],
        action_oriented: Some(true),
        stop_reason: Some(StopReason::EndTurn),
        iterations: Some(4),
        g1_reroute_count: Some(2),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1CapReached));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert_eq!(out.g1_reroute_count, Some(3));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn esplorazione_escalation_promuove_sticky() {
    // REGRESSIONE: loop di esplorazione a 2x soglia, asse gia' guidato, con un
    // candidato di escalation disponibile -> il nodo ESCALA il modello (sticky +
    // g1_escalated + auto_escalations+1 + esplorazione azzerata) invece di abortire.
    // Prima il ramo passava has_escalation_candidate=false hardcoded: qualunque esito
    // != Guide cadeva nell'abort e il modello non veniva mai cambiato.
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_cross("google", "gemini-2.5-pro"));
    let mut cfg = cfg_resolved();
    cfg.progress_controller_enabled = true;
    let (n, _m, _s) = node_esc(cfg, rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("sistema l'app")],
        consecutive_exploration_calls: Some(12), // 2x soglia default (6)
        progress_guided_axes: Some(vec!["exploration".into()]), // gia' guidato
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-x".into()),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_provider.as_deref(), Some("google"));
    assert_eq!(out.sticky_model.as_deref(), Some("gemini-2.5-pro"));
    assert_eq!(out.extra.get("auto_escalations").and_then(Value::as_i64), Some(1));
    assert_eq!(out.consecutive_exploration_calls, Some(0));
    // LLM NON chiamato: escalation = sticky + nudge, il self-loop rientra.
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn esplorazione_senza_candidato_aborta() {
    // Contro-prova: loop di esplorazione a 2x soglia, asse gia' guidato, SENZA
    // candidato di escalation (porta vuota) -> abort coordinato verso final_gate,
    // come prima del fix.
    let rc = Arc::new(StubRunControlStore::default());
    let mut cfg = cfg_resolved();
    cfg.progress_controller_enabled = true;
    let (n, _m, _s) = node(cfg, rc); // porta escalation vuota
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("sistema l'app")],
        consecutive_exploration_calls: Some(12),
        progress_guided_axes: Some(vec!["exploration".into()]),
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-x".into()),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::LoopAbort));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert!(llm.seen.lock().unwrap().is_empty());
}

// ──────────────────────────────────────────────────────────────────────────
//  PR-J2: 4 rami ON/seedati (billing fail-fast, next_actions, unfulfilled, upscale)
// ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cap_assoluto_iterazioni_chiude() {
    // REGRESSIONE loop G1 infinito: a iters_in >= iteration_cap il nodo chiude
    // deterministicamente (EndTurn, forced_close_unverified) SENZA chiamare l'LLM,
    // anche se il modello ignora il forcing e continua a descrivere.
    let rc = Arc::new(StubRunControlStore::default());
    let mut cfg = cfg_resolved();
    cfg.iteration_cap = 10;
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("task complesso")],
        iterations: Some(10), // == iteration_cap
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert!(out.result.as_deref().unwrap().contains("massimo di iterazioni"));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn billing_fail_fast_chiude_loop_abort() {
    // Soglia esplorazione raggiunta + il PROVIDER IN USO in cooldown billing ->
    // chiusura onesta loop_abort PRIMA della chiamata LLM (py:2072-2092 + fix
    // provider-in-uso: cfr. billing_fail_fast_provider_corrente_valido).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::with_exhausted(&["anthropic", "openai"]));
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("task complesso")],
        consecutive_exploration_calls: Some(6), // == soglia default 6
        tools_json: Some(vec![json!({"name": "read_file"})]),
        // Provider IN USO esausto: il fail-fast scatta solo se il provider corrente
        // e' tra gli esausti (fix: non incolpare i crediti se il run usa un
        // provider valido). Qui anthropic e' in cooldown -> abort onesto.
        provider_used: Some("anthropic".into()),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::LoopAbort));
    assert!(out.result.as_deref().unwrap().contains("cooldown"));
    assert!(out.result.as_deref().unwrap().contains("anthropic, openai"));
    // LLM NON chiamato (fail-fast pre-modello).
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn billing_nessun_esausto_prosegue() {
    // Soglia raggiunta MA nessun provider esausto -> NON fail-fast, prosegue.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default()); // vuoto
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    // Testuale -> end_turn (oltre soglia il nudge anti-esplorazione si inietta ma
    // non chiude: la chiamata LLM avviene).
    let llm = Arc::new(StubLlmGateway::with_text("Ecco il dato."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        action_oriented: Some(false),
        consecutive_exploration_calls: Some(6),
        exploration_nudge_sent: Some(true), // gia' inviato: evita nudge
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(out.stop_reason, Some(StopReason::LoopAbort));
    // LLM chiamato (nessun fail-fast).
    assert!(!llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn next_actions_rimuove_blocco_e_deriva() {
    // A end_turn: il blocco <suggested_actions> e' SEMPRE rimosso dal result
    // visibile (punto unico deterministico) + meta_step next_actions emesso se la
    // derivazione trova scelte (py:3379-3402).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::with_choices(&[
        ("Aggiungi form", "Aggiungi un form di contatto alla pagina"),
    ]));
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, meta) = node_ports(cfg_resolved(), rc, next_actions.clone(), billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text(
        "Ecco la home page.\n<suggested_actions>\n[{\"label\":\"x\",\"prompt\":\"y\"}]\n</suggested_actions>",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea la home")],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Blocco rimosso dal testo visibile.
    let result = out.result.as_deref().unwrap();
    assert!(!result.to_lowercase().contains("suggested_actions"));
    assert!(result.contains("Ecco la home page."));
    // meta_step next_actions persistito (la derivazione ha trovato scelte).
    let metas = meta.meta_steps.lock().unwrap();
    assert!(metas.iter().any(|m| m.get("kind").and_then(Value::as_str) == Some("next_actions")));
    // La porta ha ricevuto il testo GIA' ripulito.
    let seen = next_actions.seen.lock().unwrap();
    assert!(seen.last().map(|s| !s.contains("suggested_actions")).unwrap_or(false));
}

#[tokio::test]
async fn next_actions_derive_fallita_blocco_comunque_rimosso() {
    // Derivazione fallita (best-effort): il blocco <suggested_actions> resta
    // rimosso dal testo visibile (punto unico deterministico), nessun meta_step.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::failing());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, meta) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text(
        "Risposta finale.\n<suggested_actions>[{\"label\":\"x\"}]</suggested_actions>",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("fai")],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run NON deve abortire su derive fallita");
    let out = apply(state, delta);
    let result = out.result.as_deref().unwrap();
    assert!(!result.to_lowercase().contains("suggested_actions"));
    assert!(result.contains("Risposta finale."));
    // Nessun meta_step next_actions (derive fallita -> nessuna scelta).
    let metas = meta.meta_steps.lock().unwrap();
    assert!(!metas.iter().any(|m| m.get("kind").and_then(Value::as_str) == Some("next_actions")));
}

#[tokio::test]
async fn unfulfilled_report_sostituisce_in_confirm() {
    // Modalita' confirm (assente == confirm) + intento NON compiuto + turno NON
    // action-oriented -> il result e' sostituito dal resoconto onesto (py:3404-3429).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    // Promessa monca (testo che il detector unfulfilled riconosce come futuro 1p).
    let llm = Arc::new(StubLlmGateway::with_text(
        "Ora attendo che il servizio parta e poi verifichero' il risultato.",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("sistema il login"),
            ai_tool("write_file", json!({"path": "login.ts"})),
        ],
        action_oriented: Some(false),
        // automation_mode assente -> Python default "confirm" -> sostituisce.
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    let result = out.result.as_deref().unwrap();
    assert!(result.contains("resoconto onesto"));
    assert!(result.contains("NON e' completato"));
    // Il resoconto cita il file toccato dalla history.
    assert!(result.contains("login.ts"));
}

#[tokio::test]
async fn unfulfilled_report_non_sostituisce_in_automatic() {
    // Modalita' automatic: il re-entry G1 fa agire il modello; qui NON si sostituisce.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    let promessa = "Ora attendo che il servizio parta e poi verifichero' il risultato.";
    let llm = Arc::new(StubLlmGateway::with_text(promessa));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("sistema il login")],
        action_oriented: Some(false),
        automation_mode: Some(crate::state::AutomationMode::Automatic),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Result invariato (nessun resoconto sostituito).
    assert_eq!(out.result.as_deref(), Some(promessa));
}

#[tokio::test]
async fn unfulfilled_report_segue_il_lessicale_ignorando_closure_fulfilled() {
    // DISTINZIONE LOAD-BEARING (regola L): il ramo report POST end_turn usa SOLO
    // il detector lessicale (1:1 col Python :3413, che NON consulta closure_verdict
    // in questo ramo), a differenza del ramo G1 (closure-first, py:1913-1917).
    // Setup adversariale: closure_verdict = fulfilled (compiuto) MA il testo e' una
    // promessa monca che il detector lessicale riconosce come NON compiuta. Il ramo
    // report DEVE seguire il lessicale -> SOSTITUIRE col resoconto onesto, ignorando
    // il verdetto closure fulfilled. Con la vecchia logica closure-first il ramo
    // avrebbe visto fulfilled e NON avrebbe sostituito: questo test la cattura.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text(
        "Ora attendo che il servizio parta e poi verifichero' il risultato.",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("sistema il login"),
            ai_tool("write_file", json!({"path": "login.ts"})),
        ],
        action_oriented: Some(false),
        // closure_verdict fulfilled=true: il ramo report (lessicale-puro) lo IGNORA.
        closure_verdict: Some(json!({"fulfilled": true})),
        // automation_mode assente -> "confirm" -> ramo report attivo.
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    let result = out.result.as_deref().unwrap();
    // SOSTITUITO dal resoconto onesto: il lessicale "unfulfilled" ha prevalso sul
    // closure "fulfilled". La vecchia logica closure-first avrebbe lasciato il testo.
    assert!(result.contains("resoconto onesto"));
    assert!(result.contains("NON e' completato"));
    assert!(result.contains("login.ts"));
}

#[tokio::test]
async fn g1_resta_closure_first_non_conta_se_closure_fulfilled() {
    // CONTROPROVA della distinzione: il ramo G1-conteggio e' closure-first 1:1 col
    // Python (py:1913-1917, mig 0422). In una re-entry G1 autentica (prev=end_turn,
    // iter=1, no pending) con turno NON action-oriented, il conteggio dipende SOLO
    // dal segnale unfulfilled. Qui closure_verdict = fulfilled -> unfulfilled_for_g1
    // = false -> il reroute G1 NON viene contato (resta 0). Se G1 usasse il
    // lessicale (come il ramo report), il testo-promessa lo avrebbe contato a 1.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    // Testo-promessa monca: il detector lessicale direbbe unfulfilled.
    let llm = Arc::new(StubLlmGateway::with_text(
        "Ora attendo che il servizio parta e poi verifichero' il risultato.",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        // Re-entry G1: turno precedente chiuso end_turn senza agire, l'ultima
        // assistant e' una promessa monca (lessicale unfulfilled).
        messages: vec![
            human("sistema il login"),
            Message::Ai {
                content: MessageContent::text(
                    "Inizio verificando il login e poi sistemo il resto.",
                ),
                tool_calls: vec![],
                reasoning: None,
            },
        ],
        stop_reason: Some(StopReason::EndTurn),
        iterations: Some(1),
        action_oriented: Some(false),
        g1_reroute_count: Some(0),
        // closure_verdict fulfilled=true: il ramo G1 (closure-first) NON conta.
        closure_verdict: Some(json!({"fulfilled": true})),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Closure-first: fulfilled -> unfulfilled_for_g1=false -> reroute NON contato.
    assert_eq!(out.g1_reroute_count, Some(0));
}

#[tokio::test]
async fn smart_upscale_promuove_modello() {
    // Contesto stimato >= 90% del window -> la porta promuove a un modello con
    // window maggiore PRIMA della chiamata LLM; il provider/model del turno cambia
    // (py:2812-2830). Il delta riporta il provider/model promosso.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    // Window piccola (200 token) -> con un minimo di history la stima supera 180.
    let upscale = Arc::new(StubModelUpscalePort::promoting(200, "google", "gemini-2.5-pro"));
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        upscale_enabled: true,
        ..ExecutorConfig::default()
    };
    let (n, _m) = node_ports(cfg, rc, next_actions, billing, upscale.clone());
    let llm = Arc::new(StubLlmGateway::with_text("Risposta."));
    let ctx = ctx_with(llm.clone(), false);
    // History abbastanza grande da superare 90% di 200 token (~180): un testo lungo.
    let big = "x".repeat(2000);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human(&big)],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Provider/model promossi nel delta.
    assert_eq!(out.provider_used.as_deref(), Some("google"));
    assert_eq!(out.model_used.as_deref(), Some("gemini-2.5-pro"));
    // La chiamata LLM ha usato il modello promosso.
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(req.model, "gemini-2.5-pro");
    assert_eq!(req.provider, "google");
    // La porta e' stata interrogata per la selezione col modello corrente.
    assert!(!upscale.selected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn smart_upscale_sotto_soglia_non_promuove() {
    // Contesto piccolo (< 90% del window): nessun upscale, modello invariato.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    // Window enorme -> la stima non la raggiunge mai.
    let upscale = Arc::new(StubModelUpscalePort::promoting(1_000_000, "google", "gemini-2.5-pro"));
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        upscale_enabled: true,
        ..ExecutorConfig::default()
    };
    let (n, _m) = node_ports(cfg, rc, next_actions, billing, upscale.clone());
    let llm = Arc::new(StubLlmGateway::with_text("Risposta."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("ciao")],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Modello invariato (no upscale).
    assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
    assert_eq!(out.model_used.as_deref(), Some("claude-x"));
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(req.model, "claude-x");
    // select_upscale_model NON chiamata (gate should_upscale falso).
    assert!(upscale.selected.lock().unwrap().is_empty());
}

// ── rolling-summary (intervento 3): aggancio al cambio-fase ───────────────────

/// Messaggio assistant testuale (helper locale per i test rolling-summary).
fn ai(text: &str) -> Message {
    Message::Ai {
        content: MessageContent::text(text),
        tool_calls: vec![],
        reasoning: None,
    }
}

/// Config che attiva il rolling-summary con keep_recent=2 (provider/model risolti).
fn cfg_rolling() -> ExecutorConfig {
    ExecutorConfig {
        rolling_summary_enabled: true,
        rolling_keep_recent: 2,
        ..cfg_resolved()
    }
}

/// Stato a 6 messaggi testuali, iter=5 (cambio-fase 0->1 coi boundaries default
/// [5,10,20,50]). Risposta testuale dal gateway (nessun gate di testa scatta).
fn state_cambio_fase() -> AgentState {
    AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("domanda 1"),
            ai("risposta 1"),
            human("domanda 2"),
            ai("risposta 2"),
            human("domanda 3"),
            ai("risposta 3"),
        ],
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    }
}

/// Al cambio-fase con un `StubSummaryStore::with_summary(...)` la history viene
/// COLLASSATA: il summarizer riceve il prefisso serializzato e la richiesta LLM
/// del turno porta MENO messaggi (1 summary + keep_recent) rispetto agli originali.
#[tokio::test]
async fn rolling_summary_collassa_la_history_al_cambio_fase() {
    let rc = Arc::new(StubRunControlStore::default());
    let summary = Arc::new(StubSummaryStore::with_summary(
        "L'utente ha posto 3 domande, tutte gia' risposte.",
    ));
    let n = node_summary(cfg_rolling(), rc, summary.clone());
    let llm = Arc::new(StubLlmGateway::with_text("Procedo."));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_cambio_fase();
    let _ = n.run(&state, &ctx).await.expect("run");

    // Il summarizer e' stato chiamato col prefisso serializzato (6-2=4 messaggi).
    let seen = summary.summarize_seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "summarize chiamato una volta al cambio-fase");
    assert!(seen[0].contains("[human]: domanda 1"));
    drop(seen);

    // La richiesta LLM porta la history COLLASSATA: 1 summary + 2 recenti = 3
    // (contro i 6 originali).
    let req = llm.seen.lock().unwrap().last().cloned().expect("una richiesta LLM");
    assert_eq!(
        req.messages.len(),
        3,
        "history collassata a 1 summary + keep_recent (2)"
    );
    // Il primo messaggio wire e' il riassunto (ruolo user con marker RIASSUNTO).
    assert_eq!(req.messages[0].role, "user");
    let first = req.messages[0].content.as_str().unwrap_or_default();
    assert!(first.contains("[RIASSUNTO conversazione precedente]"));
}

/// Con lo stub di DEFAULT (`StubSummaryStore::default()`, nessun summary -> il
/// summarize ritorna `PortError`) la history NON cambia numero di messaggi: il
/// nodo degrada best-effort (un guasto del summarizer non riduce ne' rompe nulla).
#[tokio::test]
async fn rolling_summary_degrado_best_effort_history_invariata() {
    let rc = Arc::new(StubRunControlStore::default());
    // node_summary col default: summarize -> PortError (degrado).
    let n = node_summary(cfg_rolling(), rc, Arc::new(StubSummaryStore::default()));
    let llm = Arc::new(StubLlmGateway::with_text("Procedo."));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_cambio_fase();
    let _ = n.run(&state, &ctx).await.expect("run");

    // Nessun collasso: i 6 messaggi originali restano (compress non riduce il
    // NUMERO di messaggi, solo i contenuti dei tool_result; qui sono testuali).
    let req = llm.seen.lock().unwrap().last().cloned().expect("una richiesta LLM");
    assert_eq!(
        req.messages.len(),
        6,
        "degrado best-effort: history invariata quando il summarizer fallisce"
    );
}

/// Golden di parita' 1:1 vs Python per la LOGICA DETERMINISTICA del singolo turno
/// (gate testa, ordine nudge, risoluzione provider). Carica
/// `/tmp/golden_executor_node.json` (vedi `gen_golden_executor_node.py`). Riusa i
/// punti unici del nodo (`head_gate`, `pc::decide`, `resolve_provider_model`):
/// la stessa logica del `run`, esercitata in isolamento.
#[cfg(test)]
mod golden {
    use super::*;
    use crate::decisions::progress_controller::{self as pc, Action, ProgressSignals};
    use crate::nodes::executor::{head_gate, resolve_provider_model, HeadGate, ProviderResolution};
    use serde::Deserialize;
    use std::collections::HashSet;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: Value,
        output: Value,
    }

    /// Stringa stabile di `HeadGate` per il confronto col golden Python.
    fn head_gate_str(h: HeadGate) -> &'static str {
        match h {
            HeadGate::Superseded => "superseded",
            HeadGate::DeclaredDone => "end_turn",
            HeadGate::G1Cap => "g1_cap_reached",
            HeadGate::Proceed => "proceed",
        }
    }

    /// Stringa stabile dell'`Action` del progress_controller (== serde rename).
    fn action_str(a: Action) -> &'static str {
        match a {
            Action::Proceed => "proceed",
            Action::Guide => "guide",
            Action::ForceDiagnose => "force_diagnose",
            Action::Escalate => "escalate",
            Action::Abort => "abort",
        }
    }

    /// Replica l'ORDINE di valutazione dei nudge del nodo (1:1) usando il punto
    /// unico `pc::decide`. Ritorna `{axis, action}` del primo asse che scatta.
    #[allow(clippy::too_many_arguments)]
    fn nudge_order(input: &Value) -> Value {
        let g = |k: &str| input.get(k).and_then(Value::as_i64).unwrap_or(0);
        let b = |k: &str| input.get(k).and_then(Value::as_bool).unwrap_or(false);
        let exploration_count = g("exploration_count");
        let exploration_threshold = g("exploration_threshold");
        let repeat_cmd_count = g("repeat_cmd_count");
        let repeated_action_threshold = g("repeated_action_threshold");
        let reallocation_count = g("reallocation_count");
        let reallocation_threshold = g("reallocation_threshold");
        let g1_descriptive = b("g1_descriptive");
        let progress_on = b("progress_on");
        let already_guided: HashSet<String> = input
            .get("already_guided")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let repeated_action: Option<(String, i64)> = input
            .get("repeated_action")
            .and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    let arr = v.as_array()?;
                    Some((arr.first()?.as_str()?.to_string(), arr.get(1)?.as_i64()?))
                }
            });

        // 1) esplorazione 2x soglia (controller ON).
        if progress_on && exploration_count >= 2 * exploration_threshold {
            let dec = pc::decide(&ProgressSignals {
                exploration_count,
                exploration_threshold,
                already_guided: already_guided.clone(),
                has_escalation_candidate: false,
                ..Default::default()
            });
            return json!({"axis": "exploration", "action": action_str(dec.action)});
        }
        // 2) comando ripetuto fallito (>=3): nudge non-controller.
        if repeat_cmd_count >= 3 {
            return json!({"axis": "repeated_command", "action": "guide"});
        }
        // 3) repeated_action (controller ON).
        if progress_on {
            if let Some((label, count)) = &repeated_action {
                if !label.is_empty() && *count >= repeated_action_threshold {
                    let dec = pc::decide(&ProgressSignals {
                        repeated_action: Some((label.clone(), *count)),
                        already_guided: already_guided.clone(),
                        has_escalation_candidate: false,
                        ..Default::default()
                    });
                    return json!({"axis": "repeated_action", "action": action_str(dec.action)});
                }
            }
        }
        // 4) resource_reallocation (controller ON).
        if progress_on && reallocation_count >= reallocation_threshold {
            let dec = pc::decide(&ProgressSignals {
                reallocation_count,
                reallocation_threshold,
                already_guided: already_guided.clone(),
                has_escalation_candidate: false,
                ..Default::default()
            });
            return json!({"axis": "resource_reallocation", "action": action_str(dec.action)});
        }
        // 5) g1_descriptive (controller ON).
        if progress_on && g1_descriptive {
            let dec = pc::decide(&ProgressSignals {
                g1_over_cap: true,
                already_guided: already_guided.clone(),
                ..Default::default()
            });
            return json!({"axis": "g1_descriptive", "action": action_str(dec.action)});
        }
        json!({"axis": "none", "action": "proceed"})
    }

    fn resolve_case(input: &Value) -> Value {
        let s = |k: &str| input.get(k).and_then(Value::as_str);
        let res = resolve_provider_model(
            s("sticky_provider"),
            s("sticky_model"),
            s("provider_override"),
            s("model_override"),
            input.get("routing_provider").and_then(Value::as_str).unwrap_or(""),
            input.get("routing_model").and_then(Value::as_str).unwrap_or(""),
        );
        match res {
            ProviderResolution::Resolved(p, m) => {
                json!({"provider": p, "model": m, "no_provider": false})
            }
            ProviderResolution::NoProvider(p) => {
                // Il Python espone provider/model risolti anche nel ramo no_provider.
                let m = input.get("routing_model").and_then(Value::as_str).unwrap_or("");
                json!({"provider": p, "model": m, "no_provider": true})
            }
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_executor_node.json generato da gen_golden_executor_node.py"]
    fn golden_executor_node() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_executor_node.json",
            "gen_golden_executor_node.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 20, "attesi >= 20 casi, trovati {}", cases.len());
        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.group.as_str() {
                "head_gate" => {
                    let inp = &c.input;
                    let h = head_gate(
                        inp.get("superseded").and_then(Value::as_bool).unwrap_or(false),
                        inp.get("declared_done").and_then(Value::as_bool).unwrap_or(false),
                        inp.get("declared_done_count").and_then(Value::as_i64).unwrap_or(0),
                        inp.get("g1_cap_reached").and_then(Value::as_bool).unwrap_or(false),
                    );
                    Value::String(head_gate_str(h).to_string())
                }
                "nudge_order" => nudge_order(&c.input),
                "resolve_provider" => resolve_case(&c.input),
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        println!("golden executor_node: {checked} casi verificati, tutti verdi");
    }
}

/// Golden di parita' 1:1 vs Python per la PARTE DETERMINISTICA dei 4 rami PR-J2
/// (unfulfilled-report, rimozione `<suggested_actions>`, messaggio billing,
/// decisione smart-upscale). Carica `/tmp/golden_executor_end_turn.json` (vedi
/// `gen_golden_executor_end_turn.py`). Chiama le STESSE funzioni pure del nodo
/// (`decisions::end_turn`), esercitate in isolamento.
#[cfg(test)]
mod golden_end_turn {
    use crate::decisions::end_turn::{
        billing_fail_fast_message, build_unfulfilled_report, should_upscale,
        strip_suggested_actions, upscale_required_tokens,
    };
    use crate::state::{ContentBlock, Message, MessageContent};
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: Value,
        output: Value,
    }

    /// Costruisce i [`Message`] dai blocchi `{"content": [{type:tool_use,...}]}`
    /// del golden (forma history Python). I blocchi non-tool_use sono ignorati
    /// dal report; qui basta ricostruire i `ContentBlock::ToolUse`.
    fn messages_from_input(arr: &Value) -> Vec<Message> {
        arr.as_array()
            .map(|msgs| {
                msgs.iter()
                    .filter_map(|m| {
                        let content = m.get("content")?.as_array()?;
                        let blocks: Vec<ContentBlock> = content
                            .iter()
                            .filter_map(|b| {
                                if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                                    Some(ContentBlock::ToolUse {
                                        id: "g".into(),
                                        name: b
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or("tool")
                                            .to_string(),
                                        input: b.get("input").cloned().unwrap_or(Value::Null),
                                    })
                                } else {
                                    None
                                }
                            })
                            .collect();
                        Some(Message::Ai {
                            content: MessageContent::Blocks(blocks),
                            tool_calls: vec![],
                            reasoning: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    #[ignore = "richiede /tmp/golden_executor_end_turn.json generato da gen_golden_executor_end_turn.py"]
    fn golden_executor_end_turn() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_executor_end_turn.json",
            "gen_golden_executor_end_turn.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 15, "attesi >= 15 casi, trovati {}", cases.len());
        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.group.as_str() {
                "unfulfilled_report" => {
                    let rt = c.input.get("result_text").and_then(Value::as_str);
                    let msgs = messages_from_input(c.input.get("messages").unwrap_or(&Value::Null));
                    Value::String(build_unfulfilled_report(rt, &msgs))
                }
                "strip_suggested_actions" => {
                    let text = c.input.get("text").and_then(Value::as_str).unwrap_or("");
                    Value::String(strip_suggested_actions(text))
                }
                "billing_fail_fast" => {
                    let cnt = c.input.get("exploration_count").and_then(Value::as_i64).unwrap_or(0);
                    let thr = c
                        .input
                        .get("exploration_threshold")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let ex: Vec<String> = c
                        .input
                        .get("exhausted")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    // Il golden (parita' storica) non porta current_provider: passa
                    // il primo esausto, cosi' la semantica 3-param e' replicata
                    // (esausti non vuoti -> current in esausti -> Some). Il fix
                    // provider-in-uso e' coperto dagli unit test dedicati.
                    let cur = ex.first().map(String::as_str).unwrap_or("");
                    match billing_fail_fast_message(cnt, thr, &ex, cur) {
                        Some(s) => Value::String(s),
                        None => Value::Null,
                    }
                }
                "should_upscale" => {
                    let en = c.input.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                    let est = c.input.get("est_tokens").and_then(Value::as_i64).unwrap_or(0);
                    let win = c.input.get("current_window").and_then(Value::as_i64).unwrap_or(0);
                    Value::Bool(should_upscale(en, est, win))
                }
                "upscale_required" => {
                    let est = c.input.get("est_tokens").and_then(Value::as_i64).unwrap_or(0);
                    let ov = c.input.get("overhead").and_then(Value::as_f64).unwrap_or(1.0);
                    Value::from(upscale_required_tokens(est, ov))
                }
                other => panic!("gruppo golden sconosciuto: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA {} / {}:\n  rust   = {}\n  python = {}",
                c.group, c.case_id, got, c.output
            );
            checked += 1;
        }
        println!("golden executor_end_turn: {checked} casi verificati, tutti verdi");
    }
}

// ── continuita' tool multi-turn: Message -> HistoryMessage -> LlmMessage ──
// (bug 2026-06-26: il path perdeva tool_use/tool_result -> Anthropic HTTP 400)
#[cfg(test)]
mod multi_turn_wire {
    use super::{ai_tool, human, tool_msg_err};
    use crate::decisions::context_reduction::HistoryMessage;
    use crate::nodes::executor::{history_to_llm_messages, message_to_history};
    use crate::state::{ContentBlock, Message, MessageContent};
    use serde_json::{json, Value};

    /// Costruisce un `Message::Human` che trasporta un blocco `tool_result` (forma
    /// reale prodotta dal `tool_dispatch`: HumanMessage + anthropic_content).
    fn human_tool_result(tool_use_id: &str, result: &str) -> Message {
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: Value::String(result.into()),
                is_error: false,
                exit_code: Some(0),
            }]),
        }
    }

    #[test]
    fn multi_turn_assistant_tooluse_e_tool_result_preservati_nel_wire() {
        // Sequenza reale: [Human, Ai(tool_use id=c1 nei blocchi, tool_calls VUOTO),
        // Human(tool_result tool_use_id=c1)]. `ai_tool` riproduce il Bug A
        // (tool_use nei blocchi, tool_calls vec![]); `human_tool_result` la forma
        // del tool_dispatch.
        let messages = [
            human("leggi a.rs"),
            ai_tool("read_file", json!({"path": "a.rs"})),
            human_tool_result("c1", "contenuto di a.rs"),
        ];
        let hist: Vec<HistoryMessage> = messages.iter().map(message_to_history).collect();
        let wire = history_to_llm_messages(&hist);

        // 3 messaggi wire: user, assistant(tool_calls), tool(result).
        assert_eq!(wire.len(), 3, "wire = {wire:?}");

        // [0] user.
        assert_eq!(wire[0].role, "user");
        assert!(wire[0].tool_calls.is_none());

        // [1] assistant con tool_calls NON vuoto contenente id=c1 (NON appiattito
        // nel content): cosi' il server produce un block tool_use, non una stringa.
        assert_eq!(wire[1].role, "assistant");
        let calls = wire[1].tool_calls.as_ref().expect("assistant deve avere tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].input["path"], "a.rs");

        // [2] role "tool" con tool_call_id=c1 (round-trip): il server lo trasforma
        // in block tool_result con tool_use_id=c1.
        assert_eq!(wire[2].role, "tool");
        assert_eq!(wire[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(wire[2].content, json!("contenuto di a.rs"));

        // COERENZA id: tool_use dell'assistant == tool_call_id del messaggio tool
        // (la coppia che il server `to_anthropic_messages` riconosce -> no HTTP 400).
        assert_eq!(calls[0].id, wire[2].tool_call_id.clone().unwrap());
    }

    #[test]
    fn multi_turn_message_tool_esplicito_preserva_id() {
        // Forma alternativa: tool_result come `Message::Tool` esplicito (id su campo).
        let messages = [
            human("scrivi"),
            ai_tool("write_file", json!({"path": "b.rs"})),
            tool_msg_err("ok scritto"), // Message::Tool { tool_call_id: "c1", ... }
        ];
        let hist: Vec<HistoryMessage> = messages.iter().map(message_to_history).collect();
        let wire = history_to_llm_messages(&hist);
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[1].role, "assistant");
        assert_eq!(wire[1].tool_calls.as_ref().unwrap()[0].id, "c1");
        assert_eq!(wire[2].role, "tool");
        assert_eq!(wire[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(wire[2].content, json!("ok scritto"));
    }

    #[test]
    fn turno_testuale_resta_user_assistant_senza_tool() {
        // Nessun tool: forma minimale role/content, tool_calls/tool_call_id None.
        let messages = [
            human("ciao"),
            Message::Ai {
                content: MessageContent::text("risposta"),
                tool_calls: vec![],
                reasoning: None,
            },
        ];
        let hist: Vec<HistoryMessage> = messages.iter().map(message_to_history).collect();
        let wire = history_to_llm_messages(&hist);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0].role, "user");
        assert_eq!(wire[0].content, json!("ciao"));
        assert!(wire[0].tool_calls.is_none() && wire[0].tool_call_id.is_none());
        assert_eq!(wire[1].role, "assistant");
        assert_eq!(wire[1].content, json!("risposta"));
        assert!(wire[1].tool_calls.is_none() && wire[1].tool_call_id.is_none());
    }
}
