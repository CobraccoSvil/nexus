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
//! - `recursion_limit` da `agent.graph.recursion_limit` (pavimento) scalato a
//!   runtime da `effective_recursion_limit` su `iteration_cap` + topologia grafo;
//! - `context_window` del modello del turno da `ai_price_catalog` (predictive cap
//!   del tool_dispatch);
//! - provider/model del planner / planner_fallback / reflection da
//!   [`crate::internal_routing::resolve_purpose_model`] (tier-aware);
//! - provider/model del turno (executor) RISOLTI a monte e PASSATI in input.
//!
//! Le config dei nodi che il brain legge da `orchestrator_config.get()`
//! (`orchestrator_config.py`) — `PlannerConfig`, `FinalGateConfig`,
//! `VerifierConfig` — sono LETTE dal DB (regola G piena) da `load_*_config` col
//! punto unico `nexus_auth::get_setting` (regola L), 1:1 con le chiavi del brain;
//! il `Default` resta SOLO come safe-default se la chiave manca (identico ai
//! `_SAFE_DEFAULTS`). Le restanti config (`ExecutorConfig`/`ReflectionConfig`/...)
//! usano i loro `Default` per i campi RISOLTI A MONTE (capability, prompt text)
//! non ancora portati in questa cablatura: NON sono "magic fallback" su un
//! comportamento di business (sono i medesimi safe-default gia' validati per il
//! path Python). I gate che richiederebbero un I/O di risoluzione a monte non
//! ancora portato (es. `_resolve_build_command` per il criterio build del
//! final_gate) restano OFF (nessun comando -> nessun criterio, non blocca): un
//! TODO esplicito li traccia, niente toppa.
//!
//! ## DEBITI RESIDUI POST-CUTOVER (non bloccanti)
//!
//! Verifica adversariale 2026-06: il motore nativo Rust e' il PRIMARIO ed e'
//! instradato globalmente (`select_engine` ritorna `rust` sulla riga jolly
//! `*`=rust). I punti seguenti sono debiti di parita' RESIDUI che non bloccano
//! l'esecuzione primaria; restano da chiudere per parita' piena col brain legacy:
//! 1. CHIUSO (F5a). `build_initial_state` valorizza `behavior_mode` con la STESSA
//!    fonte del primario Python (`PRIMARY_BEHAVIOR_MODE`, costante riusata dal
//!    client brain): lo shadow attraversa il grafo col mode IDENTICO. Conta dal
//!    momento in cui il planner e' eleggibile (`plan_phase_enabled=true`).
//! 2. CHIUSO (F5a). `PlannerConfig`/`FinalGateConfig`/`VerifierConfig` sono LETTE
//!    dal DB (`load_*_config`, punto unico `get_setting`, regola G piena), 1:1 con
//!    le chiavi `orchestrator.*`/`agent.*` del brain; il `Default` resta solo come
//!    safe-default se la chiave manca.
//! 3. SSE (in gran parte CHIUSO): i nodi emettono via `ctx.emit` `MetaStep`,
//!    `ToolUse` (`executor.rs`), `ToolResult` (`tool_dispatch.rs`), `EndTurn` e
//!    `ThinkingDelta`; la finalizzazione di `agent_runs` e la gestione hollow
//!    avvengono nel call site (come nel path Python). RESTA non implementato solo
//!    lo streaming `AssistantDelta` (delta token-by-token del content assistant):
//!    NON e' una variante di `SseEvent` per scelta architetturale — il messaggio
//!    assistant e' comunque consegnato e persistito. Feature futura, non residuo.
//! 4. CHIUSO. Il ramo `engine == rust` del call site esce dal `'compute` con
//!    `break 'compute` su `Ok` (niente doppio-run); su `Err` finalizza FAILED
//!    diagnosticato (`native_engine_failure_result`), NIENTE fallback automatico
//!    al brain (regola H, verso zero-Python).
//! 6. HITL (interrupt-resume): il MOTORE gestisce gia' l'interrupt
//!    `awaiting_confirmation` (`run_until_interrupt` -> `Interrupted`) e il RESUME
//!    dal checkpoint (`resume_until_interrupt`, cablato in `resume_native` +
//!    `confirm_native_run`). Il `ToolDispatchNode` imposta il flag in modalita'
//!    Conferma quando ci sono tool mutativi pendenti (`decisions::hitl`).
//! 5. CLASSIFIER LLM nel `RouterNode` (TODO `router.rs`, FIX A): la
//!    classificazione intent via LLM (`AgenticIntentClassifier`) NON e' ancora
//!    portata. Senza `intent_hint` il RouterNode cade nel fallback
//!    `agentic_default`/`action_oriented=true`. Per lo SHADOW questo divergeva dal
//!    primario (g1 sui run 0-tool, loop sui run con tool): mitigato derivando
//!    `action_oriented`/`user_intent` dall'`intent_hint` in `build_initial_state`
//!    (ramo Shadow, punto unico `decisions::action_oriented_for_intent`). Resta da
//!    portare il classifier completo come debito residuo: senza, i turni shadow
//!    SENZA `intent_hint` non hanno l'intent reale del primario.
//!
//! ## Stato: unico motore
//!
//! Questo path esegue OGNI run agentico. Non c'e' piu' un instradamento da
//! decidere: `select_engine` e la tabella `nexus_orchestrator_engine`
//! sceglievano fra `Engine::{Rust, Python, Shadow}`, ma il brain Python e' stato
//! rimosso (mig 0462/0532) e con lui gli altri due rami — Shadow incluso, il cui
//! PRIMARIO era proprio Python. Enum, selettore e tabella sono spariti (mig
//! 0609). `agent_runs.engine` resta valorizzato per il recovery, e i run storici
//! conservano il valore che avevano davvero.

use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use nexus_agent_graph::decisions::context_reduction::{CtxMgmtConfig, TokenBrakeConfig};
use nexus_agent_graph::decisions::{LoopThresholds, RepetitionThresholds};
use nexus_agent_graph::decisions::supervisor::{
    SupervisorConfig, DEFAULT_ANOMALY_STEP_THRESHOLD, DEFAULT_INTERLEAVED_INTERVAL,
};
use nexus_agent_graph::nodes::{
    ExecutorConfig, ExecutorNode, ScaleConfig, ScaleControlNode, StallRecoveryNode, SupervisorNode,
    VerifierConfig, VerifierNode,
};
use nexus_agent_graph::runtime::ports::{
    AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, EscalationPort, EventSink,
    LlmGateway, MetaReasonerPort, MetaStepStore, ModelUpscalePort, NextActionsDeriver,
    RunControlStore, SummaryStore, TodoStore, ToolExecutor, VerifierRunStore,
};
use nexus_agent_graph::runtime::NullEventSink;
use nexus_agent_graph::{
    build_agent_graph, AgentGraphEngine, AgentGraphNodes, AgentNodeCtx, AgentState, ClarifyConfig,
    ClarifyOrExpandNode, FinalGateConfig, FinalGateNode, FinalGateVerdict, LearnerConfig,
    LearnerNode, Message, ReviewGateConfig, ReviewGateNode, ReviewGateVerdict,
    PlannerConfig, PlannerNode, ReflectionConfig, ReflectionNode, RouterNode, StopReason,
    TodoRunnerConfig, TodoRunnerNode, ToolDispatchConfig, ToolDispatchNode, UnderstandingConfig,
    UnderstandingNode, SupervisorMode,
};
use nexus_graph::outcome::StepOutcome;

use crate::agent_graph_adapter::{
    agent_step_store::PgAgentStepStore,
    billing_cooldown_port::{CooldownBillingPort, NullBillingCooldownPort},
    clarify_history_store::PgClarifyHistoryStore,
    context_offload::RagContextOffloadAdapter,
    criteria_runner::FinalGateCriteriaRunnerAdapter,
    embedding_store::PgEmbeddingStore,
    escalation_port::PgEscalationPort,
    event_sink::SseEventSinkAdapter,
    llm_gateway::{GatewayLlmAdapter, ReplayLlmGateway},
    meta_step_store::PgMetaStepStore,
    model_upscale_port::CatalogModelUpscalePort,
    next_actions_deriver::NextActionsDeriverAdapter,
    run_control_store::PgRunControlStore,
    stall_budget_store::PgStallBudgetStore,
    stall_reasoner_port::PgMetaReasonerPort,
    summary_store::PgSummaryStore,
    todo_store::PgTodoStore,
    tool_executor::ToolRunnerExecutorAdapter,
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
use nexus_agent_graph::decisions::hitl::HITL_PENDING_ACTIONS_EXTRA_KEY;
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
    /// Dati COMPLETI del classifier del turno (Tappa 1b, punto B). Popolati SOLO
    /// nel ramo Shadow (`build_initial_state` li usa per derivare
    /// `action_oriented`/`report_only` FEDELI al primario Python via
    /// `intent_classifier::derive_*`, sostituendo la mappa grossolana
    /// `action_oriented_for_intent`). Per il primario restano ai default
    /// (`None`/`false`): il primario NON forza `action_oriented` (decide il
    /// RouterNode), comportamento INVARIATO.
    ///
    /// `requires_tools`: il turno richiede tool? (giudizio LLM del classifier).
    pub requires_tools: Option<bool>,
    /// `agentic_score` (0..1) del classifier sul turno corrente.
    pub agentic_score: Option<f32>,
    /// `authorizes_changes`: l'utente AUTORIZZA modifiche in questo turno?
    /// (giudizio agentico DIRETTO report-vs-act del classifier).
    pub authorizes_changes: Option<bool>,
    /// `true` se il classifier del TURNO CORRENTE ha effettivamente prodotto un
    /// giudizio (distingue "classifier ha risolto" dal degradato). Parita' col
    /// `_classifier_resolved` del brain.
    pub classifier_resolved: bool,
    /// Soglia `routing.action_oriented_min_agentic_score` (DB, default 0.5)
    /// passata dal call site (regola G: nessun hardcode nel grafo). Usata dalla
    /// derivazione `action_oriented` quando `requires_tools` e' assente.
    pub action_oriented_min_score: f32,
    /// Modalita' automazione della sessione (study/confirm/automatic/...).
    pub automation_mode: String,
    /// Modalita' supervisore worker (none/anomaly/interleaved/continuous).
    pub supervisor_mode: SupervisorMode,
    /// Canale broadcast SSE del run (lo stesso di `run_via_brain`, `agent_channels`).
    pub step_tx: broadcast::Sender<AgentStepEvent>,
    /// Run genitore quando questo run e' un SUB-RUN (sub-agente nativo). `None` per
    /// il run principale. Propagato nello stato (`AgentState::parent_run_id`) per
    /// far convergere il sub-run col path Python (`run_subagent`).
    pub parent_run_id: Option<Uuid>,
    /// Profondita' di annidamento del sub-agente (1 = sub-run chiamato dal main).
    /// `None` per il run principale. Valorizza `AgentState::subagent_depth`: il
    /// grafo nativo lo usa per il guard anti-esplosione del fan-out explore
    /// (`UnderstandingNode`, `subagent_depth >= 1 -> skip`) e per l'anti-ricorsione.
    pub subagent_depth: Option<i64>,
    /// Budget in secondi del run CORRENTE, quando il chiamante lo conosce meglio
    /// del setting globale. `None` (run principale) -> vale `agent.run_time_budget_s`
    /// letto da `load_executor_config` (regola G). `Some(s)` (SUB-RUN/figura) -> e'
    /// il `timeout_s` gia' risolto in `prepare_subagent_run`, cioe' il tetto REALE
    /// applicato dal `tokio::time::timeout` esterno.
    ///
    /// Perche' serve: senza questo canale il motore della figura non conosce il
    /// proprio tetto (il setting globale e' `0` per policy, mig 0604/0607), quindi
    /// ogni gate a tempo dell'executor e' codice morto e la figura scade MUTA —
    /// uccisa dall'esterno senza mai poter dichiarare il proprio parere.
    pub run_time_budget_s: Option<u64>,
    /// Override della root di lavoro per un SUB-RUN ISOLATO (FASE 2 orchestrazione:
    /// git worktree effimero). Threadato fino al `ToolRunnerExecutorAdapter` ->
    /// `build_ctx_with_root`, che quando presente sovrascrive `root_path` e imposta
    /// `isolated_subrun=true` (soppressione autocommit/reindex). `None` (default per
    /// il run principale, il resume, lo shadow e i sub-run non isolati) ->
    /// comportamento invariato: il ctx usa la root del progetto. In PR3 TUTTI i
    /// call site lasciano `None`; l'accensione (passare `Some`) e' PR4.
    pub working_root: Option<std::path::PathBuf>,
    /// Sintesi advisory strutturata prodotta PRIMA del run (panel multi-provider
    /// o consiglio a monte). Seed in `AgentState.extra` per il coordinatore
    /// (regola M: segnale macchina, non parsing del blocco testuale).
    pub pre_run_advisory_synthesis: Option<serde_json::Value>,
    /// Fonte del segnale pre-run (`multi_provider_synthesis` | `advisory_synthesis`).
    pub pre_run_advisory_source: Option<&'static str>,
    /// Barriera di scrittura advisory (overlap consiglio ∥ run, mig 0606).
    /// `Some(rx)` = il run parte SUBITO mentre i panel deliberano: la prima
    /// modifica attendera' il loro verdetto (gate nel ToolDispatchNode). `None`
    /// (default) = ramo classico, i verdetti sono gia' in
    /// `pre_run_advisory_synthesis` -> gate inerte, comportamento bit-identico.
    /// Alternativi per costruzione: o si attende prima, o si attende alla prima
    /// scrittura.
    pub advisory_gate:
        Option<tokio::sync::watch::Receiver<nexus_agent_graph::nodes::AdvisoryGateState>>,
    /// Dimensionamento del turno (dal classifier): il ReviewGate lo usa per
    /// stringere il panel coi budget residui (stesso punto unico del pre-run).
    /// `None` nei percorsi senza classifier (sub-run, resume): vale il backstop.
    pub sizing_complexity:
        Option<nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity>,
    pub sizing_scope_system_wide: bool,
}

/// Campi del classifier del turno necessari a `build_initial_state` per derivare
/// `action_oriented`/`report_only` FEDELI al primario Python (Tappa 1b).
///
/// PUNTO UNICO (regola L) della loro RISOLUZIONE: sia il ramo Shadow sia il ramo
/// PRIMARY-Rust del call site (`agent_run.rs`) duplicavano la stessa sequenza
/// (classifica il turno col porting 1:1 `intent_classifier::classify` -> mappa
/// `requires_tools`/`agentic_score`/`authorizes_changes`/`classifier_resolved` +
/// legge la soglia DB `routing.action_oriented_min_agentic_score`). Ora entrambi
/// chiamano [`resolve_classifier_fields`]: la derivazione e' identica, niente
/// logica copiata-e-adattata. Per il primario PYTHON (`run_via_brain`) questo NON
/// si applica: continua a ri-classificare internamente nel `router_node`.
#[derive(Debug, Clone, Copy)]
pub struct ClassifierFields {
    /// `requires_tools` del classifier (None = classifier non interrogato/degradato).
    pub requires_tools: Option<bool>,
    /// `agentic_score` (0..1) del classifier sul turno corrente.
    pub agentic_score: Option<f32>,
    /// `authorizes_changes`: l'utente AUTORIZZA modifiche in questo turno?
    pub authorizes_changes: Option<bool>,
    /// `true` se il classifier ha prodotto un giudizio (NON un fallback di sistema).
    pub classifier_resolved: bool,
    /// Soglia DB `routing.action_oriented_min_agentic_score` (regola G).
    pub action_oriented_min_score: f32,
}

/// Risolve i [`ClassifierFields`] del turno classificando `classifier_input` col
/// PORTING 1:1 (`intent_classifier::classify`, regola L) e leggendo la soglia DB
/// `routing.action_oriented_min_agentic_score` (regola G, mig 0387).
///
/// PUNTO UNICO (regola L) condiviso dai rami Shadow e PRIMARY-Rust di
/// `spawn_agent_run`: prima ciascuno re-implementava questa sequenza. Su
/// fallback del classifier i campi del giudizio restano neutri (`None`/`false`);
/// la soglia DB e' comunque risolta (fallback al default tecnico
/// `DEFAULT_ACTION_ORIENTED_MIN_SCORE` se la chiave manca).
///
/// Il `gateway` non e' piu' `Option`: era il residuo di quando l'orchestrator
/// poteva nascere senza: quel ramo produceva `classifier_resolved=false` in
/// silenzio, che a valle spegneva il dimensionamento senza dire perche'.
pub(crate) async fn resolve_classifier_fields(
    db: &PgPool,
    gateway: &NexusGatewayClient,
    classifier_input: &str,
) -> ClassifierFields {
    let ai = crate::intent_classifier::classify(db, gateway, classifier_input).await;
    // classifier_resolved = il classifier ha prodotto un giudizio (NON un
    // fallback di sistema).
    let (requires_tools, agentic_score, authorizes_changes, classifier_resolved) = (
        Some(ai.requires_tools),
        Some(ai.agentic_score),
        Some(ai.authorizes_changes),
        !ai.fallback_used,
    );
    // Soglia DB action_oriented_min_agentic_score (regola G, mig 0387).
    let action_oriented_min_score =
        nexus_auth::get_setting(db, "routing.action_oriented_min_agentic_score")
            .await
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE);
    ClassifierFields {
        requires_tools,
        agentic_score,
        authorizes_changes,
        classifier_resolved,
        action_oriented_min_score,
    }
}

/// Esito di un run nativo, normalizzato per il chiamante.
///
/// I campi token/iterazioni/intent (oltre ai campi base) servono al call site per
/// costruire un [`crate::agent_types::AgentRunResult`] COMPLETO e farlo convergere
/// sullo STESSO finalizzatore del primario Python (regola L: un solo finalize):
/// `agent_runs` (status/final_answer/usage), `chat_messages`, budget, worklog.
#[derive(Debug, Clone)]
pub struct NativeRunOutcome {
    /// `true` se il run e' arrivato a `End` (Completed); `false` se si e' fermato
    /// su un interrupt HITL (`awaiting_confirmation`) o fan-in
    /// (`awaiting_subagents`).
    pub completed: bool,
    /// `true` se l'interrupt che ha fermato il run e' l'attesa dei sub-run
    /// background (fan-in deterministico, Fase D): distingue questo interrupt da
    /// quello HITL (`awaiting_confirmation`). Popolato da `map_outcome` leggendo
    /// `state.is_awaiting_subagents()`. Letto da `classify_status` per mappare
    /// `AwaitingSubagents` invece di `AwaitingConfirmation` sul ramo interrotto.
    pub awaiting_subagents: bool,
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
    /// Numero di iterazioni dell'executor (campo `iterations` dello stato finale).
    pub iterations: i64,
    /// Token di prompt dell'ULTIMA iterazione: lo stato del grafo e' last-write
    /// per-turno (reducer overwrite in executor.rs), NON cumulativo. Il valore
    /// cumulativo per il billing viene riconciliato a valle dal ledger
    /// (`reconcile_run_cost_from_ledger`). Questo valore alimenta
    /// `last_prompt_tokens` (context ratio della UI).
    pub prompt_tokens: i64,
    /// Token di completion dell'ultima iterazione (stesso reducer last-write).
    pub completion_tokens: i64,
    /// Token totali dell'ultima iterazione (stesso reducer last-write).
    pub total_tokens: i64,
    /// Costo CUMULATIVO del run in USD (`run_cost_cumulative_usd` dello stato:
    /// somma dei costi di tutti i turni). 0.0 se non calcolato a monte.
    ///
    /// ASIMMETRIA VOLUTA rispetto ai token qui sopra, che sono dell'ULTIMO turno:
    /// il costo serve al billing (che vuole il run intero), i token servono al
    /// context ratio (che vuole l'ultima iterazione). Non e' un'incoerenza da
    /// "uniformare": rendere cumulativi i token romperebbe `last_prompt_tokens`
    /// (badge "5046% ctx"). Nel percorso normale entrambi vengono comunque
    /// sovrascritti dal ledger, che e' la fonte autoritativa
    /// (`reconcile_run_cost_from_ledger`); questi valori restano quelli pubblicati
    /// solo quando il gateway non ha contabilizzato nulla per il run.
    pub total_cost: f64,
    /// Intent del turno (campo `user_intent` dello stato): pilota la decisione
    /// hollow/conversational del finalizzatore (parita' col `nexus_task_type`
    /// che il primario Python propaga nell'end_turn).
    pub user_intent: Option<String>,
    /// Ragionamento (thinking) accumulato del run (campo `reasoning_acc` dello
    /// stato): persistito nel `metadata.reasoning` del messaggio assistant cosi'
    /// il blocco "Ragionamento" sopravvive al refresh (FIX D4). `None` se il
    /// modello non ha prodotto thinking.
    pub reasoning: Option<String>,
    /// Conversazione finale del grafo (campo `messages` dello stato), serializzata
    /// nel formato `[{role, content, ...}]` (la forma `Message` del canale interno,
    /// la stessa attesa dal resume e da `generate_agent_turn`). PERSISTITA in
    /// `agent_runs.messages_json` cosi' il resume (`status='interrupted'` filtra
    /// `messages_json IS NOT NULL`) e il trace panel la trovano valorizzata (prima
    /// il run nativo non scriveva mai questa colonna -> NULL). `None` se la
    /// serializzazione fallisce o la conversazione e' vuota.
    pub messages_json: Option<String>,
    /// Esito DICHIARATO dal modello via `task_complete` (ADR 0034): il dict
    /// normalizzato (`outcome`/`summary`/`blocker`/`refusal`/...) dello stato
    /// (`declared_outcome`). Segnale MACCHINA per lo status canonico del
    /// finalizzatore (blocked/needs_input/refusal -> BlockedNeedsInput,
    /// partial -> FailedDiagnosed); il summary e' testo umano di display.
    /// `None` se il modello non ha dichiarato.
    pub declared_outcome: Option<serde_json::Value>,
    /// Verdetto strutturato del REVISORE via tool `review_verdict` (Fase B
    /// ultracode, campo `review_verdict` dello stato): dict normalizzato
    /// {verdict, summary, findings[]} o `None` se il run non era una review o
    /// il revisore non ha dichiarato. Viaggia oltre il confine sub-run dentro
    /// [`Self::structured_verdict`] (campo `review`) cosi' un coordinatore
    /// compone i verdetti dei giudici senza parsare prosa (regola M).
    pub review_verdict: Option<serde_json::Value>,
    /// Parere strutturato di una FIGURA del consiglio di analisi a monte via tool
    /// `advisory_verdict` (campo `advisory_verdict` dello stato): dict normalizzato
    /// {verdict, summary, requirements[], risks[], recommendations[]} o `None` se il
    /// run non era una figura di analisi o non ha dichiarato. Viaggia oltre il
    /// confine sub-run dentro [`Self::structured_verdict`] (campo `advisory`) cosi'
    /// il coordinatore compone la sintesi dei pareri senza parsare prosa (regola M).
    pub advisory_verdict: Option<serde_json::Value>,
    /// Posizione strutturata dell'AVVOCATO via tool `debate_position` (tesi
    /// contrapposte): dict normalizzato {assigned_position, stance, summary,
    /// key_arguments[], risks[]}. `None` nei run che non sono avvocati.
    /// Propagato oltre il confine sub-run in `structured_verdict` (campo
    /// `debate`, regola M) e letto dal punto unico `compose_debate_synthesis`.
    pub debate_position: Option<serde_json::Value>,
    /// Classe d'errore STRUTTURATA del run (`extra.error_class` dello stato, es.
    /// `context_overflow` — ADR 0016 D2): segnale MACCHINA per il finalizzatore
    /// (regola M: mai dedotta dal testo). `None` se il run non ha classificato.
    pub error_class: Option<String>,
    /// Segnale AUTORITATIVO (mig 0386) che il run e' stato chiuso da un abort
    /// anti-loop senza verifica: sopravvive alla riscrittura di `stop_reason`
    /// operata dal final_gate sul ramo forced_close. Senza questo trasporto il
    /// mapping deduceva il forced-close SOLO da stop_reason e un loop ripulito
    /// dal final_gate finiva "completed" col testo di sistema come risposta
    /// (run b833a83d).
    pub forced_close_unverified: bool,
    /// Verdetto del final_gate (`AgentState::final_gate_passed`): `Some(true)`
    /// verifica superata, `Some(false)` verifica NON superata (gate eseguito, i
    /// criteri non passano al cap/forced), `None` gate non eseguito (task
    /// non-software o run interrotto prima). Segnale strutturato (regola M) letto
    /// dal finalizzatore per (a) mappare FailedDiagnosed anche su una
    /// dichiarazione "done" ottimista del modello e (b) annotare il resoconto con
    /// l'esito reale della verifica (run e91d4892: resoconto "completato" ma
    /// status failed_diagnosed).
    pub final_gate_passed: Option<bool>,
    /// `Some(true)` quando il final gate e' entrato ma NON ha potuto verificare
    /// (profilo di verifica dell'ambiente assente): lavoro svolto ma non
    /// verificato. Letto dal finalizzatore per l'esito onesto `CompletedUnverified`
    /// (distinto da `Completed`/`CompletedVerified`). `None` = gate non entrato o
    /// verifica eseguita. Segnale strutturato (regola M).
    pub final_gate_unverified: Option<bool>,
    /// `true` quando l'ULTIMO verdetto del final gate e' una BOCCIATURA con
    /// correzione rimandata all'executor e la ri-verifica non e' mai avvenuta,
    /// con il run chiuso in modo anomalo TRA il gate fallito e il gate
    /// successivo (es. provider esauriti che bruciano i turni fino al cap
    /// iterazioni, run a5db0985).
    ///
    /// Derivazione (regola M): dal SEGNALE `AgentState::final_gate_verdict`
    /// (`FailedPendingCorrection` = l'unico ramo con ri-verifica attesa), non
    /// dal CONTATORE `final_gate_cycle`.
    ///
    /// Storia, perche' non si ripeta: qui c'era
    /// `final_gate_cycle > 0 && !plan_phase_active`, giustificato con
    /// "il gate azzera il ciclo a 0 su OGNI chiusura, quindi solo il ramo di
    /// bocciatura intermedia lascia un ciclo > 0". L'enumerazione era FALSA: il
    /// turno di GRAZIA lascia `cycle = max_cycles` proprio quando i criteri
    /// oggettivi sono TUTTI passati -> un lavoro riuscito chiudeva
    /// `FailedDiagnosed` con "Verifica automatica fallita e non ripetuta". Il
    /// consumatore non puo' tenere aggiornata a mano la lista dei rami del
    /// produttore: l'esito deve avere un campo proprio, e il `match` esaustivo
    /// e' cio' che rende l'accoppiamento verificabile dal compilatore.
    ///
    /// `plan_phase_active` resta come discriminante SOLO dentro il ramo
    /// `FailedPendingCorrection`: in plan-phase il gate e' one-shot
    /// (`final_gate_eligible` la esclude), quindi nessuna ri-verifica era
    /// prevista e la bocciatura non e' "pendente".
    ///
    /// Letto dal finalizzatore: mai `Completed` con una bocciatura pendente.
    pub final_gate_failed_pending: bool,
    /// `true` quando la review adversariale del ReviewGate (nodo del grafo,
    /// prima della chiusura) NON ha approvato (Fail/NeedsChanges) su codice
    /// modificato: segnale STRUTTURATO (regola M) che il run non e' un successo
    /// pulito. Letto da `classify_status` (punto unico, regola L) per mappare
    /// FailedDiagnosed invece di Completed. `Inconclusive` (quorum non
    /// raggiunto) NON e' un rifiuto: limite infra, non difetto del codice ->
    /// resta `false`.
    pub review_panel_rejected: bool,
    /// Esito dell'ultimo panel (`PanelOutcome::to_value`, trasportato dal
    /// ReviewGate in `extra.review_panel_last`): alimenta la nota onesta nel
    /// resoconto. `None` = panel mai convocato.
    pub review_panel_last: Option<serde_json::Value>,
    /// Azioni in attesa di conferma utente (HITL modalita' Conferma), serializzate
    /// dal grafo in `extra.hitl_pending_actions`. Vuoto se nessuna sospensione HITL.
    pub pending_actions: Vec<serde_json::Value>,
}

/// Chiavi del blocco esito STRUTTURATO ([`NativeRunOutcome::structured_verdict`]).
/// PUNTO UNICO (regola L): il produttore, il poll (`tool_nexus_subagent_poll`),
/// i rami degradati/terminali di `subagent_native` e i test riferiscono le
/// STESSE chiavi, mai literal sparsi che divergono nel tempo (era proprio il
/// drift che aveva reso `terminal_verdict` incoerente da `structured_verdict`).
pub(crate) mod verdict_keys {
    /// Status canonico (`AgentRunStatus` snake_case), sempre presente.
    pub const VERDICT: &str = "verdict";
    /// Semantica di successo (`AgentRunStatus::is_success`), sempre presente.
    pub const SUCCESS: &str = "success";
    /// Dict ADR 0034 normalizzato passato as-is (null se non dichiarato).
    pub const DECLARED: &str = "declared";
    /// Verdetto strutturato del REVISORE (Fase B) via `review_verdict`
    /// ({verdict, summary, findings[]}); null nei run non-review.
    pub const REVIEW: &str = "review";
    /// Parere strutturato di una FIGURA del consiglio a monte via `advisory_verdict`
    /// ({verdict, summary, requirements[], risks[], recommendations[]}); null nei
    /// run che non sono figure di analisi.
    pub const ADVISORY: &str = "advisory";
    /// Posizione strutturata di un AVVOCATO del dibattito via `debate_position`
    /// ({assigned_position, stance, summary, key_arguments[], risks[]}); null nei
    /// run che non sono avvocati.
    pub const DEBATE: &str = "debate";
    /// Segnali strutturati grezzi del final_gate / chiusura (ADR 0036).
    pub const FINAL_GATE_PASSED: &str = "final_gate_passed";
    pub const FINAL_GATE_UNVERIFIED: &str = "final_gate_unverified";
    pub const FINAL_GATE_FAILED_PENDING: &str = "final_gate_failed_pending";
    pub const FORCED_CLOSE_UNVERIFIED: &str = "forced_close_unverified";
    pub const ERROR_CLASS: &str = "error_class";
}

/// Stop_reason che denotano una chiusura coordinata anti-loop/forced: elenco
/// piatto (array + `contains`) invece di un `matches!` multi-variante, cosi'
/// `classify_status` resta senza annidamento profondo. Il segnale AUTORITATIVO
/// resta `forced_close_unverified` (mig 0386); questa lista lo integra quando
/// lo stop e' esplicito.
const FORCED_CLOSE_STOPS: [StopReason; 4] = [
    StopReason::LoopDetected,
    StopReason::LoopAbort,
    StopReason::G1Escalated,
    StopReason::G1CapReached,
];

/// Chiave dell'esito DICHIARATO dal modello nel dict normalizzato (ADR 0034,
/// `normalize_declared_outcome`): valori ammessi in `nexus_agent_graph`
/// `VALID_OUTCOMES` (`done`/`blocked`/`needs_input`/`partial`).
const DECLARED_OUTCOME_KEY: &str = "outcome";

impl NativeRunOutcome {
    /// Status CANONICO del run dai segnali strutturati dell'esito (regola M).
    /// PUNTO UNICO (regola L) della classificazione: estratto dal finalizzatore
    /// (`native_outcome_to_run_result`, che vi delega) ed usato anche dal ponte
    /// esito del SUB-agente (`agent_tools::subagent_native`), cosi' il verdetto
    /// che un coordinatore legge da un sub-run e' lo STESSO che il run padre
    /// otterrebbe. L'ordine dei rami e' significativo (dichiarazione onesta del
    /// modello > forced_close > verdetto oggettivo del gate).
    pub fn classify_status(&self) -> crate::agent_types::AgentRunStatus {
        use crate::agent_types::AgentRunStatus;

        // `forced_close_unverified` e' il segnale AUTORITATIVO (mig 0386):
        // sopravvive alla riscrittura di `stop_reason` operata dal final_gate
        // sul ramo forced_close (senza, un abort anti-loop ripulito dal
        // final_gate finiva "completed" col testo di sistema come risposta —
        // run b833a83d).
        let forced_close = self.forced_close_unverified
            || self
                .stop_reason
                .is_some_and(|s| FORCED_CLOSE_STOPS.contains(&s));
        // Esito DICHIARATO dal modello via task_complete (ADR 0034): segnale
        // MACCHINA (enum/bool), letto dal dict normalizzato — mai dalla prosa
        // (regola M). Ha precedenza sul forced_close: una dichiarazione onesta
        // (es. blocked su credenziale mancante) e' piu' specifica del segnale
        // generico di chiusura coordinata.
        let declared_kind = self
            .declared_outcome
            .as_ref()
            .and_then(|v| v.get(DECLARED_OUTCOME_KEY))
            .and_then(Value::as_str);
        let declared_refusal = self
            .declared_outcome
            .as_ref()
            .and_then(|v| v.get("refusal"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !self.completed && self.resume_at.is_some() {
            // Ramo INTERROTTO (run SOSPESO, resumibile): due motivi di
            // interrupt-resume oggi. Il fan-in dei sub-run background (Fase D)
            // porta il segnale strutturato `awaiting_subagents` (regola M); il
            // resto e' l'attesa di conferma umana (HITL). Discriminati qui cosi'
            // il worker fan-in seleziona SOLO i propri run (CAS su questo status).
            if self.awaiting_subagents {
                AgentRunStatus::AwaitingSubagents
            } else {
                AgentRunStatus::AwaitingConfirmation
            }
        } else if matches!(self.stop_reason, Some(StopReason::Error)) {
            AgentRunStatus::Failed
        } else if declared_refusal || matches!(declared_kind, Some("blocked") | Some("needs_input"))
        {
            // Bloccato per causa esterna / serve input umano / rifiuto safety:
            // esito canonico BlockedNeedsInput (parita' WAVE 3.2 del path brain).
            AgentRunStatus::BlockedNeedsInput
        } else if matches!(declared_kind, Some("partial")) {
            // Lavoro dichiarato PARZIALE: onesto, non un successo (mai
            // "completed" su una dichiarazione esplicita di incompletezza).
            AgentRunStatus::FailedDiagnosed
        } else if forced_close {
            AgentRunStatus::FailedDiagnosed
        } else if self.final_gate_passed == Some(false) {
            // Verifica oggettiva pre-chiusura NON superata (final_gate al
            // cap/forced): il verdetto STRUTTURATO del gate (regola M) prevale
            // su una dichiarazione "done" ottimista del modello -> mai
            // "completed" su un task la cui verifica e' fallita. Difesa in
            // profondita' rispetto a `forced_close`: il cap del final_gate NON
            // imposta forced_close_unverified.
            AgentRunStatus::FailedDiagnosed
        } else if self.review_panel_rejected {
            // Review adversariale programmatica NON approvata (Fail/NeedsChanges)
            // su codice modificato: il verdetto STRUTTURATO del panel (regola M)
            // prevale sulla dichiarazione "done" ottimista del modello. Mai
            // "completed" su un lavoro che un panel indipendente ha bocciato con
            // difetti bloccanti. Stessa classe di `final_gate_passed == false`.
            AgentRunStatus::FailedDiagnosed
        } else if self.final_gate_failed_pending {
            // L'ULTIMO verdetto del final_gate e' una BOCCIATURA di ciclo
            // intermedio (correzione rimandata all'executor) e il run e' morto
            // PRIMA della ri-verifica (run a5db0985): il lavoro post-bocciatura
            // non e' mai stato verificato e l'ultimo esito oggettivo noto e' un
            // fallimento — mai `Completed` (regola M).
            AgentRunStatus::FailedDiagnosed
        } else if self.final_gate_unverified == Some(true) {
            // Lavoro SVOLTO ma verifica tecnica NON eseguita (gate entrato
            // senza profilo di verifica dell'ambiente): esito ONESTO distinto
            // da un "completato" pieno (is_success=true ma etichettato).
            AgentRunStatus::CompletedUnverified
        } else {
            AgentRunStatus::Completed
        }
    }

    /// Blocco esito STRUTTURATO machine-readable dell'intero run (regola M /
    /// ADR 0034), il PONTE del confine padre<->figlio: persistito su
    /// `nexus_subagent_runs.verdict` (mig project/0009) per il fan-in asincrono
    /// (poll/resume) ed esposto come campo `outcome` nel tool_result dei
    /// finalizzatori del sub-run; disponibile per un coordinatore che compone i
    /// verdetti dei sub-run in modo deterministico.
    ///
    /// `verdict`/`success` derivano dal punto unico [`Self::classify_status`],
    /// cosi' il verdetto del sub-run coincide con quello che il run padre
    /// otterrebbe sugli stessi segnali. `declared` e' il dict gia' normalizzato
    /// (ADR 0034) o `null`; gli altri campi sono i segnali del gate as-is.
    pub fn structured_verdict(&self) -> Value {
        use verdict_keys as k;
        let status = self.classify_status();
        serde_json::json!({
            k::VERDICT: status.as_str(),
            k::SUCCESS: status.is_success(),
            k::DECLARED: self.declared_outcome,
            // Verdetto del REVISORE (Fase B ultracode): presente solo nei run
            // di review col tool `review_verdict` whitelistato; `null` altrove.
            k::REVIEW: self.review_verdict,
            // Parere della FIGURA (consiglio a monte): presente solo nei run di
            // analisi col tool `advisory_verdict` whitelistato; `null` altrove.
            k::ADVISORY: self.advisory_verdict,
            // Posizione dell'AVVOCATO (dibattito): presente solo nei run col tool
            // `debate_position` whitelistato; `null` altrove.
            k::DEBATE: self.debate_position,
            k::FINAL_GATE_PASSED: self.final_gate_passed,
            k::FINAL_GATE_UNVERIFIED: self.final_gate_unverified,
            k::FINAL_GATE_FAILED_PENDING: self.final_gate_failed_pending,
            k::FORCED_CLOSE_UNVERIFIED: self.forced_close_unverified,
            k::ERROR_CLASS: self.error_class,
        })
    }
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

/// Performance tier del modello del turno INIZIALE dal catalog (`ai_price_catalog`,
/// colonna `performance_tier`). PUNTO UNICO (regola L/H) della risoluzione del tier
/// iniziale dello scale-controller (FIX-A): il DB e' interrogato UNA volta all'avvio
/// del run (percorso Real, fuori dal loop del grafo), quindi il replay del grafo NON
/// vede questa query (il tier viaggia nello stato checkpointato). Fallback
/// deterministico `"medium"` (default catalog, mig 0032) se il modello non e' nel
/// catalog o su errore: NON e' un magic value (regola G) ma il default della colonna
/// `performance_tier`, coerente con il fallback di `build_scale_context`.
async fn resolve_initial_tier(db: &PgPool, provider: &str, model: &str) -> String {
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT performance_tier FROM ai_price_catalog \
         WHERE provider = $1 AND model = $2 LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    row.and_then(|(t,)| t)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "medium".to_string())
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

// ── Coercizione setting tipizzati (parita' 1:1 con `_coerce` del brain) ───────
//
// Il brain (`orchestrator_config.py::_coerce`) converte il `value` testuale del DB
// nel tipo del default: bool da `{true,1,yes,on}` (case-insensitive); CSV ->
// `list[str]` con strip + scarto dei vuoti; int/float con parse e fallback al
// default. Replichiamo qui la STESSA semantica perche' le config nodi Rust devono
// coincidere coi valori che il brain calcola dalle medesime chiavi (presupposto
// del confronto shadow). `get_setting` (punto unico, regola L) gia' fa trim +
// scarto dei vuoti -> una chiave assente/vuota torna `None` e si usa il default.

/// `value` -> bool con la semantica `_coerce` del brain (`{true,1,yes,on}` truthy).
fn coerce_bool(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// `value` CSV -> `Vec<String>` (strip per elemento + scarto dei vuoti), come
/// `_coerce` sui default di tipo `list`.
fn coerce_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Legge un setting bool dal DB (chiave `key`) col fallback al `default` se
/// assente/vuoto (safe-default identico al brain). Punto unico `get_setting`.
async fn setting_bool(db: &PgPool, key: &str, default: bool) -> bool {
    match nexus_auth::get_setting(db, key).await {
        Some(raw) => coerce_bool(&raw),
        None => default,
    }
}

/// Legge un setting i64 dal DB col fallback al `default` (parse tollerante: un
/// valore non parsabile cade sul default, come `_coerce`).
async fn setting_i64(db: &PgPool, key: &str, default: i64) -> i64 {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// Legge un setting f64 dal DB col fallback al `default`.
async fn setting_f64(db: &PgPool, key: &str, default: f64) -> f64 {
    nexus_auth::get_setting(db, key)
        .await
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

/// Legge un setting CSV dal DB col fallback al `default` (lista) se assente/vuoto.
async fn setting_csv(db: &PgPool, key: &str, default: Vec<String>) -> Vec<String> {
    match nexus_auth::get_setting(db, key).await {
        Some(raw) => coerce_csv(&raw),
        None => default,
    }
}

/// Legge un setting CSV di interi (es. "3,7,15,30") col fallback al `default`
/// (lista). Degrada IN BLOCCO al default se la chiave manca, e' vuota o se UN
/// QUALSIASI elemento non e' parsabile: niente liste parziali silenziose (le
/// fasi di compressione vanno coerenti tra boundaries/keep_recent/max_chars).
async fn setting_i64_csv(db: &PgPool, key: &str, default: Vec<i64>) -> Vec<i64> {
    match nexus_auth::get_setting(db, key).await {
        Some(raw) => {
            let parsed: Option<Vec<i64>> = raw
                .split(',')
                .map(|s| s.trim().parse::<i64>().ok())
                .collect();
            match parsed {
                Some(v) if !v.is_empty() => v,
                _ => default,
            }
        }
        None => default,
    }
}

/// Legge un setting stringa dal DB col fallback al `default` se la chiave e'
/// assente. NB: una stringa presente ma VUOTA viene restituita com'e' (stringa
/// vuota), NON cade sul default: la semantica "vuoto = disabilitato" e'
/// responsabilita' del chiamante (es. build_command vuoto -> nessun criterio).
async fn setting_string(db: &PgPool, key: &str, default: &str) -> String {
    nexus_auth::get_setting(db, key)
        .await
        .unwrap_or_else(|| default.to_string())
}

async fn load_supervisor_config(db: &PgPool) -> SupervisorConfig {
    SupervisorConfig {
        interleaved_interval: setting_i64(
            db,
            "agent.supervisor.interleaved_interval",
            DEFAULT_INTERLEAVED_INTERVAL,
        )
        .await
        .max(1),
        anomaly_step_threshold: setting_i64(
            db,
            "agent.supervisor.anomaly_step_threshold",
            DEFAULT_ANOMALY_STEP_THRESHOLD,
        )
        .await
        .max(1),
    }
}

/// Converte il tipo condiviso mcp-core nel tipo del grafo (stesse varianti).
pub fn graph_supervisor_mode(mode: crate::agent_types::SupervisorMode) -> SupervisorMode {
    mode.as_str().parse::<SupervisorMode>().unwrap()
}

/// Costruisce la [`PlannerConfig`] DB-driven (regola G), 1:1 con le chiavi
/// `orchestrator.*` lette dal brain (`orchestrator_config.py`). I campi che il
/// brain NON popola da `orchestrator_config` restano al loro `Default`:
/// - `planner_system_text`: prompt RISOLTO A MONTE dal registry (regola G) — non
///   ancora portato nella cablatura nativa, resta vuoto (`prompt_missing` -> skip,
///   parita' col safe-default);
/// - `turn_focus_enabled`: viene dalla CONTINUITY config (`agent.context.turn_focus_enabled`),
///   non da `orchestrator_config` — TODO wiring continuity (default true).
async fn load_planner_config(db: &PgPool) -> PlannerConfig {
    let d = PlannerConfig::default();
    PlannerConfig {
        plan_phase_enabled: setting_bool(
            db,
            "orchestrator.plan_phase_enabled",
            d.plan_phase_enabled,
        )
        .await,
        plan_behavior_modes: setting_csv(
            db,
            "orchestrator.plan_behavior_modes",
            d.plan_behavior_modes,
        )
        .await,
        plan_intents: setting_csv(db, "orchestrator.plan_intents", d.plan_intents).await,
        plan_min_token_budget: setting_i64(
            db,
            "orchestrator.plan_min_token_budget",
            d.plan_min_token_budget,
        )
        .await,
        planner_prompt_key: nexus_auth::get_setting(db, "orchestrator.planner_prompt_key")
            .await
            .unwrap_or(d.planner_prompt_key),
        clarifying_questions_enabled: setting_bool(
            db,
            "orchestrator.clarifying_questions_enabled",
            d.clarifying_questions_enabled,
        )
        .await,
        clarifying_questions_max: setting_i64(
            db,
            "orchestrator.clarifying_questions_max",
            d.clarifying_questions_max,
        )
        .await,
        plan_rationale_enabled: setting_bool(
            db,
            "orchestrator.plan_rationale_enabled",
            d.plan_rationale_enabled,
        )
        .await,
        dag_topological_enabled: setting_bool(
            db,
            "orchestrator.dag_topological_enabled",
            d.dag_topological_enabled,
        )
        .await,
        // Gate orchestrazione LLM-driven della plan-phase (Fase 1). DEFAULT SAFE
        // FALSE (regola G): il setting sara' seminato dalla migrazione del
        // sotto-blocco successivo; finche' assente o false il gate ricade sempre
        // su is_eligible -> comportamento bit-identico a oggi.
        orchestration_enabled: setting_bool(
            db,
            "agent.orchestration.enabled",
            d.orchestration_enabled,
        )
        .await,
        // Risolti a monte / da altra fonte (vedi doc della funzione): default.
        planner_system_text: d.planner_system_text,
        turn_focus_enabled: d.turn_focus_enabled,
    }
}

/// Costruisce la [`FinalGateConfig`] DB-driven (regola G), 1:1 con le chiavi che
/// il brain legge da `orchestrator_config` (prefisso `agent.final_gate.*` +
/// `agent.no_orphan.min_ratio` + `agent.import_staging_dirs`; `criteria_timeout_s`
/// = `orchestrator.verifier_timeout_s`).
///
/// Restano al `Default` i campi RISOLTI PER-PROGETTO a monte (regola G), non
/// ancora portati nella cablatura nativa (gli stessi gia' OFF nel TODO esistente):
/// `build_command`/`build_working_dir` (`_resolve_build_command`), `log_command`
/// (`_resolve_log_command`), `endpoint_criterion` (`_resolve_endpoint_check`). Con
/// `None`/vuoto il criterio corrispondente NON si aggiunge (non blocca, niente
/// toppa). `build_timeout_s`/`build_output_max_chars` vivono in DB ma servono SOLO
/// quando `build_command` e' risolto: si leggono comunque per fedelta'.
/// Config del ReviewGate (gemella del loader del final_gate). Le chiavi sono
/// quelle della review programmatica gia' esistenti + il cap dei rimandi
/// (mig 0625). Regola G: tutto dal DB, il default e' solo safe-default.
async fn load_review_gate_config(db: &PgPool) -> ReviewGateConfig {
    ReviewGateConfig {
        enabled: setting_bool(db, "orchestrator.review_panel_autoconvene_enabled", true).await,
        max_cycles: setting_i64(db, "orchestrator.review_max_correction_cycles", 1).await,
    }
}

async fn load_final_gate_config(db: &PgPool) -> FinalGateConfig {
    let d = FinalGateConfig::default();
    // Criteri COMANDO (ADR 0036): la catena per-ambiente arriva dal profilo
    // inferito da LLM (`verify_profile::ensure_profile`), risolta in
    // `run_engine` e innestata in `verify_steps` DOPO questo loader (serve
    // project/root della sessione, che qui non ci sono). Il vecchio
    // `agent.final_gate.build_command` generico e' stato RIMOSSO (mig 0508,
    // decisione utente: nessuna conoscenza d'ambiente fissa — era proprio il
    // "npm run build" cieco che per Vite non type-checka).
    FinalGateConfig {
        enabled: setting_bool(db, "agent.final_gate.enabled", d.enabled).await,
        max_cycles: setting_i64(db, "agent.final_gate.max_cycles", d.max_cycles).await,
        runtime_check_enabled: setting_bool(
            db,
            "agent.final_gate.runtime_check_enabled",
            d.runtime_check_enabled,
        )
        .await,
        build_timeout_s: setting_f64(db, "agent.final_gate.build_timeout_s", d.build_timeout_s)
            .await,
        build_output_max_chars: setting_i64(
            db,
            "agent.final_gate.build_output_max_chars",
            d.build_output_max_chars,
        )
        .await,
        runtime_error_patterns: setting_csv(
            db,
            "agent.final_gate.runtime_error_patterns",
            d.runtime_error_patterns,
        )
        .await,
        no_orphan_min_ratio: setting_f64(db, "agent.no_orphan.min_ratio", d.no_orphan_min_ratio)
            .await,
        import_staging_dirs: setting_csv(db, "agent.import_staging_dirs", d.import_staging_dirs)
            .await,
        criteria_timeout_s: setting_f64(
            db,
            "orchestrator.verifier_timeout_s",
            d.criteria_timeout_s,
        )
        .await,
        // verify_steps/verify_profile_missing innestati in run_engine (profilo
        // per-ambiente, ADR 0036). log_command / endpoint_criterion restano
        // risolti per-progetto a monte (non ancora portati): default
        // vuoto/None (niente criterio, non blocca).
        verify_steps: d.verify_steps,
        verify_profile_missing: d.verify_profile_missing,
        log_command: d.log_command,
        endpoint_criterion: d.endpoint_criterion,
        design_verify_enabled: setting_bool(
            db,
            "agent.final_gate.design_verify_enabled",
            d.design_verify_enabled,
        )
        .await,
        design_verify_min_score: setting_i64(
            db,
            "agent.final_gate.design_verify_min_score",
            d.design_verify_min_score,
        )
        .await,
        // ADR 0018 leva 3 (mig 0503): kill-switch dei criteri strutturali.
        structural_criteria_enabled: setting_bool(
            db,
            "agent.final_gate.structural_criteria_enabled",
            d.structural_criteria_enabled,
        )
        .await,
        // Escalation su non-convergenza del gate (mig 0577): al cap di max_cycles con
        // criteri oggettivi ancora falliti, cede il turno all'executor per promuovere
        // un modello piu' capace invece di chiudere secco. `max_escalations` RIUSA la
        // chiave dell'executor (stesso budget `auto_escalations` condiviso, regola L/G).
        escalate_on_nonconvergence: setting_bool(
            db,
            "agent.final_gate.escalate_on_nonconvergence",
            d.escalate_on_nonconvergence,
        )
        .await,
        max_escalations: setting_i64(db, "agent.executor.max_escalations", d.max_escalations).await,
    }
}

/// Costruisce la [`VerifierConfig`] DB-driven (regola G), 1:1 con le chiavi che il
/// brain legge da `orchestrator_config`: `verifier_enabled`, `max_verify_cycles`,
/// `verifier_fail_closed` (`agent.verifier.fail_closed`), `dag_topological_enabled`.
/// `exploratory_verify_max_total` e' un default-locale del verifier_node (rami
/// esplorativi OFF + non portati): resta al `Default`.
async fn load_verifier_config(db: &PgPool) -> VerifierConfig {
    let d = VerifierConfig::default();
    VerifierConfig {
        enabled: setting_bool(db, "orchestrator.verifier_enabled", d.enabled).await,
        max_verify_cycles: setting_i64(db, "orchestrator.max_verify_cycles", d.max_verify_cycles)
            .await,
        fail_closed: setting_bool(db, "agent.verifier.fail_closed", d.fail_closed).await,
        dag_topological_enabled: setting_bool(
            db,
            "orchestrator.dag_topological_enabled",
            d.dag_topological_enabled,
        )
        .await,
        exploratory_verify_max_total: d.exploratory_verify_max_total,
    }
}

/// Costruisce la [`RoutingConfig`] DB-driven (regola G): legge dal DB i campi che
/// il brain risolve da `orchestrator_config` / `_load_g1_max_nudges` /
/// `_load_pending_steps_config`, col PUNTO UNICO `nexus_auth::get_setting` (helper
/// `setting_*`). Il `recursion_limit` effettivo e' calcolato da
/// [`nexus_agent_graph::routing::effective_recursion_limit`] (punto unico, regola L):
/// `max(pavimento DB, topologia da iteration_cap + stall/G1/final_gate)`.
/// Tutti gli ALTRI campi restano al `Default` (safe-default identico ai
/// `_SAFE_DEFAULTS` del brain: valgono SOLO se la chiave manca o il DB e'
/// irraggiungibile, mai come magic fallback nella logica).
async fn load_routing_config(db: &PgPool) -> RoutingConfig {
    let d = RoutingConfig::default();
    let db_floor: u32 = nexus_auth::get_setting(db, "agent.graph.recursion_limit")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(d.recursion_limit);
    let iteration_cap = setting_i64(
        db,
        "agent.executor.iteration_cap",
        ExecutorConfig::default().iteration_cap,
    )
    .await;
    let stall_recovery_enabled =
        setting_bool(db, "agent.stall_recovery.enabled", false).await;
    let stall_max_moves = setting_i64(
        db,
        "agent.stall_recovery.max_moves_per_session",
        6,
    )
    .await;
    let g1_max_nudges = setting_i64(db, "agent.g1_max_nudges", d.g1_max_nudges).await;
    let final_gate_max_cycles =
        setting_i64(db, "agent.final_gate.max_cycles", d.final_gate_max_cycles).await;
    let recursion_limit = nexus_agent_graph::routing::effective_recursion_limit(
        &nexus_agent_graph::routing::GraphTopologyLimits {
            db_floor,
            iteration_cap,
            stall_recovery_enabled,
            stall_max_moves,
            g1_max_nudges,
            final_gate_max_cycles,
        },
    );
    if recursion_limit > db_floor {
        tracing::debug!(
            db_floor,
            effective = recursion_limit,
            iteration_cap,
            stall_recovery_enabled,
            stall_max_moves,
            g1_max_nudges,
            final_gate_max_cycles,
            "recursion_limit scalato sulla topologia del grafo agentico"
        );
    }
    RoutingConfig {
        recursion_limit,
        g1_max_nudges,
        final_gate_max_cycles,
        todo_isolation_enabled: setting_bool(
            db,
            "agent.continuous.todo_isolation_enabled",
            d.todo_isolation_enabled,
        )
        .await,
        pending_steps_detection_enabled: setting_bool(
            db,
            "agent.closure.pending_steps_detection_enabled",
            d.pending_steps_detection_enabled,
        )
        .await,
        pending_steps_min_items: setting_i64(
            db,
            "agent.closure.pending_steps_min_items",
            d.pending_steps_min_items,
        )
        .await,
        final_gate_software_intents: setting_csv(
            db,
            "agent.final_gate.software_intents",
            d.final_gate_software_intents,
        )
        .await,
        fs_mutator_tools: setting_csv(
            db,
            "agent.tools.result_cache_mutators",
            d.fs_mutator_tools,
        )
        .await,
        ..RoutingConfig::default()
    }
}

/// Costruisce la [`ExecutorConfig`] DB-driven (regola G): provider/model sono
/// RISOLTI A MONTE (passati come parametri, mai letti dal nodo); i flag/soglie
/// anti-loop + smart-upscale + direttiva di verifica vengono letti dal DB col
/// PUNTO UNICO `nexus_auth::get_setting` (helper `setting_*`). Il `context_window`
/// del modello del turno (gia' risolto a monte da `resolve_context_window`, regola G)
/// arriva come PARAMETRO ed e' impostato qui: era il bug che lo lasciava a 0 (il
/// valore finiva solo nel `ToolDispatchConfig`, mai nell'ExecutorConfig), rendendo
/// INERTI tutte le difese gate da `if context_window > 0` (token_brake, forced_rag,
/// smart-upscale). I parametri di context management (`agent.context.*`, mig 0199/0429)
/// sono ora letti dal DB e popolano `ctx_mgmt` / `token_brake` / `forced_rag_*` invece
/// di restare ai safe-default hardcoded del nodo (regola G: il DB e' l'unica fonte; i
/// `Default` valgono SOLO se la chiave manca o il DB e' irraggiungibile). Le chiavi
/// `agent.progress_controller_enabled` / `agent.repeated_action_force_diagnose_enabled`
/// usano l'underscore: e' la chiave reale del setting nel DB.
/// Attesa massima della barriera di scrittura advisory (mig 0606), CLAMPATA
/// alla deadline residua del run (fase 3, mig 0604).
///
/// Perche' il clamp: se la barriera potesse attendere oltre la deadline, un run
/// col tempo scaduto verrebbe chiuso dall'executor con reason `time_budget`
/// mentre in realta' stava aspettando il consiglio — la causa vera sparirebbe
/// dietro un sintomo (regola M: il motivo dichiarato dev'essere quello reale).
/// Senza deadline configurata (`run_time_budget_s=0`) resta il timeout nudo.
async fn advisory_gate_timeout(db: &PgPool) -> u64 {
    let configured = setting_i64(
        db,
        "orchestrator.advisory_gate_timeout_s",
        ToolDispatchConfig::default().advisory_gate_timeout_s as i64,
    )
    .await
    .max(0) as u64;
    let deadline_s = setting_i64(db, "agent.run_time_budget_s", 0).await;
    if deadline_s <= 0 {
        return configured;
    }
    // Il run e' appena partito: il residuo e' l'intera deadline meno il tempo
    // gia' speso dai panel (che qui e' ~0: nel ramo overlap partono INSIEME).
    configured.min(deadline_s as u64)
}

/// Budget di tempo EFFETTIVO del run: l'override del chiamante vince sul setting
/// globale letto dal DB. PUNTO UNICO (regola L) della precedenza, cosi' non vive
/// inline in mezzo alla costruzione della config.
///
/// Perche' esiste: `agent.run_time_budget_s` e' `0` per policy (mig 0604/0607),
/// quindi per un SUB-RUN il valore dal DB e' inservibile — il tetto reale della
/// figura e' il `timeout_s` risolto in `prepare_subagent_run` e applicato dal
/// `tokio::time::timeout` esterno. Senza questa precedenza il gate a tempo
/// dell'executor resta codice morto per le figure e nessun sollecito di chiusura
/// puo' scattare: la figura muore muta.
fn effective_run_time_budget_s(from_db: u64, from_caller: Option<u64>) -> u64 {
    from_caller.unwrap_or(from_db)
}

async fn load_executor_config(
    db: &PgPool,
    provider: &str,
    model: &str,
    context_window: i64,
) -> ExecutorConfig {
    let d = ExecutorConfig::default();
    ExecutorConfig {
        routing_provider: provider.to_string(),
        routing_model: model.to_string(),
        g1_max_nudges: setting_i64(db, "agent.g1_max_nudges", d.g1_max_nudges).await,
        // Safety net finale del run: il doc di ExecutorConfig la dichiarava
        // DB-driven ma il loader NON la leggeva (restava la costante 60).
        // Chiave seminata dalla mig 0506 (regola G, incoerenza rilevata dal
        // censimento anti-loop / ADR 0035).
        iteration_cap: setting_i64(db, "agent.executor.iteration_cap", d.iteration_cap).await,
        // Ex costanti hardcoded portate in DB (regola G): offset forced-text,
        // budget escalation unico, soglie loop-by-signature.
        forced_text_offset: setting_i64(
            db,
            "agent.executor.forced_text_offset",
            d.forced_text_offset,
        )
        .await,
        max_escalations: setting_i64(db, "agent.executor.max_escalations", d.max_escalations).await,
        // Limiti anti-runaway basati sui TOKEN (mig 0520, regola G): budget token
        // cumulativo per run + fast-fail su turni solo-testo consecutivi. `0` =
        // disabilitato -> bit-identico. Complementari a iteration_cap (che conta
        // ITERAZIONI): chiudono il buco del modello che ignora force_tool_choice e
        // brucia token in turni solo-testo (osservato 1.8M token / $2.42 senza
        // convergere). Lettura via setting_i64 (punto unico) + clamp non-negativo.
        // Budget token PER-TURNO: TRIGGER del giudice/escalation (si resetta
        // all'escalation). Il freno di SPESA del run e' separato e in dollari
        // (run_cost_budget_usd sotto): il costo cumulativo reale, esatto anche dopo
        // un'escalation cross-tier, senza convertire $ in token sul modello iniziale.
        run_token_budget: setting_i64(db, "agent.run_token_budget", d.run_token_budget as i64)
            .await
            .max(0) as u64,
        // BACKSTOP di catastrofe (mig 0521, regola G): col meta-reasoner acceso il
        // run_token_budget diventa il TRIGGER del giudice, questo hard-cap e' la rete
        // di sicurezza non-negoziabile che chiude d'autorita' senza consultarlo.
        run_token_hard_cap: setting_i64(db, "agent.run_token_hard_cap", d.run_token_hard_cap as i64)
            .await
            .max(0) as u64,
        // Freno di SPESA in dollari dell'intero run (mig 0533, regola G): 0 =
        // disabilitato. Confrontato col costo cumulativo reale accumulato per-turno.
        run_cost_budget_usd: setting_f64(db, "agent.run_cost_budget_usd", d.run_cost_budget_usd)
            .await
            .max(0.0),
        // Deadline dell'intero run in secondi (mig 0604, fase 3 paradigma
        // orchestrazione): 0 = disabilitato (bit-identico).
        run_time_budget_s: setting_i64(db, "agent.run_time_budget_s", d.run_time_budget_s as i64)
            .await
            .max(0) as u64,
        // Soglia del turno di grazia a tempo (mig 0614): percentuale del budget
        // oltre cui un canale di ruolo ancora muto viene SOLLECITATO a chiudere,
        // invece di morire n/d allo scadere. 0 = disabilitato.
        time_grace_pct: setting_i64(db, "agent.time_grace_pct", d.time_grace_pct as i64)
            .await
            .clamp(0, 100) as u64,
        // Tetto sui turni consecutivi falliti al gateway con causa deterministica
        // (mig 0619): oltre, si chiude con esito onesto invece di ritentare la
        // stessa chiamata fino al budget. 0 = disabilitato.
        gateway_deterministic_streak_max: setting_i64(
            db,
            "agent.gateway_deterministic_streak_max",
            d.gateway_deterministic_streak_max as i64,
        )
        .await
        .max(0) as u64,
        max_consecutive_text_only_turns: setting_i64(
            db,
            "agent.max_consecutive_text_only_turns",
            d.max_consecutive_text_only_turns as i64,
        )
        .await
        .max(0) as u32,
        // 3.4 (provider-no-progress switch): DB-driven, default OFF (bit-identico).
        provider_no_progress_switch_enabled: setting_bool(
            db,
            "agent.provider_no_progress.enabled",
            d.provider_no_progress_switch_enabled,
        )
        .await,
        loop_thresholds: LoopThresholds {
            signature: setting_i64(
                db,
                "agent.loop.signature_threshold",
                d.loop_thresholds.signature as i64,
            )
            .await
            .max(1) as usize,
            cap: setting_i64(
                db,
                "agent.loop.recent_signatures_cap",
                d.loop_thresholds.cap as i64,
            )
            .await
            .max(1) as usize,
        },
        // Anti repetition-collapse del testo (mig 0531, regola G): soglie della
        // rilevazione del turno degenere (stessa sottostringa ripetuta N+ volte).
        // `scan_tail_cap=0` disabilita -> bit-identico. Clamp non-negativo; le
        // lunghezze minime >=1 (0 sarebbe insensato).
        repetition: RepetitionThresholds {
            min_unit_len: setting_i64(
                db,
                "agent.anti_repetition.min_unit_len",
                d.repetition.min_unit_len as i64,
            )
            .await
            .max(1) as usize,
            max_unit_len: setting_i64(
                db,
                "agent.anti_repetition.max_unit_len",
                d.repetition.max_unit_len as i64,
            )
            .await
            .max(1) as usize,
            min_repeats: setting_i64(
                db,
                "agent.anti_repetition.min_repeats",
                d.repetition.min_repeats as i64,
            )
            .await
            .max(0) as usize,
            min_total_len: setting_i64(
                db,
                "agent.anti_repetition.min_total_len",
                d.repetition.min_total_len as i64,
            )
            .await
            .max(0) as usize,
            scan_tail_cap: setting_i64(
                db,
                "agent.anti_repetition.scan_tail_cap",
                d.repetition.scan_tail_cap as i64,
            )
            .await
            .max(0) as usize,
        },
        // Soglia dell'asse RepeatedUserQuestion (loop clarify cross-run, mig 0510).
        repeated_user_question_threshold: setting_i64(
            db,
            "agent.loop.repeated_user_question_threshold",
            d.repeated_user_question_threshold,
        )
        .await,
        // ── Meta-reasoner di recovery-da-stallo (mig 0510, opt-in DB, regola G) ─
        // Con enabled=false (default) il gate di emissione StallReason nell'executor
        // non scatta mai -> comportamento bit-identico a oggi. Il budget per-sessione
        // e' il cap duro anti meta-loop/costo del gate.
        stall_recovery_enabled: setting_bool(
            db,
            "agent.stall_recovery.enabled",
            d.stall_recovery_enabled,
        )
        .await,
        stall_recovery_max_moves_per_session: setting_i64(
            db,
            "agent.stall_recovery.max_moves_per_session",
            d.stall_recovery_max_moves_per_session,
        )
        .await,
        // ── Scale-controller (mig 0516, opt-in DB, regola G) ──────────────────
        // Con enabled=false (default) il detector-emissione dell'executor salta
        // PRIMA di ogni lavoro -> nessun ScaleReason -> nodo ScaleControl mai
        // raggiunto -> BIT-IDENTICO. Tutte le soglie dai settings agent.scale.*.
        scale: ScaleConfig {
            enabled: setting_bool(db, "agent.scale.enabled", d.scale.enabled).await,
            downscale_enabled: setting_bool(
                db,
                "agent.scale.downscale_enabled",
                d.scale.downscale_enabled,
            )
            .await,
            eval_every_iters: setting_i64(
                db,
                "agent.scale.eval_every_iters",
                d.scale.eval_every_iters,
            )
            .await,
            min_tail_iters: setting_i64(db, "agent.scale.min_tail_iters", d.scale.min_tail_iters)
                .await,
            min_confidence: setting_f64(db, "agent.scale.min_confidence", d.scale.min_confidence)
                .await,
            change_cooldown_turns: setting_i64(
                db,
                "agent.scale.change_cooldown_turns",
                d.scale.change_cooldown_turns,
            )
            .await,
            downscale_clean_window: setting_i64(
                db,
                "agent.scale.downscale_clean_window",
                d.scale.downscale_clean_window,
            )
            .await,
            max_reversals: setting_i64(db, "agent.scale.max_reversals", d.scale.max_reversals)
                .await,
            max_tier_changes_per_run: setting_i64(
                db,
                "agent.scale.max_tier_changes_per_run",
                d.scale.max_tier_changes_per_run,
            )
            .await,
            max_evals_per_run: setting_i64(
                db,
                "agent.scale.max_evals_per_run",
                d.scale.max_evals_per_run,
            )
            .await,
            window_overhead_ratio: setting_f64(
                db,
                "agent.scale.window_overhead_ratio",
                d.scale.window_overhead_ratio,
            )
            .await,
            // ── Sizing agentico (mig 0524, kill-switch nested, opt-in DB, regola G) ─
            // Con sizing_enabled=false (default) il detector non popola i segnali
            // sizing e il gate degrada ogni AdjustSizing a KeepTier -> flusso tier
            // BIT-IDENTICO anche con scale ON.
            sizing_enabled: setting_bool(db, "agent.scale.sizing_enabled", d.scale.sizing_enabled)
                .await,
            sizing_cooldown_turns: setting_i64(
                db,
                "agent.scale.sizing_cooldown_turns",
                d.scale.sizing_cooldown_turns,
            )
            .await,
            sizing_aggressiveness: setting_f64(
                db,
                "agent.scale.sizing_aggressiveness",
                d.scale.sizing_aggressiveness,
            )
            .await,
        },
        progress_controller_enabled: setting_bool(
            db,
            "agent.progress_controller_enabled",
            d.progress_controller_enabled,
        )
        .await,
        repeated_action_threshold: setting_i64(
            db,
            "agent.repeated_action_threshold",
            d.repeated_action_threshold,
        )
        .await,
        repeated_action_threshold_read_only: setting_i64(
            db,
            "agent.repeated_action_threshold.read_only",
            d.repeated_action_threshold_read_only,
        )
        .await,
        recoverable_client_error_codes: setting_csv(
            db,
            "routing.client_error_failover_codes",
            d.recoverable_client_error_codes.clone(),
        )
        .await,
        repeated_action_force_diagnose_enabled: setting_bool(
            db,
            "agent.repeated_action_force_diagnose_enabled",
            d.repeated_action_force_diagnose_enabled,
        )
        .await,
        reallocation_threshold: setting_i64(
            db,
            "agent.loop.resource_reallocation_threshold",
            d.reallocation_threshold,
        )
        .await,
        upscale_enabled: setting_bool(db, "agent.upscale.enabled", d.upscale_enabled).await,
        upscale_overhead_ratio: setting_f64(
            db,
            "agent.upscale.target_overhead_ratio",
            d.upscale_overhead_ratio,
        )
        .await,
        verification_directive_enabled: setting_bool(
            db,
            "agent.verification_directive_enabled",
            d.verification_directive_enabled,
        )
        .await,
        // ── tool_choice forcing (ADR 0018 leva 2, mig 0300) ───────────────────
        // Il porting Rust aveva PERSO questi tre campi: restavano ai Default del
        // nodo (enabled=false, style=None) -> il force-action era INERTE per ogni
        // provider (force_now sempre false). Ora il DB e' di nuovo l'unica fonte
        // (regola G): i flag dai settings (default-DB-down nel `Default`), lo
        // stile dal catalog via punto unico `capability::resolve_tool_choice_style`.
        tool_choice_forcing_enabled: setting_bool(
            db,
            "agent.tool_choice_forcing_enabled",
            d.tool_choice_forcing_enabled,
        )
        .await,
        tool_choice_forcing_max_iteration: setting_i64(
            db,
            "agent.tool_choice_forcing_max_iteration",
            d.tool_choice_forcing_max_iteration,
        )
        .await,
        tool_choice_style: crate::capability::resolve_tool_choice_style(db, provider, model).await,
        // ── context management (mig 0199/0429): il bug + l'inerzia ────────────
        // context_window (passato come parametro): senza questo era 0 -> token_brake
        // / forced_rag / smart-upscale tutti no-op (gate `if context_window > 0`).
        context_window,
        // Fasi di compressione DB-driven: i valori AGGRESSIVI (mig 0429,
        // compress_start_iter=3, boundaries [3,7,15,30], keep_recent [5,3,2,1],
        // max_chars [1200,600,300,100]) erano IGNORATI perche' load_executor_config
        // chiudeva con `..Default::default()` (safe-default permissivi). Ora dal DB.
        ctx_mgmt: CtxMgmtConfig {
            compress_start_iter: setting_i64(
                db,
                "agent.context.compress_start_iter",
                d.ctx_mgmt.compress_start_iter,
            )
            .await,
            compress_phase_boundaries: setting_i64_csv(
                db,
                "agent.context.compress_phase_boundaries",
                d.ctx_mgmt.compress_phase_boundaries.clone(),
            )
            .await,
            compress_phase_keep_recent: setting_i64_csv(
                db,
                "agent.context.compress_phase_keep_recent",
                d.ctx_mgmt.compress_phase_keep_recent.clone(),
            )
            .await,
            compress_phase_max_chars: setting_i64_csv(
                db,
                "agent.context.compress_phase_max_chars",
                d.ctx_mgmt.compress_phase_max_chars.clone(),
            )
            .await,
        },
        // Freno token: la soglia hard (0.55 in DB vs 0.70 default) e i parametri
        // aggressivi, ora dal DB.
        token_brake: TokenBrakeConfig {
            max_context_ratio: setting_f64(
                db,
                "agent.context.max_context_ratio",
                d.token_brake.max_context_ratio,
            )
            .await,
            aggressive_keep_recent: setting_i64(
                db,
                "agent.context.aggressive_keep_recent",
                d.token_brake.aggressive_keep_recent as i64,
            )
            .await
            .max(0) as usize,
            aggressive_max_chars: setting_i64(
                db,
                "agent.context.aggressive_max_chars",
                d.token_brake.aggressive_max_chars as i64,
            )
            .await
            .max(0) as usize,
        },
        // Forced-RAG reminder: ratio + testo dal DB (erano vuoti/0.0 -> reminder mai
        // iniettato anche con offload attivo).
        forced_rag_ratio: setting_f64(
            db,
            "agent.context.forced_rag_threshold_ratio",
            d.forced_rag_ratio,
        )
        .await,
        forced_rag_reminder_text: setting_string(
            db,
            "agent.context.forced_rag_reminder_text",
            &d.forced_rag_reminder_text,
        )
        .await,
        turn_focus_enabled: setting_bool(
            db,
            "agent.context.turn_focus_enabled",
            d.turn_focus_enabled,
        )
        .await,
        discovery_max_injected: setting_i64(
            db,
            "agent.tools.discovery_max_injected",
            d.discovery_max_injected as i64,
        )
        .await
        .max(0) as usize,
        // ── rolling-summary (intervento 3): RIASSUME i vecchi via LLM economico ─
        // Flag + keep_recent dal DB (regola G). Il MODELLO economico vive nell'impl
        // della porta PgSummaryStore (agent.context.rolling_summary_model).
        rolling_summary_enabled: setting_bool(
            db,
            "agent.context.rolling_summary_enabled",
            d.rolling_summary_enabled,
        )
        .await,
        rolling_keep_recent: setting_i64(
            db,
            "agent.context.rolling_keep_recent_turns",
            d.rolling_keep_recent,
        )
        .await,
        // Governance costo/beneficio del rolling-summary (opt-in, mig 0523). OFF di
        // default -> il gate non si applica (comportamento storico bit-identico).
        governance_rolling_summary_adaptive: setting_bool(
            db,
            "agent.governance.rolling_summary_adaptive",
            d.governance_rolling_summary_adaptive,
        )
        .await,
        governance_rolling_summary_min_prefix: setting_i64(
            db,
            "agent.governance.rolling_summary_min_prefix",
            d.governance_rolling_summary_min_prefix,
        )
        .await,
        // ── continuity-trim SEMANTICO + offload retrievabile (EmbeddingStore) ──
        // Tutti OFF di default (regola G): con questi valori il comportamento e'
        // bit-identico a oggi. La porta embedding/offload e' iniettata dal wiring.
        continuity_trim_enabled: setting_bool(
            db,
            "agent.context.continuity_trim_enabled",
            d.continuity_trim_enabled,
        )
        .await,
        continuity_trim_min_score: setting_f64(
            db,
            "agent.context.continuity_trim_min_score",
            d.continuity_trim_min_score as f64,
        )
        .await as f32,
        continuity_trim_max_drop: setting_i64(
            db,
            "agent.context.continuity_trim_max_drop",
            d.continuity_trim_max_drop,
        )
        .await,
        compress_offload_enabled: setting_bool(
            db,
            "agent.context.compress_offload_enabled",
            d.compress_offload_enabled,
        )
        .await,
        rolling_summary_offload_enabled: setting_bool(
            db,
            "agent.context.rolling_summary_offload_enabled",
            d.rolling_summary_offload_enabled,
        )
        .await,
        // ADR 0018 fase 3: rilevamento report passi pendenti nei rami G1/report
        // dell'executor (stesse chiavi della RoutingConfig, regola L).
        pending_steps_detection_enabled: setting_bool(
            db,
            "agent.closure.pending_steps_detection_enabled",
            d.pending_steps_detection_enabled,
        )
        .await,
        pending_steps_min_items: setting_i64(
            db,
            "agent.closure.pending_steps_min_items",
            d.pending_steps_min_items,
        )
        .await,
        // ── hard cap post-brake (ADR 0016 fase D2, mig 0286) ──────────────────
        // Ratio dal DB (0.95 in produzione; il Default 0.0 = gate OFF vale SOLO
        // a DB irraggiungibile) + template del messaggio overflow risolto qui
        // (regola G: il testo redazionale vive SOLO in nexus_prompt_templates).
        hard_cap_ratio: setting_f64(db, "agent.context.hard_cap_ratio", d.hard_cap_ratio).await,
        overflow_message_template: {
            let key = setting_string(
                db,
                "agent.context.overflow_message_key",
                "system.context_overflow",
            )
            .await;
            resolve_prompt_template(db, &key).await.unwrap_or_default()
        },
        ..ExecutorConfig::default()
    }
}

/// Tokenizer selezionato per le stime contesto (ADR 0016 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenizerKind {
    /// tiktoken cl100k_base via `mcp-token` (BPE cacheata).
    Cl100k,
    /// Stima char-based storica (chars/3.5): fallback deterministico.
    Chars,
}

/// Legge `agent.context.tokenizer` (mig 0286). Solo `cl100k_base` attiva la BPE
/// reale; qualunque altro valore o chiave assente -> char-based (safe-DB-down).
async fn resolve_tokenizer_kind(db: &PgPool) -> TokenizerKind {
    match nexus_auth::get_setting(db, "agent.context.tokenizer")
        .await
        .as_deref()
    {
        Some("cl100k_base") => TokenizerKind::Cl100k,
        _ => TokenizerKind::Chars,
    }
}

/// Adapter [`nexus_agent_graph::runtime::ports::TokenCounter`] -> `mcp-token`
/// (tiktoken cl100k, BPE cacheata in-process). CPU-only, nessun I/O.
struct TiktokenCounter;

impl nexus_agent_graph::runtime::ports::TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> i64 {
        mcp_token::count_tokens(text) as i64
    }
}

/// Risolve un template attivo da `nexus_prompt_templates` per chiave (stesso SQL
/// di `summary_store::system_prompt`, qui parametrico: punto unico locale del
/// lookup template per le config del motore nativo). `None` se assente/vuoto.
async fn resolve_prompt_template(db: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates \
         WHERE key = $1 AND is_active = TRUE LIMIT 1",
    )
    .bind(key)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .filter(|t| !t.trim().is_empty())
}

/// Costruisce la [`TodoRunnerConfig`] DB-driven (regola G): legge dal DB il kind
/// del sub-agent e il numero massimo di retry col PUNTO UNICO
/// `nexus_auth::get_setting` (per `todo_isolation_kind`, campo String, il pattern
/// `get_setting().unwrap_or(default)` come `planner_prompt_key`). Gli ALTRI campi
/// (on_failure, dag_topological_enabled, summary_max_chars) restano al `Default`
/// (safe-default identico ai `_SAFE_DEFAULTS` del brain: valgono SOLO se la chiave
/// manca o il DB e' irraggiungibile, mai come magic fallback nella logica).
async fn load_todo_runner_config(db: &PgPool) -> TodoRunnerConfig {
    let d = TodoRunnerConfig::default();
    TodoRunnerConfig {
        todo_isolation_kind: nexus_auth::get_setting(db, "agent.continuous.todo_isolation_kind")
            .await
            .unwrap_or(d.todo_isolation_kind),
        max_retries: setting_i64(
            db,
            "agent.continuous.todo_isolation_max_retries",
            d.max_retries,
        )
        .await,
        dag_topological_enabled: setting_bool(
            db,
            "orchestrator.dag_topological_enabled",
            d.dag_topological_enabled,
        )
        .await,
        dag_parallel_min_ready: setting_i64(
            db,
            "orchestrator.dag_parallel_min_ready",
            d.dag_parallel_min_ready,
        )
        .await,
        ..TodoRunnerConfig::default()
    }
}

/// Ruolo del run nel motore nativo: distingue il run PRIMARIO (side-effect
/// reali, output SSE, checkpoint persistente) dal run SHADOW (read-only).
///
/// PUNTO UNICO (regola L) della differenza primario/shadow: `build_native_engine`
/// e `run_engine` leggono SOLO questo enum per decidere tools/emit/checkpointer
/// e il flag `shadow` del ctx; non c'e' alcun altro `if shadow` sparso. Lo shadow
/// porta con se' il `primary_run_id` da cui rileggere i tool_result in Replay.
#[derive(Clone, Copy)]
enum RunRole {
    /// Run primario: tool REALI, SSE verso il frontend, checkpoint Postgres.
    Primary,
    /// Run shadow read-only: tool in Replay dal primario (`primary_run_id`),
    /// EventSink no-op, checkpointer in-memory (niente scritture).
    Shadow { primary_run_id: Uuid },
}

impl RunRole {
    /// `true` se questo e' un run shadow (read-only).
    fn is_shadow(&self) -> bool {
        matches!(self, RunRole::Shadow { .. })
    }
}

/// Modalita' di ingresso del motore nativo (punto unico, regola L): distingue
/// l'avvio nuovo dal resume HITL. Estrae la decisione "init Some/None +
/// resume_delta" da `run_engine` in un solo enum, cosi' i tre call site
/// (run nuovo, resume HITL, shadow) la esprimono in modo esplicito.
enum RunMode {
    /// Avvio nuovo: `build_initial_state` dal prompt -> parte da `entry`.
    New,
    /// Resume HITL: nessun initial_state (riparte dal checkpoint), `resume_delta`
    /// sblocca l'interrupt (azzera `awaiting_confirmation` + inietta l'approvazione).
    Resume {
        resume_delta: nexus_graph::StateDelta,
    },
}

/// Costruisce le 14 impl concrete + gli 11 nodi (porte iniettate) + la
/// `RoutingConfig` e la `PlannerConfig` DB-driven, e assembla il
/// [`AgentGraphEngine`].
///
/// Baseline PRE-LAVORO degli step gate (delta-aware sui criteri): per gli step
/// gate senza baseline misura l'exit code sull'albero corrente (= stato
/// pre-lavoro di questo run). Il final_gate non bocciera' un criterio che
/// fallisce IDENTICO alla baseline (fallimento pre-esistente dell'ambiente, es.
/// `npx eslint` exit 2 per config assente — incidente run 695794af col build
/// verde). Un comando non misurabile resta senza baseline -> fail-closed come
/// prima. Ritorna true se ha misurato qualcosa (=> profilo da persistere).
async fn measure_gate_baselines(
    steps: &mut [crate::verify_profile::VerifyProfileStep],
    criteria_adapter: &FinalGateCriteriaRunnerAdapter,
) -> bool {
    let mut measured = false;
    for s in steps
        .iter_mut()
        .filter(|s| s.gate && s.baseline_exit_code.is_none())
    {
        if let Some(exit) = criteria_adapter
            .measure_command_exit(&s.command, s.working_dir.as_deref())
            .await
        {
            s.baseline_exit_code = Some(exit);
            measured = true;
            tracing::info!(
                step = %s.step,
                baseline_exit = exit,
                "verify_profile: baseline pre-lavoro misurata per lo step gate"
            );
        }
    }
    measured
}

/// Prova di efficacia degli step gate (regola H): la baseline misura lo STATO
/// dell'albero, questa misura il POTERE DISCRIMINANTE del comando — introduce
/// una rottura NOTA in cio' che il comando dichiara di coprire e guarda se
/// arrossisce. Senza, un gate che passa SEMPRE (es. `node --check a.js b.js`,
/// che ignora gli argomenti oltre il primo) e' indistinguibile da un gate che
/// passa perche' il codice e' sano — ed e' cosi' che il backend di Beaty-Book
/// non e' mai stato verificato da nessun run. Ritorna true se ha misurato
/// qualcosa (=> profilo da persistere).
async fn probe_gate_steps(
    root: &str,
    steps: &mut [crate::verify_profile::VerifyProfileStep],
    criteria_adapter: &FinalGateCriteriaRunnerAdapter,
) -> bool {
    let root_path = std::path::PathBuf::from(root);
    let mut measured = false;
    for s in steps
        .iter_mut()
        .filter(|s| s.gate && s.probe.is_none() && s.baseline_exit_code.is_some())
    {
        let (cmd, wd, baseline) = (
            s.command.clone(),
            s.working_dir.clone(),
            s.baseline_exit_code,
        );
        let outcome = crate::verify_probe::probe_step(&root_path, &cmd, baseline, || {
            criteria_adapter.measure_command_exit(&cmd, wd.as_deref())
        })
        .await;
        if outcome != crate::verify_probe::ProbeOutcome::NotProbed {
            s.probe = Some(outcome);
            measured = true;
        }
        tracing::info!(
            step = %s.step,
            probe = ?outcome,
            "verify_profile: prova di efficacia dello step gate"
        );
    }
    measured
}

/// Misura gli step gate del profilo di verifica PRIMA che l'executor tocchi i
/// file, e persiste il profilo solo se qualcosa e' cambiato. Da chiamare solo
/// nel primario (in shadow niente inferenza: zero side-effect e zero costo).
/// Entrambe le misure passano dal PUNTO UNICO del runner criteri (regola L/M:
/// exit code strutturato), e questo e' l'unico posto che esegue i comandi gate
/// ad albero pulito.
///
/// # Mutua esclusione per ROOT (difetto D2, incidente consiglio 2026-07-15)
///
/// Il probe PIANTA un file rotto nell'albero e ri-esegue il comando
/// ([`crate::verify_probe`]). Con 6 figure concorrenti sulla STESSA root le
/// misure si corrompevano a vicenda: A pianta il file, B misura la baseline e
/// vede exit=1; A rimuove, B misura il probe e lo dichiara `Blind`.
/// Serializzare e' la CORRETTEZZA della misura, non una preferenza: due misure
/// sovrapposte sullo stesso albero non misurano niente.
///
/// Il guard e' per ROOT (non per progetto): e' l'albero la risorsa condivisa. E
/// sta DENTRO questa funzione, non al call site, perche' e' il lavoro
/// sull'albero a dover essere serializzato: chi la ri-estrae domani non deve
/// poter dimenticare il guard restandone fuori. E' esattamente cosi' che si era
/// perso (il refactor `1207a229` l'ha estratta da una versione che non l'aveva
/// ancora, e main e' rimasto scoperto finche' non e' rientrato il ramo).
///
/// # Ri-lettura dopo il guard (doppio controllo)
///
/// Chi ha atteso in coda ha in mano la copia letta PRIMA di entrare, con
/// `baseline_exit_code`/`probe` ancora a `None`: i filtri `is_none()` delle due
/// misure rifarebbero da capo il typecheck e il probe che chi era davanti ha
/// appena eseguito e persistito. Senza la ri-lettura il guard non CONDIVIDE il
/// lavoro, lo mette in FILA (6 figure = 6 baseline + 6 probe in sequenza sullo
/// stesso albero): e' l'altra meta' del difetto D2, lo spreco che occupava la
/// finestra prima che le figure raggiungessero il loro modello. Il razionale di
/// `PROFILE_LOCKS` avverte esattamente di questo: un lock che serializza N
/// misure invece di condividerne una sarebbe solo un altro difetto.
///
/// Due sottigliezze del contratto, entrambe mutation-checked:
/// - `steps` vuoto e' anche il modo in cui `ensure_profile` segnala il
///   kill-switch `agent.verify_infer.enabled` OFF -> si esce PRIMA del guard,
///   altrimenti la ri-lettura ripescherebbe dal DB il profilo appena escluso;
/// - una ri-lettura VUOTA non e' un profilo svuotato: `profile_steps` inghiotte
///   l'errore DB e ritorna il default, quindi il `Vec` non distingue "nessuna
///   riga" da "il DB non ha risposto" (regola M). Su un segnale che non
///   discrimina si tiene la copia in mano.
async fn measure_gate_steps(
    meta_db: &PgPool,
    project_id: Uuid,
    root: &str,
    steps: &mut Vec<crate::verify_profile::VerifyProfileStep>,
    criteria_adapter: &FinalGateCriteriaRunnerAdapter,
) {
    // Kill-switch OFF o profilo assente: niente da misurare, niente guard.
    if steps.is_empty() {
        return;
    }
    let _tree_guard = crate::verify_profile::project_tree_lock(root)
        .lock_owned()
        .await;
    // Doppio controllo: chi era davanti puo' aver gia' misurato tutto.
    let persisted = crate::verify_profile::profile_steps(meta_db, project_id).await;
    if !persisted.is_empty() {
        *steps = persisted;
    }
    // `|` e non `||`: la seconda misura va eseguita comunque, non e' un
    // cortocircuito.
    let measured = measure_gate_baselines(steps, criteria_adapter).await
        | probe_gate_steps(root, steps, criteria_adapter).await;
    if measured {
        crate::verify_profile::persist_steps(meta_db, project_id, steps).await;
    }
}

/// Il `role` decide le sole tre porte che cambiano fra primario e shadow (regola
/// L: un solo punto): `tools` (Real vs Replay), `emit` (SSE vs no-op),
/// `checkpointer` (Postgres vs in-memory). Tutto il resto (nodi, config DB-driven,
/// purpose model) e' IDENTICO -> lo shadow attraversa la STESSA topologia del
/// primario, presupposto del confronto di parita'.
///
/// Ritorna anche la `RoutingConfig` risolta (serve a popolare il ctx, il cui
/// `recursion_limit` viene letto dal motore) + le porte gateway/tools per il ctx.
async fn build_native_engine(
    deps: &NativeDeps,
    input: &NativeRunInput,
    role: RunRole,
) -> anyhow::Result<(
    AgentGraphEngine,
    RoutingConfig,
    Arc<dyn LlmGateway>,
    Arc<dyn ToolExecutor>,
    Arc<dyn EventSink>,
    // `isolation_available`: worktree git effimeri disponibili per questo run
    // (flag ON + root git isolabile). Alimenta il gate di orchestrazione del
    // planner (Fase C3 Part B); `false` di default -> ParallelIsolated degrada a
    // Sequential (invariato).
    bool,
)> {
    let db = deps.db.clone();

    // ── Config DB-driven (regola G) ──────────────────────────────────────────
    let routing_cfg = load_routing_config(&db).await;

    let context_window = resolve_context_window(&db, &input.provider, &input.model).await;

    // Provider/model dei purpose interni del planner + reflection (tier-aware).
    let (planner_provider, planner_model) = resolve_purpose(&db, "planner").await;
    let (fallback_provider, fallback_model) = resolve_purpose(&db, "planner_fallback").await;
    let (reflection_provider, reflection_model) = resolve_purpose(&db, "reflection").await;

    // ── Pool del dominio run (separazione DB per-progetto, punto unico regola L) ─
    // Tutte le porte che PERSISTONO dati per-progetto del run (agent_runs,
    // agent_steps, nexus_agent_*, nexus_graph_checkpoints, nexus_agent_traces,
    // todos/plans) girano su QUESTO pool: il DB del progetto (`<slug>_nexus`) a
    // flag separazione ON, il meta-DB a flag OFF / sessione non mappata
    // (comportamento storico). Risolto UNA volta dal session_id via la directory
    // di routing. Le porte che leggono SOLO config/catalogo GLOBALI (settings,
    // ai_price_catalog, nexus_prompt_templates, routing matrix) restano su `db`.
    let run_db =
        crate::project_db_routes::project_data_pool_by_session_from(&db, input.session_id).await;

    // Fase C3 Part B: disponibilita' dell'isolamento fisico (worktree git) per
    // questo run, calcolata UNA volta qui e passata al ctx (i nodi puri non fanno
    // I/O). Corto-circuito su flag OFF (default) -> nessun probe git: zero costo
    // aggiunto al percorso normale. Vedi `compute_run_isolation_available`.
    let isolation_available =
        compute_run_isolation_available(&db, &run_db, input.session_id, role).await;

    // ── Porte I/O concrete (14 impl FASE 2) ──────────────────────────────────
    // Gateway LLM: dipende dal ruolo (regola L, punto unico, stesso switch del
    // `tools` sotto).
    //  - Primary: GatewayLlmAdapter REAL (provider/model gia' risolti, il client
    //    non re-instrada).
    //  - Shadow: ReplayLlmGateway (executor rigioca le decisioni del primario da
    //    agent_steps, ausiliari neutralizzati): nessuna chiamata LLM reale ->
    //    num_tool_calls converge col primario, costo zero, zero RNG-divergenza.
    let llm: Arc<dyn LlmGateway> = match role {
        RunRole::Primary => {
            // Identita' del run per il ledger di billing: ricavata dalla sessione
            // (chat_sessions.project_id/user_id). Senza, il gateway scarta la
            // registrazione usage (record_usage_to_ledger esce su tenant vuoto) e
            // il costo risulta sempre 0. Lettura puntuale (una volta per run).
            let (proj_id, usr_id) = sqlx::query_as::<_, (Option<Uuid>, Option<Uuid>)>(
                "SELECT project_id, user_id FROM chat_sessions WHERE id = $1",
            )
            .bind(input.session_id)
            .fetch_optional(&run_db)
            .await
            .ok()
            .flatten()
            .map(|(p, u)| {
                (
                    p.map(|x| x.to_string()).unwrap_or_default(),
                    u.map(|x| x.to_string()).unwrap_or_default(),
                )
            })
            .unwrap_or_default();
            Arc::new(GatewayLlmAdapter::new(
                deps.gateway.clone(),
                deps.db.clone(),
                proj_id,
                usr_id,
            ))
        }
        RunRole::Shadow { primary_run_id } => {
            Arc::new(ReplayLlmGateway::new(run_db.clone(), primary_run_id))
        }
    };

    // ToolExecutor: dipende dal ruolo (regola L, punto unico).
    //  - Primary: ToolRunner in-process REALE (mcp-core E' il ToolRunner, no gRPC
    //    su se' stesso). `primary_run_id=None`.
    //  - Shadow: Replay-only by-construction (nessun `ToolRunnerDeps`): rilegge i
    //    tool_result del primario da `agent_steps`, ZERO side-effect.
    let tools: Arc<dyn ToolExecutor> = match role {
        RunRole::Primary => Arc::new(ToolRunnerExecutorAdapter::new(
            deps.tool_runner_deps.clone(),
            input.session_id,
            None,
            // Override root del sub-run isolato (FASE 2). `None` per default (run
            // principale/sub-run non isolato) -> ctx sulla root del progetto,
            // comportamento invariato. In PR3 e' sempre `None` (accensione in PR4).
            input.working_root.clone(),
            // Narrazione verso QUESTO run: i tool a lunga durata (dispatch_
            // subagents) emettono meta-step di avvio/progresso/chiusura sul
            // canale SSE del run invocante mentre lavorano. Vale anche per i
            // sub-run (il loro canale e' il ponte verso il padre): la
            // narrazione risale la catena in modo ricorsivo.
            Some(crate::agent_tools::context::ParentNarration {
                run_id: input.run_id,
                session_id: input.session_id,
                step_tx: input.step_tx.clone(),
            }),
        )),
        RunRole::Shadow { primary_run_id } => Arc::new(
            ToolRunnerExecutorAdapter::from_db_for_replay(run_db.clone(), Some(primary_run_id)),
        ),
    };

    // Canale eventi: dipende dal ruolo.
    //  - Primary: STESSO broadcast SSE del run (parita' 1:1 con run_via_brain).
    //  - Shadow: NullEventSink (no-op): il run shadow non emette NULLA verso il
    //    frontend (l'output all'utente resta quello del primario).
    let emit: Arc<dyn EventSink> = if role.is_shadow() {
        Arc::new(NullEventSink)
    } else {
        // Primario: oltre a emettere gli eventi LIVE, l'adapter ricostruisce e
        // PERSISTE le tracce gateway (`AITraceEvent`) su `nexus_agent_traces` cosi'
        // il trace panel sopravvive al refresh (FIX persistenza tracing nativo:
        // prima il ramo nativo non scriveva mai questa tabella). Best-effort,
        // punto unico `trace_store::persist_trace` (regola L).
        Arc::new(SseEventSinkAdapter::with_persistence(
            input.step_tx.clone(),
            input.run_id,
            input.session_id,
            run_db.clone(),
        ))
    };

    // Store DB + porte ausiliarie. Le store del dominio run (agent_runs,
    // agent_steps, nexus_agent_meta_steps, nexus_agent_todos/plans,
    // nexus_agent_verifier_runs) persistono su `run_db` (DB del progetto a flag
    // ON, meta a flag OFF). `todos` riceve ANCHE `db` (meta) per le letture di
    // `settings` (config globale, non per-progetto: regola G). `offload`/
    // `escalation`/`next_actions` leggono solo config/template/catalogo GLOBALI
    // (settings, nexus_prompt_templates, ai_price_catalog) -> restano su `db`.
    let run_control: Arc<dyn RunControlStore> = Arc::new(PgRunControlStore::new(run_db.clone()));
    let steps: Arc<dyn AgentStepStore> = Arc::new(PgAgentStepStore::new(run_db.clone()));
    let meta_steps: Arc<dyn MetaStepStore> =
        Arc::new(PgMetaStepStore::new(run_db.clone(), input.run_id));
    let todos: Arc<dyn TodoStore> = Arc::new(PgTodoStore::new(run_db.clone(), db.clone()));
    let verifier_runs: Arc<dyn VerifierRunStore> =
        Arc::new(PgVerifierRunStore::new(run_db.clone()));
    let offload: Arc<dyn ContextOffload> = Arc::new(RagContextOffloadAdapter::new(db.clone()));
    let escalation: Arc<dyn EscalationPort> = Arc::new(PgEscalationPort::new(db.clone()));
    let next_actions: Arc<dyn NextActionsDeriver> =
        Arc::new(NextActionsDeriverAdapter::new(db.clone()));
    // Porta billing: dipende dal ruolo (FIX shadow LLM-Replay).
    //  - Primary: cooldown LIVE (fonte unica `provider_cooldown`), il fail-fast
    //    esplorazione riflette lo stato reale dei provider.
    //  - Shadow: NO-OP (lista vuota). Lo shadow rigioca la DECISIONE del primario,
    //    non rivaluta il billing corrente: leggere lo snapshot LIVE introdurrebbe
    //    non-determinismo e un fail-fast SPURIO (-> canonical "loop") da stato
    //    esterno evoluto dopo il primario. Allineata agli altri ausiliari shadow
    //    gia' neutralizzati (NullEventSink, MemoryCheckpointer).
    let billing: Arc<dyn BillingCooldownPort> = if role.is_shadow() {
        Arc::new(NullBillingCooldownPort::new())
    } else {
        Arc::new(CooldownBillingPort::new())
    };
    let upscale: Arc<dyn ModelUpscalePort> = Arc::new(CatalogModelUpscalePort::new(db.clone()));
    // Rolling-summary (intervento 3): riassume i vecchi via LLM economico (modello
    // da `agent.context.rolling_summary_model`, regola G). Gata Real (no-op shadow).
    let summary_store: Arc<dyn SummaryStore> = Arc::new(PgSummaryStore::new(db.clone()));

    // Motore criteri del final_gate / verifier: delega al tool_executor (punto
    // unico, regola L) per i criteri run_command/list_files + DB per outputs_exist.
    // Il tipo CONCRETO resta visibile per la misura baseline pre-lavoro
    // (measure_command_exit non fa parte del trait CriteriaRunner).
    let criteria_adapter = Arc::new(FinalGateCriteriaRunnerAdapter::new(
        tools.clone(),
        run_db.clone(),
    ));
    let criteria: Arc<dyn CriteriaRunner> = criteria_adapter.clone();

    // Porta del panel di review (ReviewGate). Nel ruolo shadow il nodo passa
    // in Replay e non chiama la porta; il concreto resta comunque innocuo.
    let review_panel: Arc<dyn nexus_agent_graph::runtime::ports::ReviewPanelPort> =
        Arc::new(crate::agent_graph_adapter::review_panel::ReviewPanelAdapter::new(
            db.clone(),
            run_db.clone(),
            deps.tool_runner_deps.clone(),
            input.session_id,
            input.sizing_complexity,
            input.sizing_scope_system_wide,
        ));

    // ── Config dei nodi (DB-driven, regola G piena) ──────────────────────────
    // DEBITO 2 chiuso (TODO Fase 5): le config dei nodi che il brain Python legge
    // da `orchestrator_config.get()` (`orchestrator_config.py`) vengono ora LETTE
    // dal DB col PUNTO UNICO `nexus_auth::get_setting` (regola L), 1:1 con le chiavi
    // settings del brain. Il `Default` di ciascuna config resta SOLO come
    // safe-default se la chiave manca (identico ai `_SAFE_DEFAULTS` del brain): mai
    // come magic fallback (regola G). Copre SIA il primario nativo (run_native) SIA
    // lo shadow (run_shadow): entrambi passano di qui (punto unico).
    let planner_cfg = load_planner_config(&db).await;
    let mut final_gate_cfg = load_final_gate_config(&db).await;
    let review_gate_cfg = load_review_gate_config(&db).await;
    let verifier_cfg = load_verifier_config(&db).await;

    // ── Catena di verifica per-AMBIENTE (ADR 0036) ───────────────────────────
    // Il profilo del progetto e' INFERITO da un LLM che osserva l'ambiente
    // reale (verify_profile::ensure_profile: sceglie lui i file da leggere e
    // definisce step liberi con flag gate). Qui, risolto a monte del grafo
    // (regola G), si innestano nel final_gate gli step gate=true. In SHADOW
    // niente inferenza (zero side-effect/costo): sola lettura del persistito.
    if final_gate_cfg.enabled {
        if let Some((pid, root)) =
            resolve_session_project_root(&run_db, &db, input.session_id).await
        {
            let mut steps = if role.is_shadow() {
                crate::verify_profile::profile_steps(&db, pid).await
            } else {
                crate::verify_profile::ensure_profile(
                    &db,
                    &deps.tool_runner_deps.neural,
                    pid,
                    std::path::Path::new(&root),
                )
                .await
            };

            if !role.is_shadow() {
                measure_gate_steps(&db, pid, &root, &mut steps, &criteria_adapter).await;
            }
            final_gate_cfg.verify_steps = steps
                .into_iter()
                // Uno step PROVATO CIECO non e' una verifica, qualunque cosa
                // dica il suo nome: escluderlo e' cio' che rende l'esito onesto.
                // Se dopo l'esclusione non resta nessun comando, il gate lo
                // dichiara (`verify_profile_missing` qui sotto) e il run chiude
                // CompletedUnverified invece di spacciare per verificato un
                // lavoro che nessuno ha controllato.
                .filter(|s| s.gate && s.probe != Some(crate::verify_probe::ProbeOutcome::Blind))
                .map(|s| nexus_agent_graph::nodes::final_gate::VerifyStepCmd {
                    step: s.step,
                    command: s.command,
                    working_dir: s.working_dir,
                    baseline_exit_code: s.baseline_exit_code,
                })
                .collect();
            // Il segnale onesto e' "NESSUN comando di verifica verra' eseguito",
            // quindi si misura sugli step GATE effettivi, dopo il filtro.
            // Prima era `steps.is_empty()`, cioe' la PRESENZA del profilo, letta
            // come ESITO ("verifica eseguita"): un profilo con tutti gli step
            // `gate:false` -> `verify_profile_missing=false` con `verify_steps`
            // VUOTO -> il gate dichiarava l'esito VERIFICATO senza aver eseguito
            // un solo comando. Stessa classe di `final_gate_verdict`: un proxy di
            // presenza/conteggio non e' un segnale di esito (regola M).
            final_gate_cfg.verify_profile_missing = final_gate_cfg.verify_steps.is_empty();
        } else {
            // Sessione senza progetto/root (es. run di servizio): niente
            // catena, dichiarazione onesta come per il profilo mancante.
            final_gate_cfg.verify_profile_missing = true;
        }
    }

    let mut exec_cfg =
        load_executor_config(&db, &input.provider, &input.model, context_window).await;
    // Budget del run CORRENTE. `load_executor_config` resta il punto unico DB-driven
    // (regola G) e legge il setting GLOBALE `agent.run_time_budget_s`; qui vince il
    // tetto che il chiamante conosce meglio: per un SUB-RUN e' il `timeout_s` della
    // figura, l'unico orologio che la governa davvero (il `tokio::time::timeout`
    // esterno). Senza questo override il gate a tempo dell'executor resta codice
    // morto per le figure e il sollecito di chiusura non scatterebbe mai.
    exec_cfg.run_time_budget_s =
        effective_run_time_budget_s(exec_cfg.run_time_budget_s, input.run_time_budget_s);

    let tool_dispatch_cfg = ToolDispatchConfig {
        context_window,
        fs_mutator_tools: routing_cfg.fs_mutator_tools.clone(),
        // Barriera advisory (mig 0606): attesa massima della prima scrittura.
        // CLAMP alla deadline residua del run (fase 3): una barriera che attende
        // oltre la deadline produrrebbe un `time_budget` mascherato da gate.
        advisory_gate_timeout_s: advisory_gate_timeout(&db).await,
        ..ToolDispatchConfig::default()
    };

    let reflection_cfg = ReflectionConfig {
        enabled: setting_bool(
            &db,
            "reflection_enabled",
            ReflectionConfig::default().enabled,
        )
        .await,
        provider: reflection_provider,
        model: reflection_model,
        ..ReflectionConfig::default()
    };

    // ── Meta-reasoner LLM CONDIVISO (regola L: UNA istanza, iniettata sia nel
    // gate orchestrazione del planner sia nel nodo recovery StallRecovery). Impl
    // concreta `PgMetaReasonerPort` (paradigma ADR 0036 di `verify_profile`):
    // legge config/purpose/template dal meta-DB e consulta l'LLM via
    // `NeuralCoreClient`. OPT-IN: con `agent.stall_recovery.enabled` /
    // `agent.orchestration.enabled` = false (default, regola G) i due metodi
    // ritornano `Ok(None)` -> planner ricade su `is_eligible`, StallRecovery sulla
    // gerarchia fissa -> comportamento BIT-IDENTICO a oggi. In `Replay` la porta
    // gatta su `mode` -> `Ok(None)` senza consultare l'LLM.
    let reasoner: Arc<dyn MetaReasonerPort> = Arc::new(PgMetaReasonerPort::new(
        db.clone(),
        deps.tool_runner_deps.neural.clone(),
    ));

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
            meta_steps.clone(),
            reasoner.clone(),
        )),
        todo_runner: Arc::new(TodoRunnerNode::new(
            load_todo_runner_config(&db).await,
            todos.clone(),
            tools.clone(),
        )),
        executor: {
            let mut executor = ExecutorNode::new(
                exec_cfg,
                run_control.clone(),
                meta_steps.clone(),
                steps.clone(),
                escalation.clone(),
                next_actions.clone(),
                billing.clone(),
                upscale.clone(),
                summary_store.clone(),
            );
            // ADR 0016 D1: tokenizer REALE per le stime contesto dell'executor
            // (upscale/brake/hard-cap/forced-RAG), selezionato dal DB
            // (`agent.context.tokenizer`, mig 0286, default seed cl100k_base).
            // Qualunque altro valore o chiave assente -> stima char-based
            // storica (fallback deterministico, regola G: niente magic default).
            if resolve_tokenizer_kind(&db).await == TokenizerKind::Cl100k {
                executor = executor.with_token_counter(Arc::new(TiktokenCounter));
            }
            // Budget CROSS-RUN del meta-reasoner (per SESSIONE): contatore
            // append+count su nexus_agent_meta_steps (kind='stall_budget') sul pool
            // del dominio run (separazione DB per-progetto). Rende il cap
            // `agent.stall_recovery.max_moves_per_session` effettivo per-sessione
            // (chiude il loop email cross-run). Con flag OFF il gate non e' MAI
            // raggiunto -> la porta resta inerte (nessuna lettura/scrittura).
            executor = executor.with_stall_budget(Arc::new(PgStallBudgetStore::new(
                run_db.clone(),
                input.run_id,
            )));
            // Continuity-trim SEMANTICO (EmbeddingStore, embedder ONNX in-process) +
            // offload RAG del contesto (tool_result compressi + originali del
            // rolling-summary). Le porte sono SEMPRE iniettate; i FLAG DB
            // (`agent.context.continuity_trim_enabled`, `compress_offload_enabled`,
            // `rolling_summary_offload_enabled`, tutti OFF di default) governano se
            // scattano (regola G). `offload` riusa lo stesso adapter RAG del
            // tool_dispatch (punto unico, regola L).
            executor = executor
                .with_embedding_store(Arc::new(PgEmbeddingStore::new()))
                .with_context_offload(offload.clone());
            Arc::new(executor)
        },
        tool_dispatch: Arc::new(ToolDispatchNode::new(
            tool_dispatch_cfg,
            tools.clone(),
            steps.clone(),
            run_control.clone(),
            todos.clone(),
            offload.clone(),
            meta_steps.clone(),
        )),
        // Meta-reasoner di recovery-da-stallo. Riusa la STESSA istanza
        // `PgMetaReasonerPort` iniettata nel gate orchestrazione del planner
        // (regola L: nessuna duplicazione della porta). Il nodo consuma SOLO
        // `recover`; il planner consuma SOLO `orchestrate`. OPT-IN via
        // `agent.stall_recovery.enabled` (default false): con OFF la porta ritorna
        // `Ok(None)` -> il nodo ricade sulla gerarchia fissa
        // `progress_controller::decide` -> comportamento BIT-IDENTICO a oggi.
        stall_recovery: Arc::new(StallRecoveryNode::new(reasoner.clone())),
        // Scale-controller (gemello di stall_recovery). Riusa la STESSA istanza
        // `PgMetaReasonerPort` (regola L: UNA sola porta, tre scope disgiunti; il
        // nodo consuma SOLO `assess_scale`). OPT-IN via `agent.scale.enabled`
        // (default false). Con PR-B3 il detector-emissione (`maybe_scale_reason_delta`
        // nell'executor, pre-LLM) EMETTE `ScaleReason` a flag ON, quindi il nodo E'
        // raggiunto quando `enabled=true`. Il bit-identico NON deriva piu' dall'assenza
        // del detector ma dal GUARD `agent.scale.enabled=OFF` (default): a flag OFF il
        // detector ritorna subito None (zero overhead) e la porta ritorna `Ok(None)`,
        // il nodo non e' mai raggiunto -> comportamento BIT-IDENTICO a oggi. Le soglie
        // DB-driven del gate anti-oscillazione arrivano al nodo via extra (trasportate
        // dal detector che possiede `ExecutorConfig.scale`, FIX-B), non via costruttore.
        scale_control: Arc::new(ScaleControlNode::new(reasoner.clone())),
        supervisor: Arc::new(SupervisorNode::new(
            reasoner.clone(),
            load_supervisor_config(&db).await,
        )),
        verifier: Arc::new(VerifierNode::new(
            verifier_cfg,
            final_gate_cfg.clone(),
            routing_cfg.clone(),
            todos.clone(),
            criteria.clone(),
            verifier_runs,
            meta_steps.clone(),
        )),
        final_gate: Arc::new(FinalGateNode::new(
            final_gate_cfg,
            routing_cfg.clone(),
            criteria,
            meta_steps.clone(),
        )),
        review_gate: Arc::new(ReviewGateNode::new(
            review_gate_cfg,
            review_panel,
            meta_steps.clone(),
        )),
        reflection: Arc::new(ReflectionNode::new(reflection_cfg)),
        learner: Arc::new(LearnerNode::new(LearnerConfig::default())),
    };

    // Checkpointer: dipende dal ruolo.
    //  - Primary: Postgres (persistenza per-superstep su nexus_graph_checkpoints,
    //    serve al recovery di un run interrotto).
    //  - Shadow: IN-MEMORY. Lo shadow gira UNA volta fino a End e NON deve scrivere
    //    su nexus_graph_checkpoints (i checkpoint Python e Rust hanno topologie
    //    diverse: persisterli inquinerebbe la tabella di recovery del primario).
    let checkpointer: Arc<dyn nexus_graph::checkpoint::Checkpointer<AgentState>> =
        if role.is_shadow() {
            Arc::new(nexus_graph::MemoryCheckpointer::<AgentState>::new())
        } else {
            Arc::new(nexus_agent_graph::PgCheckpointer::new(run_db.clone()))
        };

    let supervisor_cfg = load_supervisor_config(&db).await;
    let engine = build_agent_graph(
        nodes,
        routing_cfg.clone(),
        planner_cfg,
        supervisor_cfg,
        checkpointer,
    );
    Ok((engine, routing_cfg, llm, tools, emit, isolation_available))
}

/// Risolve `(project_id, repository_root_path)` di una sessione. PUNTO UNICO
/// (regola L) della catena `chat_sessions.project_id` (sul DB del progetto,
/// `run_db`) -> `projects.repository_root_path` (sul meta-DB): riusato dal setup
/// del final_gate e dal compute dell'isolamento. `None` se la sessione non e'
/// mappata a un progetto o la root e' vuota/assente.
async fn resolve_session_project_root(
    run_db: &PgPool,
    meta_db: &PgPool,
    session_id: Uuid,
) -> Option<(Uuid, String)> {
    let project_id: Option<Uuid> =
        sqlx::query_scalar("SELECT project_id FROM chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(run_db)
            .await
            .ok()
            .flatten();
    let pid = project_id?;
    let root: Option<String> =
        sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id = $1")
            .bind(pid)
            .fetch_optional(meta_db)
            .await
            .ok()
            .flatten();
    root.filter(|r| !r.trim().is_empty()).map(|r| (pid, r))
}

/// Isolamento fisico DISPONIBILE per questo run (Fase C3 Part B). Ordine dei
/// corto-circuiti scelto per NON aggiungere I/O al percorso normale:
///   1. run SHADOW -> `false` (nessuna orchestrazione delegata in replay);
///   2. flag `orchestrator.subagent_isolation_enabled` OFF (default) -> `false`
///      SENZA risolvere la root ne' fare il probe git (costo zero);
///   3. root del progetto non risolvibile -> `false`;
///   4. `probe_isolatable` (FAIL-CLOSED) sulla root: `git worktree` utilizzabile.
/// Riusa gli STESSI punti unici del batch tool (`isolation_flag_enabled`,
/// `probe_isolatable`), cosi' il gate del planner e l'esecuzione reale
/// dell'isolamento vedono lo stesso verdetto.
///
/// ATTENZIONE ai pool: il flag `orchestrator.subagent_isolation_enabled` e' un
/// setting GLOBALE di piattaforma -> tabella `settings`, che vive SOLO nel
/// META-DB (`meta_db`), non nei DB-progetto (`run_db`, `<slug>_nexus`: con
/// separazione DB ON non ha affatto la tabella `settings`). Va quindi letto da
/// `meta_db`, coerentemente col batch tool che legge da `ctx.core.db` (= meta).
/// Solo la catena `chat_sessions -> projects` di `resolve_session_project_root`
/// usa `run_db` per `chat_sessions` (dominio run) e `meta_db` per `projects`.
async fn compute_run_isolation_available(
    meta_db: &PgPool,
    run_db: &PgPool,
    session_id: Uuid,
    role: RunRole,
) -> bool {
    if role.is_shadow() {
        return false;
    }
    // Flag GLOBALE -> meta_db (NON run_db: li' `settings` non esiste a
    // separazione DB ON e la query fallirebbe mascherandosi come flag OFF).
    if !crate::agent_tools::subagent_native::isolation_flag_enabled(meta_db).await {
        return false;
    }
    let Some((_pid, root)) = resolve_session_project_root(run_db, meta_db, session_id).await else {
        return false;
    };
    nexus_tool_kit::worktree::probe_isolatable(std::path::Path::new(&root)).await
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
            // Ricostruzione da JSON minimale (role/content): nessun reasoning.
            reasoning: None,
            thinking_signature: None,
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
///
/// `role` distingue primario da shadow. Sia il ramo Shadow sia il PRIMARIO RUST
/// (`RunRole::Primary`, usato SOLO da `run_via_native`/`Engine::Rust`, non
/// instradato globalmente) valorizzano `user_intent`/`action_oriented`/
/// `report_only` derivandoli dai dati del classifier del turno (`requires_tools`/
/// `agentic_score`/`authorizes_changes`) col PUNTO UNICO `intent_classifier::
/// derive_*` (regola L): cosi' il primario Rust converge col primario Python (no
/// G1 spurio sui turni read-only, tool sui turni d'azione). Quando i dati del
/// classifier sono ASSENTI il primario resta INTATTO (None -> il `RouterNode`
/// decide come oggi), comportamento INVARIATO. Il primario PYTHON
/// (`run_via_brain`) NON passa di qui: ri-classifica internamente nel router_node.
fn build_initial_state(input: &NativeRunInput, role: RunRole) -> AgentState {
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

    // ── Tappa 1b (B) + parita' PRIMARIO RUST: action_oriented/report_only FEDELI ─
    // RADICE della divergenza stop_reason (g1 sui run 0-tool, loop sui run con
    // tool): il `RouterNode` del grafo Rust NON riclassifica (classifier LLM
    // delegato a un PR successivo), quindi senza dati cade nel fallback
    // `agentic_default` con `action_oriented=true` SEMPRE. Il primario Python
    // invece deriva `action_oriented` dal classifier del turno: per i turni
    // conversazionali read-only e' `false` -> niente G1.
    //
    // Per far CONVERGERE sia lo SHADOW sia il PRIMARIO RUST col primario Python
    // deriviamo `action_oriented`/`report_only` dagli STESSI dati del classifier
    // del turno (`requires_tools`/`agentic_score`/`authorizes_changes`) col PUNTO
    // UNICO `intent_classifier::derive_*` (regola L: porting 1:1 di
    // `brain/agents/nodes/__init__.py:686-739`). Il call site popola questi campi
    // in `NativeRunInput` via [`resolve_classifier_fields`] (helper condiviso,
    // regola L) in ENTRAMBI i rami Shadow e PRIMARY-Rust; il fix precedente
    // grossolano (`action_oriented_for_intent(intent_hint)`) resta SOLO come
    // fallback (ramo shadow) quando i dati del classifier non sono disponibili.
    // Quando NESSUN dato del classifier e' presente il primario resta None ->
    // il RouterNode decide (comportamento INVARIATO). Il primario PYTHON NON
    // passa di qui.
    //
    // `derive_from_classifier`: i dati del classifier sono presenti (popolati dal
    // call site). Sia per Shadow sia per Primary la derivazione FEDELE e' identica.
    let derive_from_classifier = input.classifier_resolved
        || input.requires_tools.is_some()
        || input.agentic_score.is_some();
    let intent_hint = input
        .intent_hint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (initial_intent, initial_action_oriented): (Option<String>, Option<bool>) =
        if derive_from_classifier {
            // Derivazione FEDELE (Shadow o Primary-Rust), identica al primario Python.
            let action_oriented = crate::intent_classifier::derive_action_oriented(
                intent_hint,
                input.requires_tools,
                input.agentic_score,
                input.action_oriented_min_score,
            );
            // user_intent: l'intent del primario se noto (intent_hint), altrimenti
            // None (l'intent vive nel routing, non e' richiesto dal grafo per
            // action_oriented).
            (intent_hint.map(str::to_string), Some(action_oriented))
        } else if role.is_shadow() {
            // Fallback grossolano SOLO shadow (classifier non disponibile): deriva
            // dall'intent_hint con la mappa deterministica. Quando manca anche
            // l'hint, None -> il RouterNode shadow applica il fallback del Python
            // degradato. Il PRIMARIO RUST senza dati resta None (decide il
            // RouterNode), comportamento INVARIATO.
            match intent_hint {
                Some(intent) => (
                    Some(intent.to_string()),
                    Some(nexus_agent_graph::decisions::action_oriented_for_intent(
                        intent,
                    )),
                ),
                None => (None, None),
            }
        } else {
            // Primario Rust senza dati classifier: INTATTO (None -> RouterNode).
            (None, None)
        };

    // report_only FEDELE (porting `__init__.py:736-739`), CABLATO nello stato del
    // grafo: l'executor lo consuma per NON strippare i tool read-only sui turni
    // di sola lettura (incidente 2026-07-02: "elenca i file" ha requires_tools=true
    // -> action_oriented=true, ma authorizes_changes=false -> report_only=true; lo
    // strip lasciava solo tool di scrittura e il run degenerava in edit-loop).
    // Vale per ENTRAMBI Shadow e Primary-Rust quando i dati del classifier sono
    // presenti; senza dati resta None (guard inerti solo su Some(true)).
    let initial_report_only: Option<bool> = if derive_from_classifier {
        let report_only = crate::intent_classifier::derive_report_only(
            input.classifier_resolved,
            intent_hint,
            input.authorizes_changes.unwrap_or(true),
        );
        tracing::debug!(
            run_id = %input.run_id,
            role_shadow = role.is_shadow(),
            report_only,
            action_oriented = ?initial_action_oriented,
            "native: derivazione fedele action_oriented/report_only dal classifier"
        );
        Some(report_only)
    } else {
        None
    };

    let mut extra = serde_json::Map::new();
    // Task del turno CORRENTE fissato all'origine (punto unico, regola L): il
    // supervisore lo legge da qui (`extract_original_task`) invece di ri-derivarlo
    // dalla cronologia, che in una sessione multi-turno contiene i task dei turni
    // PRECEDENTI. Senza questo, il supervisore inseguiva il primo Human del run —
    // spesso un auto-debug di crash iniettato dall'observer — invece del task reale
    // (incidente Chat 11 Beaty-Book: 60 iterazioni sul crash frontend al posto del
    // task di sicurezza auth).
    extra.insert(
        nexus_agent_graph::decisions::ORIGINAL_TASK_KEY.to_string(),
        serde_json::Value::String(input.initial_msg.clone()),
    );
    let mut declared_outcome = None;
    let mut stop_reason = None;
    let mut result = None;
    if let Some(synthesis) = &input.pre_run_advisory_synthesis {
        extra.insert(
            nexus_agent_graph::nodes::PRE_RUN_ADVISORY_SYNTHESIS_KEY.to_string(),
            synthesis.clone(),
        );
        let source = input
            .pre_run_advisory_source
            .unwrap_or("advisory_synthesis");
        if let Some(enforcement) =
            nexus_agent_graph::nodes::panel_enforcement_from_advisory_synthesis(synthesis, source)
        {
            let terminal = enforcement
                .get("terminal")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            extra.insert(
                nexus_agent_graph::nodes::PANEL_ENFORCEMENT_KEY.to_string(),
                enforcement.clone(),
            );
            if terminal {
                declared_outcome = enforcement.get("declared_outcome").cloned();
                stop_reason = Some(nexus_agent_graph::state::StopReason::EndTurn);
                result = enforcement
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
        }
    }

    let graph_automation_mode = parse_automation_mode(&input.automation_mode);
    let skip_hitl = automation_mode_skips_hitl(graph_automation_mode);

    AgentState {
        messages,
        thread_id: Some(input.run_id.to_string()),
        session_id: Some(input.session_id.to_string()),
        system_text: Some(input.system_text.clone()),
        intent_hint: input.intent_hint.clone(),
        user_intent: initial_intent,
        action_oriented: initial_action_oriented,
        report_only: initial_report_only,
        provider_override: Some(input.provider.clone()),
        model_override: Some(input.model.clone()),
        tools_json: tools,
        automation_mode: graph_automation_mode,
        approved: skip_hitl.then_some(true),
        supervisor_mode: Some(input.supervisor_mode),
        declared_outcome,
        stop_reason,
        result,
        extra,
        // DEBITO 1 chiuso (TODO Fase 5): `behavior_mode` valorizzato con la STESSA
        // fonte del primario Python, il quale lo riceve dal payload
        // `/agent/run/stream` (campo `behavior_mode`) e lo copia in
        // `initial_state["behavior_mode"]` (`agent.py:621`). mcp-core invia la
        // costante `PRIMARY_BEHAVIOR_MODE` (`agent_turn_setup.rs`): la riusiamo
        // qui (punto unico, regola L) cosi' lo shadow confronta un grafo col mode
        // IDENTICO. Conta sul serio dal momento in cui il planner e' eleggibile
        // (`plan_phase_enabled=true`, mig 0426/0439): `PlannerConfig::is_eligible`
        // gata su questo mode; senza valorizzarlo, lo shadow divergerebbe (None vs
        // "bilanciata"). Il valore-vero-dal-turno (derivarlo dall'automation_mode/
        // routing) e' un miglioramento separato, valido per ENTRAMBI i motori
        // (fuori scope: andrebbe cambiato PRIMA lato Python, vedi nota costante).
        behavior_mode: Some(crate::agent_turn_setup::PRIMARY_BEHAVIOR_MODE.to_string()),
        // Sub-agente nativo (porting di `run_subagent`): valorizza parent/depth nello
        // stato cosi' il grafo applica i guard di annidamento (UnderstandingNode skip
        // del fan-out explore se depth>=1). `None` per il run principale -> stato
        // INVARIATO (default None). Solo `dispatch_subagent` popola questi campi.
        parent_run_id: input.parent_run_id.map(|u| u.to_string()),
        subagent_depth: input.subagent_depth,
        // Epoch di avvio per la deadline di run (fase 3): scritto UNA volta qui
        // e checkpointato — un resume riparte dal checkpoint, quindi la deadline
        // misura il run INTERO. I sub-run hanno il proprio epoch ma il loro
        // tetto effettivo resta il tokio timeout clampato in prepare.
        run_started_at_epoch_s: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64),
        ..Default::default()
    }
}

/// Parsing della modalita' automazione nel enum del grafo. Delega al punto unico
/// `orchestrator::AutomationMode::try_parse` (identificatori canonici inglesi).
fn parse_automation_mode(s: &str) -> Option<nexus_agent_graph::AutomationMode> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    crate::orchestrator::AutomationMode::try_parse(Some(trimmed))
        .ok()
        .map(|m| m.to_graph_mode())
}

fn automation_mode_skips_hitl(mode: Option<nexus_agent_graph::AutomationMode>) -> bool {
    matches!(
        mode,
        Some(nexus_agent_graph::state::AutomationMode::Automatic)
            | Some(nexus_agent_graph::state::AutomationMode::Continuous)
    )
}

/// Esegue un run sul motore nativo end-to-end: costruisce engine+ctx,
/// `initial_state` dal prompt, gira `run_until_interrupt` e mappa l'esito.
///
/// `init` distingue nuovo run (Some) da resume (None, riparte dal checkpoint).
/// Per il path primario `shadow=false` (`ExecMode::Real`): i tool hanno
/// side-effect reali sul progetto.
pub async fn run_native(
    deps: &NativeDeps,
    input: &NativeRunInput,
) -> anyhow::Result<NativeRunOutcome> {
    let outcome = run_engine(deps, input, RunMode::New, RunRole::Primary).await?;
    Ok(map_outcome(outcome))
}

/// RESUME HITL di un run nativo PRIMARIO sospeso su `awaiting_confirmation`.
///
/// Riprende il run dal checkpoint Postgres (`nexus_graph_checkpoints`) iniettando
/// l'input umano di approvazione (`resume_message`) come messaggio Human in coda +
/// azzerando `awaiting_confirmation` — entrambi via il delta tipizzato del grafo,
/// che passa per il reducer (punto unico, regola L: non si scrivono i campi a
/// mano). PUNTO UNICO del resume nativo (regola L): riusa lo stesso
/// `build_native_engine` del run nuovo e delega al motore
/// [`AgentGraphEngine::resume_until_interrupt`], nessuna logica di loop duplicata.
///
/// Gli `input` portano provider/model/session/step_tx del run originale (li
/// ricostruisce il call site dai dati persistiti su `agent_runs`); il GRAFO
/// riparte comunque dal nodo salvato nel checkpoint, NON da `entry`: prompt/tools/
/// history dell'`input` non vengono usati per ricostruire lo stato (gia' nel
/// checkpoint), servono solo a popolare ctx + porte I/O.
pub async fn resume_native(
    deps: &NativeDeps,
    input: &NativeRunInput,
    resume_message: &str,
) -> anyhow::Result<NativeRunOutcome> {
    let resume_delta = build_resume_delta(resume_message, Some(&input.automation_mode));
    let outcome = run_engine(
        deps,
        input,
        RunMode::Resume { resume_delta },
        RunRole::Primary,
    )
    .await?;
    Ok(map_outcome(outcome))
}

/// Costruisce il delta opaco del runtime che sblocca un interrupt HITL: azzera
/// `awaiting_confirmation` e accoda il messaggio umano di approvazione (campo
/// `messages`, reducer append). Costruito col delta TIPIZZATO del grafo ->
/// `into_opaque` (punto unico tipizzato->opaco, regola L).
fn build_resume_delta(resume_message: &str, automation_mode: Option<&str>) -> nexus_graph::StateDelta {
    use nexus_agent_graph::state::{Message, MessageContent};
    let parsed_mode = automation_mode.and_then(parse_automation_mode);
    let typed = nexus_agent_graph::state::StateDelta {
        // Azzera il predicato di interrupt: senza, il motore si re-interrompe sul
        // checkpoint ancora-in-attesa (loop di conferma).
        awaiting_confirmation: Some(Some(false)),
        // Dopo l'approvazione esegui i tool mutativi senza re-interrompere.
        approved: Some(Some(true)),
        // Ripara automation_mode su checkpoint legacy (None -> HITL spurio).
        automation_mode: parsed_mode.map(Some),
        // Accoda l'approvazione come turno utente: l'executor la rilegge nel
        // contesto come ultimo messaggio (parita' col `resume_message` iniettato
        // dal brain nello state al `/agent/approve`).
        messages: Some(vec![Message::Human {
            content: MessageContent::text(resume_message.to_string()),
        }]),
        ..Default::default()
    };
    typed.into_opaque()
}

/// Costruisce il delta opaco del runtime che sblocca l'interrupt FAN-IN (Fase D):
/// azzera `awaiting_subagents` (`Some(Some(false))`, load-bearing per non
/// re-interrompere sul checkpoint ancora-in-attesa) e CONSEGNA al modello gli esiti
/// dei figli background completati. Gemello di [`build_resume_delta`] (regola L):
/// la differenza dall'HITL e' il MOTIVO di interrupt azzerato.
///
/// CONSEGNA via `messages` (append), NON solo via `subagent_results`: il nodo che
/// riprende e' l'`executor` (`interrupt_before=["executor"]`), che costruisce il
/// prompt LLM da `state.messages` e NON legge il campo `subagent_results` (letto
/// solo dal `todo_runner`). Iniettare solo nel campo lascerebbe il resume INERTE —
/// il padre riprenderebbe CIECO sugli esiti (in history solo il marker
/// `background_dispatched`). Il Message porta i verdetti STRUTTURATI (regola M).
/// `subagent_results` resta valorizzato nel campo per osservabilita' / eventuali
/// consumatori non-executor. Il delta e' TIPIZZATO e passa per il reducer
/// (`into_opaque`): non si scrivono i campi a mano.
fn build_resume_delta_subagents(subagent_results: Vec<Value>) -> nexus_graph::StateDelta {
    use nexus_agent_graph::state::{Message, MessageContent};
    let results_msg = format_fanin_results_message(&subagent_results);
    let typed = nexus_agent_graph::state::StateDelta {
        // Azzera il predicato di interrupt fan-in: senza, il motore si
        // re-interrompe sul checkpoint ancora-in-attesa (loop di fan-in).
        awaiting_subagents: Some(Some(false)),
        // Campo di stato (osservabilita' / consumatori non-executor). NON e' il
        // canale che il modello legge: quello e' `messages` qui sotto.
        subagent_results: Some(Some(subagent_results)),
        // Turno utente coi risultati: l'executor lo rilegge come ultimo messaggio
        // della history al riavvio del turno (stesso canale del resume HITL).
        messages: Some(vec![Message::Human {
            content: MessageContent::text(results_msg),
        }]),
        ..Default::default()
    };
    typed.into_opaque()
}

/// Cap per-figlio del `summary` iniettato al resume (caratteri). Evita che N figli
/// con summary enormi (final_summary non ha cap alla fonte) producano un singolo
/// turno gigantesco -> context_overflow al resume. Solo il `summary` (testo libero)
/// e' troncato; `status`/`outcome` (segnali autoritativi, regola M) restano integri.
const FANIN_SUMMARY_CAP: usize = 2000;

/// Formatta i risultati strutturati dei figli background nel testo del turno utente
/// iniettato al resume fan-in (`build_resume_delta_subagents`). Presenta un ARRAY
/// JSON (i segnali autoritativi `status`/`outcome` sono struttura, non prosa: regola
/// M); il modello lo parsa come JSON. NIENTE delimitatori XML: un `summary` LLM che
/// contenesse la stringa di chiusura confonderebbe un parsing naive del delimitatore
/// (l'array JSON e' auto-delimitante, un tag di chiusura dentro una stringa e' solo
/// contenuto). Array vuoto -> nota esplicita (il modello riprende comunque non cieco).
fn format_fanin_results_message(results: &[Value]) -> String {
    if results.is_empty() {
        return "I sub-agenti in background che avevi dispatchato sono terminati \
                (nessun nuovo esito strutturato da riportare)."
            .to_string();
    }
    // Tronca i soli `summary` (testo libero LLM, potenzialmente enorme).
    let capped: Vec<Value> = results
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if let Some(s) = r.get("summary").and_then(Value::as_str) {
                if s.chars().count() > FANIN_SUMMARY_CAP {
                    let head: String = s.chars().take(FANIN_SUMMARY_CAP).collect();
                    r["summary"] = Value::String(format!("{head}... [troncato]"));
                }
            }
            r
        })
        .collect();
    let json = serde_json::to_string_pretty(&capped).unwrap_or_else(|_| "[]".to_string());
    format!(
        "I sub-agenti in background che avevi dispatchato sono terminati. Di seguito \
         i loro esiti come array JSON (un oggetto per figlio; `status` e `outcome` \
         sono i segnali autoritativi, `summary` e' descrittivo):\n{json}"
    )
}

/// RESUME FAN-IN di un run nativo PRIMARIO sospeso su `awaiting_subagents`
/// (Fase D). Gemello di [`resume_native`] (regola L): riparte dal checkpoint
/// Postgres via [`run_engine`] con `RunMode::Resume`, ma applicando il delta
/// fan-in (azzera `awaiting_subagents` + inietta `subagent_results`) invece del
/// delta HITL. Gli `input` portano provider/model/session/step_tx del run
/// originale; il GRAFO riparte dal nodo salvato nel checkpoint.
pub async fn resume_native_fanin(
    deps: &NativeDeps,
    input: &NativeRunInput,
    subagent_results: Vec<Value>,
) -> anyhow::Result<NativeRunOutcome> {
    let resume_delta = build_resume_delta_subagents(subagent_results);
    let outcome = run_engine(
        deps,
        input,
        RunMode::Resume { resume_delta },
        RunRole::Primary,
    )
    .await?;
    Ok(map_outcome(outcome))
}

/// Esegue il grafo nativo end-to-end e ritorna lo [`StepOutcome`] COMPLETO (lo
/// stato finale, non solo il sommario): il run shadow ne ha bisogno per la
/// proiezione canonica (conteggio tool, produced_work). Punto unico (regola L)
/// dell'esecuzione del motore: sia il primario che lo shadow passano di qui,
/// distinti solo dal `role` (e dal `mode`).
///
/// `mode` distingue avvio nuovo (`RunMode::New`, initial_state dal prompt) da
/// resume HITL (`RunMode::Resume`, riparte dal checkpoint applicando il delta di
/// approvazione). `role` decide tools/emit/checkpointer + il flag `shadow` del ctx.
async fn run_engine(
    deps: &NativeDeps,
    input: &NativeRunInput,
    mode: RunMode,
    role: RunRole,
) -> anyhow::Result<StepOutcome<AgentState>> {
    let (engine, routing_cfg, llm, tools, emit, isolation_available) =
        build_native_engine(deps, input, role).await?;

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
        // `shadow` deriva dal ruolo: in Shadow i nodi usano ExecMode::Replay (punto
        // unico AgentNodeCtx::exec_mode) -> tutti gli store gatati no-op, zero
        // side-effect. Il primario e' Real.
        shadow: role.is_shadow(),
        // Fase C3 Part B: isolamento fisico dei sub-run disponibile (calcolato in
        // build_native_engine, flag-gated). Alimenta il gate di orchestrazione.
        isolation_available,
        // Barriera di scrittura advisory (overlap, mig 0606): presente solo se il
        // chiamante ha avviato il run PRIMA dei panel. `None` = ramo classico
        // (verdetti gia' nello stato iniziale) -> gate inerte, bit-identico.
        advisory_gate: input.advisory_gate.clone(),
    };

    // Avvio nuovo: parte da `entry` con l'initial_state dal prompt.
    // Resume HITL: nessun init (carica il checkpoint), applica il resume_delta.
    match mode {
        RunMode::New => {
            let mut init_state = build_initial_state(input, role);
            // ── ROUTING INIZIALE tier (FIX-A scale-controller) ───────────────────
            // Il primo modello del run e' `(input.provider, input.model)` risolto dal
            // routing a monte (il primo turno dell'executor lo usa via
            // provider_override/model_override, sticky ancora assente). La
            // RoutingMatrix::lookup non porta il tier; lo risolviamo qui dal catalog
            // (punto unico `resolve_initial_tier`, percorso Real all'avvio) e lo
            // scriviamo nel checkpoint iniziale. Cosi' `current_tier` e' popolato dal
            // primo turno, non solo dopo un'escalation/upscale. INERTE (PR-A): nessun
            // decisore lo legge ancora -> bit-identico. Determinismo: la query e'
            // FUORI dal loop del grafo (non nel path di Replay); il tier vive poi
            // nello stato checkpointato, condiviso da Primary e Shadow (convergono).
            init_state.current_tier =
                Some(resolve_initial_tier(&deps.db, &input.provider, &input.model).await);
            // Playbook matcher (punto unico, regola L): popola i passi del playbook
            // che matcha il task, cosi' il planner genera i todo deterministici
            // (es. nexus_visual_compare per la verifica figma) e il final_gate puo'
            // applicare design_verify. Senza, `playbook_steps` resta vuoto: era
            // l'anello mancante del porting (il matcher viveva nel brain Python).
            // project_root None: i trigger con `project_markers` non sono valutati
            // qui (root non risolta nel punto di costruzione dello state); i
            // playbook senza markers — es. verify.design_align — matchano comunque.
            if let Some(pm) = crate::playbook_engine::match_playbook(
                &deps.db,
                input.intent_hint.as_deref(),
                &input.initial_msg,
                None,
            )
            .await
            {
                tracing::info!(
                    target: "mcp_core::native_engine",
                    playbook = %pm.key,
                    steps = pm.steps.len(),
                    "playbook matchato -> playbook_steps popolati"
                );
                init_state.playbook_steps = Some(pm.steps);
                init_state.playbook_key = Some(pm.key);
            }

            // ── Detector clarification CROSS-RUN (loop email, blocco #5) ─────────
            // Calcolato UNA volta all'avvio del run PRIMARIO, FUORI dal grafo
            // (replay-safe): il valore e' checkpointato in `AgentState`, cosi' il
            // grafo e il replay lo rileggono dallo stato senza re-interrogare il DB
            // (il principio guida del piano: la decisione e' presa fuori dal
            // percorso caldo, il percorso caldo la rilegge). Solo PRIMARIO: nello
            // shadow (Replay) NON leggiamo stato live che evolve (come
            // `NullBillingCooldownPort`) — lo shadow rigioca lo stato del primario,
            // che porta gia' `repeated_clarify_count` checkpointato.
            //
            // FONTE STRUTTURATA (regola M): i meta_step `kind='clarify'` della
            // sessione. Il pool e' quello del DOMINIO RUN (agent_runs/meta_steps
            // migrati al DB progetto), risolto col PUNTO UNICO per-sessione.
            // Fail-open: guasto DB -> conteggio 0 (asse mai attivo, invariato).
            if !role.is_shadow() {
                let run_db = crate::project_db_routes::project_data_pool_by_session_from(
                    &deps.db,
                    input.session_id,
                )
                .await;
                let clarify_history = PgClarifyHistoryStore::new(run_db);
                let (sig, repeat_count) = clarify_history
                    .latest_signature_and_repeat_count(input.session_id)
                    .await;
                if repeat_count > 0 {
                    tracing::info!(
                        target: "mcp_core::native_engine",
                        session_id = %input.session_id,
                        repeated_clarify_count = repeat_count,
                        signature = sig.as_deref().unwrap_or(""),
                        "detector clarify cross-run: domande-chiarimento ripetute nella sessione"
                    );
                }
                init_state.repeated_clarify_count = Some(repeat_count);
            }

            let init = Some(init_state);
            engine
                .run_until_interrupt(input.run_id, init, &ctx)
                .await
                .map_err(|e| anyhow::anyhow!("motore nativo: run_until_interrupt fallita: {e}"))
        }
        RunMode::Resume { resume_delta } => engine
            .resume_until_interrupt(input.run_id, resume_delta, &ctx)
            .await
            .map_err(|e| anyhow::anyhow!("motore nativo: resume_until_interrupt fallita: {e}")),
    }
}

/// Mappa lo [`StepOutcome`] del motore nel [`NativeRunOutcome`] del chiamante.
/// Estrae anche usage/iterazioni/intent dallo stato finale (servono al
/// finalizzatore condiviso, regola L).
fn map_outcome(outcome: StepOutcome<AgentState>) -> NativeRunOutcome {
    let (state, completed, resume_at) = match outcome {
        StepOutcome::Completed(state) => (state, true, None),
        StepOutcome::Interrupted { state, resume_at } => {
            (state, false, Some(resume_at.as_label().to_string()))
        }
    };
    NativeRunOutcome {
        completed,
        // Fan-in (Fase D): l'interrupt e' l'attesa dei sub-run background se lo
        // stato porta ancora `awaiting_subagents=true`. Segnale strutturato
        // (regola M) letto dallo stato, non dedotto dal `resume_at`.
        awaiting_subagents: state.is_awaiting_subagents(),
        final_answer: state.result.clone(),
        stop_reason: state.stop_reason,
        provider_used: state.provider_used.clone(),
        model_used: state.model_used.clone(),
        resume_at,
        iterations: state.iterations.unwrap_or(0),
        prompt_tokens: state.prompt_tokens.unwrap_or(0),
        completion_tokens: state.completion_tokens.unwrap_or(0),
        total_tokens: state.total_tokens.unwrap_or(0),
        // Costo del RUN, non del turno: `total_cost_usd` ha un reducer overwrite e
        // vale l'ULTIMA iterazione, mentre `run_cost_cumulative_usd` somma i costi
        // di tutti i turni (executor.rs, "Costo cumulativo REALE del run") ed e' lo
        // STESSO campo su cui gia' decide il freno di spesa. Asimmetria VOLUTA coi
        // token qui sopra, che restano dell'ultimo turno by design (alimentano
        // `last_prompt_tokens` = riempimento contesto): non "uniformarli".
        total_cost: state.run_cost_cumulative_usd.unwrap_or(0.0),
        user_intent: state.user_intent.clone(),
        reasoning: state.reasoning_acc.clone().filter(|s| !s.trim().is_empty()),
        // Conversazione finale serializzata per agent_runs.messages_json (resume +
        // trace panel). `Message` serializza in `{role, content, [tool_calls|
        // tool_call_id]}`, la forma attesa dal resume. Conversazione vuota o
        // serializzazione fallita -> None (la colonna resta NULL, nessun valore
        // spurio).
        messages_json: if state.messages.is_empty() {
            None
        } else {
            serde_json::to_string(&state.messages).ok()
        },
        declared_outcome: state.declared_outcome.clone(),
        review_verdict: state.review_verdict.clone(),
        advisory_verdict: state.advisory_verdict.clone(),
        debate_position: state.debate_position.clone(),
        error_class: state
            .extra
            .get("error_class")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        forced_close_unverified: state.forced_close_unverified.unwrap_or(false),
        // Esito della review adversariale dal SEGNALE del ReviewGate (regola M),
        // match esaustivo: un ramo nuovo del nodo non compila finche' non
        // dichiara cosa significa qui. `PendingCorrection` a fine run e' vero
        // per costruzione: la review aveva bocciato e la ri-review non e'
        // avvenuta (run morto prima di rientrare).
        review_panel_rejected: match state.review_gate_verdict {
            Some(ReviewGateVerdict::PendingCorrection | ReviewGateVerdict::RejectedFinal) => true,
            Some(
                ReviewGateVerdict::Approved
                | ReviewGateVerdict::NotApplicable
                | ReviewGateVerdict::Inconclusive
                | ReviewGateVerdict::Unavailable,
            )
            | None => false,
        },
        review_panel_last: state.extra.get("review_panel_last").cloned(),
        final_gate_passed: state.final_gate_passed,
        final_gate_unverified: state.final_gate_unverified,
        // Bocciatura del gate rimasta pendente a fine run: letta dal SEGNALE del
        // gate (regola M), non piu' dedotta dal CONTATORE `final_gate_cycle`.
        // Il `match` e' esaustivo di proposito: un ramo nuovo del gate non
        // compila finche' non dichiara cosa significa qui.
        final_gate_failed_pending: match state.final_gate_verdict {
            // L'unico caso vero: bocciato con ri-verifica ATTESA e mai avvenuta.
            // In plan-phase il gate e' one-shot (`final_gate_eligible` la esclude):
            // nessuna ri-verifica era prevista, quindi non e' "pendente".
            Some(FinalGateVerdict::FailedPendingCorrection) => {
                !state.plan_phase_active.unwrap_or(false)
            }
            // Grazia: criteri oggettivi TUTTI passati, manca solo la firma. Era
            // questo il falso positivo: lascia `cycle = max_cycles` e veniva
            // letto come "verifica fallita e non ripetuta".
            Some(FinalGateVerdict::ObjectivePassedSignatureMissing) => false,
            // Verdetti espliciti: l'esito lo portano `final_gate_passed` /
            // `final_gate_unverified`, non questo flag.
            Some(FinalGateVerdict::Passed | FinalGateVerdict::FailedFinal) => false,
            // Il run e' proseguito su un modello promosso: non e' un esito.
            Some(FinalGateVerdict::EscalationHandoff) => false,
            // Il gate non e' mai entrato: niente da dichiarare fallito.
            None => false,
        },
        pending_actions: state
            .extra
            .get(HITL_PENDING_ACTIONS_EXTRA_KEY)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
    }
}

// ===========================================================================
// SHADOW (F4): driver di confronto STATO FINALE primario Python <-> shadow Rust.
//
// Modello STRADA B (confronto stato finale, NON per-nodo): il grafo Python e
// quello Rust hanno topologie DIVERSE (i nodi non corrispondono 1:1), quindi un
// confronto per-nodo e' inutile. Si esegue l'INTERO grafo Rust in shadow (Replay
// dei tool_result del primario, LlmGateway REAL) e si confronta la PROIEZIONE
// CANONICA dell'esito (segnali STRUTTURALI), NON il testo della risposta (con
// LLM Real il testo diverge sempre -> rumore inutile). Si persiste UN solo record
// in nexus_shadow_telemetry con node_name "__final_state__".
// ===========================================================================

/// Pseudo-nodo della telemetria shadow: il confronto e' sullo STATO FINALE, non
/// per-nodo (le topologie Python/Rust differiscono). Un solo record per run.
const SHADOW_FINAL_STATE_NODE: &str = "__final_state__";

/// Tool che producono LAVORO concreto sul progetto (scrittura/modifica/esecuzione).
/// Se il run ne ha invocato almeno uno, `has_produced_work=true` nella proiezione
/// canonica. Lista MINIMA estendibile (e' una proiezione, non un enforcement).
const MUTATING_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "create_file",
    "apply_patch",
    "rename_file",
    "fs_move",
    "run_command",
];

/// Canonicalizza un `stop_reason` (Rust o Python) al VOCABOLARIO COMUNE della
/// proiezione: `end_turn` / `tool_use` / `failed` / `interrupted` / `loop` /
/// `superseded` / `other`. Punto unico (regola L): sia il primario (stringa dal
/// DB) sia lo shadow (enum Rust mappato a stringa via serde) passano di qui, cosi'
/// i due lati sono confrontabili anche se nascono da rappresentazioni diverse.
///
/// `None` (nessun stop_reason) -> `"none"`. Stringhe Python note (es. lo status
/// `agent_runs.status` quando lo stop_reason non e' separato) sono mappate ai
/// valori del vocabolario; le ignote ricadono su `"other"` (esplicito, niente
/// magic fallback su un valore di business — regola G).
fn canonical_stop_reason(raw: Option<&str>) -> &'static str {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        None => "none",
        Some(s) => match s.as_str() {
            // Vocabolario comune diretto.
            "end_turn"
            | "endturn"
            | "stop"
            | "completed"
            | "completed_verified"
            | "completed_unverified" => "end_turn",
            "tool_use" | "tooluse" => "tool_use",
            "error" | "failed" | "failed_diagnosed" => "failed",
            "interrupted" | "awaiting_confirmation" | "blocked_needs_input" => "interrupted",
            "loop_detected" | "loop_abort" | "loopdetected" | "loopabort" => "loop",
            "superseded" => "superseded",
            "g1_escalated" | "g1_cap_reached" | "g1escalated" | "g1capreached" => "g1",
            "" => "none",
            _ => "other",
        },
    }
}

/// Serializza lo `StopReason` Rust nella sua forma snake_case (la stessa del
/// `#[serde(rename_all = "snake_case")]` dell'enum) per poi canonicalizzarla con
/// lo STESSO `canonical_stop_reason` del primario (un solo vocabolario, regola L).
fn stop_reason_label(sr: Option<StopReason>) -> Option<String> {
    sr.and_then(|r| match serde_json::to_value(r) {
        Ok(Value::String(s)) => Some(s),
        _ => None,
    })
}

/// Proiezione canonica MINIMA dell'esito di un run (chiavi STRUTTURALI, NON il
/// testo della risposta). E' il punto unico del confronto shadow (regola L):
/// sia il primario sia lo shadow producono questa stessa forma, cosi'
/// `compute_diff` opera su chiavi omogenee. Estendibile con altre chiavi
/// strutturali (es. files_touched) senza toccare il confronto.
///
/// Chiavi:
/// - `completed` (bool): il grafo e' arrivato a completare?
/// - `stop_reason` (string): vocabolario comune (vedi `canonical_stop_reason`).
/// - `num_tool_calls` (int): quanti tool sono stati invocati.
/// - `has_produced_work` (bool): almeno un tool mutativo (write/edit/run/...)?
fn make_canonical(
    completed: bool,
    stop_reason: &str,
    num_tool_calls: i64,
    has_produced_work: bool,
) -> Value {
    serde_json::json!({
        "completed": completed,
        "stop_reason": stop_reason,
        "num_tool_calls": num_tool_calls,
        "has_produced_work": has_produced_work,
    })
}

/// Proiezione canonica dell'esito del run SHADOW dal suo `AgentState` finale.
///
/// - `completed`: lo `StepOutcome` e' `Completed` (passato dal driver).
/// - `stop_reason`: canonicalizzato dall'enum Rust.
/// - tool calls: contati dai `Message` dello stato (forma OpenAI-compat
///   `Ai.tool_calls` + forma Anthropic `ContentBlock::ToolUse` inline). Per
///   `has_produced_work` si guarda il NOME del tool contro `MUTATING_TOOLS`.
fn shadow_canonical(state: &AgentState, completed: bool) -> Value {
    use nexus_agent_graph::state::{ContentBlock, MessageContent};

    let mut num_tool_calls: i64 = 0;
    let mut produced = false;
    let mut note_tool = |name: &str| {
        num_tool_calls += 1;
        if MUTATING_TOOLS.contains(&name) {
            produced = true;
        }
    };

    for m in &state.messages {
        if let Message::Ai {
            content,
            tool_calls,
            ..
        } = m
        {
            // Forma OpenAI-compat: tool_calls fuori dal contenuto.
            for tc in tool_calls {
                note_tool(&tc.name);
            }
            // Forma Anthropic: ToolUse come blocco di contenuto inline.
            if let MessageContent::Blocks(blocks) = content {
                for b in blocks {
                    if let ContentBlock::ToolUse { name, .. } = b {
                        note_tool(name);
                    }
                }
            }
        }
    }

    let sr_label = stop_reason_label(state.stop_reason);
    make_canonical(
        completed,
        canonical_stop_reason(sr_label.as_deref()),
        num_tool_calls,
        produced,
    )
}

/// Proiezione canonica dell'esito del run PRIMARIO (Python) letta dal DB.
///
/// - `completed`: lo `status` di `agent_runs` e' uno stato di successo
///   (completed / completed_verified).
/// - `stop_reason`: canonicalizzato dallo `status` (il primario Python non
///   persiste uno `stop_reason` separato in agent_runs: lo status e' il segnale
///   strutturale di chiusura).
/// - tool calls: contati da `agent_steps` (gli step del run = i tool invocati);
///   `has_produced_work` se almeno un `tool_name` e' mutativo.
async fn primary_canonical(db: &PgPool, primary_run_id: Uuid) -> anyhow::Result<Value> {
    // status e' TEXT NOT NULL (mig 0009): fetch_optional -> Option<String> (None
    // solo se il run non esiste, caso anomalo nel flusso shadow).
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
        .bind(primary_run_id)
        .fetch_optional(db)
        .await?;

    let tool_names: Vec<String> =
        sqlx::query_scalar("SELECT tool_name FROM agent_steps WHERE run_id = $1")
            .bind(primary_run_id)
            .fetch_all(db)
            .await?;

    let num_tool_calls = tool_names.len() as i64;
    let has_produced_work = tool_names
        .iter()
        .any(|n| MUTATING_TOOLS.contains(&n.as_str()));
    let status_lc = status.as_deref().map(|s| s.to_ascii_lowercase());
    let completed = matches!(
        status_lc.as_deref(),
        Some("completed") | Some("completed_verified") | Some("completed_unverified")
    );

    Ok(make_canonical(
        completed,
        canonical_stop_reason(status.as_deref()),
        num_tool_calls,
        has_produced_work,
    ))
}

/// Driver SHADOW: dato un run PRIMARIO Python gia' concluso, ri-esegue l'intero
/// grafo Rust in modalita' shadow (read-only) e persiste UN record di telemetria
/// con il confronto della proiezione canonica.
///
/// Shadow-safety (zero side-effect, regola E/F): `ExecMode::Replay` (tool e store
/// gatati no-op), `ToolExecutor::from_db_for_replay` (rilegge i tool_result del
/// primario), `NullEventSink` (niente SSE), `MemoryCheckpointer` (niente scritture
/// su nexus_graph_checkpoints), criterio http del final_gate gatato in Replay
/// (niente reqwest). L'LLM e' in REPLAY sullo shadow (`ReplayLlmGateway`):
/// l'executor RIGIOCA la sequenza di tool del primario letta da `agent_steps` (cosi'
/// `num_tool_calls` converge col primario e le divergenze residue sono BUG VERI del
/// grafo, non artefatti LLM), gli ausiliari (planner/reflection/clarify) sono
/// neutralizzati con una risposta neutra deterministica (costo token ZERO). REAL
/// coesiste sul PRIMARIO via `RunRole` (switch nel punto unico `build_native_engine`).
///
/// Su QUALUNQUE errore lo shadow ritorna `Err` ma il chiamante lo tratta come
/// WARN: lo shadow non deve MAI impattare il run primario.
pub async fn run_shadow(
    deps: &NativeDeps,
    input: &NativeRunInput,
    primary_run_id: Uuid,
) -> anyhow::Result<()> {
    // Esegue il grafo Rust in shadow: nuovo run (initial_state dal prompt del
    // primario), tools in Replay sul primario, checkpointer in-memory.
    let outcome = run_engine(
        deps,
        input,
        RunMode::New,
        RunRole::Shadow { primary_run_id },
    )
    .await?;

    let (shadow_state, completed) = match &outcome {
        StepOutcome::Completed(s) => (s, true),
        StepOutcome::Interrupted { state, .. } => (state, false),
    };

    // Proiezioni canoniche: primario dal DB, shadow dallo stato finale.
    // Separazione DB: agent_runs/agent_steps del primario vivono nel DB del
    // progetto -> stesso pool risolto dalla sessione usato da run_engine, non il
    // meta (dove la proiezione uscirebbe vuota: 0 tool calls, completed=false).
    let primary_pool =
        crate::project_db_routes::project_data_pool_by_session_from(&deps.db, input.session_id)
            .await;
    let primary = primary_canonical(&primary_pool, primary_run_id).await?;
    let shadow = shadow_canonical(shadow_state, completed);

    // Persiste UN record "__final_state__" col diff (punto unico shadow, regola L:
    // NESSUN uso di DiffCollector::record per-nodo). persist_node_diff ricalcola
    // internamente i divergent_keys via compute_diff.
    let divergent = nexus_agent_graph::compute_diff(&primary, &shadow);
    nexus_agent_graph::persist_node_diff(
        &deps.db,
        primary_run_id,
        SHADOW_FINAL_STATE_NODE,
        &primary,
        &shadow,
    )
    .await
    .map_err(|e| anyhow::anyhow!("persistenza telemetria shadow: {e}"))?;

    // Niente leak (regola F): si logga la convergenza strutturale, non il testo.
    tracing::info!(
        primary_run_id = %primary_run_id,
        converged = divergent.is_empty(),
        divergent_keys = ?divergent,
        "shadow: confronto stato finale persistito (__final_state__)"
    );
    Ok(())
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
            // Default neutri: il classifier NON ha risolto. I test che esercitano
            // la derivazione fedele (Tappa 1b B) li sovrascrivono esplicitamente.
            requires_tools: None,
            agentic_score: None,
            authorizes_changes: None,
            classifier_resolved: false,
            action_oriented_min_score: crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE,
            automation_mode: "automatic".to_string(),
            supervisor_mode: SupervisorMode::None,
            step_tx: tx,
            // Run principale di test: nessun annidamento sub-agente.
            parent_run_id: None,
            subagent_depth: None,
            sizing_complexity: None,
            sizing_scope_system_wide: false,
            run_time_budget_s: None,
            // Test del run principale: nessun isolamento (root del progetto).
            working_root: None,
            pre_run_advisory_synthesis: None,
            pre_run_advisory_source: None,
            // Nessun overlap nei test di costruzione dello stato: la barriera e'
            // esercitata dai test del ToolDispatchNode.
            advisory_gate: None,
        }
    }

    #[test]
    fn initial_state_da_prompt_history_e_override() {
        let input = sample_input();
        let state = build_initial_state(&input, RunRole::Primary);
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

        // DEBITO 1: behavior_mode valorizzato con la STESSA fonte del primario
        // Python (la costante del client brain), non None.
        assert_eq!(
            state.behavior_mode.as_deref(),
            Some(crate::agent_turn_setup::PRIMARY_BEHAVIOR_MODE),
            "behavior_mode = fonte primario (bilanciata), per parita' con lo shadow"
        );

        // Tools propagati (array non vuoto).
        let tools = state.tools_json.expect("tools propagati");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "read_file");

        // FIX shadow: il PRIMARIO non forza action_oriented/user_intent: restano
        // None e il RouterNode reale decide come oggi (zero impatto sul Real).
        assert_eq!(
            state.action_oriented, None,
            "primario: action_oriented non forzato (decide il RouterNode)"
        );
        assert_eq!(
            state.user_intent, None,
            "primario: user_intent non forzato (decide il RouterNode)"
        );
    }

    #[test]
    fn initial_state_seeda_pre_run_advisory_synthesis() {
        let mut input = sample_input();
        input.pre_run_advisory_synthesis = Some(serde_json::json!({
            "verdict": "proceed_with_changes",
            "requirements": ["punto unico routing"],
        }));
        input.pre_run_advisory_source = Some("multi_provider_synthesis");
        let state = build_initial_state(&input, RunRole::Primary);
        assert!(state
            .extra
            .contains_key(nexus_agent_graph::nodes::PRE_RUN_ADVISORY_SYNTHESIS_KEY));
        let enforcement = state
            .extra
            .get(nexus_agent_graph::nodes::PANEL_ENFORCEMENT_KEY)
            .expect("panel enforcement");
        assert_eq!(
            enforcement.get("source").and_then(serde_json::Value::as_str),
            Some("multi_provider_synthesis")
        );
        assert_eq!(
            enforcement.get("verdict").and_then(serde_json::Value::as_str),
            Some("proceed_with_changes")
        );
    }

    /// ADVISORY NON BLOCCANTE (decisione prodotto 2026-07-13): un verdetto "block" del
    /// consiglio pre-run NON deve rendere il run terminale (niente stop_reason ne'
    /// declared_outcome "blocked" pre-seedati). Il coordinatore legge il segnale e
    /// procede coi vincoli. Regressione del bug "l'agente pianifica e si ferma dopo il
    /// consiglio" (il modello chiudeva con 'Iniziero'...' senza implementare).
    #[test]
    fn initial_state_block_consiglio_non_e_terminale() {
        let mut input = sample_input();
        input.pre_run_advisory_synthesis = Some(serde_json::json!({
            "verdict": "block",
            "risks": [{"severity": "high", "description": "x"}],
        }));
        input.pre_run_advisory_source = Some("council");
        let state = build_initial_state(&input, RunRole::Primary);
        // Il segnale advisory c'e' (il coordinatore lo LEGGE)...
        assert!(state
            .extra
            .contains_key(nexus_agent_graph::nodes::PRE_RUN_ADVISORY_SYNTHESIS_KEY));
        // ...ma l'enforcement del block NON e' terminale.
        let enforcement = state
            .extra
            .get(nexus_agent_graph::nodes::PANEL_ENFORCEMENT_KEY)
            .expect("panel enforcement seedato");
        assert_eq!(
            enforcement
                .get("terminal")
                .and_then(serde_json::Value::as_bool),
            Some(false),
            "un block del consiglio NON deve fermare il run (advisory)"
        );
        // Il run NON parte gia' terminato: niente EndTurn ne' 'blocked' pre-dichiarato.
        assert!(
            state.stop_reason.is_none(),
            "niente stop_reason pre-seedato dal block"
        );
        assert!(
            state.declared_outcome.is_none(),
            "niente declared_outcome 'blocked' pre-seedato"
        );
    }

    #[test]
    fn initial_state_tools_null_diventa_none() {
        let mut input = sample_input();
        input.tools_json = serde_json::Value::Null;
        let state = build_initial_state(&input, RunRole::Primary);
        assert!(state.tools_json.is_none(), "tools null -> None");
    }

    /// Run PRINCIPALE (default): nessun parent/depth sub-agente nello stato.
    #[test]
    fn initial_state_principale_senza_parent_depth() {
        let input = sample_input();
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(state.parent_run_id, None, "run principale: nessun parent");
        assert_eq!(state.subagent_depth, None, "run principale: nessun depth");
    }

    /// SUB-RUN nativo: parent_run_id + subagent_depth propagati nello stato cosi'
    /// il grafo applica i guard di annidamento (UnderstandingNode skip se depth>=1).
    #[test]
    fn initial_state_subrun_propaga_parent_e_depth() {
        let mut input = sample_input();
        let parent = Uuid::new_v4();
        input.parent_run_id = Some(parent);
        input.subagent_depth = Some(2);
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(
            state.parent_run_id.as_deref(),
            Some(parent.to_string().as_str()),
            "sub-run: parent_run_id propagato"
        );
        assert_eq!(
            state.subagent_depth,
            Some(2),
            "sub-run: subagent_depth propagato (anti-ricorsione/guard fan-out)"
        );
    }

    /// FIX shadow LLM-Replay: nel ramo Shadow, con `intent_hint` OPERATIVO
    /// (`code_write`), lo stato iniziale deriva `user_intent` + `action_oriented`
    /// (true) col punto unico, cosi' il grafo shadow non parte da `action_oriented`
    /// forzato dal fallback del RouterNode.
    #[test]
    fn initial_state_shadow_intent_operativo_deriva_action() {
        let input = sample_input(); // intent_hint = "code_write"
        let primary_run_id = Uuid::new_v4();
        let state = build_initial_state(&input, RunRole::Shadow { primary_run_id });
        assert_eq!(state.user_intent.as_deref(), Some("code_write"));
        assert_eq!(
            state.action_oriented,
            Some(true),
            "intent operativo -> azione"
        );
    }

    /// FIX shadow LLM-Replay: nel ramo Shadow, con `intent_hint` CONVERSAZIONALE
    /// (`chat`), `action_oriented` deriva a `false` -> niente G1 sui turni 0-tool
    /// (questa era la RADICE della divergenza `g1`).
    #[test]
    fn initial_state_shadow_intent_chat_action_false() {
        let mut input = sample_input();
        input.intent_hint = Some("chat".to_string());
        let primary_run_id = Uuid::new_v4();
        let state = build_initial_state(&input, RunRole::Shadow { primary_run_id });
        assert_eq!(state.user_intent.as_deref(), Some("chat"));
        assert_eq!(
            state.action_oriented,
            Some(false),
            "intent conversazionale -> NON azione (niente G1)"
        );
    }

    /// FIX shadow (fallback grossolano): nel ramo Shadow SENZA `intent_hint` E
    /// senza dati del classifier, lo stato resta a None (il RouterNode shadow
    /// applichera' il fallback del Python degradato).
    #[test]
    fn initial_state_shadow_senza_intent_resta_none() {
        let mut input = sample_input();
        input.intent_hint = None;
        let primary_run_id = Uuid::new_v4();
        let state = build_initial_state(&input, RunRole::Shadow { primary_run_id });
        assert_eq!(state.user_intent, None);
        assert_eq!(state.action_oriented, None);
    }

    // ── Tappa 1b (B): derivazione FEDELE dal classifier completo ─────────────

    /// Shadow + classifier RISOLTO su un turno read-only (requires_tools=false,
    /// agentic_score sotto soglia, niente intent_hint): action_oriented=false
    /// FEDELE al primario Python -> niente G1Continue -> stop_reason converge.
    /// Questa e' la RADICE della divergenza g1 che la Tappa 1b chiude.
    #[test]
    fn initial_state_shadow_classifier_read_only_action_false() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = true;
        input.requires_tools = Some(false);
        input.agentic_score = Some(0.10);
        input.authorizes_changes = Some(false);
        let primary_run_id = Uuid::new_v4();
        let state = build_initial_state(&input, RunRole::Shadow { primary_run_id });
        assert_eq!(
            state.action_oriented,
            Some(false),
            "turno read-only (classifier risolto) -> NON azione, niente G1"
        );
    }

    /// Shadow + classifier RISOLTO su un turno d'azione (requires_tools=true):
    /// action_oriented=true FEDELE, anche se l'agentic_score e' basso.
    #[test]
    fn initial_state_shadow_classifier_azione_action_true() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = true;
        input.requires_tools = Some(true);
        input.agentic_score = Some(0.20);
        input.authorizes_changes = Some(true);
        let primary_run_id = Uuid::new_v4();
        let state = build_initial_state(&input, RunRole::Shadow { primary_run_id });
        assert_eq!(
            state.action_oriented,
            Some(true),
            "requires_tools=true -> azione (fedele al primario)"
        );
    }

    /// Shadow + classifier RISOLTO con requires_tools assente ma agentic_score
    /// SOPRA la soglia: action_oriented=true via soglia (porting __init__.py:699).
    #[test]
    fn initial_state_shadow_classifier_score_sopra_soglia_action_true() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = true;
        input.requires_tools = None;
        input.agentic_score = Some(0.80);
        input.action_oriented_min_score = 0.5;
        let primary_run_id = Uuid::new_v4();
        let state = build_initial_state(&input, RunRole::Shadow { primary_run_id });
        assert_eq!(
            state.action_oriented,
            Some(true),
            "score 0.80 >= 0.5 -> azione"
        );
    }

    // ── Parita' PRIMARIO RUST: deriva action_oriented dal classifier come Python ─

    /// PRIMARIO RUST + classifier RISOLTO su turno read-only (requires_tools=false,
    /// score sotto soglia, niente intent_hint): action_oriented=false FEDELE al
    /// primario Python -> niente G1 spurio sui run conversazionali 0-tool. Questa e'
    /// la parita' che il fix introduce per `engine='rust'` (era forzato a None ->
    /// fallback RouterNode true).
    #[test]
    fn initial_state_primary_classifier_read_only_action_false() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = true;
        input.requires_tools = Some(false);
        input.agentic_score = Some(0.10);
        input.authorizes_changes = Some(false);
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(
            state.action_oriented,
            Some(false),
            "primario Rust: turno read-only -> NON azione, niente G1 spurio"
        );
    }

    /// PRIMARIO RUST + classifier RISOLTO su turno d'azione (requires_tools=true):
    /// action_oriented=true FEDELE -> l'agente usa i tool, come il primario Python.
    #[test]
    fn initial_state_primary_classifier_azione_action_true() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = true;
        input.requires_tools = Some(true);
        input.agentic_score = Some(0.20);
        input.authorizes_changes = Some(true);
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(
            state.action_oriented,
            Some(true),
            "primario Rust: turno d'azione -> azione (fedele al primario Python)"
        );
    }

    /// PRIMARIO RUST + classifier RISOLTO con requires_tools assente ma agentic_score
    /// SOPRA soglia: action_oriented=true via soglia (porting __init__.py:699),
    /// identico allo shadow e al primario Python.
    #[test]
    fn initial_state_primary_classifier_score_sopra_soglia_action_true() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = true;
        input.requires_tools = None;
        input.agentic_score = Some(0.80);
        input.action_oriented_min_score = 0.5;
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(
            state.action_oriented,
            Some(true),
            "primario Rust: score 0.80 >= 0.5 -> azione"
        );
    }

    /// PRIMARIO RUST SENZA dati del classifier (caso fallback/degradato): lo stato
    /// resta None e il RouterNode reale decide come oggi. Comportamento INVARIATO
    /// rispetto a prima del fix (zero impatto quando il classifier non e' risolto).
    #[test]
    fn initial_state_primary_senza_classifier_resta_none() {
        let mut input = sample_input();
        input.intent_hint = None;
        input.classifier_resolved = false;
        input.requires_tools = None;
        input.agentic_score = None;
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(
            state.action_oriented, None,
            "primario Rust senza dati classifier: decide il RouterNode (invariato)"
        );
        assert_eq!(state.user_intent, None);
    }

    /// PRIMARIO RUST: con intent_hint operativo (disambiguazione risolta) ma SENZA
    /// dati del classifier, lo stato resta None -> il RouterNode applica il
    /// passthrough intent_hint (action_oriented=true) come oggi. Il primario non
    /// anticipa la derivazione quando il classifier non e' stato interrogato.
    #[test]
    fn initial_state_primary_intent_hint_senza_classifier_resta_none() {
        let input = sample_input(); // intent_hint="code_write", classifier_resolved=false
        let state = build_initial_state(&input, RunRole::Primary);
        assert_eq!(
            state.action_oriented, None,
            "primario Rust: senza dati classifier non deriva (RouterNode passthrough)"
        );
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
        let state = build_initial_state(&input, RunRole::Primary);
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

    // ── L'esito non si deduce da un contatore (regola M) ─────────────────────
    //
    // Il turno di GRAZIA lascia `final_gate_cycle = max_cycles` PROPRIO perche'
    // i criteri oggettivi sono tutti passati e manca solo la firma. Finche'
    // l'esito era dedotto da `final_gate_cycle > 0 && !plan_phase_active`, quel
    // residuo dichiarava "Verifica automatica fallita e non ripetuta" su un
    // lavoro RIUSCITO (visibile nella Chat 17: completion_grace -> task complete
    // ok -> failed_diagnosed).

    /// IL test del difetto: grazia riuscita -> NON declassare.
    /// Mutazione che lo rende rosso: ripristinare la derivazione dal contatore
    /// (`final_gate_cycle.unwrap_or(0) > 0 && !plan_phase_active`) -> con
    /// cycle=2 e plan_phase assente darebbe failed_pending=true.
    #[test]
    fn grazia_riuscita_non_e_una_verifica_fallita() {
        let state = AgentState {
            result: Some("Fatto".to_string()),
            stop_reason: Some(StopReason::EndTurn),
            // Cio' che scrive davvero il ramo di grazia: cycle == max_cycles.
            final_gate_cycle: Some(2),
            final_gate_verdict: Some(FinalGateVerdict::ObjectivePassedSignatureMissing),
            ..Default::default()
        };
        let out = map_outcome(StepOutcome::Completed(state));
        assert!(
            !out.final_gate_failed_pending,
            "la grazia ha i criteri OGGETTIVI tutti passati: non e' una bocciatura"
        );
    }

    /// Il vero positivo deve restare tale: bocciatura con ri-verifica attesa e
    /// mai avvenuta -> l'esito "fallita e non ripetuta" e' onesto.
    #[test]
    fn bocciatura_con_correzione_mai_riverificata_resta_fallita() {
        let state = AgentState {
            stop_reason: Some(StopReason::EndTurn),
            final_gate_cycle: Some(1),
            final_gate_verdict: Some(FinalGateVerdict::FailedPendingCorrection),
            ..Default::default()
        };
        let out = map_outcome(StepOutcome::Completed(state));
        assert!(
            out.final_gate_failed_pending,
            "questo e' il caso per cui il flag esiste: non va perso"
        );
    }

    /// In plan-phase il gate e' one-shot: nessuna ri-verifica era prevista,
    /// quindi la bocciatura non e' "pendente" (nessuna regressione sul caso
    /// gia' coperto prima dall'eccezione ad-hoc).
    #[test]
    fn bocciatura_in_plan_phase_non_e_pendente() {
        let state = AgentState {
            stop_reason: Some(StopReason::EndTurn),
            final_gate_cycle: Some(1),
            final_gate_verdict: Some(FinalGateVerdict::FailedPendingCorrection),
            plan_phase_active: Some(true),
            ..Default::default()
        };
        let out = map_outcome(StepOutcome::Completed(state));
        assert!(!out.final_gate_failed_pending);
    }

    /// Un ciclo residuo SENZA verdetto (gate mai entrato) non inventa un
    /// fallimento: e' il contatore a non avere piu' voce in capitolo.
    #[test]
    fn contatore_residuo_senza_verdetto_non_declassa() {
        let state = AgentState {
            stop_reason: Some(StopReason::EndTurn),
            final_gate_cycle: Some(2),
            final_gate_verdict: None,
            ..Default::default()
        };
        let out = map_outcome(StepOutcome::Completed(state));
        assert!(!out.final_gate_failed_pending);
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

    /// REGRESSIONE run a5db0985: run morto TRA il final_gate fallito (ciclo
    /// intermedio: `final_gate_cycle > 0`, correzione rimandata all'executor) e
    /// la ri-verifica (provider esauriti fino al cap iterazioni). Il segnale
    /// strutturato della bocciatura pendente deve arrivare al finalizzatore,
    /// non morire nello stato del grafo.
    #[test]
    fn map_outcome_final_gate_bocciato_in_sospeso() {
        // Fuori dalla plan-phase (a5db0985): bocciatura con ri-verifica attesa e
        // mai avvenuta -> pendente. Il caso e' lo stesso di prima; cambia il
        // segnale che lo porta: il VERDETTO del ramo, non il ciclo residuo.
        let state = AgentState {
            final_gate_cycle: Some(1),
            final_gate_verdict: Some(FinalGateVerdict::FailedPendingCorrection),
            ..Default::default()
        };
        assert!(map_outcome(StepOutcome::Completed(state)).final_gate_failed_pending);
        // Gate mai entrato (None): nessuna bocciatura in sospeso.
        let state = AgentState::default();
        assert!(!map_outcome(StepOutcome::Completed(state)).final_gate_failed_pending);
        // Gate CHIUSO (passed o forced azzerano il ciclo a 0): nessun sospeso.
        let state = AgentState {
            final_gate_cycle: Some(0),
            final_gate_passed: Some(true),
            final_gate_verdict: Some(FinalGateVerdict::Passed),
            ..Default::default()
        };
        assert!(!map_outcome(StepOutcome::Completed(state)).final_gate_failed_pending);
    }

    /// REGRESSIONE (review adversariale, falso positivo): in plan-phase il final
    /// gate e' raggiunto UNA volta da `route_after_todo_runner` e, dopo una
    /// bocciatura, il loop executor NON vi rientra mai (`final_gate_eligible`
    /// esclude plan_phase). Un `final_gate_cycle > 0` residuo e' quindi il
    /// normale esito di una chiusura LEGITTIMA e NON va declassato: il gating su
    /// `!plan_phase_active` deve sopprimere il segnale.
    #[test]
    fn map_outcome_final_gate_ciclo_residuo_in_plan_phase_non_pendente() {
        let state = AgentState {
            final_gate_cycle: Some(1),
            final_gate_verdict: Some(FinalGateVerdict::FailedPendingCorrection),
            plan_phase_active: Some(true),
            ..Default::default()
        };
        assert!(
            !map_outcome(StepOutcome::Completed(state)).final_gate_failed_pending,
            "in plan-phase il gate e' one-shot: nessuna ri-verifica era prevista"
        );
        // Contro-prova: stessa bocciatura, ma plan_phase disattiva -> pendente.
        let state = AgentState {
            final_gate_cycle: Some(1),
            final_gate_verdict: Some(FinalGateVerdict::FailedPendingCorrection),
            plan_phase_active: Some(false),
            ..Default::default()
        };
        assert!(map_outcome(StepOutcome::Completed(state)).final_gate_failed_pending);
    }

    /// REGRESSIONE (porting Python->Rust): `load_executor_config` aveva smesso di
    /// popolare `tool_choice_style` / `tool_choice_forcing_enabled` /
    /// `tool_choice_forcing_max_iteration`, lasciandoli ai `Default` del nodo
    /// (style=None, enabled=false) -> il force-action era INERTE per ogni provider.
    ///
    /// Questo test esercita la STESSA catena decisionale dell'executor
    /// (`provider_style_supports_forcing` + `should_force_tool_choice`) partendo da
    /// un `ExecutorConfig` popolato come fa ora `load_executor_config`: per un
    /// provider tool-capable con forcing ON e iteration <= max, la decisione deve
    /// dare `Some(true)` (force "required"). Con i campi NON popolati (il bug)
    /// darebbe `None`.
    #[test]
    fn force_tool_choice_attivo_con_executor_config_popolato() {
        use nexus_agent_graph::decisions::{
            provider_style_supports_forcing, should_force_tool_choice, turn_action_oriented,
        };

        // ExecutorConfig come lo costruisce load_executor_config DOPO il fix:
        // style risolto dal catalog (anthropic -> anthropic_any), forcing ON, max=2.
        let cfg = ExecutorConfig {
            tool_choice_style: crate::capability::default_style_for_provider("anthropic")
                .map(str::to_string),
            tool_choice_forcing_enabled: true,
            tool_choice_forcing_max_iteration: 2,
            ..ExecutorConfig::default()
        };

        // Riproduce la logica di executor.rs (~1592-1605) con tools disponibili,
        // turno d'azione, prima iterazione, non in discovery.
        let supports = provider_style_supports_forcing(cfg.tool_choice_style.as_deref());
        assert!(supports, "anthropic_any deve supportare il forcing");

        let force_now = should_force_tool_choice(
            true,                       // tools_available
            turn_action_oriented(None), // action_oriented (None -> true conservativo)
            1,                          // iteration <= max
            false,                      // in_discovery_phase
            supports,
            cfg.tool_choice_forcing_enabled,
            cfg.tool_choice_forcing_max_iteration,
        );
        let force_tc: Option<bool> = if force_now { Some(true) } else { None };
        assert_eq!(
            force_tc,
            Some(true),
            "config popolato + provider tool-capable + iter<=max -> force required"
        );
    }

    /// Controprova del bug: con `tool_choice_style=None` (i Default del nodo, ossia
    /// lo stato PRE-fix in cui load_executor_config non popolava il campo) la
    /// decisione resta `None` qualunque sia il resto -> force-action INERTE.
    #[test]
    fn force_tool_choice_inerte_con_style_none_regressione() {
        use nexus_agent_graph::decisions::{
            provider_style_supports_forcing, should_force_tool_choice, turn_action_oriented,
        };
        let cfg = ExecutorConfig {
            tool_choice_style: None, // stato del bug
            tool_choice_forcing_enabled: true,
            tool_choice_forcing_max_iteration: 2,
            ..ExecutorConfig::default()
        };
        let supports = provider_style_supports_forcing(cfg.tool_choice_style.as_deref());
        assert!(!supports, "style None -> nessun supporto al forcing");
        let force_now = should_force_tool_choice(
            true,
            turn_action_oriented(None),
            1,
            false,
            supports,
            cfg.tool_choice_forcing_enabled,
            cfg.tool_choice_forcing_max_iteration,
        );
        assert!(
            !force_now,
            "con style None il force-action e' inerte (il bug)"
        );
    }

    #[test]
    fn build_resume_delta_azzera_await_e_accoda_messaggio() {
        use nexus_graph::GraphState;
        // Stato sospeso su HITL con un messaggio pregresso.
        let mut state = AgentState {
            awaiting_confirmation: Some(true),
            messages: vec![Message::Human {
                content: nexus_agent_graph::state::MessageContent::text("richiesta iniziale"),
            }],
            ..Default::default()
        };
        // Applica il delta di resume (via reducer, punto unico).
        let delta = build_resume_delta("Azioni confermate dall'utente.", None);
        state.merge(delta);

        assert!(
            !state.is_awaiting_confirmation(),
            "il delta deve azzerare awaiting_confirmation (sblocca l'interrupt)"
        );
        // Il messaggio di approvazione e' ACCODATO (reducer append su messages).
        assert_eq!(
            state.messages.len(),
            2,
            "messaggio di approvazione accodato"
        );
        match state.messages.last() {
            Some(Message::Human { content }) => {
                let txt = serde_json::to_string(content).unwrap_or_default();
                assert!(txt.contains("Azioni confermate"));
            }
            other => panic!("atteso Human in coda, trovato {other:?}"),
        }
    }

    #[test]
    fn build_resume_delta_subagents_azzera_await_e_inietta_risultati() {
        use nexus_graph::GraphState;
        // Stato sospeso su fan-in (Fase D) con un messaggio pregresso.
        let mut state = AgentState {
            awaiting_subagents: Some(true),
            messages: vec![Message::Human {
                content: nexus_agent_graph::state::MessageContent::text("task iniziale"),
            }],
            ..Default::default()
        };
        // Risultati strutturati dei figli background (forma tool_result poll,
        // regola M: verdict/status, non prosa).
        let results = vec![
            serde_json::json!({
                "subagent_run_id": "aaa",
                "kind": "coder",
                "status": "completed",
                "summary": "fatto",
                "outcome": {"verdict": "completed_verified", "success": true},
            }),
            serde_json::json!({
                "subagent_run_id": "bbb",
                "kind": "review",
                "status": "completed",
                "summary": "ok",
                "outcome": {"verdict": "completed", "success": true},
            }),
        ];
        let delta = build_resume_delta_subagents(results.clone());
        state.merge(delta);

        assert!(
            !state.is_awaiting_subagents(),
            "il delta deve azzerare awaiting_subagents (sblocca l'interrupt fan-in)"
        );
        // I risultati sono OVERWRITE nel campo di stato (osservabilita').
        assert_eq!(
            state.subagent_results.as_ref().map(|r| r.len()),
            Some(2),
            "i risultati dei sub-run sono iniettati nel campo di stato"
        );
        // FIX B1 (resume non piu' inerte): il delta ACCODA un turno utente coi
        // risultati, cioe' il canale che l'executor legge davvero (state.messages,
        // reducer append). Senza, il padre riprenderebbe CIECO sugli esiti (l'executor
        // NON legge il campo subagent_results, letto solo dal todo_runner).
        assert_eq!(
            state.messages.len(),
            2,
            "messaggio pregresso + turno coi risultati fan-in accodato"
        );
        match state.messages.last() {
            Some(Message::Human { content }) => {
                let txt = serde_json::to_string(content).unwrap_or_default();
                assert!(
                    txt.contains("completed_verified") && txt.contains("bbb"),
                    "il turno iniettato deve portare gli esiti STRUTTURATI di entrambi i figli: {txt}"
                );
            }
            other => panic!("atteso Human coi risultati in coda, trovato {other:?}"),
        }
    }

    #[test]
    fn format_fanin_results_message_vuoto_non_e_cieco() {
        // Array vuoto -> nota esplicita (il modello riprende comunque non cieco),
        // nessun array JSON spurio.
        let msg = format_fanin_results_message(&[]);
        assert!(msg.contains("terminati"), "nota di completamento: {msg}");
        assert!(
            !msg.contains("array JSON"),
            "niente blocco JSON quando vuoto: {msg}"
        );
    }

    #[test]
    fn format_fanin_results_message_tronca_summary_enorme() {
        // Un summary oltre il cap e' troncato (evita context_overflow al resume);
        // status/outcome restano integri.
        let big = "x".repeat(FANIN_SUMMARY_CAP + 500);
        let results = vec![serde_json::json!({
            "subagent_run_id": "aaa",
            "status": "completed",
            "summary": big,
            "outcome": {"verdict": "completed_verified"},
        })];
        let msg = format_fanin_results_message(&results);
        assert!(
            msg.contains("[troncato]"),
            "summary enorme troncato: {}",
            &msg[..80.min(msg.len())]
        );
        assert!(msg.contains("completed_verified"), "outcome integro");
        // Il testo non deve contenere l'intero summary enorme non troncato.
        assert!(
            !msg.contains(&"x".repeat(FANIN_SUMMARY_CAP + 1)),
            "il summary oltre il cap non compare per intero"
        );
    }

    #[test]
    fn parse_automation_mode_canonical_only() {
        use nexus_agent_graph::state::AutomationMode as GraphMode;
        assert_eq!(
            parse_automation_mode("automatic"),
            Some(GraphMode::Automatic)
        );
        assert_eq!(parse_automation_mode("confirm"), Some(GraphMode::Confirm));
        assert_eq!(parse_automation_mode("study"), Some(GraphMode::None));
        assert!(parse_automation_mode("").is_none());
        assert!(parse_automation_mode("automatico").is_none());
        assert!(parse_automation_mode("continuo").is_none());
        assert!(parse_automation_mode("modalita-che-non-esiste-xyz").is_none());
    }

    #[tokio::test]
    async fn gateway_client_costruibile() {
        // Sanity: il client gateway e' costruibile (l'adapter lo avvolge senza I/O).
        let gw = NexusGatewayClient::new("http://127.0.0.1:1".to_string(), "tok".to_string());
        // Pool lazy: nessuna connessione al DB, ma la costruzione del pool sqlx
        // spawna un task di manutenzione -> serve il runtime tokio del test.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nexus@localhost:1/na")
            .expect("connect_lazy non fa I/O");
        let _adapter = GatewayLlmAdapter::new(gw, pool, String::new(), String::new());
    }

    // ── SHADOW (F4): proiezione canonica + persistenza telemetria ─────────────
    //
    // Il driver completo `run_shadow` esegue il grafo via `build_native_engine`,
    // che costruisce un `GatewayLlmAdapter` su `NexusGatewayClient` HTTP: non
    // scriptabile in unit test (l'E2E del grafo con gateway stub vive in
    // `nexus_agent_graph::graph`). Qui copriamo la parte SPECIFICA del driver che
    // quel test non tocca: la PROIEZIONE CANONICA (vocabolario comune Python/Rust)
    // e la PERSISTENZA del singolo record "__final_state__". Le garanzie di
    // zero-scrittura (Replay, MemoryCheckpointer, NullEventSink, gate http in
    // Replay) sono testate ognuna nel proprio modulo.

    use serde_json::json;

    #[test]
    fn canonical_stop_reason_mappa_vocabolario_comune() {
        // Python (status agent_runs) -> vocabolario comune.
        assert_eq!(canonical_stop_reason(Some("completed")), "end_turn");
        assert_eq!(
            canonical_stop_reason(Some("completed_verified")),
            "end_turn"
        );
        assert_eq!(
            canonical_stop_reason(Some("completed_unverified")),
            "end_turn"
        );
        assert_eq!(canonical_stop_reason(Some("failed")), "failed");
        assert_eq!(canonical_stop_reason(Some("failed_diagnosed")), "failed");
        assert_eq!(
            canonical_stop_reason(Some("awaiting_confirmation")),
            "interrupted"
        );
        // Rust (StopReason snake_case) -> stesso vocabolario.
        assert_eq!(canonical_stop_reason(Some("end_turn")), "end_turn");
        assert_eq!(canonical_stop_reason(Some("tool_use")), "tool_use");
        assert_eq!(canonical_stop_reason(Some("error")), "failed");
        assert_eq!(canonical_stop_reason(Some("loop_abort")), "loop");
        assert_eq!(canonical_stop_reason(Some("superseded")), "superseded");
        assert_eq!(canonical_stop_reason(Some("g1_escalated")), "g1");
        // None / ignoto.
        assert_eq!(canonical_stop_reason(None), "none");
        assert_eq!(canonical_stop_reason(Some("qualcosa-di-strano")), "other");
    }

    #[test]
    fn stop_reason_label_serializza_snake_case() {
        // L'enum Rust serializza nella forma snake_case poi canonicalizzata uguale
        // al primario (un solo vocabolario, regola L).
        let lbl = stop_reason_label(Some(StopReason::EndTurn));
        assert_eq!(lbl.as_deref(), Some("end_turn"));
        assert_eq!(canonical_stop_reason(lbl.as_deref()), "end_turn");
        assert!(stop_reason_label(None).is_none());
    }

    #[test]
    fn shadow_canonical_conta_tool_e_produced_work() {
        use nexus_agent_graph::state::{ContentBlock, MessageContent, ToolUse};

        let state = AgentState {
            stop_reason: Some(StopReason::EndTurn),
            messages: vec![
                Message::Human {
                    content: MessageContent::text("scrivi e leggi"),
                },
                // Forma OpenAI-compat: un read_file (non mutativo).
                Message::Ai {
                    content: MessageContent::text(""),
                    tool_calls: vec![ToolUse {
                        id: "t1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({}),
                        thought_signature: None,
                    }],
                    reasoning: None,
                    thinking_signature: None,
                },
                // Forma Anthropic: un edit_file inline (mutativo) -> produced_work.
                Message::Ai {
                    content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "t2".to_string(),
                        name: "edit_file".to_string(),
                        input: json!({}),
                        thought_signature: None,
                    }]),
                    tool_calls: vec![],
                    reasoning: None,
                    thinking_signature: None,
                },
            ],
            ..Default::default()
        };

        let c = shadow_canonical(&state, true);
        assert_eq!(c["completed"], json!(true));
        assert_eq!(c["stop_reason"], json!("end_turn"));
        assert_eq!(c["num_tool_calls"], json!(2), "read_file + edit_file");
        assert_eq!(c["has_produced_work"], json!(true), "edit_file e' mutativo");
    }

    #[test]
    fn shadow_canonical_solo_testo_nessun_lavoro() {
        let state = AgentState {
            stop_reason: Some(StopReason::EndTurn),
            messages: vec![Message::Ai {
                content: nexus_agent_graph::state::MessageContent::text("risposta testuale"),
                tool_calls: vec![],
                reasoning: None,
                thinking_signature: None,
            }],
            ..Default::default()
        };
        let c = shadow_canonical(&state, true);
        assert_eq!(c["num_tool_calls"], json!(0));
        assert_eq!(c["has_produced_work"], json!(false));
    }

    /// Tabelle minimali per `primary_canonical`: agent_runs (status) + agent_steps.
    async fn create_primary_tables(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID PRIMARY KEY, \
                 status TEXT NOT NULL DEFAULT 'running' \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_runs");
        sqlx::query(
            "CREATE TABLE agent_steps ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 run_id UUID NOT NULL, \
                 step_index INT NOT NULL, \
                 tool_name TEXT NOT NULL, \
                 tool_input JSONB NOT NULL DEFAULT '{}'::jsonb, \
                 tool_result TEXT, \
                 status TEXT NOT NULL DEFAULT 'completed' \
             )",
        )
        .execute(pool)
        .await
        .expect("create agent_steps");
    }

    async fn create_shadow_telemetry_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_shadow_telemetry ( \
                 id UUID PRIMARY KEY, \
                 run_id UUID NOT NULL, \
                 node_name TEXT NOT NULL, \
                 primary_output JSONB NOT NULL, \
                 shadow_output JSONB NOT NULL, \
                 divergent_keys TEXT[] NOT NULL DEFAULT '{}', \
                 created_at TIMESTAMPTZ NOT NULL DEFAULT now() \
             )",
        )
        .execute(pool)
        .await
        .expect("create nexus_shadow_telemetry");
    }

    #[sqlx::test]
    async fn primary_canonical_legge_status_e_step(pool: sqlx::PgPool) {
        create_primary_tables(&pool).await;
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (id, status) VALUES ($1, 'completed')")
            .bind(run)
            .execute(&pool)
            .await
            .expect("insert run");
        // Due step: un read_file (non mutativo) + un write_file (mutativo).
        for (i, name) in [(1000, "read_file"), (2000, "write_file")] {
            sqlx::query(
                "INSERT INTO agent_steps (id, run_id, step_index, tool_name) \
                 VALUES (gen_random_uuid(), $1, $2, $3)",
            )
            .bind(run)
            .bind(i)
            .bind(name)
            .execute(&pool)
            .await
            .expect("insert step");
        }

        let c = primary_canonical(&pool, run).await.expect("canonical");
        assert_eq!(c["completed"], json!(true));
        assert_eq!(c["stop_reason"], json!("end_turn"), "completed -> end_turn");
        assert_eq!(c["num_tool_calls"], json!(2));
        assert_eq!(c["has_produced_work"], json!(true), "write_file mutativo");
    }

    #[sqlx::test]
    async fn shadow_persiste_un_solo_record_final_state(pool: sqlx::PgPool) {
        // Replica la parte finale di run_shadow (dopo l'esecuzione del grafo): il
        // confronto canonico primario(DB)<->shadow(stato) e la persistenza del
        // SINGOLO record "__final_state__". Verifica che ci sia ESATTAMENTE una
        // riga, sul nodo "__final_state__", con i divergent_keys attesi.
        create_primary_tables(&pool).await;
        create_shadow_telemetry_table(&pool).await;
        let run = Uuid::new_v4();
        // Primario: completed, 0 step (nessun tool, nessun lavoro).
        sqlx::query("INSERT INTO agent_runs (id, status) VALUES ($1, 'completed')")
            .bind(run)
            .execute(&pool)
            .await
            .expect("insert run");

        let primary = primary_canonical(&pool, run).await.expect("primary");
        // Shadow: completato ma con un write_file (diverge su num_tool_calls +
        // has_produced_work rispetto al primario a 0 tool).
        let shadow_state = AgentState {
            stop_reason: Some(StopReason::EndTurn),
            messages: vec![Message::Ai {
                content: nexus_agent_graph::state::MessageContent::text(""),
                tool_calls: vec![nexus_agent_graph::state::ToolUse {
                    id: "t".to_string(),
                    name: "write_file".to_string(),
                    input: json!({}),
                    thought_signature: None,
                }],
                reasoning: None,
                thinking_signature: None,
            }],
            ..Default::default()
        };
        let shadow = shadow_canonical(&shadow_state, true);

        nexus_agent_graph::persist_node_diff(
            &pool,
            run,
            SHADOW_FINAL_STATE_NODE,
            &primary,
            &shadow,
        )
        .await
        .expect("persist telemetria shadow");

        // ESATTAMENTE un record, sul nodo __final_state__.
        let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
            "SELECT node_name, divergent_keys FROM nexus_shadow_telemetry WHERE run_id = $1",
        )
        .bind(run)
        .fetch_all(&pool)
        .await
        .expect("select telemetria");
        assert_eq!(rows.len(), 1, "un solo record per run shadow");
        assert_eq!(rows[0].0, "__final_state__");
        // Divergenze attese: num_tool_calls (0 vs 1) + has_produced_work (false vs true).
        let mut keys = rows[0].1.clone();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "has_produced_work".to_string(),
                "num_tool_calls".to_string()
            ]
        );
    }

    /// Tabella `settings` minimale (key, value) per i test DB-driven delle config.
    async fn create_settings_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE settings ( \
                 key   TEXT PRIMARY KEY, \
                 value TEXT \
             )",
        )
        .execute(pool)
        .await
        .expect("create settings");
    }

    async fn set_setting(pool: &sqlx::PgPool, key: &str, value: &str) {
        sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2)")
            .bind(key)
            .bind(value)
            .execute(pool)
            .await
            .expect("insert setting");
    }

    #[sqlx::test]
    async fn planner_config_db_driven_legge_orchestrator_settings(pool: sqlx::PgPool) {
        // DEBITO 2: con i setting orchestrator.* nel DB, load_planner_config deve
        // leggerli (regola G), non lasciare i safe-default. Replica i valori reali
        // di produzione (plan_phase_enabled=true abilita il planner).
        create_settings_table(&pool).await;
        set_setting(&pool, "orchestrator.plan_phase_enabled", "true").await;
        set_setting(
            &pool,
            "orchestrator.plan_behavior_modes",
            "bilanciata,approfondita",
        )
        .await;
        set_setting(&pool, "orchestrator.plan_intents", "code,fix,debug").await;
        set_setting(&pool, "orchestrator.plan_min_token_budget", "800").await;
        set_setting(&pool, "orchestrator.clarifying_questions_enabled", "false").await;
        set_setting(&pool, "orchestrator.dag_topological_enabled", "true").await;

        let cfg = load_planner_config(&pool).await;
        assert!(cfg.plan_phase_enabled, "letto true dal DB");
        assert_eq!(cfg.plan_behavior_modes, vec!["bilanciata", "approfondita"]);
        assert_eq!(cfg.plan_intents, vec!["code", "fix", "debug"]);
        assert_eq!(cfg.plan_min_token_budget, 800);
        assert!(
            !cfg.clarifying_questions_enabled,
            "false dal DB sovrascrive il default true"
        );
        assert!(cfg.dag_topological_enabled);

        // Con plan_phase_enabled=true e behavior_mode "bilanciata" (fonte primario,
        // DEBITO 1) in plan_behavior_modes + un intent in plan_intents + budget
        // sufficiente, il planner Rust e' eleggibile (accoppiamento debito 1+2).
        assert!(
            cfg.is_eligible(
                Some(crate::agent_turn_setup::PRIMARY_BEHAVIOR_MODE),
                Some("code"),
                1000
            ),
            "planner eleggibile col behavior_mode del primario + intent + budget"
        );
    }

    #[sqlx::test]
    async fn load_executor_config_aggancia_context_window_e_compress_settings(pool: sqlx::PgPool) {
        // REGRESSIONE (gap porting context): context_window finiva SOLO nel
        // ToolDispatchConfig, mai nell'ExecutorConfig -> restava 0 -> token_brake /
        // forced_rag / smart-upscale INERTI (gate `if context_window > 0`). Inoltre i
        // settings agent.context.* (mig 0429, valori aggressivi) erano IGNORATI perche'
        // load_executor_config chiudeva con `..Default::default()`. Qui verifichiamo il
        // wiring DB-driven (regola G): context_window agganciato + ctx_mgmt/token_brake
        // dal DB; + degrado in blocco al default per un CSV malformato (max_chars).
        create_settings_table(&pool).await;
        set_setting(&pool, "agent.context.compress_start_iter", "3").await;
        set_setting(
            &pool,
            "agent.context.compress_phase_boundaries",
            "3,7,15,30",
        )
        .await;
        set_setting(&pool, "agent.context.compress_phase_keep_recent", "5,3,2,1").await;
        // CSV malformato di proposito: deve degradare IN BLOCCO al default safe.
        set_setting(
            &pool,
            "agent.context.compress_phase_max_chars",
            "1200,xxx,300,100",
        )
        .await;
        set_setting(&pool, "agent.context.max_context_ratio", "0.55").await;
        set_setting(&pool, "agent.context.forced_rag_threshold_ratio", "0.30").await;

        let cfg = load_executor_config(&pool, "anthropic", "claude-x", 200_000).await;
        // context_window agganciato (era il bug: restava 0).
        assert_eq!(cfg.context_window, 200_000);
        // ctx_mgmt dal DB (aggressivo), non il default permissivo [5,10,20,50].
        assert_eq!(cfg.ctx_mgmt.compress_start_iter, 3);
        assert_eq!(cfg.ctx_mgmt.compress_phase_boundaries, vec![3, 7, 15, 30]);
        assert_eq!(cfg.ctx_mgmt.compress_phase_keep_recent, vec![5, 3, 2, 1]);
        // CSV malformato -> degrado IN BLOCCO al default del nodo (non lista parziale).
        assert_eq!(
            cfg.ctx_mgmt.compress_phase_max_chars,
            vec![2000, 1000, 500, 150]
        );
        // token_brake dal DB (0.55 < 0.70 default).
        assert!((cfg.token_brake.max_context_ratio - 0.55).abs() < 1e-9);
        assert!((cfg.forced_rag_ratio - 0.30).abs() < 1e-9);
    }

    /// FIX-A (scale-controller): il tier del modello iniziale e' letto dal catalog e
    /// scritto in `current_tier` al checkpoint iniziale (routing iniziale).
    #[sqlx::test]
    async fn resolve_initial_tier_dal_catalog(pool: sqlx::PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, performance_tier) \
             VALUES ('anthropic', 'claude-heavy', 'heavy')",
        )
        .execute(&pool)
        .await
        .expect("insert catalog");
        assert_eq!(
            resolve_initial_tier(&pool, "anthropic", "claude-heavy").await,
            "heavy",
            "tier del modello iniziale letto dalla colonna performance_tier"
        );
    }

    /// FIX-A: modello iniziale NON nel catalog -> fallback deterministico `medium`
    /// (default della colonna `performance_tier`, mig 0032; NON un magic value).
    #[sqlx::test]
    async fn resolve_initial_tier_fallback_medium_se_ignoto(pool: sqlx::PgPool) {
        crate::test_support::create_ai_price_catalog_table(&pool).await;
        assert_eq!(
            resolve_initial_tier(&pool, "ignoto", "modello-fantasma").await,
            "medium",
            "modello non in catalog -> fallback medium (default catalog)"
        );
    }

    #[sqlx::test]
    async fn load_executor_config_hard_cap_e_template_overflow(pool: sqlx::PgPool) {
        // ADR 0016 D2: hard_cap_ratio dal DB + template overflow risolto da
        // nexus_prompt_templates via la chiave in agent.context.overflow_message_key.
        create_settings_table(&pool).await;
        set_setting(&pool, "agent.context.hard_cap_ratio", "0.95").await;
        sqlx::query(
            "CREATE TABLE nexus_prompt_templates ( \
                 key TEXT PRIMARY KEY, \
                 content TEXT NOT NULL, \
                 is_active BOOLEAN NOT NULL DEFAULT TRUE \
             )",
        )
        .execute(&pool)
        .await
        .expect("create nexus_prompt_templates");
        sqlx::query(
            "INSERT INTO nexus_prompt_templates (key, content, is_active) VALUES \
             ('system.context_overflow', 'Stima %ESTIMATED_TOKENS% su %MAX_WINDOW%.', TRUE)",
        )
        .execute(&pool)
        .await
        .expect("insert template");

        let cfg = load_executor_config(&pool, "anthropic", "claude-x", 200_000).await;
        assert!((cfg.hard_cap_ratio - 0.95).abs() < 1e-9, "ratio dal DB");
        assert_eq!(
            cfg.overflow_message_template,
            "Stima %ESTIMATED_TOKENS% su %MAX_WINDOW%."
        );
    }

    #[sqlx::test]
    async fn tokenizer_kind_cl100k_solo_con_setting_esplicito(pool: sqlx::PgPool) {
        // ADR 0016 D1: solo il valore 'cl100k_base' attiva la BPE reale;
        // chiave assente o valore diverso -> char-based (safe-DB-down).
        create_settings_table(&pool).await;
        assert_eq!(resolve_tokenizer_kind(&pool).await, TokenizerKind::Chars);
        set_setting(&pool, "agent.context.tokenizer", "cl100k_base").await;
        // Scrittura diretta: lettura cache-ata, il test invalida esplicitamente.
        nexus_auth::invalidate_setting_cache(&pool, "agent.context.tokenizer");
        assert_eq!(resolve_tokenizer_kind(&pool).await, TokenizerKind::Cl100k);
        sqlx::query("UPDATE settings SET value = 'chars' WHERE key = 'agent.context.tokenizer'")
            .execute(&pool)
            .await
            .expect("update setting");
        nexus_auth::invalidate_setting_cache(&pool, "agent.context.tokenizer");
        assert_eq!(resolve_tokenizer_kind(&pool).await, TokenizerKind::Chars);
    }

    #[sqlx::test]
    async fn load_executor_config_hard_cap_safe_default_senza_template(pool: sqlx::PgPool) {
        // Rovescio safe-DB-down: chiave assente e tabella template inesistente ->
        // ratio 0.0 (gate OFF) e template vuoto, nessun panico (regola G).
        create_settings_table(&pool).await;
        let cfg = load_executor_config(&pool, "anthropic", "claude-x", 200_000).await;
        assert_eq!(cfg.hard_cap_ratio, 0.0, "gate OFF a chiave assente");
        assert!(cfg.overflow_message_template.is_empty());
    }

    #[sqlx::test]
    async fn config_db_driven_safe_default_se_chiave_assente(pool: sqlx::PgPool) {
        // DEBITO 2 (rovescio): tabella settings VUOTA -> ogni config cade sul proprio
        // safe-default (identico ai _SAFE_DEFAULTS del brain), non panica, non
        // inventa valori. In particolare plan_phase_enabled=false (planner OFF).
        create_settings_table(&pool).await;

        let planner = load_planner_config(&pool).await;
        assert!(
            !planner.plan_phase_enabled,
            "safe-default: planner OFF se chiave assente"
        );
        assert_eq!(
            planner.plan_min_token_budget,
            PlannerConfig::default().plan_min_token_budget
        );

        let verifier = load_verifier_config(&pool).await;
        assert!(
            !verifier.enabled,
            "safe-default: verifier OFF se chiave assente"
        );
        assert_eq!(
            verifier.max_verify_cycles,
            VerifierConfig::default().max_verify_cycles
        );

        let final_gate = load_final_gate_config(&pool).await;
        assert_eq!(final_gate.enabled, FinalGateConfig::default().enabled);
        assert_eq!(final_gate.max_cycles, FinalGateConfig::default().max_cycles);
    }

    #[sqlx::test]
    async fn verifier_config_db_driven_legge_settings(pool: sqlx::PgPool) {
        // DEBITO 2: verifier_enabled + max_verify_cycles + fail_closed dal DB.
        create_settings_table(&pool).await;
        set_setting(&pool, "orchestrator.verifier_enabled", "true").await;
        set_setting(&pool, "orchestrator.max_verify_cycles", "5").await;
        set_setting(&pool, "agent.verifier.fail_closed", "false").await;

        let cfg = load_verifier_config(&pool).await;
        assert!(cfg.enabled, "verifier_enabled=true dal DB");
        assert_eq!(cfg.max_verify_cycles, 5);
        assert!(
            !cfg.fail_closed,
            "fail_closed=false dal DB sovrascrive il default true"
        );
    }

    #[sqlx::test]
    async fn final_gate_config_db_driven_legge_settings(pool: sqlx::PgPool) {
        // DEBITO 2: campi del final_gate dal DB (prefisso agent.final_gate.* +
        // no_orphan + import_staging + criteria_timeout da verifier_timeout_s).
        create_settings_table(&pool).await;
        set_setting(&pool, "agent.final_gate.enabled", "false").await;
        set_setting(&pool, "agent.final_gate.max_cycles", "4").await;
        set_setting(&pool, "agent.final_gate.runtime_check_enabled", "false").await;
        set_setting(&pool, "agent.no_orphan.min_ratio", "0.7").await;
        set_setting(&pool, "agent.import_staging_dirs", "figma_export,staging").await;
        set_setting(&pool, "orchestrator.verifier_timeout_s", "45.0").await;
        set_setting(
            &pool,
            "agent.final_gate.runtime_error_patterns",
            "ECONNREFUSED,Traceback",
        )
        .await;
        let cfg = load_final_gate_config(&pool).await;
        assert!(!cfg.enabled, "enabled=false dal DB");
        assert_eq!(cfg.max_cycles, 4);
        assert!(!cfg.runtime_check_enabled);
        assert!((cfg.no_orphan_min_ratio - 0.7).abs() < f64::EPSILON);
        assert_eq!(cfg.import_staging_dirs, vec!["figma_export", "staging"]);
        assert!((cfg.criteria_timeout_s - 45.0).abs() < f64::EPSILON);
        assert_eq!(
            cfg.runtime_error_patterns,
            vec!["ECONNREFUSED", "Traceback"]
        );
        // log_command / endpoint_criterion sono risolti per-progetto a monte:
        // restano vuoto/None dal loader. La catena comandi (ADR 0036) NON e'
        // del loader: verify_steps/verify_profile_missing sono innestati da
        // run_engine col profilo per-ambiente (nessun build_command generico,
        // mig 0508).
        assert!(cfg.log_command.is_empty());
        assert!(cfg.endpoint_criterion.is_none());
        assert!(cfg.verify_steps.is_empty(), "loader non popola la catena");
        assert!(!cfg.verify_profile_missing);
    }

    // ── classify_status / structured_verdict (punto unico esito, regola L/M) ────

    use crate::agent_types::AgentRunStatus;

    /// Esito "completato pulito": tutti i segnali neutri. Override per-test dei
    /// soli campi rilevanti (gli altri non entrano nella classificazione).
    fn base_outcome() -> NativeRunOutcome {
        NativeRunOutcome {
            completed: true,
            awaiting_subagents: false,
            final_answer: Some("fatto".to_string()),
            stop_reason: None,
            provider_used: None,
            model_used: None,
            resume_at: None,
            iterations: 1,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost: 0.0,
            user_intent: None,
            reasoning: None,
            messages_json: None,
            declared_outcome: None,
            review_verdict: None,
            advisory_verdict: None,
            debate_position: None,
            error_class: None,
            forced_close_unverified: false,
            final_gate_passed: None,
            final_gate_unverified: None,
            final_gate_failed_pending: false,
            review_panel_rejected: false,
            review_panel_last: None,
            pending_actions: Vec::new(),
        }
    }

    #[test]
    fn classify_status_completed_pulito() {
        assert_eq!(base_outcome().classify_status(), AgentRunStatus::Completed);
    }

    /// Un run bocciato dalla review adversariale programmatica non e' un
    /// successo: il verdetto strutturato del panel prevale sul "done" ottimista
    /// del modello (run reale 20/07 10:03:30: Fail 2/2, restava `completed` con
    /// i difetti bloccanti sepolti in una nota del resoconto).
    #[test]
    fn classify_status_review_bocciata_non_e_completed() {
        let o = NativeRunOutcome {
            review_panel_rejected: true,
            ..base_outcome()
        };
        // Mutazione che rende rosso: togliere il ramo `review_panel_rejected`
        // da classify_status -> torna Completed, cioe' il difetto originale.
        assert_eq!(o.classify_status(), AgentRunStatus::FailedDiagnosed);
    }

    #[test]
    fn classify_status_awaiting_confirmation() {
        let o = NativeRunOutcome {
            completed: false,
            resume_at: Some(NodeId::Executor.as_label().to_string()),
            ..base_outcome()
        };
        assert_eq!(o.classify_status(), AgentRunStatus::AwaitingConfirmation);
    }

    #[test]
    fn classify_status_awaiting_subagents_fan_in() {
        // Fase D fan-in: stesso ramo INTERROTTO (completed=false, resume_at
        // valorizzato) di AwaitingConfirmation, ma il segnale strutturato
        // `awaiting_subagents` (regola M) discrimina l'interrupt fan-in.
        let fanin = NativeRunOutcome {
            completed: false,
            awaiting_subagents: true,
            resume_at: Some(NodeId::ToolDispatch.as_label().to_string()),
            ..base_outcome()
        };
        assert_eq!(fanin.classify_status(), AgentRunStatus::AwaitingSubagents);
        // Senza il flag, lo stesso interrupt resta HITL (parita' regressione).
        let hitl = NativeRunOutcome {
            completed: false,
            awaiting_subagents: false,
            resume_at: Some(NodeId::ToolDispatch.as_label().to_string()),
            ..base_outcome()
        };
        assert_eq!(hitl.classify_status(), AgentRunStatus::AwaitingConfirmation);
    }

    #[test]
    fn classify_status_error_infrastrutturale() {
        let o = NativeRunOutcome {
            stop_reason: Some(StopReason::Error),
            ..base_outcome()
        };
        assert_eq!(o.classify_status(), AgentRunStatus::Failed);
    }

    #[test]
    fn classify_status_dichiarazione_blocked_e_refusal() {
        let blocked = NativeRunOutcome {
            declared_outcome: Some(serde_json::json!({"outcome": "blocked"})),
            ..base_outcome()
        };
        assert_eq!(blocked.classify_status(), AgentRunStatus::BlockedNeedsInput);
        let refusal = NativeRunOutcome {
            declared_outcome: Some(serde_json::json!({"outcome": "done", "refusal": true})),
            ..base_outcome()
        };
        assert_eq!(refusal.classify_status(), AgentRunStatus::BlockedNeedsInput);
    }

    #[test]
    fn classify_status_dichiarazione_partial() {
        let o = NativeRunOutcome {
            declared_outcome: Some(serde_json::json!({"outcome": "partial"})),
            ..base_outcome()
        };
        assert_eq!(o.classify_status(), AgentRunStatus::FailedDiagnosed);
    }

    #[test]
    fn classify_status_forced_close_e_gate() {
        // Abort anti-loop (segnale autoritativo).
        let forced = NativeRunOutcome {
            forced_close_unverified: true,
            ..base_outcome()
        };
        assert_eq!(forced.classify_status(), AgentRunStatus::FailedDiagnosed);
        // Verifica oggettiva non superata.
        let gate_ko = NativeRunOutcome {
            final_gate_passed: Some(false),
            ..base_outcome()
        };
        assert_eq!(gate_ko.classify_status(), AgentRunStatus::FailedDiagnosed);
        // Bocciatura del gate in sospeso (morto prima della ri-verifica).
        let pending = NativeRunOutcome {
            final_gate_failed_pending: true,
            ..base_outcome()
        };
        assert_eq!(pending.classify_status(), AgentRunStatus::FailedDiagnosed);
    }

    #[test]
    fn classify_status_completato_ma_non_verificato() {
        let o = NativeRunOutcome {
            final_gate_unverified: Some(true),
            ..base_outcome()
        };
        assert_eq!(o.classify_status(), AgentRunStatus::CompletedUnverified);
        // CompletedUnverified E' un successo (lavoro svolto, verifica non eseguita).
        assert!(o.classify_status().is_success());
    }

    #[test]
    fn classify_status_dichiarazione_onesta_precede_il_gate() {
        // Il modello dichiara blocked E il gate oggettivo e' fallito: la
        // dichiarazione onesta (piu' specifica) ha precedenza sul verdetto generico.
        let o = NativeRunOutcome {
            declared_outcome: Some(serde_json::json!({"outcome": "blocked"})),
            final_gate_passed: Some(false),
            ..base_outcome()
        };
        assert_eq!(o.classify_status(), AgentRunStatus::BlockedNeedsInput);
    }

    #[test]
    fn structured_verdict_forma_e_success() {
        // Successo pulito: verdict canonico, success=true, campi gate neutri.
        let v = base_outcome().structured_verdict();
        assert_eq!(v["verdict"], serde_json::json!("completed"));
        assert_eq!(v["success"], serde_json::json!(true));
        assert_eq!(v["declared"], serde_json::Value::Null);
        assert_eq!(v["review"], serde_json::Value::Null);
        assert_eq!(v["advisory"], serde_json::Value::Null);
        assert_eq!(v["final_gate_passed"], serde_json::Value::Null);

        // Fase B: il verdetto del REVISORE attraversa il confine dentro il
        // blocco (campo `review`), indipendente dallo status lifecycle.
        let reviewer = NativeRunOutcome {
            review_verdict: Some(serde_json::json!({
                "verdict": "needs_changes",
                "summary": "un fix richiesto",
                "findings": [{"file": "a.rs", "severity": "media", "description": "bug"}]
            })),
            ..base_outcome()
        };
        let vr = reviewer.structured_verdict();
        assert_eq!(vr["review"]["verdict"], serde_json::json!("needs_changes"));
        assert_eq!(
            vr["review"]["findings"][0]["file"],
            serde_json::json!("a.rs")
        );

        // Consiglio a monte: il parere della FIGURA attraversa il confine dentro il
        // blocco (campo `advisory`), indipendente dallo status lifecycle.
        let figure = NativeRunOutcome {
            advisory_verdict: Some(serde_json::json!({
                "verdict": "block",
                "summary": "manca PKCE",
                "risks": [{"severity": "alta", "description": "auth senza PKCE"}]
            })),
            ..base_outcome()
        };
        let vf = figure.structured_verdict();
        assert_eq!(vf["advisory"]["verdict"], serde_json::json!("block"));
        assert_eq!(
            vf["advisory"]["risks"][0]["severity"],
            serde_json::json!("alta")
        );

        // Gate fallito + dichiarazione "done" ottimista: il verdetto oggettivo
        // prevale (failed_diagnosed) e l'error_class strutturata viaggia col blocco.
        let ko = NativeRunOutcome {
            final_gate_passed: Some(false),
            declared_outcome: Some(serde_json::json!({"outcome": "done", "summary": "fatto"})),
            error_class: Some("context_overflow".to_string()),
            ..base_outcome()
        };
        let vk = ko.structured_verdict();
        assert_eq!(vk["verdict"], serde_json::json!("failed_diagnosed"));
        assert_eq!(vk["success"], serde_json::json!(false));
        assert_eq!(vk["final_gate_passed"], serde_json::json!(false));
        assert_eq!(vk["error_class"], serde_json::json!("context_overflow"));
        assert_eq!(vk["declared"]["outcome"], serde_json::json!("done"));
    }

    /// Esecutore che CONTA i comandi eseguiti: il numero di `run_command` in
    /// arrivo e' la misura dello spreco (un typecheck reale dura secondi).
    struct ContaComandi {
        eseguiti: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for ContaComandi {
        async fn execute(
            &self,
            call: nexus_agent_graph::runtime::ports::ToolCall,
            _mode: nexus_agent_graph::runtime::ports::ExecMode,
        ) -> Result<
            nexus_agent_graph::runtime::ports::ToolOutcome,
            nexus_agent_graph::runtime::ports::PortError,
        > {
            self.eseguiti
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(nexus_agent_graph::runtime::ports::ToolOutcome {
                tool_call_id: call.id,
                // `measure_command_exit` estrae l'exit dal marker del testo.
                content: serde_json::json!("tutto verde\nEXIT CODE: 0"),
                exit_code: Some(0),
                ..Default::default()
            })
        }
    }

    /// Le N figure del consiglio entrano insieme sulla STESSA root con la stessa
    /// copia del profilo, letta PRIMA di mettersi in coda sul guard dell'albero.
    /// La misura deve essere fatta UNA volta e CONDIVISA: e' il senso del doppio
    /// controllo dopo il guard. Senza la ri-lettura il guard non condivide il
    /// lavoro, lo mette in fila — e questo test conta 6 typecheck invece di 1
    /// (difetto D2, meta' spreco, persa nel refactor e nel merge 9dd68341).
    #[sqlx::test]
    async fn misure_gate_condivise_fra_figure_concorrenti(pool: PgPool) {
        // Come gli altri test del crate: la tabella che serve, senza la FK su
        // `projects` (qui il progetto non e' il soggetto della prova).
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS project_verify_profiles (
                project_id    UUID PRIMARY KEY,
                steps         JSONB NOT NULL DEFAULT '[]'::jsonb,
                environment   JSONB NOT NULL DEFAULT '{}'::jsonb,
                manifest_hash TEXT NOT NULL DEFAULT '',
                source        TEXT NOT NULL DEFAULT 'llm',
                updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
             )",
        )
        .execute(&pool)
        .await
        .expect("tabella profili");

        let pid = Uuid::new_v4();
        // UNO step gate con la baseline da misurare e il probe GIA' fatto: cosi'
        // un giro di misure costa ESATTAMENTE un'esecuzione e il conteggio non
        // dipende dal probe (che pianterebbe un file vero nell'albero).
        let profilo = vec![crate::verify_profile::VerifyProfileStep {
            step: "typecheck".to_string(),
            command: "pnpm typecheck".to_string(),
            working_dir: None,
            timeout_s: None,
            gate: true,
            rationale: None,
            baseline_exit_code: None,
            probe: Some(crate::verify_probe::ProbeOutcome::Discriminating),
        }];
        sqlx::query(
            "INSERT INTO project_verify_profiles (project_id, steps, manifest_hash) \
             VALUES ($1, $2, 'h')",
        )
        .bind(pid)
        .bind(serde_json::to_value(&profilo).expect("json"))
        .execute(&pool)
        .await
        .expect("profilo persistito");

        let eseguiti = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = Arc::new(FinalGateCriteriaRunnerAdapter::new(
            Arc::new(ContaComandi {
                eseguiti: eseguiti.clone(),
            }),
            pool.clone(),
        ));
        // Root UNICA per questo test: la chiave di TREE_LOCKS e' globale al
        // processo e i test girano in parallelo (nessun albero viene toccato:
        // il probe e' gia' risolto).
        let root = format!("D:/test-albero/{pid}");

        let mut figure = Vec::new();
        for _ in 0..6 {
            let (pool, adapter, root) = (pool.clone(), adapter.clone(), root.clone());
            // Ogni figura ha la PROPRIA copia, letta prima della coda: e' la
            // copia stantia che i filtri `is_none()` userebbero per rifare tutto.
            let mut steps = profilo.clone();
            figure.push(tokio::spawn(async move {
                measure_gate_steps(&pool, pid, &root, &mut steps, &adapter).await;
                steps
            }));
        }
        let mut esiti = Vec::new();
        for f in figure {
            esiti.push(f.await.expect("figura"));
        }

        assert_eq!(
            eseguiti.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "il comando gate si misura UNA volta per albero, non una per figura \
             (D2: erano 6 typecheck in fila)"
        );
        for steps in &esiti {
            assert_eq!(
                steps[0].baseline_exit_code,
                Some(0),
                "la misura e' CONDIVISA: chi ha atteso il guard esce con la \
                 baseline letta dal persistito, non senza"
            );
        }
    }

    /// Il kill-switch `agent.verify_infer.enabled` OFF si presenta qui come
    /// `steps` vuoto: la ri-lettura dopo il guard non deve ripescare dal DB il
    /// profilo che l'interruttore ha appena escluso.
    #[sqlx::test]
    async fn profilo_escluso_dal_killswitch_non_torna_dalla_rilettura(pool: PgPool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS project_verify_profiles (
                project_id    UUID PRIMARY KEY,
                steps         JSONB NOT NULL DEFAULT '[]'::jsonb,
                environment   JSONB NOT NULL DEFAULT '{}'::jsonb,
                manifest_hash TEXT NOT NULL DEFAULT '',
                source        TEXT NOT NULL DEFAULT 'llm',
                updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
             )",
        )
        .execute(&pool)
        .await
        .expect("tabella profili");

        let pid = Uuid::new_v4();
        let persistito = vec![crate::verify_profile::VerifyProfileStep {
            step: "typecheck".to_string(),
            command: "pnpm typecheck".to_string(),
            working_dir: None,
            timeout_s: None,
            gate: true,
            rationale: None,
            baseline_exit_code: None,
            probe: Some(crate::verify_probe::ProbeOutcome::Discriminating),
        }];
        sqlx::query(
            "INSERT INTO project_verify_profiles (project_id, steps, manifest_hash) \
             VALUES ($1, $2, 'h')",
        )
        .bind(pid)
        .bind(serde_json::to_value(&persistito).expect("json"))
        .execute(&pool)
        .await
        .expect("profilo persistito");

        let eseguiti = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let adapter = FinalGateCriteriaRunnerAdapter::new(
            Arc::new(ContaComandi {
                eseguiti: eseguiti.clone(),
            }),
            pool.clone(),
        );
        let mut steps: Vec<crate::verify_profile::VerifyProfileStep> = Vec::new();
        measure_gate_steps(
            &pool,
            pid,
            &format!("D:/test-albero/{pid}"),
            &mut steps,
            &adapter,
        )
        .await;

        assert!(
            steps.is_empty(),
            "kill-switch OFF: nessuno step rientra dalla ri-lettura"
        );
        assert_eq!(
            eseguiti.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "kill-switch OFF: nessun comando eseguito"
        );
    }

    /// Il caso REALE che rende utile la precedenza: `agent.run_time_budget_s` e' `0`
    /// per policy (mig 0604/0607), quindi per una FIGURA il valore dal DB e'
    /// inservibile. Se vincesse il DB, il gate a tempo dell'executor resterebbe
    /// codice morto e la figura morirebbe muta allo scadere, senza mai ricevere il
    /// sollecito di chiusura. Mutazione: far vincere `from_db` -> questo assert
    /// rosseggia con `0`, cioe' col valore esatto del difetto.
    #[test]
    fn budget_della_figura_vince_sul_setting_globale_a_zero() {
        assert_eq!(effective_run_time_budget_s(0, Some(300)), 300);
    }

    /// Run principale (nessun override): comanda il setting globale, comportamento
    /// invariato.
    #[test]
    fn senza_override_vale_il_setting_globale() {
        assert_eq!(effective_run_time_budget_s(900, None), 900);
        assert_eq!(effective_run_time_budget_s(0, None), 0);
    }
}
