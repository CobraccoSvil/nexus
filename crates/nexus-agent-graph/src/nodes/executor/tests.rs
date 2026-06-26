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
use crate::runtime::ports::{LlmResponse, LlmUsage};
use crate::runtime::test_doubles::{
    NullEventSink, StubAgentStepStore, StubEscalationPort, StubLlmGateway, StubMetaStepStore,
    StubRunControlStore, StubToolExecutor,
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
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://test:test@127.0.0.1:1/test")
        .expect("connect_lazy");
    AgentNodeCtx {
        db: pool,
        llm,
        tools: Arc::new(StubToolExecutor::with_success(json!("{}"))),
        emit: Arc::new(NullEventSink),
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
    let n = ExecutorNode::new(cfg, rc, meta.clone(), steps.clone(), esc);
    (n, meta, steps)
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
    // progress_controller ON, soglia repeated_action 2, asse gia' guidato ->
    // ABORT (niente escalation candidate).
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    // write_file ripetuto 2 volte SENZA mai riuscire (stallo), asse gia' guidato.
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
        iterations: Some(0),
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
        // action_oriented + iter bassa -> should_force_tool_choice scatta sulla 1a.
        action_oriented: Some(true),
        iterations: Some(0),
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
