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
//! 3. SSE: i nodi emettono via `ctx.emit` solo `MetaStep`; mancano
//!    `AssistantDelta`/`ToolUse`/`ToolResult` e il terminatore `Done`, + la
//!    finalizzazione di `agent_runs` e la gestione hollow nel call site.
//! 4. CHIUSO. Il ramo `engine == rust` del call site esce dal `'compute` con
//!    `break 'compute` su `Ok` (niente doppio-run); su `Err` finalizza FAILED
//!    diagnosticato (`native_engine_failure_result`), NIENTE fallback automatico
//!    al brain (regola H, verso zero-Python).
//! 6. HITL (interrupt-resume): il MOTORE gestisce gia' l'interrupt
//!    `awaiting_confirmation` (`run_until_interrupt` -> `Interrupted`) e il RESUME
//!    dal checkpoint (`resume_until_interrupt`, cablato in `resume_native` +
//!    `confirm_native_run`). RESTA da portare il NODO che IMPOSTA
//!    `awaiting_confirmation` (l'`interrupt_before=["executor"]` di graph.py):
//!    finche' nessun nodo nativo valorizza il flag, un run `engine='rust'` non
//!    raggiunge l'HITL e il resume nativo non viene ancora esercitato. Il resume
//!    dei run PYTHON storici/rollback resta sul brain (`resume_run`).
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
//! ## Stato (PRIMARIO, instradato globalmente)
//!
//! `select_engine` ritorna `rust` sulla riga jolly `*`=rust (regola G, tabella
//! `nexus_orchestrator_engine`): il motore nativo Rust e' il PRIMARIO in
//! produzione e questo path e' quello effettivamente eseguito per i nuovi run.
//! `Engine::Python` resta solo come default difensivo (riga DB assente / DB down /
//! valore non riconosciuto), rollback per-sessione e valore storico dei run
//! `agent_runs.engine='python'`. L'instradamento e' un dato nel DB, non codice.

use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use nexus_agent_graph::nodes::{
    ExecutorConfig, ExecutorNode, VerifierConfig, VerifierNode,
};
use nexus_agent_graph::decisions::context_reduction::{CtxMgmtConfig, TokenBrakeConfig};
use nexus_agent_graph::runtime::ports::{
    AgentStepStore, BillingCooldownPort, ContextOffload, CriteriaRunner, EscalationPort, EventSink,
    LlmGateway, MetaStepStore, ModelUpscalePort, NextActionsDeriver, RunControlStore, SummaryStore,
    TodoStore, ToolExecutor, VerifierRunStore,
};
use nexus_agent_graph::runtime::NullEventSink;
use nexus_agent_graph::{
    build_agent_graph, AgentGraphEngine, AgentGraphNodes, AgentNodeCtx, AgentState, ClarifyConfig,
    ClarifyOrExpandNode, FinalGateConfig, FinalGateNode, LearnerConfig, LearnerNode, Message,
    PlannerConfig, PlannerNode, ReflectionConfig, ReflectionNode, RouterNode, StopReason,
    TodoRunnerConfig, TodoRunnerNode, ToolDispatchConfig, ToolDispatchNode, UnderstandingConfig,
    UnderstandingNode,
};
use nexus_graph::outcome::StepOutcome;

use crate::agent_graph_adapter::{
    agent_step_store::PgAgentStepStore,
    billing_cooldown_port::{CooldownBillingPort, NullBillingCooldownPort},
    context_offload::RagContextOffloadAdapter, criteria_runner::FinalGateCriteriaRunnerAdapter,
    escalation_port::PgEscalationPort, event_sink::SseEventSinkAdapter,
    llm_gateway::{GatewayLlmAdapter, ReplayLlmGateway},
    meta_step_store::PgMetaStepStore, model_upscale_port::CatalogModelUpscalePort,
    next_actions_deriver::NextActionsDeriverAdapter, run_control_store::PgRunControlStore,
    summary_store::PgSummaryStore, todo_store::PgTodoStore,
    tool_executor::ToolRunnerExecutorAdapter, verifier_run_store::PgVerifierRunStore,
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
/// `spawn_agent_run`: prima ciascuno re-implementava questa sequenza. Senza
/// gateway o su fallback del classifier i campi del giudizio restano neutri
/// (`None`/`false`); la soglia DB e' comunque risolta (fallback al default
/// tecnico `DEFAULT_ACTION_ORIENTED_MIN_SCORE` se la chiave manca). Indipendente
/// dal flag `routing.classifier_engine`: usa SEMPRE il classifier rust (sia per
/// lo shadow-replay sia per il primario nativo, che e' il motore Rust stesso).
pub(crate) async fn resolve_classifier_fields(
    db: &PgPool,
    gateway: Option<&NexusGatewayClient>,
    classifier_input: &str,
) -> ClassifierFields {
    let (requires_tools, agentic_score, authorizes_changes, classifier_resolved) = match gateway {
        Some(gw) => {
            let ai = crate::intent_classifier::classify(db, gw, classifier_input).await;
            // classifier_resolved = il classifier ha prodotto un giudizio (NON un
            // fallback di sistema). Parita' col `_classifier_resolved` del brain.
            (
                Some(ai.requires_tools),
                Some(ai.agentic_score),
                Some(ai.authorizes_changes),
                !ai.fallback_used,
            )
        }
        None => (None, None, None, false),
    };
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
    /// Costo totale stimato in USD (0.0 se non calcolato a monte).
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
    matches!(raw.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")
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
        plan_phase_enabled: setting_bool(db, "orchestrator.plan_phase_enabled", d.plan_phase_enabled).await,
        plan_behavior_modes: setting_csv(db, "orchestrator.plan_behavior_modes", d.plan_behavior_modes).await,
        plan_intents: setting_csv(db, "orchestrator.plan_intents", d.plan_intents).await,
        plan_min_token_budget: setting_i64(db, "orchestrator.plan_min_token_budget", d.plan_min_token_budget).await,
        planner_prompt_key: nexus_auth::get_setting(db, "orchestrator.planner_prompt_key")
            .await
            .unwrap_or(d.planner_prompt_key),
        clarifying_questions_enabled: setting_bool(db, "orchestrator.clarifying_questions_enabled", d.clarifying_questions_enabled).await,
        clarifying_questions_max: setting_i64(db, "orchestrator.clarifying_questions_max", d.clarifying_questions_max).await,
        plan_rationale_enabled: setting_bool(db, "orchestrator.plan_rationale_enabled", d.plan_rationale_enabled).await,
        dag_topological_enabled: setting_bool(db, "orchestrator.dag_topological_enabled", d.dag_topological_enabled).await,
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
        runtime_check_enabled: setting_bool(db, "agent.final_gate.runtime_check_enabled", d.runtime_check_enabled).await,
        build_timeout_s: setting_f64(db, "agent.final_gate.build_timeout_s", d.build_timeout_s).await,
        build_output_max_chars: setting_i64(db, "agent.final_gate.build_output_max_chars", d.build_output_max_chars).await,
        runtime_error_patterns: setting_csv(db, "agent.final_gate.runtime_error_patterns", d.runtime_error_patterns).await,
        no_orphan_min_ratio: setting_f64(db, "agent.no_orphan.min_ratio", d.no_orphan_min_ratio).await,
        import_staging_dirs: setting_csv(db, "agent.import_staging_dirs", d.import_staging_dirs).await,
        criteria_timeout_s: setting_f64(db, "orchestrator.verifier_timeout_s", d.criteria_timeout_s).await,
        // verify_steps/verify_profile_missing innestati in run_engine (profilo
        // per-ambiente, ADR 0036). log_command / endpoint_criterion restano
        // risolti per-progetto a monte (non ancora portati): default
        // vuoto/None (niente criterio, non blocca).
        verify_steps: d.verify_steps,
        verify_profile_missing: d.verify_profile_missing,
        log_command: d.log_command,
        endpoint_criterion: d.endpoint_criterion,
        design_verify_enabled: setting_bool(db, "agent.final_gate.design_verify_enabled", d.design_verify_enabled).await,
        design_verify_min_score: setting_i64(db, "agent.final_gate.design_verify_min_score", d.design_verify_min_score).await,
        // ADR 0018 leva 3 (mig 0503): kill-switch dei criteri strutturali.
        structural_criteria_enabled: setting_bool(db, "agent.final_gate.structural_criteria_enabled", d.structural_criteria_enabled).await,
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
        max_verify_cycles: setting_i64(db, "orchestrator.max_verify_cycles", d.max_verify_cycles).await,
        fail_closed: setting_bool(db, "agent.verifier.fail_closed", d.fail_closed).await,
        dag_topological_enabled: setting_bool(db, "orchestrator.dag_topological_enabled", d.dag_topological_enabled).await,
        exploratory_verify_max_total: d.exploratory_verify_max_total,
    }
}

/// Costruisce la [`RoutingConfig`] DB-driven (regola G): legge dal DB i campi che
/// il brain risolve da `orchestrator_config` / `_load_g1_max_nudges` /
/// `_load_pending_steps_config`, col PUNTO UNICO `nexus_auth::get_setting` (helper
/// `setting_*`). Il `recursion_limit` (u32) e' letto come faceva il blocco inline.
/// Tutti gli ALTRI campi restano al `Default` (safe-default identico ai
/// `_SAFE_DEFAULTS` del brain: valgono SOLO se la chiave manca o il DB e'
/// irraggiungibile, mai come magic fallback nella logica).
async fn load_routing_config(db: &PgPool) -> RoutingConfig {
    let d = RoutingConfig::default();
    let recursion_limit: u32 = nexus_auth::get_setting(db, "agent.graph.recursion_limit")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(d.recursion_limit);
    RoutingConfig {
        recursion_limit,
        g1_max_nudges: setting_i64(db, "agent.g1_max_nudges", d.g1_max_nudges).await,
        todo_isolation_enabled: setting_bool(db, "agent.continuous.todo_isolation_enabled", d.todo_isolation_enabled).await,
        pending_steps_detection_enabled: setting_bool(db, "agent.closure.pending_steps_detection_enabled", d.pending_steps_detection_enabled).await,
        pending_steps_min_items: setting_i64(db, "agent.closure.pending_steps_min_items", d.pending_steps_min_items).await,
        final_gate_software_intents: setting_csv(db, "agent.final_gate.software_intents", d.final_gate_software_intents).await,
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
        progress_controller_enabled: setting_bool(db, "agent.progress_controller_enabled", d.progress_controller_enabled).await,
        repeated_action_threshold: setting_i64(db, "agent.repeated_action_threshold", d.repeated_action_threshold).await,
        repeated_action_threshold_read_only: setting_i64(db, "agent.repeated_action_threshold.read_only", d.repeated_action_threshold_read_only).await,
        repeated_action_force_diagnose_enabled: setting_bool(db, "agent.repeated_action_force_diagnose_enabled", d.repeated_action_force_diagnose_enabled).await,
        reallocation_threshold: setting_i64(db, "agent.loop.resource_reallocation_threshold", d.reallocation_threshold).await,
        upscale_enabled: setting_bool(db, "agent.upscale.enabled", d.upscale_enabled).await,
        upscale_overhead_ratio: setting_f64(db, "agent.upscale.target_overhead_ratio", d.upscale_overhead_ratio).await,
        verification_directive_enabled: setting_bool(db, "agent.verification_directive_enabled", d.verification_directive_enabled).await,
        // ── tool_choice forcing (ADR 0018 leva 2, mig 0300) ───────────────────
        // Il porting Rust aveva PERSO questi tre campi: restavano ai Default del
        // nodo (enabled=false, style=None) -> il force-action era INERTE per ogni
        // provider (force_now sempre false). Ora il DB e' di nuovo l'unica fonte
        // (regola G): i flag dai settings (default-DB-down nel `Default`), lo
        // stile dal catalog via punto unico `capability::resolve_tool_choice_style`.
        tool_choice_forcing_enabled: setting_bool(db, "agent.tool_choice_forcing_enabled", d.tool_choice_forcing_enabled).await,
        tool_choice_forcing_max_iteration: setting_i64(db, "agent.tool_choice_forcing_max_iteration", d.tool_choice_forcing_max_iteration).await,
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
            compress_start_iter: setting_i64(db, "agent.context.compress_start_iter", d.ctx_mgmt.compress_start_iter).await,
            compress_phase_boundaries: setting_i64_csv(db, "agent.context.compress_phase_boundaries", d.ctx_mgmt.compress_phase_boundaries.clone()).await,
            compress_phase_keep_recent: setting_i64_csv(db, "agent.context.compress_phase_keep_recent", d.ctx_mgmt.compress_phase_keep_recent.clone()).await,
            compress_phase_max_chars: setting_i64_csv(db, "agent.context.compress_phase_max_chars", d.ctx_mgmt.compress_phase_max_chars.clone()).await,
        },
        // Freno token: la soglia hard (0.55 in DB vs 0.70 default) e i parametri
        // aggressivi, ora dal DB.
        token_brake: TokenBrakeConfig {
            max_context_ratio: setting_f64(db, "agent.context.max_context_ratio", d.token_brake.max_context_ratio).await,
            aggressive_keep_recent: setting_i64(db, "agent.context.aggressive_keep_recent", d.token_brake.aggressive_keep_recent as i64).await.max(0) as usize,
            aggressive_max_chars: setting_i64(db, "agent.context.aggressive_max_chars", d.token_brake.aggressive_max_chars as i64).await.max(0) as usize,
        },
        // Forced-RAG reminder: ratio + testo dal DB (erano vuoti/0.0 -> reminder mai
        // iniettato anche con offload attivo).
        forced_rag_ratio: setting_f64(db, "agent.context.forced_rag_threshold_ratio", d.forced_rag_ratio).await,
        forced_rag_reminder_text: setting_string(db, "agent.context.forced_rag_reminder_text", &d.forced_rag_reminder_text).await,
        turn_focus_enabled: setting_bool(db, "agent.context.turn_focus_enabled", d.turn_focus_enabled).await,
        discovery_max_injected: setting_i64(db, "agent.tools.discovery_max_injected", d.discovery_max_injected as i64).await.max(0) as usize,
        // ── rolling-summary (intervento 3): RIASSUME i vecchi via LLM economico ─
        // Flag + keep_recent dal DB (regola G). Il MODELLO economico vive nell'impl
        // della porta PgSummaryStore (agent.context.rolling_summary_model).
        rolling_summary_enabled: setting_bool(db, "agent.context.rolling_summary_enabled", d.rolling_summary_enabled).await,
        rolling_keep_recent: setting_i64(db, "agent.context.rolling_keep_recent_turns", d.rolling_keep_recent).await,
        // ADR 0018 fase 3: rilevamento report passi pendenti nei rami G1/report
        // dell'executor (stesse chiavi della RoutingConfig, regola L).
        pending_steps_detection_enabled: setting_bool(db, "agent.closure.pending_steps_detection_enabled", d.pending_steps_detection_enabled).await,
        pending_steps_min_items: setting_i64(db, "agent.closure.pending_steps_min_items", d.pending_steps_min_items).await,
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
    match nexus_auth::get_setting(db, "agent.context.tokenizer").await.as_deref() {
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
        max_retries: setting_i64(db, "agent.continuous.todo_isolation_max_retries", d.max_retries).await,
        dag_topological_enabled: setting_bool(db, "orchestrator.dag_topological_enabled", d.dag_topological_enabled).await,
        dag_parallel_min_ready: setting_i64(db, "orchestrator.dag_parallel_min_ready", d.dag_parallel_min_ready).await,
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
    Resume { resume_delta: nexus_graph::StateDelta },
}

/// Costruisce le 14 impl concrete + gli 11 nodi (porte iniettate) + la
/// `RoutingConfig` e la `PlannerConfig` DB-driven, e assembla il
/// [`AgentGraphEngine`].
///
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
    let run_db = crate::project_db_routes::project_data_pool_by_session_from(&db, input.session_id)
        .await;

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
    let criteria: Arc<dyn CriteriaRunner> =
        Arc::new(FinalGateCriteriaRunnerAdapter::new(tools.clone(), run_db.clone()));

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
    let verifier_cfg = load_verifier_config(&db).await;

    // ── Catena di verifica per-AMBIENTE (ADR 0036) ───────────────────────────
    // Il profilo del progetto e' INFERITO da un LLM che osserva l'ambiente
    // reale (verify_profile::ensure_profile: sceglie lui i file da leggere e
    // definisce step liberi con flag gate). Qui, risolto a monte del grafo
    // (regola G), si innestano nel final_gate gli step gate=true. In SHADOW
    // niente inferenza (zero side-effect/costo): sola lettura del persistito.
    if final_gate_cfg.enabled {
        let project_id: Option<Uuid> =
            sqlx::query_scalar("SELECT project_id FROM chat_sessions WHERE id = $1")
                .bind(input.session_id)
                .fetch_optional(&run_db)
                .await
                .ok()
                .flatten();
        let root: Option<String> = match project_id {
            Some(pid) => sqlx::query_scalar(
                "SELECT repository_root_path FROM projects WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten(),
            None => None,
        };
        if let (Some(pid), Some(root)) = (project_id, root.filter(|r| !r.trim().is_empty())) {
            let steps = if role.is_shadow() {
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
            final_gate_cfg.verify_profile_missing = steps.is_empty();
            final_gate_cfg.verify_steps = steps
                .into_iter()
                .filter(|s| s.gate)
                .map(|s| nexus_agent_graph::nodes::final_gate::VerifyStepCmd {
                    step: s.step,
                    command: s.command,
                    working_dir: s.working_dir,
                })
                .collect();
        } else {
            // Sessione senza progetto/root (es. run di servizio): niente
            // catena, dichiarazione onesta come per il profilo mancante.
            final_gate_cfg.verify_profile_missing = true;
        }
    }

    let exec_cfg = load_executor_config(&db, &input.provider, &input.model, context_window).await;

    let tool_dispatch_cfg = ToolDispatchConfig {
        context_window,
        ..ToolDispatchConfig::default()
    };

    let reflection_cfg = ReflectionConfig {
        enabled: setting_bool(&db, "reflection_enabled", ReflectionConfig::default().enabled).await,
        provider: reflection_provider,
        model: reflection_model,
        ..ReflectionConfig::default()
    };

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
            // Ricostruzione da JSON minimale (role/content): nessun reasoning.
            reasoning: None,
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
                    Some(nexus_agent_graph::decisions::action_oriented_for_intent(intent)),
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
        automation_mode: parse_automation_mode(&input.automation_mode),
        // DEBITO 1 chiuso (TODO Fase 5): `behavior_mode` valorizzato con la STESSA
        // fonte del primario Python, il quale lo riceve dal payload
        // `/agent/run/stream` (campo `behavior_mode`) e lo copia in
        // `initial_state["behavior_mode"]` (`agent.py:621`). mcp-core invia la
        // costante `PRIMARY_BEHAVIOR_MODE` (`brain_agent_client.rs`): la riusiamo
        // qui (punto unico, regola L) cosi' lo shadow confronta un grafo col mode
        // IDENTICO. Conta sul serio dal momento in cui il planner e' eleggibile
        // (`plan_phase_enabled=true`, mig 0426/0439): `PlannerConfig::is_eligible`
        // gata su questo mode; senza valorizzarlo, lo shadow divergerebbe (None vs
        // "bilanciata"). Il valore-vero-dal-turno (derivarlo dall'automation_mode/
        // routing) e' un miglioramento separato, valido per ENTRAMBI i motori
        // (fuori scope: andrebbe cambiato PRIMA lato Python, vedi nota costante).
        behavior_mode: Some(crate::brain_agent_client::PRIMARY_BEHAVIOR_MODE.to_string()),
        // Sub-agente nativo (porting di `run_subagent`): valorizza parent/depth nello
        // stato cosi' il grafo applica i guard di annidamento (UnderstandingNode skip
        // del fan-out explore se depth>=1). `None` per il run principale -> stato
        // INVARIATO (default None). Solo `dispatch_subagent` popola questi campi.
        parent_run_id: input.parent_run_id.map(|u| u.to_string()),
        subagent_depth: input.subagent_depth,
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
    let resume_delta = build_resume_delta(resume_message);
    let outcome = run_engine(deps, input, RunMode::Resume { resume_delta }, RunRole::Primary).await?;
    Ok(map_outcome(outcome))
}

/// Costruisce il delta opaco del runtime che sblocca un interrupt HITL: azzera
/// `awaiting_confirmation` e accoda il messaggio umano di approvazione (campo
/// `messages`, reducer append). Costruito col delta TIPIZZATO del grafo ->
/// `into_opaque` (punto unico tipizzato->opaco, regola L).
fn build_resume_delta(resume_message: &str) -> nexus_graph::StateDelta {
    use nexus_agent_graph::state::{Message, MessageContent};
    let typed = nexus_agent_graph::state::StateDelta {
        // Azzera il predicato di interrupt: senza, il motore si re-interrompe sul
        // checkpoint ancora-in-attesa (loop di conferma).
        awaiting_confirmation: Some(Some(false)),
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
    let (engine, routing_cfg, llm, tools, emit) = build_native_engine(deps, input, role).await?;

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
    };

    // Avvio nuovo: parte da `entry` con l'initial_state dal prompt.
    // Resume HITL: nessun init (carica il checkpoint), applica il resume_delta.
    match mode {
        RunMode::New => {
            let mut init_state = build_initial_state(input, role);
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
        final_answer: state.result.clone(),
        stop_reason: state.stop_reason,
        provider_used: state.provider_used.clone(),
        model_used: state.model_used.clone(),
        resume_at,
        iterations: state.iterations.unwrap_or(0),
        prompt_tokens: state.prompt_tokens.unwrap_or(0),
        completion_tokens: state.completion_tokens.unwrap_or(0),
        total_tokens: state.total_tokens.unwrap_or(0),
        total_cost: state.total_cost_usd.unwrap_or(0.0),
        user_intent: state.user_intent.clone(),
        reasoning: state
            .reasoning_acc
            .clone()
            .filter(|s| !s.trim().is_empty()),
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
        error_class: state
            .extra
            .get("error_class")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        forced_close_unverified: state.forced_close_unverified.unwrap_or(false),
        final_gate_passed: state.final_gate_passed,
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
            "end_turn" | "endturn" | "stop" | "completed" | "completed_verified" => "end_turn",
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
        if let Message::Ai { content, tool_calls, .. } = m {
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
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
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
        Some("completed") | Some("completed_verified")
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
            step_tx: tx,
            // Run principale di test: nessun annidamento sub-agente.
            parent_run_id: None,
            subagent_depth: None,
        }
    }

    #[test]
    fn initial_state_da_prompt_history_e_override() {
        let input = sample_input();
        let state = build_initial_state(&input, RunRole::Primary);

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

        // DEBITO 1: behavior_mode valorizzato con la STESSA fonte del primario
        // Python (la costante del client brain), non None.
        assert_eq!(
            state.behavior_mode.as_deref(),
            Some(crate::brain_agent_client::PRIMARY_BEHAVIOR_MODE),
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
        assert_eq!(state.action_oriented, Some(true), "score 0.80 >= 0.5 -> azione");
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
            true,                        // tools_available
            turn_action_oriented(None),  // action_oriented (None -> true conservativo)
            1,                           // iteration <= max
            false,                       // in_discovery_phase
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
        assert!(!force_now, "con style None il force-action e' inerte (il bug)");
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
        let delta = build_resume_delta("Azioni confermate dall'utente.");
        state.merge(delta);

        assert!(
            !state.is_awaiting_confirmation(),
            "il delta deve azzerare awaiting_confirmation (sblocca l'interrupt)"
        );
        // Il messaggio di approvazione e' ACCODATO (reducer append su messages).
        assert_eq!(state.messages.len(), 2, "messaggio di approvazione accodato");
        match state.messages.last() {
            Some(Message::Human { content }) => {
                let txt = serde_json::to_string(content).unwrap_or_default();
                assert!(txt.contains("Azioni confermate"));
            }
            other => panic!("atteso Human in coda, trovato {other:?}"),
        }
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
        assert_eq!(canonical_stop_reason(Some("completed_verified")), "end_turn");
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
                    }],
                    reasoning: None,
                },
                // Forma Anthropic: un edit_file inline (mutativo) -> produced_work.
                Message::Ai {
                    content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                        id: "t2".to_string(),
                        name: "edit_file".to_string(),
                        input: json!({}),
                    }]),
                    tool_calls: vec![],
                    reasoning: None,
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
                }],
                reasoning: None,
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
            vec!["has_produced_work".to_string(), "num_tool_calls".to_string()]
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
        set_setting(&pool, "orchestrator.plan_behavior_modes", "bilanciata,approfondita").await;
        set_setting(&pool, "orchestrator.plan_intents", "code,fix,debug").await;
        set_setting(&pool, "orchestrator.plan_min_token_budget", "800").await;
        set_setting(&pool, "orchestrator.clarifying_questions_enabled", "false").await;
        set_setting(&pool, "orchestrator.dag_topological_enabled", "true").await;

        let cfg = load_planner_config(&pool).await;
        assert!(cfg.plan_phase_enabled, "letto true dal DB");
        assert_eq!(cfg.plan_behavior_modes, vec!["bilanciata", "approfondita"]);
        assert_eq!(cfg.plan_intents, vec!["code", "fix", "debug"]);
        assert_eq!(cfg.plan_min_token_budget, 800);
        assert!(!cfg.clarifying_questions_enabled, "false dal DB sovrascrive il default true");
        assert!(cfg.dag_topological_enabled);

        // Con plan_phase_enabled=true e behavior_mode "bilanciata" (fonte primario,
        // DEBITO 1) in plan_behavior_modes + un intent in plan_intents + budget
        // sufficiente, il planner Rust e' eleggibile (accoppiamento debito 1+2).
        assert!(
            cfg.is_eligible(Some(crate::brain_agent_client::PRIMARY_BEHAVIOR_MODE), Some("code"), 1000),
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
        set_setting(&pool, "agent.context.compress_phase_boundaries", "3,7,15,30").await;
        set_setting(&pool, "agent.context.compress_phase_keep_recent", "5,3,2,1").await;
        // CSV malformato di proposito: deve degradare IN BLOCCO al default safe.
        set_setting(&pool, "agent.context.compress_phase_max_chars", "1200,xxx,300,100").await;
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
        assert_eq!(cfg.ctx_mgmt.compress_phase_max_chars, vec![2000, 1000, 500, 150]);
        // token_brake dal DB (0.55 < 0.70 default).
        assert!((cfg.token_brake.max_context_ratio - 0.55).abs() < 1e-9);
        assert!((cfg.forced_rag_ratio - 0.30).abs() < 1e-9);
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
        assert_eq!(resolve_tokenizer_kind(&pool).await, TokenizerKind::Cl100k);
        sqlx::query("UPDATE settings SET value = 'chars' WHERE key = 'agent.context.tokenizer'")
            .execute(&pool)
            .await
            .expect("update setting");
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
        assert!(!planner.plan_phase_enabled, "safe-default: planner OFF se chiave assente");
        assert_eq!(planner.plan_min_token_budget, PlannerConfig::default().plan_min_token_budget);

        let verifier = load_verifier_config(&pool).await;
        assert!(!verifier.enabled, "safe-default: verifier OFF se chiave assente");
        assert_eq!(verifier.max_verify_cycles, VerifierConfig::default().max_verify_cycles);

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
        assert!(!cfg.fail_closed, "fail_closed=false dal DB sovrascrive il default true");
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
        assert_eq!(cfg.runtime_error_patterns, vec!["ECONNREFUSED", "Traceback"]);
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
}
