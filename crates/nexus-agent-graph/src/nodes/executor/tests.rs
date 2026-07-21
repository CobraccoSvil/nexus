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
use crate::runtime::ports::{LlmResponse, LlmUsage, PortError, SseEvent};
use crate::runtime::test_doubles::{
    NullEventSink, RecordingEventSink, StubAgentStepStore, StubBillingCooldownPort,
    StubEmbeddingStore, StubEscalationPort, StubLlmGateway, StubMetaStepStore,
    StubModelUpscalePort, StubNextActionsDeriver, StubRunControlStore, StubSummaryStore,
    StubToolExecutor,
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
        isolation_available: false,
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
        advisory_gate: None,
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
            thought_signature: None,
        }]),
        tool_calls: vec![],
        reasoning: None,
        thinking_signature: None,
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
async fn declared_done_non_chiude_durante_correzione_final_gate() {
    // Dopo un final_gate FAILED (final_gate_cycle>0) la chiusura d'autorita'
    // su done>=3 NON deve scattare: il modello deve poter applicare il fix
    // richiesto dal gate (incidente run 97cbaa45).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("applico il fix richiesto dal gate"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("crea x"),
            human("<final_gate_failed> build rotta"),
        ],
        declared_outcome: Some(json!({"outcome": "done", "summary": "fatto tutto"})),
        declared_done_count: Some(3),
        final_gate_cycle: Some(1),
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(
        out.result.as_deref(),
        Some("fatto tutto"),
        "non deve chiudere col summary stantio"
    );
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "il turno di correzione prosegue con chiamata LLM"
    );
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
    assert!(
        has_nudge,
        "il nudge anti-esplorazione deve essere nel prompt"
    );
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
    assert!(
        has_cmd_nudge,
        "il nudge anti-loop-comando deve essere nel prompt"
    );
}

#[tokio::test]
async fn repeated_action_abort_chiude() {
    let rc = Arc::new(StubRunControlStore::default());
    // progress_controller ON, soglia repeated_action 2, asse gia' guidato E
    // gia' diagnosticato -> chiusura (niente escalation candidate). Per un write
    // FALLITO cio' avviene solo DOPO che l'estratto e' stato sfruttato (GUIDE +
    // FORCE_DIAGNOSE gia' emessi): qui simuliamo entrambi gia' passati.
    // Regola M: il write FALLISCE per segnale STRUTTURATO (is_error), quindi la
    // chiusura e' ONESTA (nomina il fallimento reale, EndTurn -> final_gate), NON
    // il vecchio "ESITO: non completato / loop a vuoto".
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
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Chiusura ONESTA su fallimento REALE (regola M): EndTurn verso il final_gate,
    // messaggio che nomina il fallimento reale, non "ESITO: non completato".
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    let result = out.result.as_deref().unwrap();
    assert!(
        result.contains("continua a fallire con un errore REALE"),
        "atteso messaggio onesto di fallimento reale, ottenuto: {result}"
    );
    assert!(!result.contains("ESITO: non completato"));
    // LLM non chiamato (chiusura prima del modello).
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
    let req = llm
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("llm chiamato");
    // Il prompt contiene il nudge SPECIFICO edit-fallito.
    let has_specific_nudge = req
        .messages
        .iter()
        .any(|m| matches!(&m.content, Value::String(s) if s.contains("old_string ESATTO")));
    assert!(
        has_specific_nudge,
        "atteso il nudge specifico edit-fallito nel prompt"
    );
}

/// Messaggi con lo STESSO edit_file fallito 2 volte (signature identica).
fn edit_fallito_x2() -> Vec<Message> {
    let mut messages = vec![human("modifica il file")];
    for _ in 0..2 {
        messages.push(ai_tool(
            "edit_file",
            json!({"path": "src/lib.rs", "old_string": "fn alpha() {}"}),
        ));
        messages.push(Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("old_string non trovato".into()),
                is_error: true,
                exit_code: None,
            }]),
        });
    }
    messages
}

#[tokio::test]
async fn repeated_action_escalate_promuove_sticky_e_scrive_floor() {
    // REGRESSIONE run c4fa064b (parte 1): edit fallito ripetuto, asse gia'
    // guidato E diagnosticato, CON candidato di escalation -> il nodo ESCALA
    // (sticky promosso + G1Escalated) e scrive la grazia post-escalation
    // (repeat_scan_floor = lunghezza del prefisso messaggi persistito), invece
    // di chiudere con la diagnosi come risposta finale.
    let rc = Arc::new(StubRunControlStore::default());
    // FIX-A (scale-controller): la catena porta il tier 'heavy' -> il pick lo
    // propaga e il call-site scrive `current_tier` nel delta.
    let esc = Arc::new(StubEscalationPort::with_chain_tier(
        &["claude-piu-capace"],
        "heavy",
    ));
    let (n, meta, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let messages = edit_fallito_x2();
    let msg_len = messages.len() as i64;
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    // Il meta-step "escalation" porta il modello CADUTO (from_*), non solo il
    // to_* (bug della card "Mistral / ?": il payload costruito a mano ometteva
    // from_model, il frontend ripiegava su prev.model assente sul 1o segmento).
    // La coppia corrente = risoluzione del turno (qui il routing anthropic/claude-x).
    let metas = meta.meta_steps.lock().unwrap();
    let esc_payload = metas
        .iter()
        .find(|m| m.get("kind").and_then(Value::as_str) == Some("escalation"))
        .and_then(|m| m.get("payload"))
        .expect("meta-step escalation presente");
    assert_eq!(
        esc_payload.get("from_provider").and_then(Value::as_str),
        Some("anthropic")
    );
    assert_eq!(
        esc_payload.get("from_model").and_then(Value::as_str),
        Some("claude-x")
    );
    assert_eq!(esc_payload.get("reason").and_then(Value::as_str), Some("repeated_action"));
    drop(metas);
    assert_eq!(out.sticky_provider.as_deref(), Some("anthropic"));
    assert_eq!(out.sticky_model.as_deref(), Some("claude-piu-capace"));
    // FIX-A: current_tier scritto col performance_tier del modello promosso.
    assert_eq!(out.current_tier.as_deref(), Some("heavy"));
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        out.extra.get("repeat_scan_floor").and_then(Value::as_i64),
        Some(msg_len)
    );
    // LLM NON chiamato in questo passaggio: il self-loop rientra col promosso.
    assert!(llm.seen.lock().unwrap().is_empty());
    // La coppia corrente interrogata alla porta e' la risoluzione del turno
    // (sticky > override > routing), qui il routing anthropic/claude-x.
    let seen = esc.seen.lock().unwrap();
    assert_eq!(seen.last().unwrap().1.as_deref(), Some("anthropic"));
    assert_eq!(seen.last().unwrap().2.as_deref(), Some("claude-x"));
}

#[tokio::test]
async fn repeated_action_dopo_escalate_il_promosso_fa_il_turno() {
    // REGRESSIONE run c4fa064b (parte 2): al RIENTRO dall'escalation
    // (G1Continue), con la grazia scritta (repeat_scan_floor) e lo sticky
    // promosso, il detector NON ri-scatta sulle azioni pre-escalation e il
    // turno viene ESEGUITO dal modello promosso (una chiamata LLM col modello
    // sticky). Prima del fix: il rientro rivedeva gli stessi messaggi e
    // bruciava l'intero budget escalation (3) in pochi millisecondi -> ABORT
    // "il modello non riesce", senza che il promosso facesse UNA chiamata.
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_chain(&["claude-piu-capace"]));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("procedo col fix"));
    let ctx = ctx_with(llm.clone(), false);
    // Stato POST-merge del delta Escalate: azioni pre-escalation + nudge,
    // sticky promosso, contatore e floor scritti.
    let mut messages = edit_fallito_x2();
    let floor = messages.len() as i64;
    messages.push(human(
        "Hai ripetuto la stessa azione senza progresso. Ora rispondi tu, che sei \
un modello piu' capace: cambia approccio ed ESEGUI il prossimo step concreto.",
    ));
    let mut extra = serde_json::Map::new();
    extra.insert("auto_escalations".into(), json!(1));
    extra.insert("repeat_scan_floor".into(), json!(floor));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        sticky_provider: Some("anthropic".into()),
        sticky_model: Some("claude-piu-capace".into()),
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-x".into()),
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Nessuna nuova decisione di stallo: il turno e' stato eseguito.
    assert_ne!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_ne!(out.forced_close_unverified, Some(true));
    // UNA chiamata LLM, col modello PROMOSSO (sticky > override > routing).
    let calls = llm.seen.lock().unwrap();
    assert_eq!(calls.len(), 1, "il promosso deve fare il turno");
    assert_eq!(calls.last().unwrap().provider, "anthropic");
    assert_eq!(calls.last().unwrap().model, "claude-piu-capace");
}

#[tokio::test]
async fn abort_con_task_complete_forza_turno_dichiarativo() {
    // ADR 0034: chiusura di sistema (Abort repeated_action, nessun candidato di
    // escalation) con task_complete NEL catalogo -> NIENTE chiusura testuale:
    // il nodo chiede il turno DICHIARATIVO (G1Escalated + flag) cosi' l'esito
    // del run sara' la dichiarazione strutturata del modello.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // porta escalation vuota -> Abort
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: edit_fallito_x2(),
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![
            json!({"name": "edit_file"}),
            json!({"name": "task_complete"}),
        ]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(
        out.extra
            .get("force_outcome_declaration")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        out.extra
            .get("outcome_declaration_forced")
            .and_then(Value::as_bool),
        Some(true)
    );
    // Nessuna chiusura testuale di sistema e nessuna chiamata LLM qui.
    assert!(out.result.is_none());
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn abort_senza_task_complete_chiusura_storica() {
    // Contro-prova ADR 0034: senza la definizione di task_complete nel catalogo
    // del run il turno dichiarativo non e' possibile -> chiusura onesta storica.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: edit_fallito_x2(),
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert!(out
        .result
        .as_deref()
        .unwrap()
        .contains("continua a fallire"));
}

#[tokio::test]
async fn turno_dichiarativo_riduce_catalogo_a_task_complete() {
    // ADR 0034: al rientro col flag force_outcome_declaration il catalogo del
    // turno e' ridotto a SOLO task_complete (il modello DEVE dichiarare) e il
    // flag viene consumato nel delta (una sola finestra dichiarativa).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("dichiaro"));
    let ctx = ctx_with(llm.clone(), false);
    let mut messages = edit_fallito_x2();
    let floor = messages.len() as i64;
    messages.push(human(
        "Chiama ORA il tool task_complete dichiarando l'esito.",
    ));
    let mut extra = serde_json::Map::new();
    extra.insert("force_outcome_declaration".into(), json!(true));
    extra.insert("outcome_declaration_forced".into(), json!(true));
    extra.insert("repeat_scan_floor".into(), json!(floor));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![
            json!({"name": "edit_file"}),
            json!({"name": "task_complete"}),
        ]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // La chiamata LLM e' avvenuta col SOLO task_complete nel catalogo.
    let req = llm
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("LLM chiamato");
    let tool_names: Vec<&str> = req
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(tool_names, vec!["task_complete"]);
    // Flag consumato: la finestra dichiarativa e' una sola.
    assert!(out.extra.get("force_outcome_declaration").is_none());
    assert_eq!(
        out.extra
            .get("outcome_declaration_forced")
            .and_then(Value::as_bool),
        Some(true)
    );
}

/// History plausibile di un run a ridosso del cap: richiesta d'azione + un
/// round di tool completato (stop_reason del turno precedente = ToolUse).
fn history_pre_forced_text() -> Vec<Message> {
    vec![
        human("aggiorna il modulo di login"),
        ai_tool("read_file", json!({"path": "src/login.rs"})),
        Message::Human {
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: Value::String("contenuto file".into()),
                is_error: false,
                exit_code: None,
            }]),
        },
    ]
}

#[tokio::test]
async fn forced_text_risposta_vuota_rientra_turno_dichiarativo() {
    // REGRESSIONE incidente run b07c7e78: alla finestra forced-text (tool
    // rimossi, iters >= cap-5) il modello risponde con testo VUOTO
    // (outputTokens=0, stopReason=end_turn). L'esito NON e' verificato: con
    // task_complete nel catalogo il nodo NON chiude il run, rientra col turno
    // dichiarativo ADR 0034 (l'esito sara' la dichiarazione strutturata).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text(""));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: history_pre_forced_text(),
        iterations: Some(55), // cap default 60 -> soglia forced-text = 55
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![
            json!({"name": "edit_file"}),
            json!({"name": "task_complete"}),
        ]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Il turno forced-text HA chiamato l'LLM, col catalogo svuotato.
    {
        let calls = llm.seen.lock().unwrap();
        assert_eq!(calls.len(), 1, "una chiamata LLM (finestra forced-text)");
        assert!(
            calls[0].tools.as_deref().unwrap_or_default().is_empty(),
            "forced-text: nessun tool nel catalogo della chiamata"
        );
    }
    // Risposta vuota -> turno dichiarativo richiesto, NESSUNA chiusura.
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(
        out.extra
            .get("force_outcome_declaration")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert!(out.result.is_none());
    assert_ne!(out.forced_close_unverified, Some(true));
}

#[tokio::test]
async fn forced_text_risposta_vuota_senza_task_complete_forced_close() {
    // Rete di sicurezza: risposta VUOTA al forced-text e turno dichiarativo
    // NON disponibile (task_complete assente dal catalogo) -> il delta e'
    // marcato forced_close_unverified: il finalizzatore mappa FailedDiagnosed
    // e produce il recap deterministico (mai un 'completed' muto).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text(""));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: history_pre_forced_text(),
        iterations: Some(55),
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        out.forced_close_unverified,
        Some(true),
        "esito non verificato: il segnale autoritativo deve essere nel delta"
    );
    // Il testo del turno resta vuoto: il recap e' compito del finalizzatore.
    assert_eq!(out.result.as_deref(), Some(""));
}

#[tokio::test]
async fn turno_dichiarativo_risposta_vuota_forced_close() {
    // Rete FINALE: anche il turno dichiarativo (una tantum, gia' consumato)
    // torna VUOTO -> chiusura con forced_close_unverified, nessun retry
    // infinito della finestra dichiarativa.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text(""));
    let ctx = ctx_with(llm.clone(), false);
    let mut messages = history_pre_forced_text();
    messages.push(human(
        "Chiama ORA il tool task_complete dichiarando l'esito.",
    ));
    let mut extra = serde_json::Map::new();
    extra.insert("force_outcome_declaration".into(), json!(true));
    extra.insert("outcome_declaration_forced".into(), json!(true));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages,
        tools_json: Some(vec![
            json!({"name": "edit_file"}),
            json!({"name": "task_complete"}),
        ]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(
        out.forced_close_unverified,
        Some(true),
        "dichiarativo vuoto -> forced_close (mai 'completed')"
    );
    // Finestra dichiarativa consumata: nessun secondo giro.
    assert!(out.extra.get("force_outcome_declaration").is_none());
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
}

#[tokio::test]
async fn forced_text_risposta_testuale_chiude_senza_forced_close() {
    // Contro-prova: il forced-text che produce il RESOCONTO atteso chiude
    // normalmente (nessun forced_close, nessun turno dichiarativo).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("resoconto finale del lavoro"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: history_pre_forced_text(),
        iterations: Some(55),
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![
            json!({"name": "edit_file"}),
            json!({"name": "task_complete"}),
        ]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_ne!(out.forced_close_unverified, Some(true));
    assert_eq!(out.result.as_deref(), Some("resoconto finale del lavoro"));
    assert!(out.extra.get("force_outcome_declaration").is_none());
}

#[tokio::test]
async fn dichiarazione_avvenuta_chiude_con_summary() {
    // ADR 0034: dopo il turno dichiarativo forzato (flag consumato) con
    // declared_outcome presente, la testa dell'executor chiude d'autorita' col
    // summary DICHIARATO: nessuna chiamata LLM aggiuntiva, esito dal segnale
    // macchina.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert("outcome_declaration_forced".into(), json!(true));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("sistema il file")],
        declared_outcome: Some(json!({
            "outcome": "blocked",
            "summary": "Serve la API key di produzione per proseguire.",
            "blocker": "credential"
        })),
        tools_json: Some(vec![json!({"name": "task_complete"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        out.result.as_deref(),
        Some("Serve la API key di produzione per proseguire.")
    );
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn chiusura_dichiarativa_una_tantum_non_ricattura_rientro_final_gate() {
    // ADR 0034 (fix review): dopo la chiusura d'autorita' col summary
    // (outcome_declaration_closed=true), un RIENTRO nell'executor (es. dal
    // final_gate FAILED che chiede di applicare un fix) NON deve ri-chiudere
    // col summary stantio: il turno prosegue normale (chiamata LLM).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("applico il fix richiesto"));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert("outcome_declaration_forced".into(), json!(true));
    extra.insert("outcome_declaration_closed".into(), json!(true));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("sistema il file"),
            human("<final_gate_failed> build rotta"),
        ],
        declared_outcome: Some(json!({"outcome": "done", "summary": "stantio"})),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // NON la chiusura d'autorita' col summary stantio: il modello ha lavorato.
    assert_ne!(out.result.as_deref(), Some("stantio"));
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "il turno normale prosegue"
    );
}

#[tokio::test]
async fn escalation_current_pair_ancora_a_model_used_senza_sticky() {
    // Senza sticky, la coppia corrente per l'escalation e' (provider_used,
    // model_used) — l'ultima chiamata REALE, smart-upscale incluso — NON il
    // modello di routing: il filtro finestra-aware della catena deve ancorarsi
    // alla finestra del modello davvero in uso (un ancoraggio al routing
    // pre-upscale puo' promuovere un modello con finestra minore del contesto
    // corrente).
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_chain(&["claude-piu-capace"]));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: edit_fallito_x2(),
        provider_used: Some("anthropic".into()),
        model_used: Some("claude-upscalato-1m".into()),
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "edit_file"})]),
        ..Default::default()
    };
    let _ = n.run(&state, &ctx).await.expect("run");
    let seen = esc.seen.lock().unwrap();
    assert_eq!(seen.last().unwrap().1.as_deref(), Some("anthropic"));
    assert_eq!(
        seen.last().unwrap().2.as_deref(),
        Some("claude-upscalato-1m")
    );
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
            thought_signature: None,
        }],
        usage: LlmUsage::default(),
        stop_reason: Some("tool_use".into()),
        ..Default::default()
    };
    let llm = Arc::new(StubLlmGateway {
        canned,
        error: None,
        error_provider_unavailable: false,
        provider_unavailable_cause: None,
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
    assert_eq!(
        pending[0].get("name").and_then(Value::as_str),
        Some("write_file")
    );
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
            thought_signature: None,
        }],
        usage: LlmUsage::default(),
        stop_reason: Some("tool_use".into()),
        ..Default::default()
    };
    let llm = Arc::new(StubLlmGateway {
        canned,
        error: None,
        error_provider_unavailable: false,
        provider_unavailable_cause: None,
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
    let err = n
        .run(&state, &ctx)
        .await
        .expect_err("deve fallire (no provider)");
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
        req.force_tool_choice,
        Some(true),
        "BUG-e preservato: turno precedente che non ha agito -> forcing early ON"
    );
}

#[tokio::test]
async fn lettura_ripetuta_identica_informativa_guida_a_concludere() {
    // FIX #2 loop-control (regola H): una LETTURA ripetuta identica (stesso path)
    // oltre la soglia read-only scatta come repeated_action di SOLA LETTURA. Su un
    // turno INFORMATIVO (action_oriented=false) il progress_controller inietta un
    // nudge "concludi con testo" SENZA forzare un'altra tool call, ben prima del
    // cap esplorazione 2x (=12) e dell'escalation. Cosi' il loop read-only non
    // arriva a 14 iterazioni. Qui la soglia read-only e' 2 per esercitare il GUIDE
    // al 2o read (la soglia di produzione e' piu' alta: testiamo la DECISIONE).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        repeated_action_threshold_read_only: 2,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc); // progress_controller ON
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
    let has_concludi = req
        .messages
        .iter()
        .any(|m| matches!(&m.content, Value::String(s) if s.contains("Rispondi ORA a parole")));
    assert!(
        has_concludi,
        "atteso nudge 'concludi con testo' per la lettura ripetuta informativa"
    );
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
        // Soglia read-only a 2 per esercitare il GUIDE al 2o read (vedi test gemello).
        repeated_action_threshold_read_only: 2,
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
        tools_json: Some(vec![
            json!({"name": "read_file"}),
            json!({"name": "edit_file"}),
        ]),
        // Turno di modifica: il nudge deve orientare all'edit, non alla resa.
        action_oriented: Some(true),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _ = apply(state, delta);
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    // Nudge orientato all'AZIONE iniettato, NON quello "rispondi a parole".
    let has_edit_nudge = req
        .messages
        .iter()
        .any(|m| matches!(&m.content, Value::String(s) if s.contains("ESEGUI l'azione")));
    assert!(
        has_edit_nudge,
        "atteso nudge orientato all'azione per la lettura ripetuta su un fix"
    );
    let has_concludi = req
        .messages
        .iter()
        .any(|m| matches!(&m.content, Value::String(s) if s.contains("Rispondi ORA a parole")));
    assert!(
        !has_concludi,
        "su un fix il nudge NON deve guidare a rispondere a parole"
    );
    // Forza la tool call: l'agente DEVE applicare l'edit, non rinunciare.
    assert_eq!(
        req.force_tool_choice,
        Some(true),
        "lettura ripetuta su un fix -> force-action verso l'edit"
    );
}

#[tokio::test]
async fn lettura_ripetuta_esaurita_chiude_onestamente_non_fallimento() {
    // FASE 4 (regola H): un read-only ripetuto oltre soglia, gia' guidato e
    // senza candidato escalation, NON deve chiudere con "ESITO: non completato /
    // mi sono bloccato" (percepito come incapacita' del modello capace). Deve
    // chiudere in modo ONESTO instradando al final_gate (EndTurn), invitando a
    // rispondere con quanto raccolto. Causa radice del falso "il modello non riesce".
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        repeated_action_threshold_read_only: 2,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc); // progress_controller ON
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    // read_file stesso path 2 volte, entrambe RIUSCITE: contenuto GIA' nel contesto.
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
        action_oriented: Some(false),
        // Gia' guidato e diagnosticato -> il prossimo stadio e' ABORT (nessun
        // candidato escalation dallo stub) -> chiusura onesta per read-only.
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Chiusura ONESTA verso final_gate: EndTurn, non LoopAbort.
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    let result = out.result.as_deref().unwrap();
    assert!(
        result.contains("Ho gia' raccolto il contenuto"),
        "atteso messaggio onesto, non un fallimento: {result}"
    );
    assert!(
        !result.contains("ESITO: non completato"),
        "una lettura idempotente non deve dichiarare fallimento del modello"
    );
    // LLM non chiamato (chiusura prima del modello).
    assert!(llm.seen.lock().unwrap().is_empty());
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
        recent_tool_signatures: Some(vec![sig.clone(), "list_files|abc".into(), sig.clone()]),
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
    let llm = Arc::new(StubLlmGateway::with_error(
        "billing_error: credito esaurito",
    ));
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
    let delta = n
        .run(&state, &ctx)
        .await
        .expect("run NON deve abortire su errore gateway");
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
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(0)
    );
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
    let esc = Arc::new(StubEscalationPort::with_failover_tier(
        "mistral",
        "mistral-large-2411",
        "medium",
    ));
    let (n, meta, _s) = node_esc(cfg_resolved(), rc, esc.clone());
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
    // FIX-A: current_tier scritto col tier del modello di failover.
    assert_eq!(out.current_tier.as_deref(), Some("medium"));
    assert_eq!(out.g1_reroute_count, Some(0));
    assert_eq!(out.action_nudge_count, Some(0));
    assert!(out.pending_tool_uses.unwrap().is_empty());
    // auto_escalations incrementato (0 -> 1): gate < 3 rispettato.
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
    // failover_tried accumula sia il provider CADUTO sia quello SCELTO, cosi' un
    // eventuale secondo salto li esclude entrambi (cascata).
    let tried: Vec<String> = out
        .extra
        .get("failover_tried")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(tried, vec!["anthropic".to_string(), "mistral".to_string()]);
    // UNA sola chiamata LLM (quella fallita): la ri-esecuzione avviene nel self-loop
    // successivo del grafo, non dentro questo turno.
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    // failover_provider e' stato interrogato escludendo il provider caduto.
    let seen = esc.failover_seen.lock().unwrap();
    assert_eq!(seen.last().unwrap(), &vec!["anthropic".to_string()]);
    // REGRESSIONE (card "Mistral / ?" del 20/07): il meta-step dello switch
    // deve portare ANCHE il modello caduto (`from_model`), non solo il
    // provider -- il frontend ripiega su prev.model, che sul primo segmento
    // non esiste, e la card mostrava "?". Payload dal PRODUTTORE reale
    // (regola O): questo test attraversa l'emissione vera dell'executor.
    let steps = meta.meta_steps.lock().unwrap();
    let switch = steps
        .iter()
        .find(|m| {
            m.get("payload")
                .and_then(|p| p.get("reason"))
                .and_then(Value::as_str)
                == Some("provider_failover")
        })
        .expect("meta-step provider_failover emesso");
    let payload = switch.get("payload").expect("payload presente");
    assert_eq!(
        payload.get("from_model").and_then(Value::as_str),
        Some("claude-x"),
        "il modello caduto deve viaggiare nel payload: {payload}"
    );
    assert_eq!(
        payload.get("from_provider").and_then(Value::as_str),
        Some("anthropic")
    );
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
    let esc = Arc::new(StubEscalationPort::with_failover(
        "google",
        "gemini-2.5-pro",
    ));
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
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(2)
    );
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
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        tried,
        vec![
            "deepseek".to_string(),
            "anthropic".to_string(),
            "google".to_string()
        ]
    );
}

#[tokio::test]
async fn client_error_non_scatena_failover_cross_provider() {
    use crate::runtime::ports::ProviderFailureCause;

    // client_error: il gateway ha gia' provato sanificazione; l'executor NON deve
    // fare failover cieco (incidente f0ad0337).
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_failover("google", "gemini-2.5-flash"));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc.clone());
    let llm = Arc::new(StubLlmGateway::with_provider_unavailable_cause(
        ProviderFailureCause::ClientError,
        "deepseek HTTP 400 invalid_request_error",
    ));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi file")],
        provider_used: Some("deepseek".into()),
        model_used: Some("deepseek-v4-flash".into()),
        sticky_provider: Some("deepseek".into()),
        sticky_model: Some("deepseek-v4-flash".into()),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run NON deve abortire");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::Error));
    assert_eq!(out.sticky_provider.as_deref(), Some("deepseek"));
    assert_eq!(out.sticky_model.as_deref(), Some("deepseek-v4-flash"));
    assert!(esc.failover_seen.lock().unwrap().is_empty());
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
                        thought_signature: None,
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
    // Catena intra-provider: anthropic/claude-x -> claude-piu-capace (tier heavy).
    let esc = Arc::new(StubEscalationPort::with_chain_tier(
        &["claude-piu-capace"],
        "heavy",
    ));
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
        recent_tool_signatures: Some(vec![sig.clone(), "list_files|abc".into(), sig.clone()]),
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
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
    // provider/model promossi nel delta (provider_used = richiesto escalato).
    assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
    assert_eq!(out.model_used.as_deref(), Some("claude-piu-capace"));
    // La porta escalation e' stata interrogata col modello corrente.
    let seen = esc.seen.lock().unwrap();
    assert_eq!(seen.last().unwrap().2.as_deref(), Some("claude-x"));
    // REGRESSIONE run c4fa064b: la promozione del signature-loop e' STICKY (i
    // turni successivi restano sul promosso invece di ricadere sul modello di
    // routing debole, che rientrava subito in loop) e fa partire la grazia
    // post-escalation (floor del detector repeated_action = prefisso persistito
    // pre-turno, qui il solo messaggio human).
    assert_eq!(out.sticky_provider.as_deref(), Some("anthropic"));
    assert_eq!(out.sticky_model.as_deref(), Some("claude-piu-capace"));
    // FIX-A: la promozione del signature-loop scrive anche current_tier col tier
    // del modello promosso (catturato dal pick, delta finale del turno).
    assert_eq!(out.current_tier.as_deref(), Some("heavy"));
    assert_eq!(
        out.extra.get("repeat_scan_floor").and_then(Value::as_i64),
        Some(1)
    );
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
                        thought_signature: None,
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
        recent_tool_signatures: Some(vec![sig.clone(), "list_files|abc".into(), sig.clone()]),
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
        recent_tool_signatures: Some(vec![sig.clone(), "list_files|abc".into(), sig.clone()]),
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
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(0)
    );
    // Il segnale STRUTTURATO di chiusura anti-loop viaggia nel delta: il
    // finalizzatore mappa FailedDiagnosed anche se il final_gate riscrive
    // stop_reason (run b833a83d chiudeva "completed" col testo di sistema).
    assert_eq!(out.forced_close_unverified, Some(true));
}

#[tokio::test]
async fn signature_loop_secco_con_task_complete_forza_dichiarazione() {
    // ADR 0034: chiusura secca del signature-loop (nessun candidato di
    // escalation) con task_complete NEL catalogo -> turno dichiarativo forzato
    // invece del "[LOOP RILEVATO]" di sistema: l'esito del run sara' la
    // dichiarazione strutturata del modello.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // porta escalation vuota
    let same_input = json!({"path": "x"});
    let sig = build_signature("read_file", &same_input);
    let llm = llm_tool_call("read_file", same_input.clone());
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("leggi")],
        recent_tool_signatures: Some(vec![sig.clone(), "list_files|abc".into(), sig.clone()]),
        tools_json: Some(vec![
            json!({"name": "read_file"}),
            json!({"name": "task_complete"}),
        ]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(
        out.extra
            .get("force_outcome_declaration")
            .and_then(Value::as_bool),
        Some(true)
    );
    // Nessuna chiusura testuale di sistema.
    assert!(out.result.is_none());
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
        recent_tool_signatures: Some(vec![sig.clone(), "list_files|abc".into(), sig.clone()]),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    assert_eq!(out.stop_reason, Some(StopReason::LoopDetected));
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(3)
    );
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
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
    // LLM NON chiamato (escalation G1 = sticky + nudge, niente turno).
    assert!(llm.seen.lock().unwrap().is_empty());
    // La porta e' stata interrogata col modello corrente (provider_used).
    assert_eq!(
        esc.seen.lock().unwrap().last().unwrap().1.as_deref(),
        Some("anthropic")
    );
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
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
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
    assert!(out
        .result
        .as_deref()
        .unwrap()
        .contains("massimo di iterazioni"));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn iteration_cap_prova_escalation_prima_del_backstop() {
    // Simmetria col ramo budget_token (mig 0577): al cap iterazioni, PRIMA del
    // backstop di chiusura, prova un'ultima ESCALATION. Con un candidato ->
    // promuove (sticky + G1Escalated) e reset_iterations=true azzera `iterations`
    // cosi' il promosso riparte con un ciclo pieno invece di ri-scattare subito il
    // cap. (Il caso senza candidato -> chiusura secca e' cap_assoluto_iterazioni_chiude.)
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_chain_tier(
        &["claude-piu-capace"],
        "heavy",
    ));
    let mut cfg = cfg_resolved();
    cfg.iteration_cap = 10;
    let (n, _m, _s) = node_esc(cfg, rc, esc);
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
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_model.as_deref(), Some("claude-piu-capace"));
    // reset_iterations: ciclo pieno per il promosso (non ri-scatta subito il cap).
    assert_eq!(out.iterations, Some(0));
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn final_gate_nonconvergence_promuove_e_consuma_flag() {
    // Rientro dal final_gate con FINAL_GATE_ESCALATION_KEY in extra (cap del gate
    // non convergente): l'executor delega al PUNTO UNICO maybe_escalate_nonconvergence.
    // Con un candidato -> promuove (sticky + G1Escalated), incrementa auto_escalations
    // e CONSUMA il flag (niente escalation a raffica al rientro del promosso).
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_chain_tier(
        &["claude-piu-capace"],
        "heavy",
    ));
    let (n, _m, _s) = node_esc(cfg_resolved(), rc, esc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert(
        crate::nodes::FINAL_GATE_ESCALATION_KEY.into(),
        json!(true),
    );
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea x")],
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_provider.as_deref(), Some("anthropic"));
    assert_eq!(out.sticky_model.as_deref(), Some("claude-piu-capace"));
    assert_eq!(
        out.extra.get("auto_escalations").and_then(Value::as_i64),
        Some(1)
    );
    // Flag CONSUMATO: il modello promosso non ri-scatta il ramo al rientro.
    assert!(out
        .extra
        .get(crate::nodes::FINAL_GATE_ESCALATION_KEY)
        .is_none());
    // Nessuna chiamata LLM: il self-loop rientra col promosso.
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn escalation_nonconvergenza_payload_porta_from_model() {
    // REGRESSIONE (card "Mistral / ?"): il ramo di escalation per non-convergenza
    // del final_gate (maybe_escalate_nonconvergence) era il 4o emettitore di card
    // "CAMBIO PROVIDER" che bypassava il punto unico switch_payload OMETTENDO
    // from_provider/from_model -> il pill "Da" degradava a "?" (il frontend ripiega
    // su prev.model, assente sul 1o segmento). Payload dal PRODUTTORE reale (regola
    // O): il test attraversa n.run, NON costruisce il payload a mano. Coppia
    // corrente = scenario utente (mistral/mistral-small-latest).
    let rc = Arc::new(StubRunControlStore::default());
    let esc = Arc::new(StubEscalationPort::with_chain_tier(
        &["claude-piu-capace"],
        "heavy",
    ));
    let (n, meta, _s) = node_esc(cfg_resolved(), rc, esc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert(crate::nodes::FINAL_GATE_ESCALATION_KEY.into(), json!(true));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea x")],
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        // Coppia corrente NOTA: cio' che DEVE finire in from_provider/from_model.
        provider_used: Some("mistral".into()),
        model_used: Some("mistral-small-latest".into()),
        extra,
        ..Default::default()
    };
    let _ = n.run(&state, &ctx).await.expect("run");

    let steps = meta.meta_steps.lock().unwrap();
    let switch = steps
        .iter()
        .find(|m| {
            m.get("payload")
                .and_then(|p| p.get("reason"))
                .and_then(Value::as_str)
                == Some("final_gate_nonconvergence")
        })
        .expect("meta-step final_gate_nonconvergence emesso");
    let payload = switch.get("payload").expect("payload presente");
    assert_eq!(
        payload.get("from_model").and_then(Value::as_str),
        Some("mistral-small-latest"),
        "il modello corrente deve viaggiare nel payload -> niente pill '?': {payload}"
    );
    assert_eq!(
        payload.get("from_provider").and_then(Value::as_str),
        Some("mistral")
    );
}

#[tokio::test]
async fn final_gate_nonconvergence_senza_candidato_chiude() {
    // Flag presente ma catena escalation VUOTA (porta default): maybe_escalate
    // ritorna None -> chiusura FailedDiagnosed via il PUNTO UNICO close_runaway
    // (EndTurn + forced_close_unverified), non un re-loop sullo stesso modello.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // porta escalation vuota
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert(
        crate::nodes::FINAL_GATE_ESCALATION_KEY.into(),
        json!(true),
    );
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea x")],
        stop_reason: Some(StopReason::ToolUse),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn billing_fail_fast_chiude_loop_abort() {
    // Soglia esplorazione raggiunta + il PROVIDER IN USO in cooldown billing ->
    // chiusura onesta loop_abort PRIMA della chiamata LLM (py:2072-2092 + fix
    // provider-in-uso: cfr. billing_fail_fast_provider_corrente_valido).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::with_exhausted(&[
        "anthropic",
        "openai",
    ]));
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
    let next_actions = Arc::new(StubNextActionsDeriver::with_choices(&[(
        "Aggiungi form",
        "Aggiungi un form di contatto alla pagina",
    )]));
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
    assert!(metas
        .iter()
        .any(|m| m.get("kind").and_then(Value::as_str) == Some("next_actions")));
    // La porta ha ricevuto il testo GIA' ripulito.
    let seen = next_actions.seen.lock().unwrap();
    assert!(seen
        .last()
        .map(|s| !s.contains("suggested_actions"))
        .unwrap_or(false));
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
    let delta = n
        .run(&state, &ctx)
        .await
        .expect("run NON deve abortire su derive fallita");
    let out = apply(state, delta);
    let result = out.result.as_deref().unwrap();
    assert!(!result.to_lowercase().contains("suggested_actions"));
    assert!(result.contains("Risposta finale."));
    // Nessun meta_step next_actions (derive fallita -> nessuna scelta).
    let metas = meta.meta_steps.lock().unwrap();
    assert!(!metas
        .iter()
        .any(|m| m.get("kind").and_then(Value::as_str) == Some("next_actions")));
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
    // Report STRUTTURALE di passi pendenti (ADR 0018 fase 3: il segnale del
    // ramo report e' detect_pending_steps_report_with, non piu' la narrazione).
    let llm = Arc::new(StubLlmGateway::with_text(
        "Analisi fatta. Prossimi passi:\n1. Avviare il servizio\n2. Verificare il login",
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
async fn g1_nudge_scatta_via_segnale_strutturale_senza_blacklist() {
    // REGRESSIONE ADR 0018 fase 3 (caso Beauty-Book chat 7): turno precedente
    // chiuso end_turn con ZERO tool call, tool disponibili, richiesta d'azione.
    // Con la blacklist lessicale rimossa il nudge G1 deve scattare comunque,
    // dal solo segnale STRUTTURALE (structural_unfulfilled_signal).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text("Risposta."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("sistema il login"),
            Message::Ai {
                content: MessageContent::text("Procedero' a creare il file di login."),
                tool_calls: vec![],
                reasoning: None,
                thinking_signature: None,
            },
        ],
        // Turno precedente: end_turn SENZA tool call (segnale strutturale).
        stop_reason: Some(StopReason::EndTurn),
        iterations: Some(1),
        action_oriented: Some(true),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _out = apply(state, delta);
    // Il nudge G1 e' stato iniettato: la richiesta LLM contiene il messaggio
    // forza-azione ("AGISCI ADESSO"), senza alcun match lessicale sulla prosa.
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    let nudged = req
        .messages
        .iter()
        .any(|m| m.content.to_string().contains("AGISCI ADESSO"));
    assert!(nudged, "nudge G1 atteso via segnale strutturale");
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
async fn unfulfilled_report_segue_il_segnale_post_ignorando_closure_fulfilled() {
    // DISTINZIONE LOAD-BEARING (regola L): il ramo report POST end_turn valuta il
    // TESTO FINALE (report strutturale di passi pendenti, ADR 0018 fase 3) e NON
    // consulta closure_verdict, a differenza del ramo G1 (closure-first).
    // Setup adversariale: closure_verdict = fulfilled (compiuto, potenzialmente
    // stale del turno precedente) MA il testo finale elenca passi pendenti. Il
    // ramo report DEVE seguire il segnale sul testo -> SOSTITUIRE col resoconto
    // onesto, ignorando il verdetto closure fulfilled.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_resolved(), rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text(
        "Analisi fatta. Prossimi passi:\n1. Avviare il servizio\n2. Verificare il login",
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
    // SOSTITUITO dal resoconto onesto: il report strutturale sul testo finale ha
    // prevalso sul closure "fulfilled" (che qui e' deliberatamente ignorato).
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
                thinking_signature: None,
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
    let upscale = Arc::new(StubModelUpscalePort::promoting(
        200,
        "google",
        "gemini-2.5-pro",
    ));
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
    let upscale = Arc::new(StubModelUpscalePort::promoting(
        1_000_000,
        "google",
        "gemini-2.5-pro",
    ));
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

// ── hard cap post-brake (ADR 0016 fase D2) ────────────────────────────────────

#[tokio::test]
async fn hard_cap_termina_il_run_senza_chiamare_llm() {
    // Un singolo messaggio enorme che il brake NON puo' comprimere (primo human
    // preservato): la stima resta oltre ratio*window -> fail-fast strutturato,
    // NESSUNA chiamata LLM, messaggio dal template renderizzato.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        context_window: 100,
        hard_cap_ratio: 0.95,
        overflow_message_template:
            "Contesto stimato %ESTIMATED_TOKENS% token oltre la finestra %MAX_WINDOW%.".to_string(),
        ..ExecutorConfig::default()
    };
    let (n, meta) = node_ports(cfg, rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text("MAI CHIAMATO"));
    let ctx = ctx_with(llm.clone(), false);
    let big = "x".repeat(2000); // ~500 token stimati >> 95 (0.95*100)
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human(&big)],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Chiusura strutturata: StopReason::Error + error_class macchina (regola M).
    assert_eq!(out.stop_reason, Some(StopReason::Error));
    assert_eq!(
        out.extra.get("error_class").and_then(|v| v.as_str()),
        Some("context_overflow")
    );
    // Messaggio dal template DB renderizzato (placeholder sostituiti).
    let result = out.result.expect("result presente");
    assert!(result.contains("oltre la finestra 100"), "result: {result}");
    assert!(!result.contains("%ESTIMATED_TOKENS%"));
    // NESSUNA chiamata LLM: fail-fast prima della richiesta al provider.
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "LLM non deve essere chiamato"
    );
    // Meta_step strutturato context_overflow persistito.
    let metas = meta.meta_steps.lock().unwrap();
    assert!(
        metas
            .iter()
            .any(|m| m.get("kind").and_then(|k| k.as_str()) == Some("context_overflow")),
        "meta_step context_overflow atteso"
    );
}

#[tokio::test]
async fn hard_cap_inerte_con_ratio_default() {
    // Default safe-DB-down (`hard_cap_ratio=0.0`): gate spento, il run procede
    // e l'LLM viene chiamato anche con contesto oltre la window.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        context_window: 100,
        ..ExecutorConfig::default()
    };
    let (n, _m) = node_ports(cfg, rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text("Risposta."));
    let ctx = ctx_with(llm.clone(), false);
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
    assert_ne!(out.stop_reason, Some(StopReason::Error));
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "LLM chiamato normalmente"
    );
}

#[tokio::test]
async fn hard_cap_non_scatta_dopo_upscale_a_window_grande() {
    // L'upscale promuove a un modello con window molto piu' grande: il gate usa
    // la window EFFETTIVA del modello promosso e NON scatta (ordine
    // upscale -> brake -> hard cap).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::promoting_to_window(
        100,
        "google",
        "gemini-2.5-pro",
        100_000,
    ));
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        context_window: 100,
        hard_cap_ratio: 0.95,
        upscale_enabled: true,
        ..ExecutorConfig::default()
    };
    let (n, _m) = node_ports(cfg, rc, next_actions, billing, upscale);
    let llm = Arc::new(StubLlmGateway::with_text("Risposta."));
    let ctx = ctx_with(llm.clone(), false);
    let big = "x".repeat(2000); // ~500 token: oltre la window 100, sotto 95k
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human(&big)],
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Nessun overflow: il run e' proseguito col modello promosso.
    assert_ne!(out.stop_reason, Some(StopReason::Error));
    assert_eq!(out.model_used.as_deref(), Some("gemini-2.5-pro"));
    let req = llm.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(req.model, "gemini-2.5-pro");
}

// ── tokenizer reale iniettato (ADR 0016 D1) ───────────────────────────────────

/// Contatore stub: valore fisso, registra le chiamate.
struct FixedTokenCounter {
    tokens: i64,
    calls: std::sync::Mutex<usize>,
}

impl crate::runtime::ports::TokenCounter for FixedTokenCounter {
    fn count(&self, _text: &str) -> i64 {
        *self.calls.lock().unwrap() += 1;
        self.tokens
    }
}

#[tokio::test]
async fn token_counter_iniettato_pilota_l_hard_cap() {
    // Con la porta iniettata la stima viene dal CONTATORE, non dai char: un
    // counter che dichiara 1M token fa scattare l'hard cap anche su una
    // history minuscola (e viceversa il default char-based non scatterebbe).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::default());
    let cfg = ExecutorConfig {
        routing_provider: "anthropic".to_string(),
        routing_model: "claude-x".to_string(),
        context_window: 200_000,
        hard_cap_ratio: 0.95,
        ..ExecutorConfig::default()
    };
    let meta = Arc::new(StubMetaStepStore::default());
    let steps = Arc::new(StubAgentStepStore::default());
    let counter = Arc::new(FixedTokenCounter {
        tokens: 1_000_000,
        calls: std::sync::Mutex::new(0),
    });
    let n = ExecutorNode::new(
        cfg,
        rc,
        meta,
        steps,
        Arc::new(StubEscalationPort::default()),
        next_actions,
        billing,
        upscale,
        Arc::new(StubSummaryStore::default()),
    )
    .with_token_counter(counter.clone());
    let llm = Arc::new(StubLlmGateway::with_text("MAI CHIAMATO"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("ciao")], // ~1 token reale: e' il counter a dire 1M
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::Error));
    assert_eq!(
        out.extra.get("error_class").and_then(|v| v.as_str()),
        Some("context_overflow")
    );
    assert!(llm.seen.lock().unwrap().is_empty(), "LLM non chiamato");
    assert!(
        *counter.calls.lock().unwrap() > 0,
        "il contatore iniettato e' stato usato"
    );
}

// ── rolling-summary (intervento 3): aggancio al cambio-fase ───────────────────

/// Messaggio assistant testuale (helper locale per i test rolling-summary).
fn ai(text: &str) -> Message {
    Message::Ai {
        content: MessageContent::text(text),
        tool_calls: vec![],
        reasoning: None,
        thinking_signature: None,
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
    let req = llm
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("una richiesta LLM");
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

    // Degrado best-effort: il summarizer NON collassa il prefisso. L'invariante
    // robusto (non un numero magico: altre iniezioni come il forced-RAG reminder
    // possono aggiungere un messaggio) e' che la history NON e' ridotta a
    // 1 summary + keep_recent (3, come nel caso col summarizer) e che il primo
    // messaggio NON e' il riassunto.
    let req = llm
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("una richiesta LLM");
    assert!(
        req.messages.len() >= 6,
        "degrado best-effort: history non collassata (len={}, attesi >= 6 originali)",
        req.messages.len()
    );
    let first = req.messages[0].content.as_str().unwrap_or_default();
    assert!(
        !first.contains("[RIASSUNTO"),
        "nel degrado (summarizer fallito) NON deve comparire il messaggio di riassunto"
    );
}

// ── continuity-trim SEMANTICO (EmbeddingStore): aggancio al cambio-fase ───────

/// Config che attiva il continuity-trim (rolling-summary OFF per isolare l'effetto).
fn cfg_continuity() -> ExecutorConfig {
    ExecutorConfig {
        continuity_trim_enabled: true,
        continuity_trim_min_score: 0.5,
        continuity_trim_max_drop: 8,
        rolling_summary_enabled: false,
        rolling_keep_recent: 2,
        ..cfg_resolved()
    }
}

/// Nodo con un [`StubEmbeddingStore`] iniettato (continuity-trim). Summarizer di
/// default (nessun rolling-summary).
fn node_continuity(
    cfg: ExecutorConfig,
    rc: Arc<StubRunControlStore>,
    embedding: Arc<StubEmbeddingStore>,
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
        Arc::new(StubSummaryStore::default()),
    )
    .with_embedding_store(embedding)
}

/// Al cambio-fase, con l'embedder che marca un atomo come IRRILEVANTE (coseno sotto
/// soglia), il continuity-trim lo SCARTA dalla history: la richiesta LLM non porta
/// piu' quel messaggio. Gli atomi rilevanti restano.
#[tokio::test]
async fn continuity_trim_scarta_atomo_irrilevante_al_cambio_fase() {
    let rc = Arc::new(StubRunControlStore::default());
    // state_cambio_fase: 2 atomi candidati (ai "risposta 1" idx1, ai "risposta 2"
    // idx3); focus = ultimo human "domanda 3". texts = [focus, r1, r2] -> 3 vettori:
    // focus [1,0]; r1 [0,1] (coseno 0 < 0.5 -> scartato); r2 [1,0] (coseno 1 -> tenuto).
    let embedding = Arc::new(StubEmbeddingStore::with_vectors(vec![
        vec![1.0, 0.0],
        vec![0.0, 1.0],
        vec![1.0, 0.0],
    ]));
    let n = node_continuity(cfg_continuity(), rc, embedding.clone());
    let llm = Arc::new(StubLlmGateway::with_text("Procedo."));
    let ctx = ctx_with(llm.clone(), false);
    let _ = n.run(&state_cambio_fase(), &ctx).await.expect("run");

    // L'embedder ha ricevuto il focus (per primo) + i 2 atomi candidati.
    let seen = embedding.embed_seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "embed chiamato con focus + 2 candidati");
    assert!(
        seen.iter().any(|t| t.contains("risposta 1")),
        "candidato r1 embeddato"
    );
    assert!(
        seen.iter().any(|t| t.contains("risposta 2")),
        "candidato r2 embeddato"
    );
    drop(seen);

    // La richiesta LLM NON porta piu' "risposta 1" (atomo scartato), ma porta "risposta 2".
    let req = llm
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("una richiesta LLM");
    let contiene = |needle: &str| {
        req.messages
            .iter()
            .any(|m| m.content.as_str().unwrap_or_default().contains(needle))
    };
    assert!(
        !contiene("risposta 1"),
        "l'atomo irrilevante deve essere scartato"
    );
    assert!(contiene("risposta 2"), "l'atomo rilevante resta");
}

/// A flag OFF il continuity-trim NON scatta anche con la porta iniettata: l'embedder
/// non viene chiamato e la history resta invariata (bit-identico).
#[tokio::test]
async fn continuity_trim_flag_off_non_chiama_embedder() {
    let rc = Arc::new(StubRunControlStore::default());
    let embedding = Arc::new(StubEmbeddingStore::with_vectors(vec![vec![1.0, 0.0]]));
    let cfg = ExecutorConfig {
        continuity_trim_enabled: false,
        ..cfg_continuity()
    };
    let n = node_continuity(cfg, rc, embedding.clone());
    let llm = Arc::new(StubLlmGateway::with_text("Procedo."));
    let ctx = ctx_with(llm.clone(), false);
    let _ = n.run(&state_cambio_fase(), &ctx).await.expect("run");

    assert!(
        embedding.embed_seen.lock().unwrap().is_empty(),
        "flag OFF: l'embedder NON deve essere chiamato"
    );
    // History invariata: "risposta 1" resta nella richiesta.
    let req = llm
        .seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("una richiesta LLM");
    assert!(
        req.messages.iter().any(|m| m
            .content
            .as_str()
            .unwrap_or_default()
            .contains("risposta 1")),
        "flag OFF: nessun trim, la history resta completa"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  Meta-reasoner di recovery-da-stallo (blocco #6): gate emissione + consumo.
// ──────────────────────────────────────────────────────────────────────────

use crate::decisions::meta_reason::work_epoch as stall_work_epoch;
use crate::nodes::stall_recovery::{stall_move_key, STALL_CONTEXT_KEY};
use crate::runtime::ports::{RecoveryMove, StallContext};

/// Stato che fa scattare l'asse repeated_action con mossa COSTOSA (Abort): write
/// FALLITO ripetuto, asse GIA' guidato+diagnosed+strategy (livelli cheap spesi).
/// A flag OFF `pc::decide` -> Abort (chiude EndTurn); a flag ON il gate emette
/// StallReason PRIMA della chiusura.
fn state_stallo_repeated_action() -> AgentState {
    let tr_err = |id: &str| Message::Human {
        content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: Value::String("permission denied".into()),
            is_error: true,
            exit_code: None,
        }]),
    };
    AgentState {
        thread_id: Some("r1".into()),
        messages: vec![
            human("scrivi"),
            ai_tool("write_file", json!({"path": "b.rs"})),
            tr_err("c1"),
            ai_tool("write_file", json!({"path": "b.rs"})),
            tr_err("c1"),
        ],
        progress_guided_axes: Some(vec!["repeated_action".into()]),
        progress_diagnosed_axes: Some(vec!["repeated_action".into()]),
        progress_strategy_axes: Some(vec!["repeated_action".into()]),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    }
}

#[tokio::test]
async fn stall_recovery_off_non_emette_stall_reason() {
    // Flag OFF (default): il gate non scatta MAI. Lo stesso stato che a flag ON
    // emetterebbe StallReason qui chiude con la gerarchia fissa (EndTurn onesto),
    // BIT-IDENTICO a oggi. Nessuno StallContext scritto in extra.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: false,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_stallo_repeated_action();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(out.stop_reason, Some(StopReason::StallReason));
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert!(out.extra.get(STALL_CONTEXT_KEY).is_none());
}

#[tokio::test]
async fn stall_recovery_on_emette_stall_reason() {
    // Flag ON + budget: il gate emette StallReason (instrada al nodo StallRecovery)
    // con lo StallContext strutturato in extra, invece di applicare la mossa fissa.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_stallo_repeated_action();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::StallReason));
    // StallContext strutturato presente (regola M): asse repeated_action.
    let ctx_val = out
        .extra
        .get(STALL_CONTEXT_KEY)
        .expect("StallContext in extra");
    let sc: StallContext = serde_json::from_value(ctx_val.clone()).expect("StallContext");
    assert_eq!(sc.axis, "repeated_action");
    // L'LLM NON e' chiamato dall'executor (lo fara' il nodo dedicato, replay-safe).
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stall_recovery_budget_esaurito_ricade_su_fissa() {
    // Budget per-sessione esaurito: il gate NON emette StallReason (rete di
    // sicurezza) e la gerarchia fissa chiude come a flag OFF.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 2,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = state_stallo_repeated_action();
    state.extra.insert("stall_moves_used".to_string(), json!(2));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(out.stop_reason, Some(StopReason::StallReason));
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
}

/// Costruisce lo stato di RIENTRO dal nodo StallRecovery: `StallResolved` +
/// StallContext + (opzionale) la mossa persistita alla chiave-cache.
fn state_rientro(axis: &str, epoch: i64, mv: Option<RecoveryMove>) -> AgentState {
    let stall = StallContext {
        axis: axis.to_string(),
        work_epoch: epoch,
        ..Default::default()
    };
    let mut extra = serde_json::Map::new();
    extra.insert(
        STALL_CONTEXT_KEY.to_string(),
        serde_json::to_value(&stall).expect("serialize StallContext"),
    );
    if let Some(m) = mv {
        extra.insert(
            stall_move_key(axis, epoch),
            serde_json::to_value(&m).expect("serialize mossa"),
        );
    }
    AgentState {
        thread_id: Some("r1".into()),
        stop_reason: Some(StopReason::StallResolved),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        extra,
        ..Default::default()
    }
}

#[tokio::test]
async fn stall_recovery_rientro_fallback_marca_epoca_e_prosegue() {
    // Rientro senza mossa in extra (nodo degradato: reasoner Ok(None)/Fallback):
    // il consumo marca l'epoca come fallback e RI-DA il turno pulito. Al re-entry
    // il gate non ri-emettera' (guardia anti meta-loop) -> gerarchia fissa.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("x"));
    let ctx = ctx_with(llm.clone(), false);
    let epoch = stall_work_epoch(0, 0, 0);
    let state = state_rientro("repeated_action", epoch, None);
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Nessuna mossa applicata: budget invariato, epoca marcata fallback.
    assert_eq!(out.extra.get("stall_moves_used"), None);
    let epochs = out
        .extra
        .get("stall_fallback_epochs")
        .and_then(Value::as_array)
        .expect("stall_fallback_epochs presente");
    assert!(epochs.iter().any(|v| v.as_i64() == Some(epoch)));
}

#[tokio::test]
async fn stall_recovery_consume_ask_user_produce_needs_input() {
    // Consumo AskUser: chiusura DIRETTA con esito strutturato needs_input (ADR 0034),
    // la question nel summary/result. Budget incrementato (consultazione effettiva).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("x"));
    let ctx = ctx_with(llm.clone(), false);
    let epoch = stall_work_epoch(0, 0, 0);
    let mv = RecoveryMove::AskUser {
        question: "Qual e' l'email reale da usare per il login?".into(),
    };
    let state = state_rientro("repeated_user_question", epoch, Some(mv));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    // Esito STRUTTURATO needs_input (regola M): letto dal segnale, non dalla prosa.
    let outcome = out.declared_outcome.as_ref().expect("declared_outcome");
    assert_eq!(
        outcome.get("outcome").and_then(Value::as_str),
        Some("needs_input")
    );
    assert!(out
        .result
        .as_deref()
        .unwrap_or_default()
        .contains("email reale"));
    // Budget consumato.
    assert_eq!(
        out.extra.get("stall_moves_used").and_then(Value::as_i64),
        Some(1)
    );
}

#[tokio::test]
async fn stall_recovery_consume_declare_blocked_produce_blocked() {
    // Consumo DeclareBlocked: chiusura DIRETTA con esito strutturato blocked +
    // blocker validato ADR 0034 (credential). Chiude il loop senza turno LLM libero.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("x"));
    let ctx = ctx_with(llm.clone(), false);
    let epoch = stall_work_epoch(0, 0, 0);
    let mv = RecoveryMove::DeclareBlocked {
        blocker: "credential".into(),
    };
    let state = state_rientro("repeated_user_question", epoch, Some(mv));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    let outcome = out.declared_outcome.as_ref().expect("declared_outcome");
    assert_eq!(
        outcome.get("outcome").and_then(Value::as_str),
        Some("blocked")
    );
    assert_eq!(
        outcome.get("blocker").and_then(Value::as_str),
        Some("credential")
    );
    assert_eq!(
        out.extra.get("stall_moves_used").and_then(Value::as_i64),
        Some(1)
    );
}

// ── R1: redaction_rejected da SEGNALE STRUTTURATO nello StallContext ──────────

/// Come [`state_stallo_repeated_action`] ma AGGIUNGE in coda un tool_result che
/// porta il CODICE STRUTTURATO [REDACTION_REJECTED] (la fonte ha rifiutato un
/// input per placeholder di redazione), SENZA alterare la sequenza write-fallito
/// che fa scattare l'asse repeated_action. Prova che lo StallContext espone
/// `redaction_rejected` dal segnale strutturato (regola M), non dal placeholder.
fn state_stallo_con_redazione_rifiutata() -> AgentState {
    let mut s = state_stallo_repeated_action();
    s.messages.push(Message::Human {
        content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "cr".into(),
            content: Value::String(
                "\u{274C} [REDACTION_REJECTED] [BLOCCATO — placeholder di redazione nell'input]"
                    .into(),
            ),
            is_error: true,
            exit_code: None,
        }]),
    });
    s
}

#[tokio::test]
async fn stall_context_redaction_rejected_da_segnale_strutturato() {
    // R1: quando un tool_result recente porta il codice strutturato
    // [REDACTION_REJECTED], lo StallContext emesso ha redaction_rejected=true
    // (regola M: dal segnale, mai dal contains sul placeholder umano).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("x")), false);
    let state = state_stallo_con_redazione_rifiutata();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::StallReason));
    let sc: StallContext = serde_json::from_value(
        out.extra
            .get(STALL_CONTEXT_KEY)
            .expect("StallContext")
            .clone(),
    )
    .expect("StallContext");
    assert!(
        sc.redaction_rejected,
        "il segnale strutturato deve alzare il flag"
    );
}

#[tokio::test]
async fn stall_context_redaction_rejected_false_senza_segnale() {
    // Contro-prova: lo stesso stallo SENZA il codice strutturato ->
    // redaction_rejected=false (nessuna deduzione dal testo).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("x")), false);
    let state = state_stallo_repeated_action();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    let sc: StallContext = serde_json::from_value(
        out.extra
            .get(STALL_CONTEXT_KEY)
            .expect("StallContext")
            .clone(),
    )
    .expect("StallContext");
    assert!(
        !sc.redaction_rejected,
        "senza segnale strutturato resta false"
    );
}

// ── R2: budget stall CROSS-RUN (StallBudgetPort) ──────────────────────────────

/// Porta budget stub configurabile: ritorna un conteggio fisso (cross-run) o un
/// errore (per il ramo fail-open) e registra le scritture.
struct StubStallBudget {
    /// Conteggio cross-run ritornato da `consultations_in_session`.
    count: i64,
    /// Se `true`, `consultations_in_session` ritorna `Err` (test fail-open).
    fail_read: bool,
    /// Registra il numero di `record_consultation` in Real (per asserire l'append).
    recorded: std::sync::Mutex<i64>,
}

impl StubStallBudget {
    fn with_count(count: i64) -> Self {
        Self {
            count,
            fail_read: false,
            recorded: std::sync::Mutex::new(0),
        }
    }
    fn failing_read() -> Self {
        Self {
            count: 0,
            fail_read: true,
            recorded: std::sync::Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl crate::runtime::ports::StallBudgetPort for StubStallBudget {
    async fn consultations_in_session(
        &self,
        _session_id: Uuid,
    ) -> Result<i64, crate::runtime::ports::PortError> {
        if self.fail_read {
            return Err(crate::runtime::ports::PortError::Llm("read down".into()));
        }
        Ok(self.count)
    }
    async fn record_consultation(
        &self,
        _session_id: Uuid,
        mode: crate::runtime::ports::ExecMode,
    ) -> Result<(), crate::runtime::ports::PortError> {
        if mode == crate::runtime::ports::ExecMode::Real {
            *self.recorded.lock().unwrap() += 1;
        }
        Ok(())
    }
}

/// Nodo con la porta budget stall iniettata (per i test cross-run).
fn node_budget(
    cfg: ExecutorConfig,
    budget: Arc<dyn crate::runtime::ports::StallBudgetPort>,
) -> ExecutorNode {
    let (n, _m, _s) = node(cfg, Arc::new(StubRunControlStore::default()));
    n.with_stall_budget(budget)
}

#[tokio::test]
async fn stall_budget_cross_run_esaurito_ricade_su_fissa() {
    // Il per-run e' 0 (extra vuoto) ma il CROSS-RUN letto dalla porta raggiunge il
    // cap per-sessione: il gate NON emette StallReason (chiude il loop email
    // cross-run che il solo cap per-run non fermava).
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 3,
        ..cfg_resolved()
    };
    let n = node_budget(cfg, Arc::new(StubStallBudget::with_count(3)));
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("non chiamato")), false);
    let state = state_stallo_repeated_action();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(out.stop_reason, Some(StopReason::StallReason));
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
}

#[tokio::test]
async fn stall_budget_cross_run_somma_al_per_run() {
    // La somma per-run (extra) + cross-run (porta) raggiunge il cap: il gate non
    // emette, anche se nessuno dei due da solo lo supera.
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 3,
        ..cfg_resolved()
    };
    let n = node_budget(cfg, Arc::new(StubStallBudget::with_count(2)));
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("x")), false);
    let mut state = state_stallo_repeated_action();
    state.extra.insert("stall_moves_used".to_string(), json!(1)); // 1 + 2 == cap 3
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(out.stop_reason, Some(StopReason::StallReason));
}

#[tokio::test]
async fn stall_budget_cross_run_sotto_cap_emette() {
    // Cross-run sotto il cap: il gate emette StallReason (comportamento invariato
    // rispetto al solo per-run). Prova che la porta non blocca quando c'e' margine.
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        ..cfg_resolved()
    };
    let n = node_budget(cfg, Arc::new(StubStallBudget::with_count(2)));
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("x")), false);
    let state = state_stallo_repeated_action();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::StallReason));
}

#[tokio::test]
async fn stall_budget_cross_run_fail_open_non_blocca() {
    // FAIL-OPEN: la porta budget in errore -> conteggio 0 -> il gate NON viene
    // bloccato da un guasto di lettura (emette StallReason come se il cross-run
    // fosse a 0). Un guasto infrastrutturale non deve impedire il recovery.
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 3,
        ..cfg_resolved()
    };
    let n = node_budget(cfg, Arc::new(StubStallBudget::failing_read()));
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("x")), false);
    let state = state_stallo_repeated_action();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::StallReason));
}

#[tokio::test]
async fn stall_budget_consumo_registra_cross_run() {
    // Al consumo di una mossa applicata la porta registra la consultazione
    // cross-run (append per-sessione), oltre a incrementare il per-run in extra.
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        ..cfg_resolved()
    };
    let budget = Arc::new(StubStallBudget::with_count(0));
    let n = node_budget(cfg, budget.clone());
    let ctx = ctx_with(Arc::new(StubLlmGateway::with_text("x")), false);
    let epoch = stall_work_epoch(0, 0, 0);
    let mv = RecoveryMove::DeclareBlocked {
        blocker: "credential".into(),
    };
    let state = state_rientro("repeated_user_question", epoch, Some(mv));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Per-run incrementato E cross-run registrato una volta (Real).
    assert_eq!(
        out.extra.get("stall_moves_used").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        *budget.recorded.lock().unwrap(),
        1,
        "una consultazione cross-run registrata"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  SCALE-CONTROLLER (PR-B3): detector-emissione + rientro-applicazione.
// ──────────────────────────────────────────────────────────────────────────

use crate::nodes::scale_control::{SCALE_CONTEXT_KEY as SCALE_CTX_KEY, SCALE_MOVE_CACHE_KEY_KEY};
use crate::runtime::ports::{ScaleContext, ScaleMove, ScaleTier};

/// Config con lo scale-controller ATTIVO (flag ON) e soglie seed mig 0516. Per i
/// test del detector-emissione.
fn cfg_scale_on() -> ExecutorConfig {
    ExecutorConfig {
        scale: ScaleConfig {
            enabled: true,
            eval_every_iters: 4,
            min_tail_iters: 6,
            max_evals_per_run: 6,
            window_overhead_ratio: 1.3,
            ..ScaleConfig::default()
        },
        ..cfg_resolved()
    }
}

/// LLM stub che emette una tool call CON `stop_reason="tool_use"` (come il path
/// reale del provider): il turno prosegue con tool pendenti e `stop_reason_enum`
/// e' `ToolUse` (lo `with_tool_call` base non imposta lo stop_reason -> EndTurn).
fn llm_tool_use(name: &str, input: Value) -> Arc<StubLlmGateway> {
    Arc::new(StubLlmGateway {
        canned: LlmResponse {
            content: String::new(),
            tool_calls: vec![ToolUse {
                id: "stub-tc".to_string(),
                name: name.to_string(),
                input,
                thought_signature: None,
            }],
            usage: LlmUsage::default(),
            stop_reason: Some("tool_use".into()),
            ..Default::default()
        },
        error: None,
        error_provider_unavailable: false,
        provider_unavailable_cause: None,
        seen: std::sync::Mutex::new(vec![]),
    })
}

/// Stato con un turno che PROSEGUE (ToolUse): iterations a cadenza (4 % 4 == 0),
/// coda ampia (cap 60 - 4 = 56 >= min_tail 6). Nessuno stallo (tool_result pulito).
fn state_scale_turn() -> AgentState {
    AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua il lavoro")],
        action_oriented: Some(true),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        iterations: Some(4),
        token_budget: Some(400),
        ..Default::default()
    }
}

#[tokio::test]
async fn scale_off_non_emette_scale_reason() {
    // (a) Flag OFF (default): il detector salta PRIMA di ogni lavoro. Il turno
    // ToolUse chiude normale (bit-identico), nessuno ScaleContext in extra.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc); // scale.enabled=false di default
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_scale_turn();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(
        out.stop_reason,
        Some(StopReason::ToolUse),
        "turno normale, non ScaleReason"
    );
    assert!(
        out.extra.get(SCALE_CTX_KEY).is_none(),
        "nessuno ScaleContext a flag OFF"
    );
}

#[tokio::test]
async fn scale_on_trigger_emette_scale_reason() {
    // (b) Flag ON + cadenza + coda ampia -> il detector emette ScaleReason con lo
    // ScaleContext strutturato e la chiave-cache in extra. FIX-A: il detector e'
    // PRE-LLM, quindi la `complete` del turno NON e' stata chiamata (nessun turno
    // LLM produttivo scartato; in shadow-replay non consuma un gruppo dal cursore).
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_scale_on(), rc);
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_scale_turn();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::ScaleReason));
    let ctx_val = out.extra.get(SCALE_CTX_KEY).expect("ScaleContext in extra");
    let sc: ScaleContext = serde_json::from_value(ctx_val.clone()).expect("ScaleContext");
    // current_tier assente nello stato -> fallback deterministico Medium.
    assert_eq!(sc.current_tier, ScaleTier::Medium);
    assert_eq!(sc.iterations, 4);
    // La chiave-cache e' trasportata (il nodo la legge senza ricalcolarla).
    assert!(out.extra.get(SCALE_MOVE_CACHE_KEY_KEY).is_some());
    // FIX-B: le soglie DB-driven del gate anti-oscillazione sono trasportate al nodo.
    assert!(
        out.extra.get("scale_hysteresis_cfg").is_some(),
        "la ScaleHysteresisConfig DB-driven deve raggiungere il nodo (FIX-B)"
    );
    // Budget consultazioni incrementato.
    assert_eq!(
        out.extra.get("scale_evals_used").and_then(Value::as_i64),
        Some(1)
    );
    // FIX-A/F1/F4/F6: emissione PRE-LLM -> la `complete` del turno NON e' avvenuta.
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "emissione ScaleReason pre-LLM: nessuna chiamata complete (replay-safe, niente turno scartato)"
    );
}

#[tokio::test]
async fn scale_precedenza_stallo_no_scale_reason() {
    // (c) Precedenza stallo (FIX-E): con un tool_result ERRORE recente lo stallo e'
    // attivo questo turno -> il detector NON emette ScaleReason (l'escalation
    // reattiva ha priorita' sul risparmio pre-emptivo). Il turno chiude normale.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_scale_on(), rc);
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = state_scale_turn();
    // Ultimo tool_result = errore -> detect_recent_tool_error true -> stall_active.
    // `Message::Tool` (ToolMessage) con hint errore: e' la forma che
    // `detect_recent_tool_error` scandisce (filtra i soli Message::Tool).
    state
        .messages
        .push(ai_tool("write_file", json!({"path": "a.rs"})));
    state.messages.push(Message::Tool {
        tool_call_id: "c1".into(),
        content: MessageContent::text("Error: permission denied"),
    });
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(
        out.stop_reason,
        Some(StopReason::ScaleReason),
        "stallo attivo -> no ScaleReason"
    );
    assert!(out.extra.get(SCALE_CTX_KEY).is_none());
}

#[tokio::test]
async fn scale_break_even_coda_corta_no_trigger() {
    // (e) Break-even: tail_headroom < min_tail_iters -> nessun trigger. iteration_cap
    // basso (8) e iterations 4 -> tail 4 < 6. Il turno chiude normale.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        iteration_cap: 8,
        ..cfg_scale_on()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_scale_turn(); // iterations 4 -> tail 8-4=4 < 6
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(
        out.stop_reason,
        Some(StopReason::ScaleReason),
        "coda corta -> no trigger"
    );
    assert!(out.extra.get(SCALE_CTX_KEY).is_none());
}

/// Stato di RIENTRO dal nodo ScaleControl: `ScaleResolved` + ScaleContext + la
/// ScaleMove persistita alla chiave-cache trasportata.
fn state_scale_rientro(current: ScaleTier, mv: ScaleMove, est_tokens: i64) -> AgentState {
    let scale_ctx = ScaleContext {
        current_tier: current,
        intent_tier_floor: ScaleTier::Light,
        behavior_mode: "automatic".to_string(),
        iterations: 8,
        iteration_cap: 60,
        tail_headroom: 52,
        est_tokens,
        turns_since_change: 5,
        requires_tool_use: true,
        ..Default::default()
    };
    let key = crate::decisions::scale_reason::scale_cache_key(&scale_ctx, 4);
    let mut extra = serde_json::Map::new();
    extra.insert(
        SCALE_CTX_KEY.to_string(),
        serde_json::to_value(&scale_ctx).expect("serialize ScaleContext"),
    );
    extra.insert(SCALE_MOVE_CACHE_KEY_KEY.to_string(), json!(key));
    extra.insert(key, serde_json::to_value(&mv).expect("serialize ScaleMove"));
    AgentState {
        thread_id: Some("r1".into()),
        stop_reason: Some(StopReason::ScaleResolved),
        messages: vec![human("task")],
        tools_json: Some(vec![json!({"name": "write_file"})]),
        iterations: Some(8),
        extra,
        ..Default::default()
    }
}

#[tokio::test]
async fn scale_rientro_downscale_con_modello_applica_sticky() {
    // (d) DownscaleTo con modello del tier disponibile -> sticky + current_tier
    // aggiornati, self-loop G1Escalated (ri-fa il turno). La porta risolve un
    // modello del tier target col vincolo finestra (FIX-B).
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    // La porta risolve (provider, model) per il tier target.
    let upscale = Arc::new(StubModelUpscalePort::tier_resolving(
        "mistral",
        "mistral-small",
    ));
    let (n, _m) = node_ports(cfg_scale_on(), rc, next_actions, billing, upscale.clone());
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_scale_rientro(
        ScaleTier::Medium,
        ScaleMove::DownscaleTo {
            tier: ScaleTier::Light,
            confidence: 0.9,
        },
        5000,
    );
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(
        out.stop_reason,
        Some(StopReason::G1Escalated),
        "self-loop ri-fa il turno"
    );
    assert_eq!(out.sticky_provider.as_deref(), Some("mistral"));
    assert_eq!(out.sticky_model.as_deref(), Some("mistral-small"));
    assert_eq!(
        out.current_tier.as_deref(),
        Some("light"),
        "current_tier al tier target"
    );
    // FIX-B: la porta e' stata chiamata col vincolo finestra (est 5000 * 1.3 = 6500).
    let calls = upscale.tier_selected.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "light");
    assert_eq!(calls[0].1, 6500);
    // L'LLM del turno NON e' chiamato (il rientro ritorna prima della chiamata).
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn scale_rientro_downscale_senza_modello_annulla() {
    // (d-bis) DownscaleTo ma nessun modello del tier con finestra sufficiente
    // (porta -> None): fail-safe, il cambio e' ANNULLATO. Lo sticky resta invariato
    // (None nel delta): il turno prosegue col modello corrente.
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    // Porta di default: tier_pick None -> nessun modello del tier.
    let upscale = Arc::new(StubModelUpscalePort::default());
    let (n, _m) = node_ports(cfg_scale_on(), rc, next_actions, billing, upscale.clone());
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let state = state_scale_rientro(
        ScaleTier::Medium,
        ScaleMove::DownscaleTo {
            tier: ScaleTier::Light,
            confidence: 0.9,
        },
        5000,
    );
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Cambio annullato: il rientro ritorna None -> il turno prosegue normalmente
    // (nessun sticky/current_tier scritto dal cambio annullato).
    assert_ne!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_ne!(
        out.current_tier.as_deref(),
        Some("light"),
        "downscale annullato: tier invariato"
    );
    // La porta e' stata interrogata (poi ha ritornato None -> annullo).
    assert_eq!(upscale.tier_selected.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn scale_replay_detector_non_emette_doppia_complete() {
    // (F1) In Replay il detector-emissione e' PRE-LLM: se emette ScaleReason,
    // il superstep NON chiama `complete` (nessun consumo di un gruppo dal cursore
    // replay); se prosegue, chiama `complete` UNA sola volta. In entrambi i casi il
    // conteggio complete di QUESTO superstep e' <= 1, mai 2. Qui il turno emette
    // ScaleReason -> zero complete nel superstep di emissione (allineato al gemello
    // stallo, che e' gia' pre-LLM). Prima del fix (detector post-LLM) lo shadow
    // consumava un gruppo in emissione + un secondo al rientro annullato = cursore
    // disallineato.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_scale_on(), rc);
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    // shadow=true -> ExecMode::Replay.
    let ctx = ctx_with(llm.clone(), true);
    let state = state_scale_turn();
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(
        out.stop_reason,
        Some(StopReason::ScaleReason),
        "detector emette anche in Replay"
    );
    // Il superstep di emissione NON ha chiamato complete (replay-safe: nessun
    // gruppo consumato dal cursore).
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "emissione pre-LLM in Replay: zero complete nel superstep (cursore allineato al primario)"
    );
}

#[tokio::test]
async fn scale_rientro_keeptier_prosegue_una_sola_complete() {
    // (F4/F6) Rientro con KeepTier (mossa assente in cache): `consume_scale_move`
    // ritorna None e il turno PROSEGUE con UNA sola chiamata LLM (nessun turno
    // produttivo scartato e rifatto). Prima del fix il detector post-LLM scartava
    // la `complete` gia' fatta e il rientro ne innescava una seconda.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_scale_on(), rc);
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    // Rientro ScaleResolved con ScaleContext ma NESSUNA mossa persistita alla
    // chiave-cache (KeepTier non viene persistito dal nodo).
    let scale_ctx = ScaleContext {
        current_tier: ScaleTier::Medium,
        intent_tier_floor: ScaleTier::Light,
        behavior_mode: "automatic".to_string(),
        iterations: 8,
        iteration_cap: 60,
        tail_headroom: 52,
        turns_since_change: 5,
        requires_tool_use: true,
        ..Default::default()
    };
    let key = crate::decisions::scale_reason::scale_cache_key(&scale_ctx, 4);
    let mut extra = serde_json::Map::new();
    extra.insert(
        SCALE_CTX_KEY.to_string(),
        serde_json::to_value(&scale_ctx).unwrap(),
    );
    extra.insert(SCALE_MOVE_CACHE_KEY_KEY.to_string(), json!(key));
    // NB: nessuna chiave `key` -> KeepTier (mossa assente).
    let state = AgentState {
        thread_id: Some("r1".into()),
        stop_reason: Some(StopReason::ScaleResolved),
        messages: vec![human("task")],
        tools_json: Some(vec![json!({"name": "write_file"})]),
        iterations: Some(8),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Il turno prosegue e produce il ToolUse col modello corrente.
    assert_eq!(
        out.stop_reason,
        Some(StopReason::ToolUse),
        "KeepTier: prosegue il turno"
    );
    // UNA sola chiamata LLM (nessun turno scartato/rifatto).
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "KeepTier al rientro: una sola complete, nessun turno produttivo scartato (F4/F6)"
    );
}

#[tokio::test]
async fn scale_tetto_cambi_tier_pinna_heavy() {
    // (F5) Al raggiungimento di `max_tier_changes_per_run` il rientro forza il
    // cambio a Heavy (pin-UP) e marca `scale_pinned_heavy`, disattivando ulteriori
    // cambi. Qui cap=1: il PRIMO cambio (una DownscaleTo Light) e' rimpiazzato dal
    // pin a Heavy. La porta e' interrogata sul tier "heavy", non "light".
    let rc = Arc::new(StubRunControlStore::default());
    let next_actions = Arc::new(StubNextActionsDeriver::default());
    let billing = Arc::new(StubBillingCooldownPort::default());
    let upscale = Arc::new(StubModelUpscalePort::tier_resolving(
        "anthropic",
        "claude-heavy",
    ));
    let cfg = ExecutorConfig {
        scale: ScaleConfig {
            enabled: true,
            eval_every_iters: 4,
            min_tail_iters: 6,
            max_evals_per_run: 6,
            window_overhead_ratio: 1.3,
            max_tier_changes_per_run: 1, // tetto strettissimo: pinna heavy dopo 1 cambio
            ..ScaleConfig::default()
        },
        ..cfg_resolved()
    };
    let (n, _m) = node_ports(cfg, rc, next_actions, billing, upscale.clone());
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    // Rientro con una DownscaleTo Light (l'LLM proponeva un downscale) e current=Medium.
    let state = state_scale_rientro(
        ScaleTier::Medium,
        ScaleMove::DownscaleTo {
            tier: ScaleTier::Light,
            confidence: 0.9,
        },
        5000,
    );
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(
        out.stop_reason,
        Some(StopReason::G1Escalated),
        "cambio applicato (pin heavy)"
    );
    // Pin-UP a Heavy invece del Light proposto.
    assert_eq!(
        out.current_tier.as_deref(),
        Some("heavy"),
        "tetto raggiunto -> pin a Heavy"
    );
    // La porta e' stata interrogata sul tier HEAVY, non light.
    let calls = upscale.tier_selected.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0, "heavy",
        "risolve il modello del tier pinnato (heavy)"
    );
    // Contatore cambi + flag pin persistiti.
    assert_eq!(
        out.extra
            .get("scale_tier_changes_used")
            .and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        out.extra.get("scale_pinned_heavy").and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn scale_pinned_heavy_disattiva_detector() {
    // (F5) Con `scale_pinned_heavy=true` il detector-emissione NON emette piu'
    // ScaleReason (controller disattivato per il resto del run): il turno prosegue
    // normalmente col modello corrente.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_scale_on(), rc);
    let llm = llm_tool_use("write_file", json!({"path": "a.rs"}));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = state_scale_turn();
    state
        .extra
        .insert("scale_pinned_heavy".to_string(), json!(true));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(
        out.stop_reason,
        Some(StopReason::ScaleReason),
        "pinnato heavy -> nessuna emissione"
    );
    assert!(
        out.extra.get(SCALE_CTX_KEY).is_none(),
        "detector disattivato dal pin"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  Limiti anti-runaway basati sui TOKEN (mig 0520)
// ──────────────────────────────────────────────────────────────────────────

/// Stub LLM che ritorna un turno SOLO-TESTO con un `usage` dato (input+output):
/// esercita il conteggio `tokens_used_total` dal segnale strutturato dell'usage.
fn llm_text_usage(text: &str, prompt: i64, completion: i64) -> Arc<StubLlmGateway> {
    Arc::new(StubLlmGateway {
        canned: LlmResponse {
            content: text.to_string(),
            tool_calls: vec![],
            usage: LlmUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
                ..LlmUsage::default()
            },
            ..Default::default()
        },
        error: None,
        error_provider_unavailable: false,
        provider_unavailable_cause: None,
        seen: std::sync::Mutex::new(vec![]),
    })
}

#[tokio::test]
async fn run_token_budget_oltre_soglia_forza_chiusura_senza_modello() {
    // Il run ha gia' consumato >= run_token_budget: il ramo PRE-LLM chiude
    // deterministicamente (forced_close_unverified, EndTurn) SENZA chiamare il
    // modello, come il cap iterazioni. Motivo strutturato nel meta_step.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 400_000,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        tokens_used_total: Some(400_001),
        iterations: Some(10),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert_eq!(out.iterations, Some(11));
    // Motivo strutturato (regola M): meta_step con reason budget_token_esaurito.
    let ms = out
        .meta_steps
        .iter()
        .find(|m| m.kind == "anti_runaway")
        .expect("meta_step anti_runaway");
    assert_eq!(
        ms.payload.get("reason").and_then(Value::as_str),
        Some("budget_token_esaurito")
    );
    // LLM NON chiamato: chiusura prima del modello.
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn run_token_budget_sotto_soglia_prosegue() {
    // Sotto la soglia il ramo NON scatta: il turno prosegue (chiamata LLM).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 400_000,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("Ecco la risposta."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        tokens_used_total: Some(399_999),
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Il turno e' avvenuto normalmente.
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.result.as_deref(), Some("Ecco la risposta."));
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn tokens_used_total_accumula_usage_del_turno() {
    // Il contatore CUMULATIVO somma i token del turno dal segnale strutturato
    // dell'usage (input+output), sopra il valore portato dallo stato.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = llm_text_usage("risposta", 1_000, 500);
    let ctx = ctx_with(llm, false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        tokens_used_total: Some(2_000),
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // 2000 (prev) + 1500 (delta prompt 1000 da 0 + completion 500) = 3500.
    assert_eq!(out.tokens_used_total, Some(3_500));
}

#[tokio::test]
async fn budget_conta_prompt_incrementale_non_la_history_ripetuta() {
    // REGRESSIONE run 8c4f5eea: la history viene ri-inviata a OGNI turno; il
    // budget deve contare solo il DELTA del prompt (contesto nuovo) + output,
    // non il prompt lordo, altrimenti un run SANO con contesto grande esaurisce
    // il budget in pochi turni (cascata di escalation "non-convergenza").
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    // Turno precedente: prompt 50_000 (history grande). Turno corrente: 51_000
    // prompt (1_000 di contesto nuovo) + 200 completion.
    let llm = llm_text_usage("risposta", 51_000, 200);
    let ctx = ctx_with(llm, false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        tokens_used_total: Some(2_000),
        prompt_tokens: Some(50_000),
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // 2000 + (51000-50000) + 200 = 3200 — NON 2000+51200=53200.
    assert_eq!(out.tokens_used_total, Some(3_200));
}

#[tokio::test]
async fn budget_prompt_compresso_clampa_a_zero_il_delta() {
    // Dopo una compressione della history il prompt SCENDE: il delta negativo
    // clampa a 0 (nessun "rimborso" di budget), conta solo l'output.
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = llm_text_usage("risposta", 30_000, 400);
    let ctx = ctx_with(llm, false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        tokens_used_total: Some(5_000),
        prompt_tokens: Some(50_000),
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // 5000 + max(0, 30000-50000) + 400 = 5400.
    assert_eq!(out.tokens_used_total, Some(5_400));
}

#[tokio::test]
async fn max_consecutive_text_only_turns_oltre_soglia_forza_chiusura() {
    // A N turni solo-testo consecutivi (>= soglia) il ramo PRE-LLM chiude
    // deterministicamente: fast-fail sul modello che descrive senza agire.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        max_consecutive_text_only_turns: 3,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea il file")],
        consecutive_text_only_turns: Some(3),
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    let ms = out
        .meta_steps
        .iter()
        .find(|m| m.kind == "anti_runaway")
        .expect("meta_step anti_runaway");
    assert_eq!(
        ms.payload.get("reason").and_then(Value::as_str),
        Some("text_only_stallo")
    );
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn figura_al_cap_text_only_riceve_turno_di_grazia_invece_di_nd() {
    // Una figura del consiglio (advisory_verdict fra i tool) senza parere, al cap
    // solo-testo, NON chiude n/d: riceve UN turno di grazia per emettere il parere.
    // Copre il caso reale fe4dc12c (functional_analyst deepseek, it=60): il
    // meta-reasoner non interviene (budget stall esaurito) e prima si andava dritti
    // a close_runaway.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        max_consecutive_text_only_turns: 3,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("analizza l'auth")],
        consecutive_text_only_turns: Some(3),
        iterations: Some(5),
        tools_json: Some(vec![
            json!({"name": "read_file"}),
            json!({"name": "advisory_verdict"}),
        ]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // NON chiude: turno di grazia (G1Escalated), niente meta_step anti_runaway.
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_ne!(out.forced_close_unverified, Some(true));
    assert!(
        !out.meta_steps.iter().any(|m| m.kind == "anti_runaway"),
        "la figura non chiude n/d: riceve la grazia"
    );
    // Flag una-tantum settato + streak solo-testo azzerato (il turno raggiunge l'LLM).
    assert_eq!(
        out.extra.get("advisory_grace_used").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(out.consecutive_text_only_turns, Some(0));
    // Direttiva di grazia iniettata come messaggio Human.
    let injected = out
        .messages
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::Human { content } => Some(content.flatten_text()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        injected.contains("advisory_verdict"),
        "direttiva di grazia iniettata: {injected}"
    );
    // L'LLM NON e' chiamato: delta PRE-LLM che ri-da il turno.
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn figura_grazia_una_tantum_poi_chiude() {
    // Se la grazia e' GIA' stata concessa (flag in extra), al cap solo-testo la
    // figura chiude col backstop: la grazia non si ripete (niente loop).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        max_consecutive_text_only_turns: 3,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut extra = serde_json::Map::new();
    extra.insert("advisory_grace_used".into(), json!(true));
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("analizza l'auth")],
        consecutive_text_only_turns: Some(3),
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "advisory_verdict"})]),
        extra,
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Grazia gia' usata -> chiusura backstop normale (nessun loop).
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert!(out.meta_steps.iter().any(|m| m.kind == "anti_runaway"));
}

#[tokio::test]
async fn consecutive_text_only_turns_incrementa_su_testo_e_azzera_su_tool() {
    // Il contatore si INCREMENTA su un turno solo-testo (nessun tool_use) e si
    // AZZERA appena il modello emette un tool_use (segnale strutturato).
    let rc = Arc::new(StubRunControlStore::default());

    // (a) Turno solo-testo: da 1 -> 2. Soglia alta per non farlo scattare.
    let cfg = ExecutorConfig {
        max_consecutive_text_only_turns: 10,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg.clone(), rc.clone());
    let llm = Arc::new(StubLlmGateway::with_text("descrivo soltanto"));
    let ctx = ctx_with(llm, false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea il file")],
        consecutive_text_only_turns: Some(1),
        action_oriented: Some(true),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.consecutive_text_only_turns, Some(2));

    // (b) Turno con tool_use: azzera a 0.
    let (n2, _m2, _s2) = node(cfg, rc);
    let llm2 = llm_tool_call("write_file", json!({"path": "a.rs"}));
    let ctx2 = ctx_with(llm2, false);
    let state2 = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea il file")],
        consecutive_text_only_turns: Some(2),
        action_oriented: Some(true),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta2 = n2.run(&state2, &ctx2).await.expect("run");
    let out2 = apply(state2, delta2);
    assert_eq!(out2.consecutive_text_only_turns, Some(0));
}

#[tokio::test]
async fn limiti_token_disabilitati_a_zero_sono_retro_compatibili() {
    // Con entrambi i limiti a 0 (disabilitati) i rami PRE-LLM NON scattano MAI,
    // anche con contatori altissimi nello stato: comportamento bit-identico a
    // prima (il turno prosegue e chiama il modello).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 0,
        // Anche il backstop hard-cap disabilitato (a 0): il test verifica la
        // retro-compat "limiti token tutti spenti -> nessuna chiusura anti-runaway".
        run_token_hard_cap: 0,
        max_consecutive_text_only_turns: 0,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("Ecco la risposta."));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("dammi un dato")],
        // Valori che, con i limiti attivi, avrebbero chiuso il run.
        tokens_used_total: Some(10_000_000),
        consecutive_text_only_turns: Some(999),
        action_oriented: Some(false),
        tools_json: Some(vec![json!({"name": "read_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Nessuna chiusura anti-runaway: il turno e' avvenuto normalmente.
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.result.as_deref(), Some("Ecco la risposta."));
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    assert!(
        !out.meta_steps.iter().any(|m| m.kind == "anti_runaway"),
        "nessun meta_step anti_runaway coi limiti a 0"
    );
}

#[tokio::test]
async fn run_token_budget_a_flag_on_emette_stall_reason_runaway() {
    // Meta-reasoner ACCESO: al superamento del budget MORBIDO l'executor NON chiude
    // direttamente (close_runaway), ma EMETTE StallReason con StallContext asse
    // "token_overflow" -> instrada al nodo StallRecovery (giudice agentico). Il
    // limite fisso 4d diventa un TRIGGER del giudice.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        run_token_budget: 400_000,
        run_token_hard_cap: 800_000,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea il servizio")],
        // Oltre il budget morbido ma SOTTO l'hard-cap: trigger del giudice.
        tokens_used_total: Some(400_001),
        iterations: Some(10),
        user_intent: Some("crea il servizio".into()),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // NON chiude: instrada al giudice.
    assert_eq!(out.stop_reason, Some(StopReason::StallReason));
    assert!(
        !out.meta_steps.iter().any(|m| m.kind == "anti_runaway"),
        "nessuna chiusura anti_runaway: il giudice decide"
    );
    // StallContext strutturato (regola M): asse token_overflow, count = token usati.
    let ctx_val = out
        .extra
        .get(STALL_CONTEXT_KEY)
        .expect("StallContext in extra");
    let sc: StallContext = serde_json::from_value(ctx_val.clone()).expect("StallContext");
    assert_eq!(sc.axis, "token_overflow");
    assert_eq!(sc.count, 400_001);
    // L'LLM NON e' chiamato dall'executor (lo fara' il nodo dedicato, replay-safe).
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn text_only_a_flag_on_emette_stall_reason_runaway() {
    // Meta-reasoner ACCESO: allo streak solo-testo oltre soglia l'executor EMETTE
    // StallReason con StallContext asse "text_only" invece di chiudere diretto.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        max_consecutive_text_only_turns: 3,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("crea il file")],
        consecutive_text_only_turns: Some(3),
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::StallReason));
    let ctx_val = out
        .extra
        .get(STALL_CONTEXT_KEY)
        .expect("StallContext in extra");
    let sc: StallContext = serde_json::from_value(ctx_val.clone()).expect("StallContext");
    assert_eq!(sc.axis, "text_only");
    assert_eq!(sc.count, 3);
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn token_hard_cap_a_flag_on_chiude_diretto_senza_giudice() {
    // BACKSTOP di catastrofe: oltre l'hard-cap l'executor chiude d'autorita'
    // (close_runaway, reason "token_hard_cap") SENZA consultare il giudice, anche
    // col meta-reasoner acceso. Rete di sicurezza non-negoziabile (regola H).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 6,
        run_token_budget: 400_000,
        run_token_hard_cap: 800_000,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        // Oltre l'hard-cap: chiusura d'autorita', il giudice NON e' consultato.
        tokens_used_total: Some(800_001),
        iterations: Some(20),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Chiusura d'autorita': NON StallReason.
    assert_ne!(out.stop_reason, Some(StopReason::StallReason));
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    assert!(out.extra.get(STALL_CONTEXT_KEY).is_none());
    let ms = out
        .meta_steps
        .iter()
        .find(|m| m.kind == "anti_runaway")
        .expect("meta_step anti_runaway (hard-cap)");
    assert_eq!(
        ms.payload.get("reason").and_then(Value::as_str),
        Some("token_hard_cap")
    );
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn run_cost_budget_oltre_soglia_chiude_diretto() {
    // Freno di SPESA del run (costo cumulativo REALE >= run_cost_budget_usd):
    // chiusura d'autorita' (close_runaway, reason "cost_budget_usd") come l'hard-cap
    // token, senza chiamare il modello. run_token_budget/hard_cap=0 isolano il cost cap.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 0,
        run_token_hard_cap: 0,
        run_cost_budget_usd: 3.0,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        run_cost_cumulative_usd: Some(3.5), // oltre i $3
        iterations: Some(15),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    let ms = out
        .meta_steps
        .iter()
        .find(|m| m.kind == "anti_runaway")
        .expect("meta_step anti_runaway (cost)");
    assert_eq!(
        ms.payload.get("reason").and_then(Value::as_str),
        Some("cost_budget_usd")
    );
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn run_cost_budget_sotto_soglia_prosegue() {
    // Sotto la soglia di spesa il ramo NON scatta: il turno prosegue (LLM chiamato).
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 0,
        run_token_hard_cap: 0,
        run_cost_budget_usd: 3.0,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("ok"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        run_cost_cumulative_usd: Some(2.5), // sotto i $3
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _out = apply(state, delta);
    // Il ramo cost NON ha chiuso il run prima del modello: l'LLM e' stato chiamato.
    assert!(!llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn run_time_budget_oltre_deadline_chiude_diretto() {
    // Deadline del run (fase 3, mig 0604): epoch di avvio nel PASSATO oltre il
    // budget -> chiusura d'autorita' (close_runaway, reason canonico
    // "time_budget"), senza chiamare il modello. Gemello del cap di spesa.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 0,
        run_token_hard_cap: 0,
        run_cost_budget_usd: 0.0,
        run_time_budget_s: 600,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let started_way_back = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64
        - 3_600; // avviato un'ora fa, budget 600s
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        run_started_at_epoch_s: Some(started_way_back),
        iterations: Some(15),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(out.forced_close_unverified, Some(true));
    let ms = out
        .meta_steps
        .iter()
        .find(|m| m.kind == "anti_runaway")
        .expect("meta_step anti_runaway (time)");
    assert_eq!(
        ms.payload.get("reason").and_then(Value::as_str),
        Some("time_budget")
    );
    assert!(llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn run_time_budget_disattivato_o_senza_epoch_prosegue() {
    // Budget 0 (disattivato) O epoch assente (run precedente alla fase 3):
    // il ramo deadline NON scatta, il turno prosegue (LLM chiamato). Mai un
    // enforcement su un default inventato.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_token_budget: 0,
        run_token_hard_cap: 0,
        run_cost_budget_usd: 0.0,
        run_time_budget_s: 600,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("ok"));
    let ctx = ctx_with(llm.clone(), false);
    let state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        run_started_at_epoch_s: None, // epoch assente -> nessun enforcement
        iterations: Some(5),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    let delta = n.run(&state, &ctx).await.expect("run");
    let _out = apply(state, delta);
    assert!(!llm.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn runaway_budget_esaurito_a_flag_on_ricade_su_close_runaway() {
    // Meta-reasoner acceso ma budget consultazioni per-sessione esaurito: il gate
    // runaway NON emette StallReason (rete di sicurezza) e ricade sul backstop
    // close_runaway (reason "budget_token_esaurito"), come a flag OFF.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        stall_recovery_max_moves_per_session: 2,
        run_token_budget: 400_000,
        run_token_hard_cap: 800_000,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("continua")],
        tokens_used_total: Some(400_001),
        iterations: Some(10),
        tools_json: Some(vec![json!({"name": "write_file"})]),
        ..Default::default()
    };
    state.extra.insert("stall_moves_used".to_string(), json!(2));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    assert_ne!(out.stop_reason, Some(StopReason::StallReason));
    assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    let ms = out
        .meta_steps
        .iter()
        .find(|m| m.kind == "anti_runaway")
        .expect("meta_step anti_runaway (backstop)");
    assert_eq!(
        ms.payload.get("reason").and_then(Value::as_str),
        Some("budget_token_esaurito")
    );
}

#[tokio::test]
async fn runaway_stall_consuma_escalate_model() {
    // Consumazione di una RecoveryMove per un asse RUNAWAY (token_overflow): il
    // rientro StallResolved con EscalateModel promuove il modello riusando il ramo
    // escalation esistente (axis-agnostico). Verifica che il consumo funzioni anche
    // per gli assi runaway, non solo per quelli classici.
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        stall_recovery_enabled: true,
        ..cfg_resolved()
    };
    // Escalation configurata: una catena che promuove a un modello diverso.
    let esc = Arc::new(StubEscalationPort::with_chain(&["claude-y"]));
    let (n, _m, _s) = node_esc(cfg, rc, esc);
    let llm = Arc::new(StubLlmGateway::with_text("x"));
    let ctx = ctx_with(llm.clone(), false);
    let epoch = stall_work_epoch(0, 0, 0);
    let state = state_rientro("token_overflow", epoch, Some(RecoveryMove::EscalateModel));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);
    // Escalation applicata: modello sticky promosso, budget consumato, ri-da il turno.
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert_eq!(out.sticky_model.as_deref(), Some("claude-y"));
    assert_eq!(
        out.extra.get("stall_moves_used").and_then(Value::as_i64),
        Some(1)
    );
}

/// Golden di parita' 1:1 vs Python per la LOGICA DETERMINISTICA del singolo turno
/// (gate testa, ordine nudge, risoluzione provider). Carica
/// `/tmp/golden_executor_node.json` (vedi `gen_golden_executor_node.py`). Riusa i
/// punti unici del nodo (`head_gate`, `pc::decide`, `resolve_provider_model`):
/// la stessa logica del `run`, esercitata in isolamento.
#[cfg(test)]
// ── Turno di grazia figura (elicit advisory_verdict su recovery) ──────────────

#[test]
fn advisory_figure_senza_verdict_e_riconosciuta() {
    // Figura del consiglio: advisory_verdict fra i tool, nessun parere ancora.
    let figura = AgentState {
        tools_json: Some(vec![
            json!({"name": "read_file"}),
            json!({"name": "advisory_verdict"}),
        ]),
        advisory_verdict: None,
        ..Default::default()
    };
    assert!(
        pending_role_channel_grace(&figura).is_some(),
        "figura con canale advisory e senza parere -> grazia attiva"
    );
}

#[test]
fn advisory_figure_con_verdict_gia_dichiarato_non_e_in_grazia() {
    let conclusa = AgentState {
        tools_json: Some(vec![json!({"name": "advisory_verdict"})]),
        advisory_verdict: Some(json!({"verdict": "block", "summary": "x"})),
        ..Default::default()
    };
    assert!(
        pending_role_channel_grace(&conclusa).is_none(),
        "parere gia' dichiarato -> niente grazia (bit-identico)"
    );
}

#[test]
fn avvocato_senza_posizione_e_in_grazia_col_proprio_canale() {
    // L'avvocato del dibattito ha lo stesso problema della figura: se si
    // impantana e tace, la sua tesi resta senza voce e il confronto e' falsato.
    let avvocato = AgentState {
        tools_json: Some(vec![
            json!({"name": "read_file"}),
            json!({"name": "debate_position"}),
        ]),
        debate_position: None,
        ..Default::default()
    };
    let directive = pending_role_channel_grace(&avvocato).expect("grazia per l'avvocato");
    assert!(
        directive.contains("debate_position"),
        "la grazia deve indicare il canale del RUOLO, non quello di un altro"
    );
    // Posizione gia' dichiarata -> niente grazia.
    let concluso = AgentState {
        tools_json: Some(vec![json!({"name": "debate_position"})]),
        debate_position: Some(json!({"assigned_position": "A", "stance": "support"})),
        ..Default::default()
    };
    assert!(pending_role_channel_grace(&concluso).is_none());
}

#[test]
fn run_principale_e_revisore_non_sono_figure() {
    // Run principale: task_complete, niente canale di ruolo.
    let principale = AgentState {
        tools_json: Some(vec![
            json!({"name": "task_complete"}),
            json!({"name": "edit_file"}),
        ]),
        advisory_verdict: None,
        ..Default::default()
    };
    assert!(pending_role_channel_grace(&principale).is_none());
    // Revisore: review_verdict, che NON ha turno di grazia (comportamento
    // storico invariato: chiude col backstop come prima).
    let revisore = AgentState {
        tools_json: Some(vec![json!({"name": "review_verdict"})]),
        advisory_verdict: None,
        ..Default::default()
    };
    assert!(pending_role_channel_grace(&revisore).is_none());
    // Nessun tool -> nessun canale di ruolo.
    let vuoto = AgentState {
        tools_json: None,
        advisory_verdict: None,
        ..Default::default()
    };
    assert!(pending_role_channel_grace(&vuoto).is_none());
}

#[test]
fn recovery_nudge_appende_la_grazia_solo_alle_figure() {
    let figura = AgentState {
        tools_json: Some(vec![json!({"name": "advisory_verdict"})]),
        advisory_verdict: None,
        ..Default::default()
    };
    let msg = recovery_nudge_msg(&figura, "diagnostica la causa radice");
    let Message::Human { content } = msg else {
        panic!("atteso Message::Human")
    };
    let text = content.flatten_text();
    assert!(
        text.contains("diagnostica la causa radice"),
        "conserva il nudge del reasoner: {text}"
    );
    assert!(
        text.contains("advisory_verdict"),
        "appende la direttiva di grazia: {text}"
    );

    // Run non-figura: bit-identico al nudge nudo.
    let principale = AgentState {
        tools_json: Some(vec![json!({"name": "task_complete"})]),
        advisory_verdict: None,
        ..Default::default()
    };
    let msg2 = recovery_nudge_msg(&principale, "diagnostica la causa radice");
    let Message::Human { content } = msg2 else {
        panic!("atteso Message::Human")
    };
    assert_eq!(
        content.flatten_text(),
        "diagnostica la causa radice",
        "non-figura: nudge invariato"
    );
}

mod golden {
    use super::*;
    use crate::decisions::progress_controller::{self as pc, ProgressSignals};
    use crate::nodes::executor::{
        action_str, head_gate, resolve_provider_model, HeadGate, ProviderResolution,
    };
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
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let repeated_action: Option<(String, i64)> = input.get("repeated_action").and_then(|v| {
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
            input
                .get("routing_provider")
                .and_then(Value::as_str)
                .unwrap_or(""),
            input
                .get("routing_model")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        match res {
            ProviderResolution::Resolved(p, m) => {
                json!({"provider": p, "model": m, "no_provider": false})
            }
            ProviderResolution::NoProvider(p) => {
                // Il Python espone provider/model risolti anche nel ramo no_provider.
                let m = input
                    .get("routing_model")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({"provider": p, "model": m, "no_provider": true})
            }
        }
    }

    #[test]
    fn head_gate_sopprime_declared_done_in_correzione_final_gate() {
        assert_eq!(
            head_gate(false, true, 3, false, true),
            HeadGate::Proceed
        );
        assert_eq!(
            head_gate(false, true, 3, false, false),
            HeadGate::DeclaredDone
        );
    }

    #[test]
    fn forced_text_soppresso_in_correzione_final_gate() {
        use super::forced_text_turn_active;
        assert!(
            !forced_text_turn_active(56, 55, Some(StopReason::ToolUse), true, true),
            "correzione final_gate: i tool devono restare disponibili"
        );
        assert!(
            forced_text_turn_active(56, 55, Some(StopReason::ToolUse), false, true),
            "fuori correzione final_gate la finestra forced-text resta attiva"
        );
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
        assert!(
            cases.len() >= 20,
            "attesi >= 20 casi, trovati {}",
            cases.len()
        );
        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.group.as_str() {
                "head_gate" => {
                    let inp = &c.input;
                    let h = head_gate(
                        inp.get("superseded")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        inp.get("declared_done")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        inp.get("declared_done_count")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                        inp.get("g1_cap_reached")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        inp.get("final_gate_correction_active")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
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
                                        thought_signature: None,
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
                            thinking_signature: None,
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
        assert!(
            cases.len() >= 15,
            "attesi >= 15 casi, trovati {}",
            cases.len()
        );
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
                    let cnt = c
                        .input
                        .get("exploration_count")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let thr = c
                        .input
                        .get("exploration_threshold")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let ex: Vec<String> = c
                        .input
                        .get("exhausted")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
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
                    let en = c
                        .input
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let est = c
                        .input
                        .get("est_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let win = c
                        .input
                        .get("current_window")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    Value::Bool(should_upscale(en, est, win))
                }
                "upscale_required" => {
                    let est = c
                        .input
                        .get("est_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    let ov = c
                        .input
                        .get("overhead")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0);
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
        let calls = wire[1]
            .tool_calls
            .as_ref()
            .expect("assistant deve avere tool_calls");
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
                thinking_signature: None,
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

// ── complete_or_cancel: cancellazione COOPERATIVA durante la chiamata ──────────
// A livello TOP del modulo `tests` (super = `executor`): qui `use super::*`
// (in testa al file) porta in scope l'helper privato `complete_or_cancel`.

/// Il cuore del fix "Stop -> completed" (incidente 18/07): con lo Stop gia'
/// segnalato nel DB, una chiamata LENTA (qui 300ms, in produzione 90-150s sotto
/// carico) deve essere ABORTITA dalla corsa PRIMA di rientrare. Senza il fix il
/// gate la vedrebbe solo a fine chiamata: la mutazione (helper che non cancella)
/// lascia rientrare la chiamata -> `Completed` -> questo assert ROSSEGGIA.
#[tokio::test]
async fn complete_or_cancel_interrompe_chiamata_lenta() {
    let rc = StubRunControlStore {
        superseded: true,
        ..Default::default()
    };
    let slow = async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok::<_, PortError>(LlmResponse::default())
    };
    let out = complete_or_cancel(slow, &rc, "run1", std::time::Duration::from_millis(5)).await;
    assert!(out.is_none(), "lo Stop deve abortire la chiamata lenta (None)");
}

/// Nessuno Stop: la chiamata che rientra con successo e' onorata (nessun falso
/// abort su un run che procede normalmente).
#[tokio::test]
async fn complete_or_cancel_onora_la_risposta_senza_stop() {
    let rc = StubRunControlStore::default(); // superseded=false
    let ok = async { Ok(LlmResponse::default()) };
    let out = complete_or_cancel(ok, &rc, "run1", std::time::Duration::from_millis(50)).await;
    assert!(matches!(out, Some(Ok(_))));
}

/// Fail-open: un errore di lettura del segnale di cancellazione NON abortisce la
/// chiamata (coerente con `is_superseded`): il run prosegue, mai un abort per un
/// guasto DB.
#[tokio::test]
async fn complete_or_cancel_fail_open_non_abortisce() {
    let rc = StubRunControlStore {
        fail_is_superseded: true,
        ..Default::default()
    };
    let ok = async { Ok(LlmResponse::default()) };
    let out = complete_or_cancel(ok, &rc, "run1", std::time::Duration::from_millis(5)).await;
    assert!(matches!(out, Some(Ok(_))));
}

// ── sollecito di chiusura a TEMPO (turno di grazia prima del kill) ─────────────

/// Epoch di avvio tale che risultino `elapsed_s` secondi trascorsi.
fn started_epoch_fa(elapsed_s: i64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    now - elapsed_s
}

/// Stato di una FIGURA del consiglio senza parere (advisory_verdict fra i tool,
/// `advisory_verdict` non ancora dichiarato), con `elapsed_s` secondi trascorsi.
fn figura_muta_da(elapsed_s: i64) -> AgentState {
    AgentState {
        thread_id: Some("r1".into()),
        messages: vec![human("analizza l'auth")],
        iterations: Some(5),
        run_started_at_epoch_s: Some(started_epoch_fa(elapsed_s)),
        tools_json: Some(vec![
            json!({"name": "read_file"}),
            json!({"name": "advisory_verdict"}),
        ]),
        ..Default::default()
    }
}

/// Il cuore del fix: col budget quasi esaurito, una figura ancora muta viene
/// SOLLECITATA a chiudere invece di essere uccisa allo scadere. Prima moriva n/d
/// ("tempo scaduto") portandosi via il lavoro gia' svolto: il timer esterno
/// droppava il future senza mai concederle un turno per dichiarare il parere.
#[tokio::test]
async fn figura_sotto_deadline_riceve_sollecito_invece_di_morire_muta() {
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_time_budget_s: 100,
        time_grace_pct: 70,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let state = figura_muta_da(80); // 80% del budget: oltre la soglia del 70%
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);

    // NON chiude: turno di grazia (self-loop G1Escalated), nessuna chiusura d'autorita'.
    assert_eq!(out.stop_reason, Some(StopReason::G1Escalated));
    assert!(
        !out.meta_steps.iter().any(|m| m.kind == "anti_runaway"),
        "la figura non deve morire muta: riceve il sollecito"
    );
    // Flag una-tantum + streak azzerato (senza, il re-entry richiuderebbe subito).
    assert_eq!(
        out.extra.get("advisory_grace_used").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(out.consecutive_text_only_turns, Some(0));
    // La direttiva raggiunge davvero il modello come messaggio.
    assert!(
        out.messages
            .iter()
            .rev()
            .take(2)
            .any(|m| format!("{m:?}").contains("advisory_verdict")),
        "il sollecito deve chiedere esplicitamente di chiamare advisory_verdict"
    );
}

/// Sotto la soglia di TEMPO il sollecito pre-LLM non scatta: il modello viene
/// interrogato normalmente (nessun turno sprecato quando c'e' ancora tempo).
/// MA se a quel turno il modello CHIUDE MUTO (end_turn di sola prosa, il caso
/// reale del run 10:03 del 20/07), il terzo call site della grazia lo
/// intercetta POST-LLM: la chiusura volontaria senza parere non e' piu' una
/// chiusura n/d. Questo test fissava il comportamento pre-fix (chiusura muta
/// ammessa); ora fissa quello nuovo.
#[tokio::test]
async fn sotto_soglia_il_modello_viene_interrogato_ma_la_chiusura_muta_no() {
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_time_budget_s: 100,
        time_grace_pct: 70,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("risposta"));
    let ctx = ctx_with(llm.clone(), false);
    let state = figura_muta_da(50); // 50% del budget: sotto la soglia
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);

    // Il sollecito e' arrivato DOPO l'interrogazione (post-LLM), non al suo
    // posto: il modello e' stato chiamato davvero.
    assert!(
        !llm.seen.lock().unwrap().is_empty(),
        "sotto soglia il modello va interrogato, non sostituito dal sollecito"
    );
    assert_eq!(
        out.extra.get("advisory_grace_used").and_then(Value::as_bool),
        Some(true),
        "la chiusura MUTA post-LLM riceve il turno di grazia"
    );
    // Il turno successivo e' dichiarativo di ruolo: catalogo ridotto + forcing.
    assert_eq!(
        out.extra
            .get("force_role_declaration")
            .and_then(Value::as_bool),
        Some(true),
        "la grazia arma il turno dichiarativo di ruolo"
    );
    // La prosa del modello NON e' andata persa: precede la direttiva.
    assert!(
        out.messages
            .iter()
            .any(|m| format!("{m:?}").contains("risposta")),
        "il resoconto in prosa del modello resta in conversazione"
    );
    assert!(
        !llm.seen.lock().unwrap().is_empty(),
        "sotto soglia il turno prosegue e interroga il modello"
    );
}

/// Con la grazia GIA' concessa e il budget esaurito, il ramo di chiusura resta
/// quello di prima: si chiude d'autorita' con reason `time_budget` (il sollecito
/// e' una-tantum, non un modo per vivere per sempre).
#[tokio::test]
async fn grazia_gia_concessa_a_budget_esaurito_chiude_time_budget() {
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        run_time_budget_s: 100,
        time_grace_pct: 70,
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("non chiamato"));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = figura_muta_da(120); // budget esaurito
    state
        .extra
        .insert("advisory_grace_used".into(), json!(true));
    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);

    assert!(
        out.meta_steps
            .iter()
            .any(|m| m.kind == "anti_runaway" || format!("{m:?}").contains("time_budget")),
        "a budget esaurito, con la grazia gia' usata, si chiude"
    );
    assert!(llm.seen.lock().unwrap().is_empty(), "nessuna chiamata al modello");
}

// ── Tetto sui fallimenti gateway deterministici (mig 0619) ────────────────────

#[test]
fn streak_deterministico_cresce_solo_su_iterazioni_contigue() {
    use crate::nodes::executor::next_deterministic_streak;

    // Primo fallimento: count 1.
    let (c1, v1) = next_deterministic_streak(None, "openrouter", "glm", "empty_completion", 4);
    assert_eq!(c1, 1);

    // Stesso (provider, model, causa), iterazione contigua: count 2.
    let (c2, v2) =
        next_deterministic_streak(Some(&v1), "openrouter", "glm", "empty_completion", 5);
    assert_eq!(c2, 2);

    // Un turno RIUSCITO in mezzo consuma l'iterazione 6: il fallimento a 7 non
    // e' piu' contiguo -> lo streak riparte. E' il reset implicito che evita di
    // chiudere un run per errori sporadici non consecutivi.
    let (c3, _) = next_deterministic_streak(Some(&v2), "openrouter", "glm", "empty_completion", 7);
    assert_eq!(c3, 1, "un successo in mezzo azzera la catena");

    // Cambio di modello (failover riuscito): tupla diversa -> riparte.
    let (c4, _) = next_deterministic_streak(Some(&v2), "mistral", "large", "empty_completion", 6);
    assert_eq!(c4, 1, "il failover cambia la coppia e azzera la catena");
}

#[test]
fn solo_le_cause_deterministiche_contano_per_il_tetto() {
    use crate::nodes::executor::deterministic_gateway_cause;
    use crate::runtime::ports::{
        PortError, ProviderFailureCause, ProviderUnavailableInfo,
    };

    let mk = |cause| {
        PortError::ProviderUnavailable(ProviderUnavailableInfo::new(cause, "x".to_string()))
    };
    // Deterministica: rifare la chiamata da' la stessa risposta.
    assert_eq!(
        deterministic_gateway_cause(&mk(ProviderFailureCause::EmptyCompletion), &[]),
        Some("empty_completion")
    );
    // Transitorie: FUORI dal tetto (possono risolversi da sole). Mutazione che
    // rende rosso: includere Cooldown nel match dell'helper.
    assert_eq!(
        deterministic_gateway_cause(&mk(ProviderFailureCause::Cooldown), &[]),
        None
    );
    assert_eq!(
        deterministic_gateway_cause(&mk(ProviderFailureCause::Billing), &[]),
        None
    );
    assert_eq!(
        deterministic_gateway_cause(&PortError::Llm("boom".to_string()), &[]),
        None,
        "un errore generico non tipizzato non entra nel tetto"
    );
}

/// T2 del piano: il turno dichiarativo di RUOLO arriva al wire con il catalogo
/// ridotto al solo tool del canale e il forcing attivo. Con UN solo tool,
/// tool_choice=required equivale a forzare QUEL tool su ogni dialetto.
#[tokio::test]
async fn il_turno_di_ruolo_forza_il_tool_sul_wire() {
    let rc = Arc::new(StubRunControlStore::default());
    let cfg = ExecutorConfig {
        // Stile che supporta il forcing (in produzione: openrouter/openai ->
        // openai_required via capability). Senza stile il forcing e' spento e
        // il turno di ruolo degraderebbe a sola riduzione del catalogo.
        tool_choice_style: Some("openai_required".to_string()),
        ..cfg_resolved()
    };
    let (n, _m, _s) = node(cfg, rc);
    let llm = Arc::new(StubLlmGateway::with_text("ancora prosa"));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = figura_muta_da(10);
    state
        .extra
        .insert("force_role_declaration".to_string(), json!(true));

    let _ = n.run(&state, &ctx).await.expect("run");

    let req = llm.seen.lock().unwrap().last().cloned().expect("request");
    let tools = req.tools.clone().unwrap_or_default();
    // Mutazione che rende rosso: togliere la riduzione del catalogo nel blocco
    // `declaring_role_turn` -> qui compare anche read_file.
    assert_eq!(tools.len(), 1, "catalogo ridotto al solo canale: {tools:?}");
    assert_eq!(
        tools[0].get("name").and_then(Value::as_str),
        Some("advisory_verdict")
    );
    // Mutazione che rende rosso: togliere `force_action_hard = true` -> None.
    assert_eq!(
        req.force_tool_choice,
        Some(true),
        "il tool_choice deve essere forzato"
    );
}

/// T4 del piano: la grazia e' one-shot. Un secondo end_turn muto DOPO la grazia
/// consumata chiude come oggi — nessun loop di solleciti.
#[tokio::test]
async fn la_grazia_sulla_chiusura_volontaria_e_one_shot() {
    let rc = Arc::new(StubRunControlStore::default());
    let (n, _m, _s) = node(cfg_resolved(), rc);
    let llm = Arc::new(StubLlmGateway::with_text("chiudo senza dichiarare"));
    let ctx = ctx_with(llm.clone(), false);
    let mut state = figura_muta_da(10);
    state
        .extra
        .insert("advisory_grace_used".to_string(), json!(true));

    let delta = n.run(&state, &ctx).await.expect("run");
    let out = apply(state, delta);

    // Mutazione che rende rosso: togliere il check ADVISORY_GRACE_USED_KEY in
    // maybe_advisory_grace_delta_preserving -> secondo rientro invece di chiudere.
    assert_eq!(
        out.stop_reason,
        Some(StopReason::EndTurn),
        "grazia gia' consumata: la chiusura volontaria resta una chiusura"
    );
}

// ── compact_provider_error: il blob JSON del gateway non arriva in chat ──────

#[test]
fn errore_provider_compresso_senza_blob_json() {
    // Input REALE (run 1db02ed3, 21/07): l'errore del gateway incorpora il
    // body JSON del provider; nel resoconto in chat arrivava intero.
    let raw = r#"provider non disponibile: Nexus Gateway 400 Bad Request: {"error":"tutti i provider hanno fallito -> mistral (mistral HTTP 400: {\"object\":\"error\",\"message\":\"Unexpected role 'tool' after role 'user'\"})","code":"PROVIDER_ERROR"}"#;
    let out = super::compact_provider_error(raw);
    // Mutazione che rende rosso: usare {err} invece di {err_short} al call
    // site, o rimuovere il taglio alla prima '{' in compact_provider_error.
    assert!(!out.contains('{'), "il payload JSON non deve comparire: {out}");
    assert_eq!(out, "provider non disponibile: Nexus Gateway 400 Bad Request");
}

#[test]
fn errore_provider_senza_json_resta_intatto_e_lungo_viene_troncato() {
    assert_eq!(
        super::compact_provider_error("timeout di rete verso il gateway"),
        "timeout di rete verso il gateway"
    );
    let lungo = "x".repeat(400);
    let out = super::compact_provider_error(&lungo);
    assert!(out.len() <= 203, "tetto caratteri: {}", out.len());
    assert!(out.ends_with("..."));
    // Solo payload tecnico -> frase di ripiego onesta, mai stringa vuota.
    let solo_json = r#"{"error":"x"}"#;
    assert_eq!(
        super::compact_provider_error(solo_json),
        "richiesta rifiutata dal provider (dettaglio tecnico nei log del run)"
    );
}
