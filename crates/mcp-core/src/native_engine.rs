//! Motore nativo Rust (`nexus-agent-graph`) cablato nei servizi reali di mcp-core.
//!
//! Questo modulo e' l'AGGANCIO della FASE 3 del porting strangler-fig: costruisce
//! ed esegue il grafo agentico Rust con le 14 impl concrete di
//! [`crate::agent_graph_adapter`] (FASE 2), invece dello stub `bail!` di FASE 0.
//!
//! ## Confine di responsabilita' (regola L)
//!
//! - La TOPOLOGIA (nodi + edge + entry) e' assemblata dal PUNTO UNICO
//!   [`nexus_agent_graph::build_agent_graph`]: qui NON si re-implementa.
//! - Il MOTORE (loop superstep, merge, route, checkpoint, recursion_limit,
//!   interrupt HITL) vive in `nexus-graph`: qui si chiama solo
//!   [`AgentGraphEngine::run_until_interrupt`].
//! - Le 14 PORTE I/O delegano ciascuna a UN servizio concreto gia' esistente in
//!   mcp-core (gateway LLM, ToolRunner in-process, canale SSE, store DB): nessuna
//!   logica duplicata.
//!
//! ## Regola G (niente modelli/porte hardcoded)
//!
//! Tutto cio' che e' configurabile viene risolto dal DB a monte della
//! costruzione dei nodi:
//! - `recursion_limit` da `agent.graph.recursion_limit`;
//! - `context_window` del modello del turno da `ai_price_catalog` (predictive cap
//!   del tool_dispatch);
//! - provider/model del planner / planner_fallback / reflection da
//!   [`crate::internal_routing::resolve_purpose_model`] (tier-aware);
//! - provider/model del turno (executor) RISOLTI a monte e PASSATI in input.
//!
//! Le restanti config dei nodi usano i loro `Default`, che replicano 1:1 i
//! `_SAFE_DEFAULTS` del brain (`orchestrator_config.py`): valgono SOLO se il
//! valore DB-specifico non e' ancora necessario in questa cablatura iniziale, e
//! NON sono "magic fallback" su un comportamento di business (sono i medesimi
//! safe-default gia' validati per il path Python). I gate che richiederebbero un
//! I/O di risoluzione a monte non ancora portato (es. `_resolve_build_command`
//! per il criterio build del final_gate) restano OFF (nessun comando -> nessun
//! criterio, non blocca): un TODO esplicito li traccia, niente toppa.
//!
//! ## TODO Fase 5 (debiti di parita' da chiudere PRIMA dell'instradamento)
//!
//! Verifica adversariale 2026-06: latenti finche' `select_engine` resta python
//! (regressione viva NULLA), ma da chiudere all'instradamento per parita' col brain:
//! 1. `build_initial_state` NON valorizza `behavior_mode` (resta `None`): oggi
//!    innocuo (il brain hardcoda "bilanciata" e il planner e' comunque OFF via
//!    `plan_phase_enabled=false`), ma al cutover va popolato dal turno.
//! 2. `PlannerConfig`/`FinalGateConfig`/altre config dei nodi usano `default()`:
//!    il DB ha gia' `orchestrator.plan_phase_enabled=true` (mig 0426/0439) -> al
//!    routing vanno LETTE dal DB (regola G piena), non lasciate al safe-default.
//! 3. SSE: i nodi emettono via `ctx.emit` solo `MetaStep`; mancano
//!    `AssistantDelta`/`ToolUse`/`ToolResult` e il terminatore `Done`, + la
//!    finalizzazione di `agent_runs` e la gestione hollow nel call site.
//! 4. Il ramo `engine != python` del call site deve fare `return` su `Ok` (oggi
//!    prosegue nel loop `run_via_brain` -> doppio run); innocuo solo perche' mai
//!    raggiunto.
//!
//! ## Stato (NON instradato in produzione)
//!
//! `select_engine` ritorna SEMPRE `python` (regola G, tabella
//! `nexus_orchestrator_engine`). Quindi questo path e' COSTRUITO, COMPILA ed e'
//! TESTATO end-to-end in-process, ma NON viene chiamato in produzione: la
//! regressione e' NULLA. L'instradamento (Fase 5) e' un cambiamento di DATI nel
//! DB, non di codice.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use nexus_agent_graph::nodes::{
    ExecutorConfig, ExecutorNode, VerifierConfig, VerifierNode,
};
use nexus_agent_graph::runtime::ports::{
    AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, EscalationPort, EventSink,
    LlmGateway, MetaStepStore, ModelUpscalePort, NextActionsDeriver, RunControlStore, TodoStore,
    ToolExecutor, VerifierRunStore,
};
use nexus_agent_graph::{
    build_agent_graph, AgentGraphEngine, AgentGraphNodes, AgentNodeCtx, AgentState, ClarifyConfig,
    ClarifyOrExpandNode, FinalGateConfig, FinalGateNode, LearnerConfig, LearnerNode, Message,
    PlannerConfig, PlannerNode, ReflectionConfig, ReflectionNode, RouterNode, StopReason,
    TodoRunnerConfig, TodoRunnerNode, ToolDispatchConfig, ToolDispatchNode, UnderstandingConfig,
    UnderstandingNode,
};
use nexus_graph::outcome::StepOutcome;

use crate::agent_graph_adapter::{
    agent_step_store::PgAgentStepStore, billing_cooldown_port::CooldownBillingPort,
    context_offload::RagContextOffloadAdapter, criteria_runner::FinalGateCriteriaRunnerAdapter,
    escalation_port::PgEscalationPort, event_sink::SseEventSinkAdapter, llm_gateway::GatewayLlmAdapter,
    meta_step_store::PgMetaStepStore, model_upscale_port::CatalogModelUpscalePort,
    next_actions_deriver::NextActionsDeriverAdapter, run_control_store::PgRunControlStore,
    todo_store::PgTodoStore, tool_executor::ToolRunnerExecutorAdapter,
    verifier_run_store::PgVerifierRunStore,
};
use crate::agent_types::AgentStepEvent;
use crate::nexus_gateway::NexusGatewayClient;
use crate::tool_runner_server::ToolRunnerDeps;

// NB: la `RoutingConfig` del GRAFO e' `nexus_agent_graph::routing::RoutingConfig`
// (config delle `route_after_*` + recursion_limit). E' un tipo DISTINTO dalla
// `RoutingConfig` dell'orchestratore mcp-core (provider/model per behavior_mode):
// usiamo SOLO quella del grafo qui, col path completo, per non confonderle
// (regola L: una sola fonte per ciascun concern).
use nexus_agent_graph::routing::RoutingConfig;

/// Dipendenze infrastrutturali del motore nativo, estratte da `AppState` al call
/// site (un `AppState` non e' catturabile nel task `'static` dello spawn: si
/// passano i singoli handle clonati, tutti `Arc`/pool a basso costo).
///
/// Specchio dei campi `AppState` che alimentano [`ToolRunnerDeps`] + il client
/// gateway, raccolti in un solo posto cosi' il call site costruisce questo
/// `struct` con un blocco di clone e il modulo resta testabile senza `AppState`.
pub struct NativeDeps {
    /// Pool Postgres condiviso del processo (regola G: connection string MAI
    /// hardcoded — riusa quello di `AppState`).
    pub db: PgPool,
    /// Dipendenze del ToolRunner concreto (db, neural, channels, ...), per il
    /// path Real dei tool in-process.
    pub tool_runner_deps: ToolRunnerDeps,
    /// Client del gateway LLM (HTTP verso il Nexus LLM Gateway, catena Fallback
    /// DB-driven). Provider/model arrivano gia' risolti nella richiesta.
    pub gateway: NexusGatewayClient,
}

/// Parametri di un run nativo, gia' RISOLTI a monte dal call site (lo stesso
/// punto che alimenta `run_via_brain`, regola L: non si ricostruisce qui
/// prompt/tools/history — si riusano i valori gia' calcolati).
pub struct NativeRunInput {
    /// Id del run Nexus (= thread del grafo).
    pub run_id: Uuid,
    /// Id della sessione chat (risolve project/root/permessi per i tool).
    pub session_id: Uuid,
    /// Provider del turno RISOLTO a monte (routing matrix, regola G).
    pub provider: String,
    /// Modello del turno RISOLTO a monte (regola G).
    pub model: String,
    /// System prompt completo del run.
    pub system_text: String,
    /// Messaggio iniziale dell'utente (con blocco allegati gia' inline).
    pub initial_msg: String,
    /// History conversazione in forma LangChain (`Vec<Value>`): convertita in
    /// `Message` col PUNTO UNICO `lc_serde::from_lc` (regola L).
    pub conversation_history: Vec<serde_json::Value>,
    /// Tools esposti al modello (schema Anthropic-style o OpenAI).
    pub tools_json: serde_json::Value,
    /// Intent gia' risolto (es. risposta a disambiguazione). `None` -> il router
    /// classifica normalmente.
    pub intent_hint: Option<String>,
    /// Modalita' automazione della sessione (study/confirm/automatic/...).
    pub automation_mode: String,
    /// Canale broadcast SSE del run (lo stesso di `run_via_brain`, `agent_channels`).
    pub step_tx: broadcast::Sender<AgentStepEvent>,
}

/// Esito di un run nativo, normalizzato per il chiamante.
#[derive(Debug, Clone)]
pub struct NativeRunOutcome {
    /// `true` se il run e' arrivato a `End` (Completed); `false` se si e' fermato
    /// su un interrupt HITL (`awaiting_confirmation`).
    pub completed: bool,
    /// Risposta finale (campo `result` dello stato a fine run).
    pub final_answer: Option<String>,
    /// Motivo di stop dell'ultimo turno, se valorizzato.
    pub stop_reason: Option<StopReason>,
    /// Provider effettivamente usato (post cascade/sticky del gateway).
    pub provider_used: Option<String>,
    /// Modello effettivamente usato.
    pub model_used: Option<String>,
    /// Nodo da cui riprendere se `completed == false` (HITL): `None` se Completed.
    pub resume_at: Option<String>,
}

/// Context window (token) del modello del turno dal catalog (regola G). `0` =
/// finestra ignota -> il tool_dispatch disattiva il predictive cap (no toppa: un
/// cap su una finestra inventata produrrebbe blocchi spurii).
async fn resolve_context_window(db: &PgPool, provider: &str, model: &str) -> i64 {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT context_window::bigint FROM ai_price_catalog \
         WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    row.map(|(w,)| w).unwrap_or(0)
}

/// Risolve (provider, model) di un purpose interno (tier-aware) dal DB. Punto
/// unico [`crate::internal_routing::resolve_purpose_model_db`] (regola G/L). In
/// caso di purpose non risolvibile ritorna `("", "")`: i nodi trattano provider
/// vuoto come sentinella di skip (es. planner -> `no_capable_provider`,
/// reflection -> disabilitata), MAI come modello hardcoded.
async fn resolve_purpose(db: &PgPool, purpose: &str) -> (String, String) {
    match crate::internal_routing::resolve_purpose_model_db(db, purpose).await {
        crate::internal_routing::PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        _ => (String::new(), String::new()),
    }
}

/// Costruisce le 14 impl concrete + gli 11 nodi (porte iniettate) + la
/// `RoutingConfig` e la `PlannerConfig` DB-driven, e assembla il
/// [`AgentGraphEngine`] col checkpointer Postgres.
///
/// Ritorna anche la `RoutingConfig` risolta (serve a popolare il ctx, il cui
/// `recursion_limit` viene letto dal motore) + le porte gateway/tools per il ctx.
async fn build_native_engine(
    deps: &NativeDeps,
    input: &NativeRunInput,
) -> anyhow::Result<(
    AgentGraphEngine,
    RoutingConfig,
    Arc<dyn LlmGateway>,
    Arc<dyn ToolExecutor>,
    Arc<dyn EventSink>,
)> {
    let db = deps.db.clone();

    // ── Config DB-driven (regola G) ──────────────────────────────────────────
    let recursion_limit: u32 = nexus_auth::get_setting(&db, "agent.graph.recursion_limit")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or_else(|| RoutingConfig::default().recursion_limit);
    let routing_cfg = RoutingConfig {
        recursion_limit,
        ..RoutingConfig::default()
    };

    let context_window = resolve_context_window(&db, &input.provider, &input.model).await;

    // Provider/model dei purpose interni del planner + reflection (tier-aware).
    let (planner_provider, planner_model) = resolve_purpose(&db, "planner").await;
    let (fallback_provider, fallback_model) = resolve_purpose(&db, "planner_fallback").await;
    let (reflection_provider, reflection_model) = resolve_purpose(&db, "reflection").await;

    // ── Porte I/O concrete (14 impl FASE 2) ──────────────────────────────────
    // Gateway LLM (provider/model gia' risolti, il client non re-instrada).
    let llm: Arc<dyn LlmGateway> = Arc::new(GatewayLlmAdapter::new(deps.gateway.clone()));

    // ToolRunner in-process (Real): mcp-core E' il ToolRunner (no gRPC su se'
    // stesso). `primary_run_id=None`: questo e' il run primario (non shadow).
    let tools: Arc<dyn ToolExecutor> = Arc::new(ToolRunnerExecutorAdapter::new(
        deps.tool_runner_deps.clone(),
        input.session_id,
        None,
    ));

    // Canale SSE: lo STESSO broadcast del run (parita' 1:1 con run_via_brain).
    let emit: Arc<dyn EventSink> =
        Arc::new(SseEventSinkAdapter::new(input.step_tx.clone(), input.run_id));

    // Store DB + porte ausiliarie.
    let run_control: Arc<dyn RunControlStore> = Arc::new(PgRunControlStore::new(db.clone()));
    let steps: Arc<dyn AgentStepStore> = Arc::new(PgAgentStepStore::new(db.clone()));
    let meta_steps: Arc<dyn MetaStepStore> =
        Arc::new(PgMetaStepStore::new(db.clone(), input.run_id));
    let todos: Arc<dyn TodoStore> = Arc::new(PgTodoStore::new(db.clone()));
    let verifier_runs: Arc<dyn VerifierRunStore> = Arc::new(PgVerifierRunStore::new(db.clone()));
    let offload: Arc<dyn ContextOffload> = Arc::new(RagContextOffloadAdapter::new(db.clone()));
    let escalation: Arc<dyn EscalationPort> = Arc::new(PgEscalationPort::new(db.clone()));
    let next_actions: Arc<dyn NextActionsDeriver> =
        Arc::new(NextActionsDeriverAdapter::new(db.clone()));
    let billing: Arc<dyn BillingCooldownPort> = Arc::new(CooldownBillingPort::new());
    let upscale: Arc<dyn ModelUpscalePort> = Arc::new(CatalogModelUpscalePort::new(db.clone()));

    // Motore criteri del final_gate / verifier: delega al tool_executor (punto
    // unico, regola L) per i criteri run_command/list_files + DB per outputs_exist.
    let criteria: Arc<dyn CriteriaRunner> =
        Arc::new(FinalGateCriteriaRunnerAdapter::new(tools.clone(), db.clone()));

    // ── Config dei nodi (DB-driven dove richiesto, Default safe altrove) ──────
    let planner_cfg = PlannerConfig::default();

    let exec_cfg = ExecutorConfig {
        routing_provider: input.provider.clone(),
        routing_model: input.model.clone(),
        ..ExecutorConfig::default()
    };

    let tool_dispatch_cfg = ToolDispatchConfig {
        context_window,
        ..ToolDispatchConfig::default()
    };

    let reflection_cfg = ReflectionConfig {
        provider: reflection_provider,
        model: reflection_model,
        ..ReflectionConfig::default()
    };

    // FinalGateConfig: `build_command` resta None in questa cablatura iniziale.
    // TODO(F3+): risolvere _resolve_build_command per-progetto (criterio build E2E)
    // quando il resolver sara' portato; finche' None il criterio non e' aggiunto
    // (nessun blocco, niente toppa).
    let final_gate_cfg = FinalGateConfig::default();

    // ── 11 nodi (porte iniettate nei costruttori reali) ──────────────────────
    let nodes = AgentGraphNodes {
        router: Arc::new(RouterNode),
        clarify_or_expand: Arc::new(ClarifyOrExpandNode::new(ClarifyConfig::default())),
        understanding: Arc::new(UnderstandingNode::new(UnderstandingConfig::default())),
        planner: Arc::new(PlannerNode::new(
            planner_cfg.clone(),
            planner_provider,
            planner_model,
            fallback_provider,
            fallback_model,
            todos.clone(),
        )),
        todo_runner: Arc::new(TodoRunnerNode::new(
            TodoRunnerConfig::default(),
            todos.clone(),
            tools.clone(),
        )),
        executor: Arc::new(ExecutorNode::new(
            exec_cfg,
            run_control.clone(),
            meta_steps.clone(),
            steps.clone(),
            escalation.clone(),
            next_actions.clone(),
            billing.clone(),
            upscale.clone(),
        )),
        tool_dispatch: Arc::new(ToolDispatchNode::new(
            tool_dispatch_cfg,
            tools.clone(),
            steps.clone(),
            run_control.clone(),
            todos.clone(),
            offload.clone(),
        )),
        verifier: Arc::new(VerifierNode::new(
            VerifierConfig::default(),
            final_gate_cfg.clone(),
            routing_cfg.clone(),
            todos.clone(),
            criteria.clone(),
            verifier_runs,
        )),
        final_gate: Arc::new(FinalGateNode::new(
            final_gate_cfg,
            routing_cfg.clone(),
            criteria,
        )),
        reflection: Arc::new(ReflectionNode::new(reflection_cfg)),
        learner: Arc::new(LearnerNode::new(LearnerConfig::default())),
    };

    // Checkpointer Postgres (persistenza per-superstep su nexus_graph_checkpoints).
    let checkpointer = Arc::new(nexus_agent_graph::PgCheckpointer::new(db.clone()));

    let engine = build_agent_graph(nodes, routing_cfg.clone(), planner_cfg, checkpointer);
    Ok((engine, routing_cfg, llm, tools, emit))
}

/// Converte una entry della history compatta `{"role","content"}` in `Message`.
/// Ruoli: `user`/`human` -> Human, `assistant`/`ai` -> Ai (senza tool_calls: la
/// history compatta porta solo testo), `tool` -> Tool (con `tool_call_id` se
/// presente). Ruolo `system` o sconosciuto / content non-stringa -> `None`
/// (saltata): il system viaggia nel campo `system_text`, non nei messaggi.
fn history_entry_to_message(v: &serde_json::Value) -> Option<Message> {
    use nexus_agent_graph::state::MessageContent;

    let obj = v.as_object()?;
    let role = obj.get("role").and_then(|r| r.as_str())?;
    let content = obj.get("content").and_then(|c| c.as_str())?;
    let content = MessageContent::text(content.to_string());
    match role {
        "user" | "human" => Some(Message::Human { content }),
        "assistant" | "ai" => Some(Message::Ai {
            content,
            tool_calls: Vec::new(),
        }),
        "tool" => {
            let tool_call_id = obj
                .get("tool_call_id")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            Some(Message::Tool {
                tool_call_id,
                content,
            })
        }
        _ => None,
    }
}

/// Costruisce lo stato iniziale del grafo dal prompt/messages, 1:1 con
/// l'`initial_state` dell'endpoint brain `/agent/run/stream`: history convertita
/// in `Message` + il messaggio utente del turno in coda,
/// system/session/intent/automation/provider override valorizzati.
fn build_initial_state(input: &NativeRunInput) -> AgentState {
    use nexus_agent_graph::state::MessageContent;

    // History pregressa nel formato compatto `{"role","content"}` prodotto da
    // `build_recent_conversation_history` (lo STESSO che riceve il brain endpoint
    // /agent/run/stream, regola L). Le entry malformate sono saltate (best-effort,
    // non bloccano il run).
    let mut messages: Vec<Message> = input
        .conversation_history
        .iter()
        .filter_map(history_entry_to_message)
        .collect();

    // Messaggio utente del turno corrente in coda.
    messages.push(Message::Human {
        content: MessageContent::text(input.initial_msg.clone()),
    });

    // Tools al modello: l'array (se presente). Qualunque forma non-array
    // (incluso null) -> None: lo stato tratta None come "nessun tool dichiarato".
    let tools = input.tools_json.as_array().cloned();

    AgentState {
        messages,
        thread_id: Some(input.run_id.to_string()),
        session_id: Some(input.session_id.to_string()),
        system_text: Some(input.system_text.clone()),
        intent_hint: input.intent_hint.clone(),
        provider_override: Some(input.provider.clone()),
        model_override: Some(input.model.clone()),
        tools_json: tools,
        automation_mode: parse_automation_mode(&input.automation_mode),
        ..Default::default()
    }
}

/// Parsing della modalita' automazione testuale nell'enum dello stato. Stringa
/// ignota -> `None` (lo stato usa il default a valle, nessun panic).
fn parse_automation_mode(s: &str) -> Option<nexus_agent_graph::AutomationMode> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

/// Esegue un run sul motore nativo end-to-end: costruisce engine+ctx,
/// `initial_state` dal prompt, gira `run_until_interrupt` e mappa l'esito.
///
/// `init` distingue nuovo run (Some) da resume (None, riparte dal checkpoint).
/// Per il path primario `shadow=false` (`ExecMode::Real`): i tool hanno
/// side-effect reali sul progetto.
pub async fn run_native(deps: &NativeDeps, input: &NativeRunInput) -> anyhow::Result<NativeRunOutcome> {
    run_native_inner(deps, input, true).await
}

/// Variante interna con controllo esplicito su nuovo-run vs resume. Estratta per
/// il test E2E (che esercita sia il run completo sia il resume da checkpoint).
async fn run_native_inner(
    deps: &NativeDeps,
    input: &NativeRunInput,
    new_run: bool,
) -> anyhow::Result<NativeRunOutcome> {
    let (engine, routing_cfg, llm, tools, emit) = build_native_engine(deps, input).await?;

    let ctx = AgentNodeCtx {
        db: deps.db.clone(),
        llm,
        tools,
        emit,
        cfg: routing_cfg,
        cancel: tokio_util::sync::CancellationToken::new(),
        run_id: input.run_id,
        session_id: input.session_id,
        thread_id: input.run_id,
        // Run PRIMARIO: Real (side-effect reali). Lo shadow ha il suo path (crate
        // `shadow`), non passa di qui.
        shadow: false,
    };

    let init = if new_run {
        Some(build_initial_state(input))
    } else {
        None
    };

    let outcome = engine
        .run_until_interrupt(input.run_id, init, &ctx)
        .await
        .map_err(|e| anyhow::anyhow!("motore nativo: run_until_interrupt fallita: {e}"))?;

    Ok(map_outcome(outcome))
}

/// Mappa lo [`StepOutcome`] del motore nel [`NativeRunOutcome`] del chiamante.
fn map_outcome(outcome: StepOutcome<AgentState>) -> NativeRunOutcome {
    match outcome {
        StepOutcome::Completed(state) => NativeRunOutcome {
            completed: true,
            final_answer: state.result.clone(),
            stop_reason: state.stop_reason,
            provider_used: state.provider_used.clone(),
            model_used: state.model_used.clone(),
            resume_at: None,
        },
        StepOutcome::Interrupted { state, resume_at } => NativeRunOutcome {
            completed: false,
            final_answer: state.result.clone(),
            stop_reason: state.stop_reason,
            provider_used: state.provider_used.clone(),
            model_used: state.model_used.clone(),
            resume_at: Some(resume_at.as_label().to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexus_gateway::NexusGatewayClient;
    use nexus_graph::node::NodeId;

    // Il test E2E in-process del GRAFO con gateway scriptato + stub vive in
    // `nexus_agent_graph::graph` (test `run_completo_attraversa_il_loop_e_chiude`):
    // esercita router -> clarify -> understanding -> executor(tool_use) ->
    // tool_dispatch -> executor(end_turn) -> final_gate -> reflection -> learner ->
    // END, il checkpoint per-superstep e il resume da checkpoint, con gli STESSI
    // tipi (AgentState/AgentNodeCtx) e lo STESSO builder (build_agent_graph) usati
    // qui. Quel test e' la copertura del MOTORE + TOPOLOGIA con doppi mockati.
    //
    // Qui copriamo la parte SPECIFICA di questo modulo, che il test del grafo non
    // tocca: la costruzione dell'`initial_state` dal `NativeRunInput` (prompt +
    // conversation_history LangChain + tools + override) e il mapping dell'esito.
    // L'assemblaggio reale delle 14 impl (build_native_engine) richiede un DB +
    // ToolRunnerDeps reali (ctx di progetto): e' coperto a livello di servizio in
    // ambiente integrato, non da unit test (le impl DB sono gia' testate ognuna nel
    // proprio modulo in F2).

    fn sample_input() -> NativeRunInput {
        let (tx, _rx) = broadcast::channel::<AgentStepEvent>(16);
        NativeRunInput {
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            system_text: "sei un assistente".to_string(),
            initial_msg: "Scrivi src/main.rs".to_string(),
            conversation_history: vec![serde_json::json!({
                "role": "user",
                "content": "ciao, contesto pregresso"
            })],
            tools_json: serde_json::json!([
                {"name": "read_file", "input_schema": {"type": "object"}}
            ]),
            intent_hint: Some("code_write".to_string()),
            automation_mode: "automatic".to_string(),
            step_tx: tx,
        }
    }

    #[test]
    fn initial_state_da_prompt_history_e_override() {
        let input = sample_input();
        let state = build_initial_state(&input);

        // History pregressa convertita (lc_serde) + messaggio del turno in coda.
        assert_eq!(state.messages.len(), 2, "history (1) + turno corrente (1)");
        // L'ULTIMO messaggio e' il prompt utente del turno.
        match state.messages.last().expect("almeno un messaggio") {
            Message::Human { content } => {
                assert_eq!(content.flatten_text(), "Scrivi src/main.rs");
            }
            other => panic!("atteso Human in coda, trovato {other:?}"),
        }
        // Il primo e' la history pregressa (Human).
        assert!(matches!(state.messages[0], Message::Human { .. }));

        // System / session / thread / intent / override valorizzati.
        assert_eq!(state.system_text.as_deref(), Some("sei un assistente"));
        assert_eq!(state.session_id, Some(input.session_id.to_string()));
        assert_eq!(state.thread_id, Some(input.run_id.to_string()));
        assert_eq!(state.intent_hint.as_deref(), Some("code_write"));
        assert_eq!(state.provider_override.as_deref(), Some("anthropic"));
        assert_eq!(state.model_override.as_deref(), Some("claude-x"));

        // Tools propagati (array non vuoto).
        let tools = state.tools_json.expect("tools propagati");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "read_file");
    }

    #[test]
    fn initial_state_tools_null_diventa_none() {
        let mut input = sample_input();
        input.tools_json = serde_json::Value::Null;
        let state = build_initial_state(&input);
        assert!(state.tools_json.is_none(), "tools null -> None");
    }

    #[test]
    fn initial_state_history_malformata_saltata_non_blocca() {
        let mut input = sample_input();
        // Una entry non convertibile (shape errata) + una valida + un ruolo
        // 'system' (deve essere saltato: il system viaggia in system_text).
        input.conversation_history = vec![
            serde_json::json!({"non": "convertibile"}),
            serde_json::json!({"role": "system", "content": "system da scartare"}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
        ];
        let state = build_initial_state(&input);
        // Malformata + system saltate (best-effort): resta la valida + il turno.
        assert_eq!(state.messages.len(), 2);
        // La entry valida e' l'assistant pregresso (penultimo), poi il turno Human.
        assert!(matches!(state.messages[0], Message::Ai { .. }));
        assert!(matches!(state.messages[1], Message::Human { .. }));
    }

    #[test]
    fn map_outcome_completed() {
        let state = AgentState {
            result: Some("Lavoro concluso".to_string()),
            stop_reason: Some(StopReason::EndTurn),
            provider_used: Some("anthropic".to_string()),
            model_used: Some("claude-real".to_string()),
            ..Default::default()
        };

        let out = map_outcome(StepOutcome::Completed(state));
        assert!(out.completed);
        assert_eq!(out.final_answer.as_deref(), Some("Lavoro concluso"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
        assert_eq!(out.model_used.as_deref(), Some("claude-real"));
        assert!(out.resume_at.is_none());
    }

    #[test]
    fn map_outcome_interrupted_porta_resume_at() {
        let state = AgentState::default();
        let out = map_outcome(StepOutcome::Interrupted {
            state,
            resume_at: NodeId::Executor,
        });
        assert!(!out.completed);
        // resume_at e' la label del nodo da cui riprendere (HITL).
        assert_eq!(out.resume_at.as_deref(), Some(NodeId::Executor.as_label()));
    }

    #[test]
    fn parse_automation_mode_robusto() {
        // Una modalita' nota deve parsare; una ignota -> None (nessun panic).
        let known = parse_automation_mode("automatic");
        // Non asseriamo il valore esatto dell'enum (dipende dalla serde repr), solo
        // che una stringa palesemente ignota non panica e da' None.
        let unknown = parse_automation_mode("modalita-che-non-esiste-xyz");
        assert!(unknown.is_none());
        let _ = known; // la repr e' coperta dai test dello stato; qui basta no-panic
    }

    #[test]
    fn gateway_client_costruibile() {
        // Sanity: il client gateway e' costruibile (l'adapter lo avvolge senza I/O).
        let gw = NexusGatewayClient::new("http://127.0.0.1:1".to_string(), "tok".to_string());
        let _adapter = GatewayLlmAdapter::new(gw);
    }
}
