//! `PlannerNode` — porta la SPINA DORSALE deterministica + i rami ON-di-default
//! di `planner_node` (`brain/agents/planner_node.py:62-621`).
//!
//! Il planner produce la TODO list strutturata PRIMA dell'executor quando il run
//! e' eleggibile (plan_phase). E' un nodo HIGH, OFF di default
//! (`plan_phase_enabled=false`): con i safe-default DB il nodo fa SEMPRE
//! pass-through (`is_eligible` false), path identico a oggi. Quando attivo:
//! risolve provider/model del purpose `planner` (RISOLTI A MONTE, regola G),
//! esegue UNA chiamata LLM con tool_choice forzato su `nexus_todo_write`, esegue
//! il tool via ToolRunner (persiste su `nexus_agent_todos`) e popola lo stato per
//! l'executor. In caso di problema NON blocca mai il run: segna
//! `plan_phase_active=false` + `plan_phase_skip_reason` e il loop legacy prende
//! il sopravvento (fallback).
//!
//! ## Cosa porta QUESTO PR (deterministico + rami ON di default, golden 1:1)
//!
//! - **`is_eligible`** (`orchestrator_config.py:410-428`,
//!   [`PlannerConfig::is_eligible`]): i 4 gate (plan_phase_enabled AND
//!   behavior_mode in plan_behavior_modes AND intent in plan_intents AND
//!   token_budget >= plan_min_token_budget), confronto case-insensitive. false ->
//!   pass-through (`{plan_phase_active:false}`).
//! - **Riuso piano intent/mode-aware** (`:84-113`, [`PlannerNode::plan_reuse`]):
//!   se `fetch_plan(run_id)` esiste, INVALIDA solo se `plan_intent != intent`
//!   (non-None) o `plan_mode != behavior_mode` (non-None) -> rigenera; altrimenti
//!   delta di riuso. Decisione PURA (i fetch sono I/O del nodo).
//! - **Clarifying pre-flight branching** (`:132-170`, RAMO ON,
//!   [`PlannerNode::clarifying_branch`]): dato l'esito di `_detect_clarifications`
//!   (LLM, delegato), il BRANCHING e' deterministico: Confirm/study/None ->
//!   si FERMA (`awaiting_clarifications` + `pending_clarifications`);
//!   Automatico/Continuo -> applica `suggested_default`
//!   (`applied_default_assumptions`) e PROSEGUE. Golden sul branching.
//! - **Tool catalog `nexus_todo_write`** (`:221-290`, [`tool_catalog`]): schema
//!   JSON statico. Costante 1:1.
//! - **`hinted_system`** (`:296-366`, rami ON, [`PlannerNode::build_hinted_system`]):
//!   `system_text` + RUN_ID hint + `<comprensione_preliminare>` (context_brief) +
//!   turn_focus IN CODA dietro il confine di turno (riusa
//!   [`crate::decisions::turn_focus::build_turn_focus_directive`] per il contenuto
//!   e `inject_turn_focus` per la posizione, punti unici, regola L).
//!   RAG/backlog/dag = OFF (TODO, vedi sotto).
//! - **Fallback chain decision** (`:399-492`,
//!   [`PlannerNode::resolve_todo_block`]): `next()` su tool_use per
//!   `nexus_todo_write`; se None -> fallback tool-robust (provider/model diverso,
//!   escluse sentinelle); se ancora None -> fallback DETERMINISTICO da
//!   `playbook_steps` ([`playbook_fallback_block`]); altrimenti skip
//!   `no_tool_use_emitted`.
//! - **Build tool_input + parse** (`:495-527`, [`build_tool_input`] /
//!   [`parse_tool_result`]): forza `run_id`, `setdefault planner_model`, persiste
//!   `user_intent`/`behavior_mode`, parse JSON, gate `result.ok`.
//! - **Sticky cascade M69** (`:176-184`): se `planner_sticky_provider/model`
//!   presenti, salta il purpose_model.
//! - **Sentinella gate ADR 0020** (`:198-209`): provider sentinella
//!   (`__router_unavailable__` / `__no_capable_provider__`) -> skip
//!   `no_capable_provider`.
//!
//! ## Cosa NON porta (rami OFF di default + I/O delegato, TODO espliciti)
//!
//! - **RAG decisionale** (`_retrieve_decision_context`, `:624`),
//!   **backlog brief** (`_retrieve_backlog_brief`, `:694`), **dag_kb**
//!   (`dag_kb.build_dependency_context`, `:339`), **persist rationale come nota**
//!   (`_persist_rationale_as_note`, `:735`): rami OFF di default
//!   (`plan_rationale_enabled` / `dag_topological_enabled` False). NON portati
//!   (nessuna divergenza coi default OFF); richiedono la porta KB-search non
//!   ancora presente in `runtime::ports`. TODO esplicito.
//! - **`apply_context_reduction`** (freno contesto, `:375`): non esiste ancora un
//!   punto unico lato Rust -> TODO documentato. Sui golden e' irrilevante (i
//!   messaggi LLM sono input stubati); nel runtime il chiamante dovra' applicarlo
//!   prima di passare i messaggi (vedi nota in `build_llm_messages`).
//! - **Chiamata LLM planner + fallback + `_detect_clarifications`**: I/O dietro
//!   [`crate::runtime::LlmGateway`] (`ctx.llm`); provider/model/prompt RISOLTI A
//!   MONTE (regola G), passati nella [`PlannerConfig`] / via la `LlmRequest`.
//! - **`nexus_todo_write` + `knowledge_create_note`**: I/O dietro
//!   [`crate::runtime::ToolExecutor`] (`ctx.tools`); `knowledge_create_note` e' il
//!   ramo rationale OFF (non portato).
//! - **`fetch_plan` / `list_todos`**: dietro [`crate::runtime::TodoStore`]
//!   (`fetch_plan` aggiunto come punto unico todo store, regola L).
//! - **`_persist_clarifications`** (INSERT `nexus_agent_clarifications`, `:862`):
//!   SCRITTURA DB best-effort. Non esiste una porta dedicata; e' best-effort lato
//!   Python. TODO impl concreta in mcp-core (oggi il delta clarifying viaggia
//!   comunque nello stato, la INSERT e' solo telemetria). Il nodo NON la richiede
//!   per funzionare.
//! - **`meta_steps.persist_async`** (`:562`): la persistenza best-effort del
//!   meta_step `plan` su `nexus_agent_meta_steps` e' un side-effect del brain; nel
//!   runtime Rust il meta_step viaggia gia' nel delta (`meta_steps`, reducer
//!   append) e la persistenza sara' del runtime/emit, non del nodo (parita' col
//!   commento analogo in `clarify_or_expand`).
//!
//! Il nodo NON instrada: l'edge post-planner vive in `routing::route_after_planner`
//! (gia' portato 1:1, NON duplicato).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::context_reduction as ctxr;
use crate::decisions::orchestration_reason::build_orchestration_context;
use crate::decisions::turn_focus::build_turn_focus_directive;
use crate::runtime::ports::{
    Coordination, LlmMessage, LlmRequest, OrchPhase, OrchestrationMove, PlanRow, SubTask,
    TodoStore,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{
    AgentState, Message, MessageContent, MetaStep, StateDelta, TaskComplexity, ToolUse,
};

/// Config DB-driven del nodo planner, PASSATA (regola G: nessuna lettura DB nel
/// nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Mappa i settings risolti dal brain via `orchestrator_config.get()`
/// (`orchestrator_config.py`) + il prompt risolto dal registry + i provider/
/// model del purpose `planner` / `planner_fallback` (RISOLTI A MONTE, regola G:
/// il nodo li riceve gia' decisi, non chiama la routing matrix).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannerConfig {
    /// Plan-phase abilitata (`plan_phase_enabled`, default false). OFF ->
    /// `is_eligible` sempre false -> pass-through.
    pub plan_phase_enabled: bool,
    /// Behavior_mode che attivano il planner (`plan_behavior_modes`, default
    /// `["bilanciata", "approfondita"]`). Confronto case-insensitive.
    pub plan_behavior_modes: Vec<String>,
    /// Intent che attivano il planner (`plan_intents`, default code/implement/
    /// fix/...). Confronto case-insensitive.
    pub plan_intents: Vec<String>,
    /// Token budget minimo del turno sotto cui il planner non si attiva
    /// (`plan_min_token_budget`, default 2000).
    pub plan_min_token_budget: i64,
    /// Chiave del prompt agente del planner nel registry (`planner_prompt_key`,
    /// default `agent.planner.base`). Il TESTO e' risolto a monte; questa chiave
    /// e' trasportata per diagnostica/log.
    pub planner_prompt_key: String,
    /// System prompt del planner RISOLTO a monte dal registry (regola G): vuoto
    /// = prompt non trovato -> skip `prompt_missing` (`planner_node.py:214-216`).
    pub planner_system_text: String,
    /// Clarifying pre-flight abilitata (`clarifying_questions_enabled`, default
    /// TRUE = RAMO ON).
    pub clarifying_questions_enabled: bool,
    /// Numero massimo di domande di chiarimento (`clarifying_questions_max`,
    /// default 3).
    pub clarifying_questions_max: i64,
    /// Anti-contaminazione history attiva (`turn_focus_enabled`, default true).
    /// RAMO ON: il planner appende il turn_focus al system dietro il confine di
    /// turno (punti unici riusati: contenuto e posizione).
    ///
    /// WIRING (TODO impl mcp-core): va popolato dalla CONTINUITY config
    /// (`agent.context.turn_focus_enabled`), NON da `orchestrator_config`. E' la
    /// stessa fonte che alimenta il turn_focus negli altri nodi (punto unico,
    /// regola L); l'impl concreta di mcp-core deve leggerlo da li' quando
    /// costruisce la `PlannerConfig`, non duplicare la lettura sul ramo planner.
    pub turn_focus_enabled: bool,
    /// Razionale del piano abilitato (`plan_rationale_enabled`, default false):
    /// RAMO OFF (RAG decisionale + persist nota + estrazione rationale). Quando
    /// OFF nessuna divergenza.
    pub plan_rationale_enabled: bool,
    /// DAG topologico abilitato (`dag_topological_enabled`, default false): RAMO
    /// OFF (dag_kb). Quando OFF nessuna divergenza.
    pub dag_topological_enabled: bool,
    /// Gate ORCHESTRAZIONE LLM-driven della plan-phase abilitato
    /// (`agent.orchestration.enabled`, **default false**, regola G). Fase 1
    /// dell'orchestrazione (piano design v2): quando ON, il gate
    /// plan-phase consulta [`crate::runtime::ports::MetaReasonerPort::orchestrate`]
    /// PRIMA di [`PlannerConfig::is_eligible`] e ne mappa l'esito; qualunque
    /// degrado (`Fallback`/`Ok(None)`/errore porta) RICADE su `is_eligible`
    /// (euristica esistente invariata, rete di sicurezza). OFF di default ->
    /// comportamento BIT-IDENTICO a oggi (il gate ricade sempre su `is_eligible`).
    /// Il setting sara' seminato nella migrazione del sotto-blocco successivo:
    /// con `false` il gate NON scatta.
    pub orchestration_enabled: bool,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        // Default IDENTICI ai safe-default del brain (orchestrator_config.py).
        // Valgono SOLO se il DB e' irraggiungibile, mai come magic fallback nella
        // logica. plan_phase_enabled FALSE -> il planner e' OFF di default.
        Self {
            plan_phase_enabled: false,
            plan_behavior_modes: vec!["bilanciata".to_string(), "approfondita".to_string()],
            // DEBITO 4 (TODO Fase 5): allineato 1:1 ai `_SAFE_DEFAULTS["plan_intents"]`
            // del brain (`orchestrator_config.py`, mig 0426): intent canonici del
            // classifier + hook `fix_semplice`/`fix_complesso`. Era divergente
            // (`scaffold`/`build`/`frontend` NON sono nei safe-default Python, e
            // mancavano `scaffold_app`/`architecture`/`fix_*`): un default diverso
            // produrrebbe un planner eleggibile su intent diversi quando il DB e'
            // irraggiungibile. Override admin via setting `orchestrator.plan_intents`.
            plan_intents: vec![
                "code".to_string(),
                "implement".to_string(),
                "fix".to_string(),
                "refactor".to_string(),
                "scaffold_app".to_string(),
                "architecture".to_string(),
                "debug".to_string(),
                "fix_semplice".to_string(),
                "fix_complesso".to_string(),
            ],
            plan_min_token_budget: 2000,
            planner_prompt_key: "agent.planner.base".to_string(),
            planner_system_text: String::new(),
            clarifying_questions_enabled: true,
            clarifying_questions_max: 3,
            turn_focus_enabled: true,
            plan_rationale_enabled: false,
            dag_topological_enabled: false,
            // Gate orchestrazione OFF di default (regola G, default safe): il gate
            // ricade sempre su is_eligible -> comportamento bit-identico a oggi.
            orchestration_enabled: false,
        }
    }
}

impl PlannerConfig {
    /// `is_eligible` (`orchestrator_config.py:410-428`): il run corrente puo'
    /// attivare il planner? I 4 gate (in AND), confronto CASE-INSENSITIVE su
    /// behavior_mode e intent. PURA: nessuna lettura DB (la config e' gia' qui).
    ///
    /// Parita' falsy col Python: `if behavior_mode and ...` -> un behavior_mode
    /// `None`/vuoto NON applica il gate del mode (passa); idem per `intent`. Il
    /// gate budget usa `int(token_budget or 0)` (gia' i64 qui).
    pub fn is_eligible(
        &self,
        behavior_mode: Option<&str>,
        intent: Option<&str>,
        token_budget: i64,
    ) -> bool {
        if !self.plan_phase_enabled {
            return false;
        }
        // `if behavior_mode and behavior_mode.lower() not in [...]`: stringa vuota
        // = falsy (salta il gate), come `None`.
        if let Some(bm) = behavior_mode {
            if !bm.is_empty() {
                let bm_l = bm.to_lowercase();
                if !self
                    .plan_behavior_modes
                    .iter()
                    .any(|m| m.to_lowercase() == bm_l)
                {
                    return false;
                }
            }
        }
        if let Some(it) = intent {
            if !it.is_empty() {
                let it_l = it.to_lowercase();
                if !self.plan_intents.iter().any(|i| i.to_lowercase() == it_l) {
                    return false;
                }
            }
        }
        if token_budget < self.plan_min_token_budget {
            return false;
        }
        true
    }
}

/// Esito della decisione di RIUSO PIANO (pura, `planner_node.py:84-113`). I fetch
/// (`fetch_plan`/`list_todos`/`active_todo`) sono I/O del nodo; QUESTA e' la sola
/// DECISIONE: dato il piano esistente (o la sua assenza) + l'intent/mode correnti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanReuse {
    /// Nessun piano esistente -> prosegue alla creazione di un nuovo piano.
    NoPlan,
    /// Piano esistente ma obsoleto (intent o mode divergente, non-None) ->
    /// rigenera (nessun return, prosegue alla creazione).
    Stale,
    /// Piano esistente e valido -> RIUSA (delta di riuso, niente nuova
    /// pianificazione).
    Reuse,
}

/// Decisione PURA di riuso piano (`planner_node.py:84-100`). `existing` e' il
/// piano letto via `fetch_plan` (`None` = nessun piano). Invalida SOLO con
/// informazione tracciata e divergente: i piani legacy (campo `None`) mantengono
/// il riuso storico (mig 0328). PUNTO UNICO della decisione (regola L): la `run`
/// e il golden la chiamano entrambi.
pub fn plan_reuse_decision(
    existing: Option<&PlanRow>,
    intent: Option<&str>,
    behavior_mode: Option<&str>,
) -> PlanReuse {
    let Some(plan) = existing else {
        return PlanReuse::NoPlan;
    };
    // intent_diverged = plan_intent is not None and plan_intent != intent
    let intent_diverged = plan
        .user_intent
        .as_deref()
        .map(|pi| Some(pi) != intent)
        .unwrap_or(false);
    // mode_diverged = plan_mode is not None and plan_mode != behavior_mode
    let mode_diverged = plan
        .behavior_mode
        .as_deref()
        .map(|pm| Some(pm) != behavior_mode)
        .unwrap_or(false);
    if intent_diverged || mode_diverged {
        PlanReuse::Stale
    } else {
        PlanReuse::Reuse
    }
}

/// Branching deterministico della clarifying pre-flight (`planner_node.py:144-170`).
/// Dato l'esito di `_detect_clarifications` (delegato all'LLM) e il behavior_mode,
/// decide se FERMARSI (HITL Confirm) o PROSEGUIRE applicando i default.
#[derive(Debug, Clone, PartialEq)]
pub enum ClarifyingBranch {
    /// Nessuna domanda emessa (o detection fallita): il planner prosegue
    /// normalmente, nessun campo clarifying nel delta.
    Proceed,
    /// Confirm/study/None: si FERMA per HITL. Trasporta le domande da esporre.
    Halt {
        /// Domande pendenti (`{id, question, suggested_default}`).
        questions: Vec<Value>,
    },
    /// Automatico/Continuo: applica i default e PROSEGUE. Trasporta le domande
    /// (con i loro `suggested_default`) come assunzioni applicate.
    ApplyDefaults {
        /// Domande con i default applicati (trasparenza).
        assumptions: Vec<Value>,
    },
}

/// `true` se l'`automation_mode` impone HITL sul ramo clarifying (study/confirm).
fn clarifying_requires_hitl(automation_mode: Option<crate::state::AutomationMode>) -> bool {
    matches!(
        automation_mode,
        None | Some(crate::state::AutomationMode::None)
            | Some(crate::state::AutomationMode::Confirm)
    )
}

/// Branching clarifying PURO (`planner_node.py:144-170`). `questions` sono le
/// domande emesse da `_detect_clarifications` (gia' filtrate/clampate dal lato
/// LLM): vuote -> `Proceed`. PUNTO UNICO del branching (regola L).
pub fn clarifying_branch(
    questions: &[Value],
    automation_mode: Option<crate::state::AutomationMode>,
) -> ClarifyingBranch {
    if questions.is_empty() {
        return ClarifyingBranch::Proceed;
    }
    if clarifying_requires_hitl(automation_mode) {
        ClarifyingBranch::Halt {
            questions: questions.to_vec(),
        }
    } else {
        ClarifyingBranch::ApplyDefaults {
            assumptions: questions.to_vec(),
        }
    }
}

/// Set di provider/model sentinella del gate ADR 0020 (`planner_node.py:200,420`).
const SENTINELS: [&str; 2] = ["__router_unavailable__", "__no_capable_provider__"];

/// `true` se il provider e' una sentinella del gate (no provider disponibile).
/// Replica `not provider or provider in (...)` (`planner_node.py:198-201`): anche
/// la stringa vuota e' "sentinella" (no provider).
pub fn is_sentinel_provider(provider: &str) -> bool {
    provider.is_empty() || SENTINELS.contains(&provider)
}

/// Tool catalog `nexus_todo_write` dichiarato al planner (`planner_node.py:221-290`).
/// Schema JSON STATICO (costante 1:1): action/run_id/todos[content,status,
/// priority,acceptance_criteria,node_key,dep_keys]/planner_model/rationale/
/// constraints/alternatives.
pub fn tool_catalog() -> Vec<Value> {
    vec![json!({
        "name": "nexus_todo_write",
        "description": "Crea la TODO list strutturata del piano. Chiamare UNA sola volta con action='create' e l'intera lista di todos atomici e verificabili.",
        "input_schema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create"]},
                "run_id": {"type": "string", "description": "UUID del run corrente (ti viene passato gia' valorizzato)"},
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending"]},
                            "priority": {"type": "string", "enum": ["high", "normal", "low"]},
                            "acceptance_criteria": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "type": {"type": "string"},
                                        "command": {"type": "string"},
                                        "expected": {"type": "string"},
                                        "url": {"type": "string"},
                                        "path": {"type": "string"}
                                    }
                                }
                            },
                            "node_key": {
                                "type": "string",
                                "description": "Comp.3a (DAG): chiave logica univoca del todo (es. 'schema_db', 'api', 'frontend'), per referenziarlo come dipendenza."
                            },
                            "dep_keys": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Comp.3a (DAG): node_key dei todo che devono COMPLETARSI prima di questo (dipendenze di esecuzione). Vuoto se indipendente."
                            },
                            "write_scope": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Aree file (path/prefissi relativi alla root) che questo todo dichiara di voler scrivere. Segnale per l'isolamento parallelo dei sub-agenti: due todo con scope disgiunti possono girare in parallelo. Vuoto = non parallelizzabile."
                            }
                        },
                        "required": ["content"]
                    }
                },
                "planner_model": {"type": "string"},
                "rationale": {
                    "type": "string",
                    "description": "Razionale/strategia del piano: perche' questi todos in quest'ordine, assunzioni chiave."
                },
                "constraints": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Vincoli/non-goal che hanno guidato il design del piano."
                },
                "alternatives": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "option": {"type": "string"},
                            "rejected_because": {"type": "string"}
                        }
                    },
                    "description": "Approcci alternativi considerati e perche' scartati."
                }
            },
            "required": ["action", "run_id", "todos"]
        }
    })]
}

/// Costruisce il `nexus_todo_write` block DETERMINISTICO dai passi del playbook
/// (`planner_node.py:474-483`, fallback ultimo-resort, incidente Beauty-Book mig
/// 0395). Ogni passo diventa un todo `{content, status:"pending", priority:
/// "normal"}`. Ritorna `None` se non ci sono passi (-> skip `no_tool_use_emitted`).
pub fn playbook_fallback_block(playbook_steps: &[String]) -> Option<Value> {
    if playbook_steps.is_empty() {
        return None;
    }
    let todos: Vec<Value> = playbook_steps
        .iter()
        .map(|s| json!({"content": s, "status": "pending", "priority": "normal"}))
        .collect();
    Some(json!({
        "name": "nexus_todo_write",
        "input": { "action": "create", "todos": todos }
    }))
}

/// Esito del canale LLM del planner (`planner_node.py:211-492`): il block
/// `nexus_todo_write` da eseguire, oppure il motivo di skip gia' formattato per
/// [`PlannerNode::skip`]. Segnale STRUTTURATO (regola M): il chiamante non deve
/// dedurre l'esito da stringhe libere.
enum TodoBlockOutcome {
    /// Block ottenuto (dal primario, dal fallback tool-robust o dal playbook).
    Block(Value),
    /// Nessun block ottenibile: `plan_phase_skip_reason` da propagare.
    Skip(String),
}

/// Fallback DETERMINISTICO da `playbook_steps` (`planner_node.py:461-492`): ultimo
/// resort quando ne' il primario ne' il fallback tool-robust hanno emesso la tool
/// call. Senza passi di playbook non c'e' piano -> skip `no_tool_use_emitted`.
fn playbook_todo_block(state: &AgentState) -> TodoBlockOutcome {
    let steps = state.playbook_steps.clone().unwrap_or_default();
    match playbook_fallback_block(&steps) {
        Some(block) => {
            tracing::info!(
                target: "nexus_agent_graph::planner",
                steps = steps.len(),
                "planner: todos deterministici dai passi del playbook"
            );
            TodoBlockOutcome::Block(block)
        }
        None => {
            tracing::warn!(
                target: "nexus_agent_graph::planner",
                "planner: nessuna nexus_todo_write emessa -> skip"
            );
            TodoBlockOutcome::Skip("no_tool_use_emitted".to_string())
        }
    }
}

/// PONTE PR5: MATERIALIZZA la mossa [`OrchestrationMove::DelegateSubagents`] in un
/// `nexus_todo_write` block della STESSA forma del canale LLM
/// (`{name, input:{action:"create", todos:[...]}}`), cosi' il planner lo esegue
/// col PUNTO UNICO `nexus_todo_write` (regola L: nessuna INSERT parallela, nessuna
/// logica di dipendenze re-implementata — `dep_keys` -> `depends_on` resta a carico
/// di `resolve_and_persist_deps`).
///
/// Mappatura (regola M: dal segnale strutturato, mai da prosa):
///   - `content` = `task_description`, PREFISSATO con `[kind]` quando il `kind` e'
///     valorizzato. Motivazione: il canale `todo -> dispatch_wave` NON trasporta un
///     kind per-todo (usa `TodoRunnerConfig::todo_kind()` UNIFORME per l'intera
///     wave; la colonna `kind` non esiste in `nexus_agent_todos`). Aggiungere un
///     canale kind per-todo toccherebbe PR4 (dispatch_wave/dispatch_subagents),
///     fuori scope PR5. Il prefisso preserva l'informazione nel content (visibile
///     al sub-agente nel blob di contesto) senza duplicare logica ne' schema.
///   - `write_scope` = `task.write_scope` (persistito dalla colonna mig 0006, letto
///     a valle da `dispatch_wave` per il gating dell'isolamento — PR4).
///   - `acceptance_criteria` = `[]` (MVP: il `SubTask` non porta criteri).
///   - `node_key` = `"deleg_{i}"` (chiave logica stabile e univoca per-task).
///   - `dep_keys` dalla `coordination` (regola M, enum -> dipendenze):
///       * `Sequential`       -> catena lineare (`task_i` dipende da `task_{i-1}`);
///       * `ParallelIsolated` -> nessuna dipendenza (i task girano in parallelo,
///         gia' garantiti disgiunti da `validate_orch_move`/`subtasks_are_disjoint`).
///
/// PURA: nessun I/O. `seq` = indice 1-based (parita' con `create_plan`).
pub fn materialize_delegation_block(tasks: &[SubTask], coordination: Coordination) -> Value {
    let sequential = matches!(coordination, Coordination::Sequential);
    let todos: Vec<Value> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let content = if t.kind.trim().is_empty() {
                t.task_description.clone()
            } else {
                format!("[{}] {}", t.kind.trim(), t.task_description)
            };
            let node_key = format!("deleg_{i}");
            // Sequential: catena lineare (dipende dal precedente). ParallelIsolated
            // (o primo task): nessuna dipendenza.
            let dep_keys: Vec<String> = if sequential && i > 0 {
                vec![format!("deleg_{}", i - 1)]
            } else {
                Vec::new()
            };
            json!({
                "content": content,
                "status": "pending",
                "priority": "normal",
                "acceptance_criteria": [],
                "node_key": node_key,
                "dep_keys": dep_keys,
                "write_scope": t.write_scope,
            })
        })
        .collect();
    json!({
        "name": "nexus_todo_write",
        "input": { "action": "create", "todos": todos }
    })
}

/// Costruisce il `tool_input` finale di `nexus_todo_write` (`planner_node.py:495-503`):
/// parte dall'input del todo_block, FORZA `run_id`, `setdefault planner_model` =
/// `provider/model`, persiste `user_intent`/`behavior_mode` (mig 0328,
/// invalidazione intent-aware del riuso). PURA.
pub fn build_tool_input(
    todo_block: &Value,
    run_id: &str,
    used_provider: &str,
    used_model: &str,
    intent: Option<&str>,
    behavior_mode: Option<&str>,
) -> Value {
    // `tool_input = dict(todo_block.get("input") or {})`.
    let mut map = todo_block
        .get("input")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // `tool_input["run_id"] = run_id` (forza valorizzazione corretta).
    map.insert("run_id".to_string(), json!(run_id));
    // `tool_input.setdefault("planner_model", f"{used_provider}/{used_model}")`.
    map.entry("planner_model".to_string())
        .or_insert_with(|| json!(format!("{used_provider}/{used_model}")));
    if let Some(it) = intent {
        map.insert("user_intent".to_string(), json!(it));
    }
    if let Some(bm) = behavior_mode {
        map.insert("behavior_mode".to_string(), json!(bm));
    }
    Value::Object(map)
}

/// Esito del gate di orchestrazione ([`PlannerNode::orchestration_gate`]).
///
/// PROPAGA la mossa strutturata del meta-reasoner (regola M: la decisione viaggia
/// come tipo, non ricostruita dal testo) fino al `run`, che la consuma PRIMA di
/// ricadere sull'euristica `is_eligible`. Sostituisce il vecchio `Option<bool>`:
/// il `bool` non poteva trasportare i `tasks`/`coordination` della delega (TODO
/// storico "esecuzione decompose/delega", PR5 lo chiude).
///
/// Con `orchestration_enabled=false` (default) / Fallback il gate
/// ritorna [`PlanGateOutcome::Heuristic`] -> comportamento bit-identico a oggi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanGateOutcome {
    /// Il gate NON decide: ricadi su `is_eligible` (euristica, rete di sicurezza).
    /// Flag OFF (default), `Fallback`, `Ok(None)`, errore di porta.
    Heuristic,
    /// Procedi alla plan-phase (generazione LLM dei todo): `PlanPhase`/`Decompose`.
    ProceedPlanPhase,
    /// Salta la plan-phase (`RunInline`): il task e' abbastanza semplice inline.
    SkipPlanPhase,
    /// MATERIALIZZA la delega: costruisci i todo direttamente dai `tasks` validati
    /// (regola L: via il punto unico `nexus_todo_write`, non una INSERT parallela),
    /// SALTANDO la generazione LLM della plan-phase. `coordination` mappa la catena
    /// `depends_on` (Sequential -> lineare; ParallelIsolated -> nessuna dipendenza).
    MaterializeDelegation {
        /// Sotto-task da materializzare (uno per todo). Gia' validati a monte
        /// (`validate_orch_move`): mai vuoti, mai `delegation_forbidden`.
        tasks: Vec<SubTask>,
        /// Coordinamento -> mapping su `dep_keys` in fase di materializzazione.
        coordination: Coordination,
    },
}

/// Esito del parse del `result_json` del tool `nexus_todo_write`
/// (`planner_node.py:517-527`). Il gate e' `result_obj.get("ok")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultOutcome {
    /// `ok: true` -> il piano e' stato persistito, prosegue.
    Ok,
    /// `ok` assente/false/non-truthy -> skip `tool_returned_error`.
    Error,
}

/// Parse + gate del `result_json` del tool (`planner_node.py:517-527`).
/// JSON malformato -> `{ok:false}` (Python: `{"ok": False, "raw": ...}`); poi il
/// gate `result_obj.get("ok")` (truthy). PURA.
pub fn parse_tool_result(result_json: &str) -> ToolResultOutcome {
    let parsed: Value = serde_json::from_str(result_json).unwrap_or_else(|_| json!({"ok": false}));
    // `if not result_obj.get("ok")`: truthiness del campo (bool true, o valore
    // truthy). Un `ok` assente/null/false/0/""/[]/{} e' falsy -> Error.
    let ok = match parsed.get("ok") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    };
    if ok {
        ToolResultOutcome::Ok
    } else {
        ToolResultOutcome::Error
    }
}

/// Nodo planner. Le dipendenze I/O specifiche (`TodoStore` per fetch_plan/
/// list_todos) sono CAMPI del nodo (come `TodoRunnerNode`/`FinalGateNode`); LLM e
/// ToolExecutor arrivano dal `AgentNodeCtx`. La config DB-driven (incluso
/// provider/model/prompt RISOLTI A MONTE, regola G) e' nella [`PlannerConfig`].
pub struct PlannerNode {
    /// Config DB-driven del planner (regola G: passata, mai letta dal nodo).
    cfg: PlannerConfig,
    /// Provider del purpose `planner` RISOLTO A MONTE (regola G). Vuoto =
    /// sentinella -> skip `no_capable_provider`.
    planner_provider: String,
    /// Modello del purpose `planner` RISOLTO A MONTE (regola G).
    planner_model: String,
    /// Provider del purpose `planner_fallback` RISOLTO A MONTE (tool-robust).
    fallback_provider: String,
    /// Modello del purpose `planner_fallback` RISOLTO A MONTE.
    fallback_model: String,
    /// Store dei todo/piani (`nexus_agent_todos`/`nexus_agent_plans`): fetch_plan
    /// + list_todos. Impl concreta in mcp-core; stub nei test.
    store: Arc<dyn TodoStore>,
    /// Persistenza del meta-step "Piano creato — N step" (narrazione live in
    /// chat). Pattern emit+persist, punto unico [`crate::nodes::emit_phase_meta`]
    /// (regola L). Prima il meta-step restava solo nel delta di stato: mai
    /// emesso via SSE ne' persistito -> la fase di pianificazione era muta.
    meta_steps: Arc<dyn crate::runtime::ports::MetaStepStore>,
    /// Meta-reasoner LLM (STESSA porta iniettata nel nodo `StallRecovery`, regola
    /// L: mcp-core la costruisce UNA volta e la condivide tra i due nodi). Il
    /// planner usa SOLO [`MetaReasonerPort::orchestrate`] per il gate plan-phase
    /// (Fase 1 dell'orchestrazione); il metodo `recover` e' consumato dal nodo
    /// `StallRecovery`. Con `orchestration_enabled=false` (default) il gate NON la
    /// consulta -> comportamento bit-identico a oggi.
    reasoner: Arc<dyn crate::runtime::ports::MetaReasonerPort>,
}

impl PlannerNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante
    /// (provider/model/prompt risolti a monte, regola G) e lo store dei todo.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: PlannerConfig,
        planner_provider: String,
        planner_model: String,
        fallback_provider: String,
        fallback_model: String,
        store: Arc<dyn TodoStore>,
        meta_steps: Arc<dyn crate::runtime::ports::MetaStepStore>,
        reasoner: Arc<dyn crate::runtime::ports::MetaReasonerPort>,
    ) -> Self {
        Self {
            cfg,
            planner_provider,
            planner_model,
            fallback_provider,
            fallback_model,
            store,
            meta_steps,
            reasoner,
        }
    }

    /// Delta di pass-through `{plan_phase_active: false}` (`planner_node.py:74`):
    /// il run NON e' eleggibile o un piano valido non c'e'; il loop legacy
    /// prosegue. PUNTO UNICO dei pass-through con motivo opzionale (regola L).
    fn skip(reason: Option<&str>) -> OpaqueDelta {
        let mut delta = StateDelta {
            plan_phase_active: Some(Some(false)),
            ..Default::default()
        };
        if let Some(r) = reason {
            delta.plan_phase_skip_reason = Some(Some(r.to_string()));
        }
        delta.into_opaque()
    }

    /// Delta di RIUSO PIANO (`planner_node.py:108-113`): plan_phase_active true +
    /// current_plan_id + current_todos + active_todo_id. `active_id` e' l'id del
    /// todo attivo (o None). PURO.
    fn reuse_delta(run_id: &str, todos: Vec<Value>, active_id: Option<String>) -> OpaqueDelta {
        StateDelta {
            plan_phase_active: Some(Some(true)),
            current_plan_id: Some(Some(run_id.to_string())),
            current_todos: Some(Some(todos)),
            active_todo_id: Some(active_id),
            ..Default::default()
        }
        .into_opaque()
    }

    /// GATE ORCHESTRAZIONE LLM-driven della plan-phase (Fase 2, piano design v2).
    /// Consulta [`crate::runtime::ports::MetaReasonerPort::orchestrate`] e PROPAGA
    /// la mossa strutturata come [`PlanGateOutcome`] che il chiamante consuma PRIMA
    /// di ricadere su [`PlannerConfig::is_eligible`] (regola M: la decisione viaggia
    /// come tipo, non ricostruita dal testo).
    ///
    /// Mappa degli esiti [`OrchestrationMove`] (regola L: la validazione e' del
    /// punto unico `validate_orch_move` dentro l'impl della porta; qui si consuma
    /// il risultato gia' validato):
    ///   - `PlanPhase{..}` / `Decompose{..}` -> [`PlanGateOutcome::ProceedPlanPhase`]
    ///     (procedi alla generazione LLM dei todo);
    ///   - `RunInline`     -> [`PlanGateOutcome::SkipPlanPhase`] (salta la plan-phase);
    ///   - `DelegateSubagents{tasks, coordination}` -> [`PlanGateOutcome::MaterializeDelegation`]
    ///     (PR5: MATERIALIZZA i tasks come todo saltando la generazione LLM; il
    ///     todo-runner li esegue a valle con l'isolamento deciso da PR4);
    ///   - `Fallback`      -> [`PlanGateOutcome::Heuristic`] (ricadi su `is_eligible`).
    ///
    /// REGOLA G: con `orchestration_enabled=false` (default) ritorna `Heuristic`
    /// senza consultare nulla (comportamento bit-identico a oggi).
    ///
    /// I segnali per [`build_orchestration_context`] sono raccolti dai campi
    /// STRUTTURATI di [`AgentState`] (regola M): user_intent, behavior_mode,
    /// token_budget, complessita' (`task_complexity`), agentic_score, is_ambiguous,
    /// plan_exists (`plan_phase_active`), history_len (`messages.len()`), guard di
    /// delega da `subagent_depth`/`subagent_cost_cumulative_usd`. I segnali di
    /// contesto/finestra e i cap depth/cost NON sono ancora esposti nel planner:
    /// default SICURI (0 = ignoto/nessun cap -> pressure Low, guard inattive) —
    /// non un magic fallback (regola G), ma l'assenza esplicita del vincolo. La
    /// guard `delegation_forbidden` non gata la plan-phase (Fase 1 e' sequenziale,
    /// niente delega eseguita); e' passata per completezza del contesto LLM.
    async fn orchestration_gate(&self, state: &AgentState, ctx: &AgentNodeCtx) -> PlanGateOutcome {
        // Regola G: gate OFF di default -> ricadi su is_eligible (bit-identico).
        if !self.cfg.orchestration_enabled {
            return PlanGateOutcome::Heuristic;
        }

        // Segnali strutturati (regola M): tutti gia' risolti a monte nello stato.
        let task_complexity = match state.task_complexity {
            Some(TaskComplexity::Low) => 1,
            Some(TaskComplexity::Medium) => 2,
            Some(TaskComplexity::High) => 3,
            None => 0,
        };
        // agentic_score e' un f64 0..1: scala a 0..10 intero (segnale strutturato).
        let agentic_score = state
            .agentic_score
            .map(|s| (s * 10.0).round() as i64)
            .unwrap_or(0);
        let orch_ctx = build_orchestration_context(
            OrchPhase::PlanEntry,
            state.user_intent.as_deref(),
            state.behavior_mode.as_deref().unwrap_or(""),
            state.token_budget.unwrap_or(0),
            task_complexity,
            agentic_score,
            state.is_ambiguous.unwrap_or(false),
            state.plan_phase_active.unwrap_or(false),
            // Contesto/finestra non esposti nel planner: 0 = ignoto -> pressure Low.
            0,
            0,
            state.messages.len() as i64,
            state.subagent_depth.unwrap_or(0),
            // Cap depth/cost non esposti nel planner: 0 = nessun cap -> guard inattiva.
            0,
            state.subagent_cost_cumulative_usd.unwrap_or(0.0),
            0.0,
            false,
            // Fase C3 Part B: disponibilita' REALE dell'isolamento fisico dei
            // sub-run (worktree git), risolta a run-init da mcp-core
            // (compute_run_isolation_available: flag ON + root git isolabile) e
            // passata nel ctx (regola M: il planner non fa I/O). `false` ->
            // ParallelIsolated degrada a Sequential in validate_orch_move.
            ctx.isolation_available,
        );

        match self.reasoner.orchestrate(orch_ctx).await {
            // PlanPhase / Decompose -> procedi alla generazione LLM dei todo.
            // Decompose e' trattata come PlanPhase (la scomposizione in blocchi
            // multipli non e' ancora materializzata; la delega si', sotto).
            Ok(Some(OrchestrationMove::PlanPhase { .. } | OrchestrationMove::Decompose { .. })) => {
                tracing::info!(
                    target: "nexus_agent_graph::planner",
                    "orchestration gate: procedi alla plan-phase (LLM-driven)"
                );
                PlanGateOutcome::ProceedPlanPhase
            }
            // DelegateSubagents -> MATERIALIZZA i tasks come todo (PR5): il gate
            // GOVERNA l'esecuzione, non solo la scelta di pianificare. I tasks sono
            // gia' validati (validate_orch_move: mai vuoti, mai delegation_forbidden;
            // la coordination e' gia' degradata a Sequential se l'isolamento manca).
            Ok(Some(OrchestrationMove::DelegateSubagents {
                tasks,
                coordination,
            })) => {
                tracing::info!(
                    target: "nexus_agent_graph::planner",
                    tasks = tasks.len(),
                    coordination = ?coordination,
                    "orchestration gate: materializza delega sub-agenti (LLM-driven)"
                );
                PlanGateOutcome::MaterializeDelegation {
                    tasks,
                    coordination,
                }
            }
            // RunInline -> salta la plan-phase.
            Ok(Some(OrchestrationMove::RunInline)) => {
                tracing::info!(
                    target: "nexus_agent_graph::planner",
                    "orchestration gate: run inline, salta la plan-phase (LLM-driven)"
                );
                PlanGateOutcome::SkipPlanPhase
            }
            // Fallback: nessuna mossa valida -> ricadi su is_eligible.
            Ok(Some(OrchestrationMove::Fallback)) => {
                tracing::debug!(
                    target: "nexus_agent_graph::planner",
                    "orchestration gate: Fallback -> ricado su is_eligible"
                );
                PlanGateOutcome::Heuristic
            }
            // Ok(None): kill-switch OFF / purpose NotFound / stub inerte -> degrado
            // LEGITTIMO all'euristica (opt-in, regola G: NON e' un errore).
            Ok(None) => {
                tracing::debug!(
                    target: "nexus_agent_graph::planner",
                    "orchestration gate: reasoner Ok(None) (inerte/OFF) -> ricado su is_eligible"
                );
                PlanGateOutcome::Heuristic
            }
            // Errore di porta (provider indisponibile / DB-down): NON blocca il run
            // (il gate e' best-effort, l'euristica copre la decisione). WARN.
            Err(err) => {
                tracing::warn!(
                    target: "nexus_agent_graph::planner",
                    error = %err,
                    "orchestration gate: porta reasoner in errore -> ricado su is_eligible"
                );
                PlanGateOutcome::Heuristic
            }
        }
    }

    /// Costruisce `hinted_system` con i SOLI rami ON di default
    /// (`planner_node.py:296-366`). Ordine di concatenazione:
    ///   1. `system_text`
    ///   2. RUN_ID hint
    ///   3. `<comprensione_preliminare>` (context_brief, se presente)
    ///   4. turn_focus IN CODA, dietro il confine di turno (se
    ///      `turn_focus_enabled`, riusa il punto unico dell'iniezione)
    ///
    /// Rami OFF (RAG decisionale, backlog, dag_kb) NON aggiunti (vedi doc modulo).
    /// PURO: nessun I/O (il turn_focus e' una funzione pura sullo stato).
    pub fn build_hinted_system(&self, state: &AgentState, run_id: &str) -> String {
        // (1) + (2) system_text + RUN_ID hint (`:296-299`).
        let mut hinted = format!(
            "{}\n\nRUN_ID corrente: {run_id} (usalo come parametro run_id nel tool nexus_todo_write)",
            self.cfg.planner_system_text
        );

        // (3) <comprensione_preliminare> dal context_brief del nodo understanding
        // (`:301-309`). `str(state.get("context_brief") or "").strip()`.
        let brief = state.context_brief.as_deref().unwrap_or("").trim();
        if !brief.is_empty() {
            hinted.push_str(
                "\n\n<comprensione_preliminare>\n\
                 Contesto raccolto prima di pianificare (grounding sul codebase + \
                 esplorazioni). Usalo per un piano fondato, non assunzioni alla cieca.\n",
            );
            hinted.push_str(brief);
            hinted.push_str("\n</comprensione_preliminare>");
        }

        // Rami OFF (RAG / backlog / dag_kb): NON portati. Con i default DB
        // (plan_rationale_enabled / dag_topological_enabled FALSE) il Python NON
        // li attraversa, quindi questa parte e' assente in entrambi (parita').

        // (4) turn_focus IN CODA (`:352-366`, RAMO ON). RIUSA i DUE punti unici
        // (regola L): `build_turn_focus_directive` per il contenuto,
        // `ctxr::inject_turn_focus` per la POSIZIONE e l'idempotenza — la stessa
        // chiamata che fa l'executor. Best-effort 1:1 col Python (try/except ->
        // prosegue senza directive): la funzione Rust e' infallibile (ritorna
        // Option), quindi "errore -> prosegue" diventa "None -> nessuna
        // iniezione". `new_topic=false`: il planner Python chiama
        // `build_turn_focus_directive` SENZA il flag new_topic (default del
        // continuity gate non passato qui). Prende lo STATO: la richiesta la
        // porta `turn_task`, non l'ultimo messaggio della cronologia.
        //
        // ANTEPOSTA non andava, ed era l'ultimo dei due consumatori dichiarati in
        // `turn_focus` a cui il confine di turno non era ancora arrivato: il
        // focus e' ricalcolato dallo stato del run, quindi in testa spostava i
        // primi caratteri del system e il fornitore non trovava piu' nulla da
        // riusare di tutto cio' che lo segue. Misurato su questo nodo: fra due
        // run la testa in comune scendeva da ~5860 caratteri (il
        // `planner_system_text` fino al RUN_ID) a ~75, sotto qualunque blocco
        // minimo di cache. Nessuna difesa a valle poteva accorgersene, perche'
        // `prompt_cache_key` filtra sulla `parte_stabile` e il confine il planner
        // non lo emetteva mai. In coda la directive non perde forza: e' il testo
        // adiacente alla conversazione (vedi `inject_turn_focus`).
        if self.cfg.turn_focus_enabled {
            if let Some(focus) = build_turn_focus_directive(state, false) {
                hinted = ctxr::inject_turn_focus(&hinted, &focus);
            }
        }

        hinted
    }

    /// Costruisce i messaggi minimali per la `LlmRequest` del planner dai
    /// `messages` dello stato (`planner_node.py:293,378`). Forma provider-agnostica
    /// (`role`/`content` testo o blocchi). NOTA (TODO): `apply_context_reduction`
    /// (freno contesto, punto unico Python `:375`) NON e' ancora portato lato Rust
    /// -> il chiamante (mcp-core) dovra' applicarlo PRIMA, oppure si porta qui
    /// quando esistera' il punto unico Rust. Sui golden e' irrilevante (i messaggi
    /// LLM sono input stubati).
    fn build_llm_messages(messages: &[Message]) -> Vec<LlmMessage> {
        messages
            .iter()
            .map(|m| {
                let (role, content) = match m {
                    Message::Human { content } => ("user", content),
                    Message::Ai { content, .. } => ("assistant", content),
                    Message::Tool { content, .. } => ("user", content),
                };
                // Forma minimale: il testo piatto (i blocchi tool_use/result del
                // canale interno sono ricostruiti dal gateway concreto a monte
                // della LlmRequest se servono; qui trasportiamo il testo).
                LlmMessage {
                    role: role.to_string(),
                    content: match content {
                        MessageContent::Text(s) => Value::String(s.clone()),
                        MessageContent::Blocks(_) => Value::String(content.flatten_text()),
                    },
                    ..Default::default()
                }
            })
            .collect()
    }

    /// Costruisce la `LlmRequest` del planner per un dato provider/model
    /// (tool_choice forzato su `nexus_todo_write` e' del gateway concreto: qui
    /// dichiariamo il solo tool, come il Python passa `tools_json`).
    fn build_request(
        provider: &str,
        model: &str,
        messages: Vec<LlmMessage>,
        hinted_system: &str,
    ) -> LlmRequest {
        // Il system prompt (`hinted_system`) viaggia come primo messaggio di
        // sistema (forma minimale provider-agnostica, come in clarify_or_expand).
        let mut msgs = Vec::with_capacity(messages.len() + 1);
        msgs.push(LlmMessage {
            role: "system".to_string(),
            content: Value::String(hinted_system.to_string()),
            ..Default::default()
        });
        msgs.extend(messages);
        LlmRequest {
            provider: provider.to_string(),
            model: model.to_string(),
            messages: msgs,
            tools: Some(tool_catalog()),
            // Nodo chiamante = planner (sia primario sia fallback tool-robust). Il
            // gateway concreto lo IGNORA quando il modello e' gia' risolto (regola L).
            purpose: Some("planner".into()),
            ..Default::default()
        }
    }

    /// Estrae il primo tool_call `nexus_todo_write` da una lista di tool_calls
    /// (`next((b for b in ... if b.name == "nexus_todo_write"), None)`,
    /// `planner_node.py:399`). Il `ToolUse` del canale interno porta `input`.
    fn extract_todo_block(tool_calls: &[crate::state::ToolUse]) -> Option<Value> {
        tool_calls
            .iter()
            .find(|t| t.name == "nexus_todo_write")
            .map(|t| json!({"name": t.name, "input": t.input, "id": t.id}))
    }

    /// CANALE LLM del planner (`planner_node.py:211-492`): prompt -> chiamata al
    /// primario -> fallback tool-robust -> fallback deterministico da playbook.
    /// `used_provider`/`used_model` entrano col primario e vengono RISCRITTI in
    /// loco se il fallback tool-robust prende il posto del primario (il chiamante
    /// deve vedere il provider/model vincente anche quando il block finale arriva
    /// poi dal playbook, come nel flusso originale).
    async fn llm_todo_block(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
        run_id: &str,
        used_provider: &mut String,
        used_model: &mut String,
    ) -> TodoBlockOutcome {
        // Il TESTO e' risolto a monte (regola G): vuoto -> skip prompt_missing.
        if self.cfg.planner_system_text.is_empty() {
            tracing::warn!(
                target: "nexus_agent_graph::planner",
                key = %self.cfg.planner_prompt_key,
                "planner: prompt non trovato -> skip"
            );
            return TodoBlockOutcome::Skip(format!("prompt_missing:{}", self.cfg.planner_prompt_key));
        }

        // hinted_system (rami ON) + chiamata LLM (planner_node.py:292-391).
        let hinted_system = self.build_hinted_system(state, run_id);
        let llm_messages = Self::build_llm_messages(&state.messages);
        let mut todo_block = match Self::primary_todo_block(
            ctx,
            used_provider,
            used_model,
            llm_messages.clone(),
            &hinted_system,
        )
        .await
        {
            Ok(block) => block,
            Err(reason) => return TodoBlockOutcome::Skip(reason),
        };

        if todo_block.is_none() {
            match self
                .fallback_todo_block(ctx, used_provider, used_model, llm_messages, &hinted_system)
                .await
            {
                Ok(block) => todo_block = block,
                Err(reason) => return TodoBlockOutcome::Skip(reason),
            }
        }

        match todo_block {
            Some(block) => TodoBlockOutcome::Block(block),
            None => playbook_todo_block(state),
        }
    }

    /// Chiamata al modello PRIMARIO (`planner_node.py:292-391`). `Ok(None)` =
    /// risposta senza la tool call `nexus_todo_write`; `Err(reason)` = errore LLM
    /// -> skip.
    ///
    /// NOTA parita': la `LlmResponse` minimale NON espone provider/model (il gateway
    /// concreto puo' aver fatto un cascade interno usando un provider diverso, ma
    /// quel dettaglio non arriva ai nodi — forma minimale del crate). Il chiamante
    /// tiene `used_provider`/`used_model` che ABBIAMO passato, come il fallback
    /// Python `prov_result.provider or planner_provider` quando il provider
    /// effettivo non e' disponibile.
    async fn primary_todo_block(
        ctx: &AgentNodeCtx,
        used_provider: &str,
        used_model: &str,
        llm_messages: Vec<LlmMessage>,
        hinted_system: &str,
    ) -> Result<Option<Value>, String> {
        let req = Self::build_request(used_provider, used_model, llm_messages, hinted_system);
        match ctx.llm.complete(req).await {
            Ok(resp) => Ok(Self::extract_todo_block(&resp.tool_calls)),
            Err(err) => {
                tracing::error!(
                    target: "nexus_agent_graph::planner",
                    error = %err,
                    "planner: LLM call fallita -> skip"
                );
                Err("llm_error".to_string())
            }
        }
    }

    /// FALLBACK tool-robust (`planner_node.py:401-459`, mig 0267): se il primario
    /// NON ha emesso la tool call, UN tentativo con il modello fallback risolto a
    /// monte, escluse le sentinelle e purche' diverso dal (provider,model) gia'
    /// usato. Su successo `used_provider`/`used_model` diventano quelli del
    /// fallback. `Ok(None)` = fallback non applicabile o nessuna tool call emessa;
    /// `Err(reason)` = errore LLM -> skip.
    async fn fallback_todo_block(
        &self,
        ctx: &AgentNodeCtx,
        used_provider: &mut String,
        used_model: &mut String,
        llm_messages: Vec<LlmMessage>,
        hinted_system: &str,
    ) -> Result<Option<Value>, String> {
        let fb_provider = self.fallback_provider.clone();
        let fb_model = self.fallback_model.clone();
        if is_sentinel_provider(&fb_provider)
            || is_sentinel_provider(&fb_model)
            || (fb_provider.as_str(), fb_model.as_str())
                == (used_provider.as_str(), used_model.as_str())
        {
            return Ok(None);
        }
        tracing::warn!(
            target: "nexus_agent_graph::planner",
            primario = %format!("{used_provider}/{used_model}"),
            fallback = %format!("{fb_provider}/{fb_model}"),
            "planner: nessuna tool call dal primario -> fallback tool-robust"
        );
        let fb_req = Self::build_request(&fb_provider, &fb_model, llm_messages, hinted_system);
        match ctx.llm.complete(fb_req).await {
            Ok(resp) => {
                *used_provider = fb_provider;
                *used_model = fb_model;
                Ok(Self::extract_todo_block(&resp.tool_calls))
            }
            Err(err) => {
                tracing::error!(
                    target: "nexus_agent_graph::planner",
                    error = %err,
                    "planner: LLM call fallback fallita -> skip"
                );
                Err("llm_error".to_string())
            }
        }
    }

    /// Meta-step `plan` per la pubblicazione in chat (`planner_node.py:540-559`).
    /// PURO sui todos riletti + provider/model + active_todo_id.
    fn make_plan_meta(
        run_id: &str,
        todos: &[Value],
        used_provider: &str,
        used_model: &str,
        active_id: Option<&str>,
    ) -> MetaStep {
        let todos_payload: Vec<Value> = todos
            .iter()
            .map(|t| {
                json!({
                    "id": t.get("id"),
                    "seq": t.get("seq"),
                    "content": t.get("content"),
                    "status": t.get("status"),
                    "priority": t.get("priority"),
                })
            })
            .collect();
        MetaStep {
            kind: "plan".to_string(),
            title: format!("Piano creato — {} step", todos.len()),
            payload: json!({
                "plan_id": run_id,
                "todos": todos_payload,
                "provider": used_provider,
                "model": used_model,
                "active_todo_id": active_id,
            }),
            correlation_id: None,
            created_at: None,
        }
    }
}

/// Estrae l'id del todo attivo da una lista di todos (forma JSON): `active.id` se
/// presente (`planner_node.py:531`, via `todo_store.active_todo`). Il selettore
/// e' il punto unico [`TodoStore::active_todo`]; qui derivo l'id dalla forma
/// JSON dei todos riletti per il delta/meta-step.
fn active_todo_id_from(active: Option<&crate::decisions::dag_scheduler::Todo>) -> Option<String> {
    active.map(|t| t.id.clone())
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for PlannerNode {
    fn id(&self) -> NodeId {
        NodeId::Planner
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        let behavior_mode = state.behavior_mode.as_deref();
        let intent = state.user_intent.as_deref();
        let token_budget = state.token_budget.unwrap_or(0);

        // ── GATE ORCHESTRAZIONE LLM-driven (Fase 1) prima di is_eligible ───────
        // Regola L: l'euristica `is_eligible` resta il PUNTO UNICO dell'ingresso
        // plan-phase; il gate orchestrazione la SCAVALCA solo se il reasoner decide
        // in modo esplicito (Some(true)/Some(false)), altrimenti (None: flag OFF /
        // Fallback / Ok(None) / errore) ricade su di essa INVARIATA (rete di
        // sicurezza). Con `orchestration_enabled=false` (default) ritorna sempre
        // None -> comportamento bit-identico a oggi.
        // `delegation` non-None -> il gate ha deciso di MATERIALIZZARE la delega:
        // il todo_block e' costruito DAI TASK (non generato dall'LLM), riusando il
        // punto unico nexus_todo_write (regola L). Propagato fino al ramo di
        // generazione todo_block sotto.
        let mut delegation: Option<(Vec<SubTask>, Coordination)> = None;
        let enter_plan_phase = match self.orchestration_gate(state, ctx).await {
            // Decisione LLM esplicita: procedi alla plan-phase (generazione LLM).
            PlanGateOutcome::ProceedPlanPhase => true,
            // Decisione LLM esplicita: salta la plan-phase.
            PlanGateOutcome::SkipPlanPhase => false,
            // Decisione LLM esplicita: materializza la delega (PR5). Si ENTRA nella
            // plan-phase ma la generazione todo_block e' bypassata (todo dai task).
            PlanGateOutcome::MaterializeDelegation {
                tasks,
                coordination,
            } => {
                delegation = Some((tasks, coordination));
                true
            }
            // Il gate non decide: euristica esistente (planner_node.py:64-74).
            PlanGateOutcome::Heuristic => self.cfg.is_eligible(behavior_mode, intent, token_budget),
        };
        if !enter_plan_phase {
            tracing::debug!(
                target: "nexus_agent_graph::planner",
                plan_enabled = self.cfg.plan_phase_enabled,
                "planner: skip (non eligible)"
            );
            // pass-through {plan_phase_active:false} SENZA skip_reason (Python:74).
            return Ok(Self::skip(None));
        }

        // ── Riuso piano intent/mode-aware (planner_node.py:84-113) ─────────────
        // run_id = thread_id. Senza run_id NON si puo' fetchare il piano; il
        // controllo "run_id assente -> skip no_thread_id" e' a valle (Python:123).
        let run_id = state.thread_id.clone().unwrap_or_default();
        if !run_id.is_empty() {
            let existing = self.store.fetch_plan(&run_id).await.map_err(port_err)?;
            match plan_reuse_decision(existing.as_ref(), intent, behavior_mode) {
                PlanReuse::Reuse => {
                    let todos = self.store.list_todos(&run_id).await.map_err(port_err)?;
                    let active = self.store.active_todo(&run_id).await.map_err(port_err)?;
                    let active_id = active_todo_id_from(active.as_ref());
                    tracing::info!(
                        target: "nexus_agent_graph::planner",
                        run_id = %run_id,
                        todos = todos.len(),
                        "planner: piano valido, riuso"
                    );
                    return Ok(Self::reuse_delta(
                        &run_id,
                        todos_to_values(&todos),
                        active_id,
                    ));
                }
                // Stale/NoPlan: prosegue alla creazione di un nuovo piano.
                PlanReuse::Stale | PlanReuse::NoPlan => {}
            }
        }

        // ── Pre-requisiti: run_id + session_id (planner_node.py:123-130) ───────
        // (providers/tool_runner/routing_client del Python sono il ctx + le porte
        // del runtime Rust: sempre presenti, mai None.)
        if run_id.is_empty() {
            tracing::warn!(target: "nexus_agent_graph::planner", "thread_id assente -> skip");
            return Ok(Self::skip(Some("no_thread_id")));
        }
        let session_id = state.session_id.clone().unwrap_or_default();
        if session_id.is_empty() {
            tracing::warn!(target: "nexus_agent_graph::planner", "session_id assente -> skip");
            return Ok(Self::skip(Some("no_session_id")));
        }

        // ── Clarifying pre-flight (planner_node.py:132-170, RAMO ON) ───────────
        // `pending_clarifications is None` -> abilita la detection. La detection
        // (_detect_clarifications) e' una chiamata LLM (delegata): il provider/
        // model/prompt sono risolti a monte (regola G). Best-effort: errore ->
        // proceed (Python:141-143). Il BRANCHING e' deterministico (golden).
        let mut applied_assumptions: Option<Vec<Value>> = None;
        if self.cfg.clarifying_questions_enabled && state.pending_clarifications.is_none() {
            let questions = self.detect_clarifications(state, ctx).await;
            match clarifying_branch(&questions, state.automation_mode) {
                ClarifyingBranch::Halt { questions } => {
                    tracing::info!(
                        target: "nexus_agent_graph::planner",
                        run_id = %run_id,
                        n = questions.len(),
                        "planner: pending_clarifications (HITL Confirm)"
                    );
                    // SCRITTURA DB best-effort (_persist_clarifications): TODO porta
                    // dedicata; il delta clarifying viaggia comunque nello stato.
                    return Ok(StateDelta {
                        plan_phase_active: Some(Some(false)),
                        plan_phase_skip_reason: Some(Some("awaiting_clarifications".to_string())),
                        pending_clarifications: Some(Some(questions)),
                        ..Default::default()
                    }
                    .into_opaque());
                }
                ClarifyingBranch::ApplyDefaults { assumptions } => {
                    tracing::info!(
                        target: "nexus_agent_graph::planner",
                        run_id = %run_id,
                        n = assumptions.len(),
                        "planner: applied clarifying defaults"
                    );
                    applied_assumptions = Some(assumptions);
                }
                ClarifyingBranch::Proceed => {}
            }
        }

        // ── Sticky cascade M69 / sentinella gate ADR 0020 (planner_node.py:176-209) ─
        // Sticky: se planner_sticky_* presenti, salta il purpose_model (risolto a
        // monte). Altrimenti usa planner_provider/model risolti a monte.
        let (mut used_provider, mut used_model) = match (
            state.planner_sticky_provider.as_deref(),
            state.planner_sticky_model.as_deref(),
        ) {
            (Some(p), Some(m)) if !p.is_empty() && !m.is_empty() => {
                tracing::info!(
                    target: "nexus_agent_graph::planner",
                    provider = %p, model = %m,
                    "planner: M69 sticky cascade attivo"
                );
                (p.to_string(), m.to_string())
            }
            _ => {
                // Sentinella gate: provider non disponibile -> skip (Python:198-209).
                if is_sentinel_provider(&self.planner_provider) {
                    tracing::warn!(
                        target: "nexus_agent_graph::planner",
                        provider = %self.planner_provider,
                        "planner: nessun provider disponibile -> skip"
                    );
                    return Ok(Self::skip(Some(&format!(
                        "no_capable_provider:{}",
                        self.planner_provider
                    ))));
                }
                (self.planner_provider.clone(), self.planner_model.clone())
            }
        };

        // ── PONTE PR5: materializzazione delega (bypassa la generazione LLM) ───
        // Se il gate ha deciso DelegateSubagents, i todo si costruiscono DAI TASK
        // (segnale strutturato, regola M) e non dalla plan-phase LLM. Il todo_block
        // ha la STESSA forma del canale LLM ({name, input:{action, todos}}) cosi'
        // il resto del flusso (esecuzione tool, ricarica, popolamento stato) e' 1:1
        // e riusa il PUNTO UNICO nexus_todo_write (regola L). Le colonne prompt/LLM
        // (prompt_missing / hinted_system / fallback tool-robust / playbook) sono
        // saltate: non serve consultare l'LLM per una decisione gia' presa.
        let mut todo_block: Option<Value> = delegation
            .as_ref()
            .map(|(tasks, coordination)| materialize_delegation_block(tasks, *coordination));

        // Canale LLM: eseguito SOLO se la delega non ha gia' materializzato il
        // block. Con delega il prompt/LLM/fallback tool-robust/playbook sono saltati
        // (decisione gia' presa, regola M) e todo_block resta quello dai task.
        if todo_block.is_none() {
            match self
                .llm_todo_block(state, ctx, &run_id, &mut used_provider, &mut used_model)
                .await
            {
                TodoBlockOutcome::Block(block) => todo_block = Some(block),
                TodoBlockOutcome::Skip(reason) => return Ok(Self::skip(Some(&reason))),
            }
        }
        let todo_block = todo_block.expect("todo_block presente dopo i fallback");

        // ── Esegui nexus_todo_write via ToolExecutor (planner_node.py:494-514) ──
        let tool_input = build_tool_input(
            &todo_block,
            &run_id,
            &used_provider,
            &used_model,
            intent,
            behavior_mode,
        );
        let tool_use_id = todo_block
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let call = crate::runtime::ports::ToolCall {
            id: tool_use_id.clone(),
            name: "nexus_todo_write".to_string(),
            input: tool_input,
            thought_signature: None,
        };
        // CONTINUITA' tool_use/tool_result (planner_node.py:586-602): conserva la
        // tool_use di nexus_todo_write da appendere al `Message::Ai` finale. L'id
        // e' lo STESSO del tool_result che segue (`tool_use_id`), cosi' la coppia
        // tool_use -> tool_result e' valida per le API Anthropic-compat al turno
        // successivo. Vale per entrambi i rami: id reale dal modello oppure uuid
        // sintetico del fallback playbook (l'id e' gia' risolto sopra, parita' con
        // `tool_use_id = todo_block.get("id") or str(uuid.uuid4())`).
        let planner_tool_use = ToolUse {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            thought_signature: None,
        };
        let outcome = match ctx.tools.execute(call).await {
            Ok(o) => o,
            Err(err) => {
                tracing::error!(
                    target: "nexus_agent_graph::planner",
                    error = %err,
                    "planner: execute_tool nexus_todo_write fallita -> skip"
                );
                return Ok(Self::skip(Some("tool_error")));
            }
        };

        // ── Parse risultato tool + gate ok (planner_node.py:516-527) ───────────
        let result_json = match &outcome.content {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if outcome.is_error || matches!(parse_tool_result(&result_json), ToolResultOutcome::Error) {
            tracing::warn!(
                target: "nexus_agent_graph::planner",
                "planner: tool ritorna errore -> skip"
            );
            return Ok(Self::skip(Some("tool_returned_error")));
        }

        // ── Ricarica todos persistiti + popola lo stato (planner_node.py:529-621) ─
        let todos = self.store.list_todos(&run_id).await.map_err(port_err)?;
        let active = self.store.active_todo(&run_id).await.map_err(port_err)?;
        let active_id = active_todo_id_from(active.as_ref());
        let todos_values = todos_to_values(&todos);

        tracing::info!(
            target: "nexus_agent_graph::planner",
            run_id = %run_id,
            todos = todos.len(),
            provider = %used_provider,
            model = %used_model,
            "planner: plan creato"
        );

        let plan_meta = Self::make_plan_meta(
            &run_id,
            &todos_values,
            &used_provider,
            &used_model,
            active_id.as_deref(),
        );
        // Narrazione live: "Piano creato — N step" arriva in chat e sopravvive
        // al reload (prima restava solo nel delta di stato: fase muta).
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            &plan_meta.kind,
            plan_meta.title.clone(),
            plan_meta.payload.clone(),
        )
        .await;

        // Messaggi di continuita' (assistant + tool_result) cosi' il prossimo
        // turno dell'executor vede il plan (`planner_node.py:583-602`). L'AIMessage
        // DEVE trasportare la tool_use di nexus_todo_write (stesso `tool_use_id`
        // del tool_result che segue): senza, il tool_result sarebbe ORFANO e le API
        // Anthropic-compat rifiuterebbero (400) la sequenza al turno successivo.
        // Il testo content resta vuoto (forma minimale del crate: il `LlmResponse`
        // del gateway porta `content`, ma la parita' funzionale richiesta dalla
        // continuity e' la coppia tool_use/tool_result simmetrica, non il testo;
        // il Python mette `prov_result.content or ""`, di norma vuoto col solo tool).
        let assistant_msg = Message::Ai {
            content: MessageContent::text(""),
            tool_calls: vec![planner_tool_use],
            // Assistant sintetico del planner (solo tool_use nexus_todo_write):
            // nessun reasoning del modello da ri-passare.
            reasoning: None,
            thinking_signature: None,
        };
        let tool_result_msg = Message::Tool {
            tool_call_id: tool_use_id,
            content: MessageContent::text(result_json),
        };

        // Estrazione rationale/constraints/alternatives: RAMO OFF (plan_rationale
        // _enabled false di default) -> non popolato (parita' col Python che li
        // lascia vuoti/None). TODO quando la porta KB-search permettera' il ramo ON.
        Ok(StateDelta {
            plan_phase_active: Some(Some(true)),
            current_plan_id: Some(Some(run_id.clone())),
            current_todos: Some(Some(todos_values)),
            active_todo_id: Some(active_id),
            messages: Some(vec![assistant_msg, tool_result_msg]),
            provider_used: Some(Some(used_provider.clone())),
            model_used: Some(Some(used_model.clone())),
            // M69: persisti il provider/model vincente per i replan futuri.
            planner_sticky_provider: Some(Some(used_provider)),
            planner_sticky_model: Some(Some(used_model)),
            meta_steps: Some(vec![plan_meta]),
            // Trasparenza: assunzioni di default applicate (ramo Automatico/Continuo).
            applied_default_assumptions: applied_assumptions.map(Some),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl PlannerNode {
    /// Chiamata LLM `_detect_clarifications` (`planner_node.py:775-859`): chiede al
    /// modello se il task e' ambiguo. RITORNA le domande (gia' filtrate/clampate)
    /// o vuoto. Best-effort: errore/assenza -> vuoto (proceed). L'I/O e' dietro
    /// `ctx.llm`; il BRANCHING a valle e' deterministico (golden). Il provider/
    /// model/prompt del purpose `planner` sono risolti a monte (regola G), qui
    /// riusiamo planner_provider/model.
    async fn detect_clarifications(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Vec<Value> {
        // Sentinella gate: nessun provider disponibile -> niente detection (proceed).
        if is_sentinel_provider(&self.planner_provider) {
            return Vec::new();
        }
        // Ultimo messaggio utente (il task): vuoto -> niente detection.
        let user_msg = last_user_text(&state.messages);
        if user_msg.trim().is_empty() {
            return Vec::new();
        }
        let tool = json!({
            "name": "request_clarification",
            "description": "Emetti questa lista di domande se e SOLO se il task utente e' ambiguo.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "question": {"type": "string"},
                                "suggested_default": {"type": "string"}
                            },
                            "required": ["id", "question"]
                        }
                    }
                },
                "required": ["questions"]
            }
        });
        // Il system prompt (`agent.clarifying.detect`, renderizzato con max_q) e'
        // risolto a monte; qui passiamo il solo user_msg (clamp_single_prompt =
        // TODO punto unico Rust). Forma minimale.
        let req = LlmRequest {
            provider: self.planner_provider.clone(),
            model: self.planner_model.clone(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: Value::String(user_msg),
                ..Default::default()
            }],
            tools: Some(vec![tool]),
            // Clarifying-detect del planner: stesso purpose "planner". Il gateway
            // concreto lo IGNORA quando il modello e' gia' risolto (regola L).
            purpose: Some("planner".into()),
            ..Default::default()
        };
        let resp = match ctx.llm.complete(req).await {
            Ok(r) => r,
            Err(err) => {
                tracing::debug!(
                    target: "nexus_agent_graph::planner",
                    error = %err,
                    "planner: clarifying detect saltata (best-effort)"
                );
                return Vec::new();
            }
        };
        // Estrai il tool_use request_clarification -> questions (filtrate+clampate).
        let max_q = self.cfg.clarifying_questions_max.max(0) as usize;
        for tc in &resp.tool_calls {
            if tc.name == "request_clarification" {
                let qs = tc
                    .input
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                // `[q for q in qs if isinstance(q, dict) and q.get("id") and q.get("question")][:max_q]`
                let filtered: Vec<Value> = qs
                    .into_iter()
                    .filter(|q| {
                        q.as_object()
                            .map(|m| {
                                truthy_str_field(m.get("id")) && truthy_str_field(m.get("question"))
                            })
                            .unwrap_or(false)
                    })
                    .take(max_q)
                    .collect();
                return filtered;
            }
        }
        Vec::new()
    }
}

/// `true` se il campo JSON e' una stringa NON vuota (truthy `q.get("id")` Python).
fn truthy_str_field(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::String(s)) if !s.is_empty())
}

/// Ultimo messaggio utente (in reverse) con testo non vuoto
/// (`_detect_clarifications`, `planner_node.py:783-788`): filtra ruolo human/user.
fn last_user_text(messages: &[Message]) -> String {
    for m in messages.iter().rev() {
        if let Message::Human { content } = m {
            let flat = content.flatten_text();
            if !flat.trim().is_empty() {
                return flat;
            }
        }
    }
    String::new()
}

/// Serializza i `Todo` (forma DAG) come `Vec<Value>` per il trasporto nello
/// stato (`current_todos`). I campi non-DAG (content/acceptance_criteria) NON
/// sono nel punto unico `Todo` (vedi nota `todo_runner`): la `TodoStore` concreta
/// dovra' esporre il todo completo per popolarli (TODO impl mcp-core). Qui i todos
/// trasportano i campi DAG noti, sufficienti per active_todo_id/meta-step.
fn todos_to_values(todos: &[crate::decisions::dag_scheduler::Todo]) -> Vec<Value> {
    todos
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
        .collect()
}

/// Converte un [`crate::runtime::ports::PortError`] in [`NodeError::Failed`]: un
/// guasto infrastrutturale dello store (DB down) propaga (il run NON resta morto;
/// il runtime lo gestisce). I fallimenti APPLICATIVI (LLM/tool) sono gia' gestiti
/// inline come skip (parita' col try/except Python che fa fallback al loop legacy).
fn port_err(e: crate::runtime::ports::PortError) -> NodeError {
    NodeError::Failed {
        node: "planner",
        message: format!("store todo/plan fallito: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::decisions::dag_scheduler::{Todo, TodoStatus};
    use crate::runtime::ports::{
        LlmResponse, LlmUsage, PortError, ToolCall, ToolOutcome,
    };
    use crate::runtime::test_doubles::{NullEventSink, StubTodoStore};
    use crate::runtime::AgentNodeCtx;
    use crate::state::{MessageContent, ToolUse};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    /// Config attiva (plan_phase ON, prompt risolto) per i test del flusso pieno.
    fn cfg_active() -> PlannerConfig {
        PlannerConfig {
            plan_phase_enabled: true,
            planner_system_text: "Sei il planner. Crea la TODO list.".to_string(),
            ..Default::default()
        }
    }

    /// Stato eleggibile: behavior_mode bilanciata, intent code, budget alto,
    /// thread_id + session_id presenti, un messaggio utente.
    fn eligible_state() -> AgentState {
        AgentState {
            messages: vec![human("Implementa il login del progetto in modo robusto")],
            behavior_mode: Some("bilanciata".to_string()),
            automation_mode: Some(crate::state::AutomationMode::Automatic),
            user_intent: Some("code".to_string()),
            token_budget: Some(8000),
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            session_id: Some("sess-1".to_string()),
            ..Default::default()
        }
    }

    /// LLM scriptato per il planner: la 1a chiamata `complete` ritorna
    /// `primary`, le successive `secondary` (per esercitare il fallback
    /// tool-robust). Registra il numero di chiamate.
    struct ScriptedLlm {
        primary: LlmResponse,
        secondary: LlmResponse,
        calls: Mutex<usize>,
    }

    impl ScriptedLlm {
        fn new(primary: LlmResponse, secondary: LlmResponse) -> Self {
            Self {
                primary,
                secondary,
                calls: Mutex::new(0),
            }
        }
        /// Emette sempre (primario e fallback) il tool dato.
        fn always_tool(name: &str, input: Value) -> Self {
            let r = tool_resp(name, input);
            Self::new(r.clone(), r)
        }
        /// Non emette mai tool (testo vuoto): forza i fallback.
        fn never_tool() -> Self {
            Self::new(text_resp(), text_resp())
        }
    }

    fn tool_resp(name: &str, input: Value) -> LlmResponse {
        LlmResponse {
            content: String::new(),
            tool_calls: vec![ToolUse {
                id: "tc-1".to_string(),
                name: name.to_string(),
                input,
                thought_signature: None,
            }],
            usage: LlmUsage::default(),
            ..Default::default()
        }
    }
    fn text_resp() -> LlmResponse {
        LlmResponse {
            content: "nessun tool".to_string(),
            tool_calls: vec![],
            usage: LlmUsage::default(),
            ..Default::default()
        }
    }

    #[async_trait]
    impl crate::runtime::ports::LlmGateway for ScriptedLlm {
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, PortError> {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c == 1 {
                Ok(self.primary.clone())
            } else {
                Ok(self.secondary.clone())
            }
        }
    }

    /// ToolExecutor scriptato: ritorna un `result_json` fisso e registra le
    /// chiamate ricevute.
    struct ScriptedTools {
        result_json: String,
        is_error: bool,
        seen: Mutex<Vec<ToolCall>>,
    }
    impl ScriptedTools {
        fn ok(result_json: &str) -> Self {
            Self {
                result_json: result_json.to_string(),
                is_error: false,
                seen: Mutex::new(vec![]),
            }
        }
    }
    #[async_trait]
    impl crate::runtime::ports::ToolExecutor for ScriptedTools {
        async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
            let id = call.id.clone();
            self.seen.lock().unwrap().push(call);
            Ok(ToolOutcome {
                tool_call_id: id,
                content: Value::String(self.result_json.clone()),
                is_error: self.is_error,
                ..Default::default()
            })
        }
    }

    /// Costruisce il nodo con store dato + provider/model di test. Reasoner INERTE
    /// (`StubMetaReasonerPort`, `Ok(None)`): il gate orchestrazione, se acceso,
    /// ricade su `is_eligible`.
    fn node_with(cfg: PlannerConfig, store: Arc<dyn TodoStore>) -> PlannerNode {
        node_with_reasoner(cfg, store, Arc::new(crate::runtime::StubMetaReasonerPort))
    }

    /// Come [`node_with`], ma con un reasoner esplicito (per esercitare il gate
    /// orchestrazione: spia/mossa fissa).
    fn node_with_reasoner(
        cfg: PlannerConfig,
        store: Arc<dyn TodoStore>,
        reasoner: Arc<dyn crate::runtime::ports::MetaReasonerPort>,
    ) -> PlannerNode {
        PlannerNode::new(
            cfg,
            "anthropic".to_string(),
            "modello-planner".to_string(),
            "openai".to_string(),
            "modello-fallback".to_string(),
            store,
            Arc::new(crate::runtime::test_doubles::StubMetaStepStore::default()),
            reasoner,
        )
    }

    /// Reasoner SPIA per il gate orchestrazione: conta le chiamate a `orchestrate`
    /// e ne ritorna una mossa fissa. Verifica CHE il gate consulti (o NON consulti)
    /// la porta secondo flag/modalita' (regola M: asserzione sul segnale di
    /// chiamata, non sul testo). `recover` inerte (il planner non lo usa).
    struct SpyReasoner {
        move_out: Option<crate::runtime::ports::OrchestrationMove>,
        orchestrate_calls: Mutex<usize>,
        /// Ultimo `isolation_available` OSSERVATO nell'OrchestrationContext
        /// ricevuto da `orchestrate` (Fase C3 Part B: verifica il wiring
        /// ctx -> gate, regola M: segnale strutturato, non testo).
        isolation_seen: Mutex<Option<bool>>,
    }

    impl SpyReasoner {
        fn new(move_out: Option<crate::runtime::ports::OrchestrationMove>) -> Self {
            Self {
                move_out,
                orchestrate_calls: Mutex::new(0),
                isolation_seen: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::runtime::ports::MetaReasonerPort for SpyReasoner {
        async fn recover(
            &self,
            _ctx: crate::runtime::ports::StallContext,
        ) -> Result<Option<crate::runtime::ports::RecoveryMove>, PortError> {
            Ok(None)
        }

        async fn orchestrate(
            &self,
            ctx: crate::runtime::ports::OrchestrationContext,
        ) -> Result<Option<crate::runtime::ports::OrchestrationMove>, PortError> {
            *self.orchestrate_calls.lock().unwrap() += 1;
            *self.isolation_seen.lock().unwrap() = Some(ctx.isolation_available);
            Ok(self.move_out.clone())
        }

        async fn assess_scale(
            &self,
            _ctx: crate::runtime::ports::ScaleContext,
        ) -> Result<Option<crate::runtime::ports::ScaleMove>, PortError> {
            Ok(None)
        }

        async fn supervise(
            &self,
            _ctx: crate::runtime::ports::SupervisorContext,
        ) -> Result<Option<crate::decisions::supervisor::SupervisorDecision>, PortError> {
            Ok(Some(crate::decisions::supervisor::SupervisorDecision::Continue))
        }
    }

    /// Ctx con LLM/tool dati. PgPool lazy (il planner non interroga il DB
    /// direttamente: passa per la TodoStore).
    fn ctx_with(
        llm: Arc<dyn crate::runtime::ports::LlmGateway>,
        tools: Arc<dyn crate::runtime::ports::ToolExecutor>,
    ) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm,
            tools,
            emit: Arc::new(NullEventSink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        }
    }

    fn todo(id: &str, status: TodoStatus, seq: i64) -> Todo {
        Todo {
            id: id.to_string(),
            status,
            depends_on: vec![],
            seq: Some(seq),
            write_scope: Vec::new(),
            content: None,
            priority: None,
            acceptance_criteria: Vec::new(),
        }
    }

    // ── is_eligible ──────────────────────────────────────────────────────────

    #[test]
    fn is_eligible_gates() {
        let cfg = cfg_active();
        // Eleggibile: tutto a posto. `Bilanciata`/`CODE` in mixed-case verificano
        // il match case-insensitive contro il default plan_behavior_modes
        // (`bilanciata`/`approfondita`) e plan_intents (`code`/...).
        assert!(
            cfg.is_eligible(Some("Bilanciata"), Some("CODE"), 8000),
            "case-insensitive"
        );
        // plan_phase OFF -> mai eleggibile.
        let off = PlannerConfig::default();
        assert!(!off.is_eligible(Some("bilanciata"), Some("code"), 8000));
        // behavior_mode fuori lista.
        assert!(!cfg.is_eligible(Some("confirm"), Some("code"), 8000));
        // intent fuori lista.
        assert!(!cfg.is_eligible(Some("bilanciata"), Some("chat"), 8000));
        // budget sotto soglia.
        assert!(!cfg.is_eligible(Some("bilanciata"), Some("code"), 100));
        // behavior_mode None/"" salta il gate del mode (parita' falsy Python).
        assert!(cfg.is_eligible(None, Some("code"), 8000));
        assert!(cfg.is_eligible(Some(""), Some("code"), 8000));
    }

    // ── Pass-through non eligible ───────────────────────────────────────────

    #[tokio::test]
    async fn non_eligible_pass_through() {
        // plan_phase OFF (default) -> {plan_phase_active:false}, niente reason.
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let node = node_with(PlannerConfig::default(), store);
        let ctx = ctx_with(
            Arc::new(ScriptedLlm::never_tool()),
            Arc::new(ScriptedTools::ok("{}")),
        );
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(false));
        assert_eq!(out.plan_phase_skip_reason, None);
    }

    // ── Gate orchestrazione LLM-driven (Fase 1) ─────────────────────────────

    /// Ctx minimale (LLM/tool non consultati dal solo gate).
    fn gate_ctx() -> AgentNodeCtx {
        ctx_with(
            Arc::new(ScriptedLlm::never_tool()),
            Arc::new(ScriptedTools::ok("{}")),
        )
    }

    /// Store con un piano riusabile (stesso intent/mode di `eligible_state`): il
    /// flusso pieno del planner, se eligible, RIUSA il piano (plan-phase attiva)
    /// senza chiamare l'LLM. Rende il ramo "eligible -> plan-phase" deterministico.
    fn store_reusable() -> Arc<StubTodoStore> {
        Arc::new(StubTodoStore::with_plan(
            vec![todo("t1", TodoStatus::Pending, 1)],
            Some(PlanRow {
                user_intent: Some("code".to_string()),
                behavior_mode: Some("bilanciata".to_string()),
            }),
        ))
    }

    #[tokio::test]
    async fn orchestration_off_non_consulta_e_ricade_su_is_eligible() {
        // orchestration_enabled=false (default): il gate NON consulta la porta e
        // ricade su is_eligible. Stato eligible + plan_phase ON -> procede (riuso).
        let spy = Arc::new(SpyReasoner::new(Some(OrchestrationMove::RunInline)));
        // cfg_active: plan_phase_enabled=true, orchestration_enabled=false.
        let node = node_with_reasoner(cfg_active(), store_reusable(), spy.clone());
        let ctx = gate_ctx();
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        // orchestrate NON chiamato (flag OFF): comportamento bit-identico a oggi.
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 0);
        // is_eligible=true -> plan-phase (riuso), il gate non ha scavalcato.
        assert_eq!(out.plan_phase_active, Some(true));
    }

    #[tokio::test]
    async fn orchestration_off_stato_non_eligible_skip() {
        // Flag OFF + plan_phase OFF (default) -> is_eligible=false -> skip, e la
        // porta non e' mai consultata.
        let spy = Arc::new(SpyReasoner::new(Some(OrchestrationMove::PlanPhase {
            decompose: true,
        })));
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let node = node_with_reasoner(PlannerConfig::default(), store, spy.clone());
        let ctx = gate_ctx();
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 0);
        assert_eq!(out.plan_phase_active, Some(false));
    }

    #[tokio::test]
    async fn orchestration_on_ok_none_ricade_su_is_eligible() {
        // Flag ON + reasoner Ok(None) (inerte): orchestrate consultato UNA volta,
        // poi degrado LEGITTIMO a is_eligible. Stato NON eligible (plan_phase OFF)
        // -> skip: il gate non ha deciso, l'euristica governa.
        let spy = Arc::new(SpyReasoner::new(None));
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        // plan_phase_enabled resta false (Default), orchestration ON.
        let cfg = PlannerConfig {
            orchestration_enabled: true,
            ..Default::default()
        };
        let node = node_with_reasoner(cfg, store, spy.clone());
        let ctx = gate_ctx();
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        // orchestrate consultato una volta (Real, flag ON).
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 1);
        // Ok(None) -> is_eligible (plan_phase OFF) -> skip.
        assert_eq!(out.plan_phase_active, Some(false));
    }

    #[tokio::test]
    async fn orchestration_gate_riceve_isolation_available_dal_ctx() {
        // Fase C3 Part B: il gate deve passare `ctx.isolation_available`
        // all'OrchestrationContext (prima era hardwired false). Con un ctx Real
        // isolabile, lo spy osserva `isolation_available = true`; con un ctx a
        // false osserva false -> il wiring e' vivo, non un letterale.
        for iso in [true, false] {
            let spy = Arc::new(SpyReasoner::new(None));
            let store = Arc::new(StubTodoStore::with_todos(vec![]));
            let cfg = PlannerConfig {
                orchestration_enabled: true,
                ..Default::default()
            };
            let node = node_with_reasoner(cfg, store, spy.clone());
            let mut ctx = gate_ctx();
            ctx.isolation_available = iso;
            let st = eligible_state();
            let _ = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
            assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 1);
            assert_eq!(
                *spy.isolation_seen.lock().unwrap(),
                Some(iso),
                "l'OrchestrationContext deve rispecchiare ctx.isolation_available"
            );
        }
    }

    #[tokio::test]
    async fn orchestration_on_planphase_scavalca_is_eligible_false() {
        // Flag ON + Real + PlanPhase: procede alla plan-phase ANCHE se is_eligible
        // sarebbe false (plan_phase OFF). Il gate LLM scavalca l'euristica.
        let spy = Arc::new(SpyReasoner::new(Some(OrchestrationMove::PlanPhase {
            decompose: false,
        })));
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        // plan_phase OFF (Default) -> is_eligible false; orchestration ON.
        let cfg = PlannerConfig {
            orchestration_enabled: true,
            planner_system_text: "Sei il planner.".to_string(),
            ..Default::default()
        };
        let node = node_with_reasoner(cfg, store, spy.clone());
        let ctx = ctx_with(
            // LLM che emette la tool call nexus_todo_write (flusso pieno).
            Arc::new(ScriptedLlm::always_tool(
                "nexus_todo_write",
                json!({"action": "create", "todos": [{"content": "step 1"}]}),
            )),
            Arc::new(ScriptedTools::ok(r#"{"ok":true}"#)),
        );
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 1);
        // Il gate ha scavalcato is_eligible=false -> plan-phase attiva.
        assert_eq!(out.plan_phase_active, Some(true));
    }

    #[tokio::test]
    async fn orchestration_on_runinline_scavalca_is_eligible_true() {
        // Flag ON + Real + RunInline: salta la plan-phase ANCHE se is_eligible
        // sarebbe true (cfg_active + stato eligible).
        let spy = Arc::new(SpyReasoner::new(Some(OrchestrationMove::RunInline)));
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let mut cfg = cfg_active();
        cfg.orchestration_enabled = true;
        let node = node_with_reasoner(cfg, store, spy.clone());
        let ctx = gate_ctx();
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 1);
        // RunInline -> salta la plan-phase (scavalca is_eligible=true).
        assert_eq!(out.plan_phase_active, Some(false));
        // Nessuno skip_reason (path pass-through, come is_eligible=false).
        assert_eq!(out.plan_phase_skip_reason, None);
    }

    // ── Ponte PR5: materializzazione delega ─────────────────────────────────

    fn subtask(desc: &str, kind: &str, scope: &[&str]) -> SubTask {
        SubTask {
            task_description: desc.to_string(),
            kind: kind.to_string(),
            write_scope: scope.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `materialize_delegation_block` PURO: Sequential -> catena lineare dep_keys,
    /// write_scope propagato, kind nel content, node_key stabili.
    #[test]
    fn materialize_delegation_sequential_catena() {
        let tasks = vec![
            subtask("schema DB", "db_architect", &["db/"]),
            subtask("API REST", "backend_implementer", &["crates/api/"]),
            subtask("frontend", "", &["apps/web/"]),
        ];
        let block = materialize_delegation_block(&tasks, Coordination::Sequential);
        let todos = block["input"]["todos"].as_array().expect("todos");
        assert_eq!(todos.len(), 3);
        // content prefissato col kind quando presente, grezzo quando vuoto.
        assert_eq!(todos[0]["content"], json!("[db_architect] schema DB"));
        assert_eq!(todos[1]["content"], json!("[backend_implementer] API REST"));
        assert_eq!(todos[2]["content"], json!("frontend")); // kind vuoto -> no prefisso
                                                            // write_scope propagato 1:1.
        assert_eq!(todos[0]["write_scope"], json!(["db/"]));
        assert_eq!(todos[2]["write_scope"], json!(["apps/web/"]));
        // node_key stabili.
        assert_eq!(todos[0]["node_key"], json!("deleg_0"));
        assert_eq!(todos[2]["node_key"], json!("deleg_2"));
        // Sequential: catena lineare. Il primo non ha dipendenze.
        assert_eq!(todos[0]["dep_keys"], json!([]));
        assert_eq!(todos[1]["dep_keys"], json!(["deleg_0"]));
        assert_eq!(todos[2]["dep_keys"], json!(["deleg_1"]));
        // acceptance_criteria MVP vuoto.
        assert_eq!(todos[0]["acceptance_criteria"], json!([]));
    }

    /// `materialize_delegation_block` PURO: ParallelIsolated -> nessuna dipendenza
    /// (i task girano in parallelo, gia' garantiti disgiunti a monte).
    #[test]
    fn materialize_delegation_parallel_nessuna_dipendenza() {
        let tasks = vec![
            subtask("modulo A", "impl", &["a/"]),
            subtask("modulo B", "impl", &["b/"]),
        ];
        let block = materialize_delegation_block(&tasks, Coordination::ParallelIsolated);
        let todos = block["input"]["todos"].as_array().expect("todos");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["dep_keys"], json!([]));
        assert_eq!(todos[1]["dep_keys"], json!([]));
    }

    /// Cattura la `ToolCall` `nexus_todo_write` emessa dal planner (ultima seen).
    fn last_todo_write_input(tools: &ScriptedTools) -> Value {
        let seen = tools.seen.lock().unwrap();
        let call = seen
            .iter()
            .rev()
            .find(|c| c.name == "nexus_todo_write")
            .expect("nexus_todo_write chiamato");
        call.input.clone()
    }

    #[tokio::test]
    async fn ponte_delega_sequential_materializza_senza_llm() {
        // Flag ON + Real + DelegateSubagents{Sequential}: il gate MATERIALIZZA i
        // todo dai task SENZA consultare l'LLM del planner (decisione gia' presa).
        let spy = Arc::new(SpyReasoner::new(Some(
            OrchestrationMove::DelegateSubagents {
                tasks: vec![
                    subtask("crea schema", "db_architect", &["db/"]),
                    subtask("crea API", "backend", &["crates/api/"]),
                ],
                coordination: Coordination::Sequential,
            },
        )));
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let cfg = PlannerConfig {
            orchestration_enabled: true,
            // clarifying OFF per isolare il canale di GENERAZIONE: cosi' l'unica
            // eventuale chiamata LLM sarebbe la generazione del todo_block, che il
            // ponte deve bypassare.
            clarifying_questions_enabled: false,
            // planner_system_text vuoto: se il ponte NON bypassasse l'LLM, il
            // planner farebbe skip prompt_missing. Il fatto che proceda dimostra
            // il bypass.
            ..Default::default()
        };
        let node = node_with_reasoner(cfg, store, spy.clone());
        let llm = Arc::new(ScriptedLlm::never_tool());
        let tools = Arc::new(ScriptedTools::ok(r#"{"ok":true}"#));
        let ctx = ctx_with(llm.clone(), tools.clone());
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 1);
        // L'LLM del planner NON e' consultato per la GENERAZIONE del todo_block: la
        // materializzazione bypassa il canale LLM (clarifying OFF -> 0 chiamate).
        assert_eq!(
            *llm.calls.lock().unwrap(),
            0,
            "delega bypassa la generazione LLM"
        );
        // La plan-phase e' attiva (il todo_write e' andato a buon fine).
        assert_eq!(out.plan_phase_active, Some(true));
        // Il tool nexus_todo_write ha ricevuto i todo materializzati dai task.
        let input = last_todo_write_input(&tools);
        let todos = input["todos"].as_array().expect("todos");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["content"], json!("[db_architect] crea schema"));
        assert_eq!(todos[0]["write_scope"], json!(["db/"]));
        // Sequential -> catena depends_on via dep_keys logici.
        assert_eq!(todos[0]["dep_keys"], json!([]));
        assert_eq!(todos[1]["dep_keys"], json!(["deleg_0"]));
    }

    #[tokio::test]
    async fn ponte_delega_parallel_dep_keys_vuoti() {
        // DelegateSubagents{ParallelIsolated}: i todo materializzati non hanno
        // dipendenze (parallelismo; disgiunzione gia' garantita a monte).
        let spy = Arc::new(SpyReasoner::new(Some(
            OrchestrationMove::DelegateSubagents {
                tasks: vec![subtask("A", "impl", &["a/"]), subtask("B", "impl", &["b/"])],
                coordination: Coordination::ParallelIsolated,
            },
        )));
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let cfg = PlannerConfig {
            orchestration_enabled: true,
            clarifying_questions_enabled: false,
            ..Default::default()
        };
        let node = node_with_reasoner(cfg, store, spy.clone());
        let llm = Arc::new(ScriptedLlm::never_tool());
        let tools = Arc::new(ScriptedTools::ok(r#"{"ok":true}"#));
        let ctx = ctx_with(llm.clone(), tools.clone());
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(*llm.calls.lock().unwrap(), 0);
        assert_eq!(out.plan_phase_active, Some(true));
        let input = last_todo_write_input(&tools);
        let todos = input["todos"].as_array().expect("todos");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["dep_keys"], json!([]));
        assert_eq!(todos[1]["dep_keys"], json!([]));
    }

    #[tokio::test]
    async fn ponte_bit_identico_flag_off_non_materializza() {
        // BIT-IDENTICO: con orchestration_enabled=false (default) il gate NON
        // consulta la porta e il ponte NON scatta anche se la mossa dello spy
        // sarebbe DelegateSubagents. Governa is_eligible -> plan-phase LLM normale.
        let spy = Arc::new(SpyReasoner::new(Some(
            OrchestrationMove::DelegateSubagents {
                tasks: vec![subtask("X", "impl", &["x/"])],
                coordination: Coordination::Sequential,
            },
        )));
        // cfg_active: plan_phase ON, orchestration OFF (default). Stato eligible +
        // piano riusabile -> riuso, senza chiamare LLM ne materializzare.
        let node = node_with_reasoner(cfg_active(), store_reusable(), spy.clone());
        let llm = Arc::new(ScriptedLlm::never_tool());
        let tools = Arc::new(ScriptedTools::ok(r#"{"ok":true}"#));
        let ctx = ctx_with(llm.clone(), tools.clone());
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        // Flag OFF: orchestrate MAI consultato (bit-identico).
        assert_eq!(*spy.orchestrate_calls.lock().unwrap(), 0);
        // Riuso del piano esistente: nessuna materializzazione (nessun tool write).
        let seen = tools.seen.lock().unwrap();
        assert!(
            !seen.iter().any(|c| c.name == "nexus_todo_write"),
            "flag OFF: nessun todo_write materializzato (riuso)"
        );
        assert_eq!(out.plan_phase_active, Some(true));
    }

    // ── Riuso piano ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn riuso_piano_valido() {
        // Piano con stesso intent/mode -> riuso (delta plan_phase_active true +
        // todos). L'LLM NON deve essere chiamato.
        let store = Arc::new(StubTodoStore::with_plan(
            vec![
                todo("t1", TodoStatus::Completed, 1),
                todo("t2", TodoStatus::Pending, 2),
            ],
            Some(PlanRow {
                user_intent: Some("code".to_string()),
                behavior_mode: Some("bilanciata".to_string()),
            }),
        ));
        let llm = Arc::new(ScriptedLlm::never_tool());
        let node = node_with(cfg_active(), store);
        let ctx = ctx_with(llm.clone(), Arc::new(ScriptedTools::ok("{}")));
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(true));
        assert_eq!(
            out.current_plan_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        // active_todo = primo pending (t1 e' completed) -> t2.
        assert_eq!(out.active_todo_id.as_deref(), Some("t2"));
        // Riuso: nessuna chiamata LLM.
        assert_eq!(*llm.calls.lock().unwrap(), 0, "il riuso non chiama l'LLM");
    }

    #[tokio::test]
    async fn riuso_piano_obsoleto_rigenera() {
        // Piano con intent diverso -> stale -> rigenera (chiama LLM + tool, crea).
        // clarifying OFF per isolare la sola chiamata del planner.
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let store = Arc::new(StubTodoStore::with_plan(
            vec![todo("t1", TodoStatus::Pending, 1)],
            Some(PlanRow {
                user_intent: Some("docs".to_string()), // diverso da "code"
                behavior_mode: Some("bilanciata".to_string()),
            }),
        ));
        let llm = Arc::new(ScriptedLlm::always_tool(
            "nexus_todo_write",
            json!({"action": "create", "todos": [{"content": "fai X"}]}),
        ));
        let node = node_with(cfg, store);
        let ctx = ctx_with(
            llm.clone(),
            Arc::new(ScriptedTools::ok(r#"{"ok": true}"#)),
        );
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(true));
        // Ha rigenerato: il planner ha chiamato l'LLM una volta e creato il plan
        // (sticky settato). clarifying OFF -> nessuna detection.
        assert_eq!(*llm.calls.lock().unwrap(), 1);
        assert_eq!(out.planner_sticky_provider.as_deref(), Some("anthropic"));
    }

    // ── Clarifying ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn clarifying_ferma_confirm() {
        // behavior_mode confirm + LLM emette request_clarification -> HALT.
        // confirm non e' in plan_behavior_modes di default -> per esercitare il
        // branching clarifying serve eleggibilita': aggiungo confirm alla lista.
        let mut cfg = cfg_active();
        cfg.plan_behavior_modes.push("confirm".to_string());
        let node = node_with(cfg, Arc::new(StubTodoStore::with_todos(vec![])));
        // L'LLM e' chiamato PRIMA per la detection (request_clarification).
        let llm = Arc::new(ScriptedLlm::always_tool(
            "request_clarification",
            json!({"questions": [
                {"id": "q1", "question": "Quale DB?", "suggested_default": "postgres"}
            ]}),
        ));
        let ctx = ctx_with(llm, Arc::new(ScriptedTools::ok("{}")));
        let mut st = eligible_state();
        st.behavior_mode = Some("confirm".to_string());
        // Il branching clarifying (clarifying_branch) decide HALT vs ApplyDefaults
        // dall'AUTOMATION_MODE (None/Confirm -> HITL), NON dal behavior_mode:
        // eligible_state() lo mette Automatic (-> ApplyDefaults), qui va forzato a
        // Confirm per esercitare il Halt.
        st.automation_mode = Some(crate::state::AutomationMode::Confirm);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(false));
        assert_eq!(
            out.plan_phase_skip_reason.as_deref(),
            Some("awaiting_clarifications")
        );
        let pending = out.pending_clarifications.expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["id"], json!("q1"));
    }

    #[tokio::test]
    async fn clarifying_prosegue_automatico() {
        // behavior_mode automatico + domande emesse -> applica default e PROSEGUE
        // (crea il piano). applied_default_assumptions popolato.
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "t1",
            TodoStatus::Pending,
            1,
        )]));
        // 1a chiamata: detection -> request_clarification; 2a chiamata: planner ->
        // nexus_todo_write.
        let llm = Arc::new(ScriptedLlm::new(
            tool_resp(
                "request_clarification",
                json!({"questions": [
                    {"id": "q1", "question": "Quale DB?", "suggested_default": "postgres"}
                ]}),
            ),
            tool_resp(
                "nexus_todo_write",
                json!({"action": "create", "todos": [{"content": "X"}]}),
            ),
        ));
        let node = node_with(cfg_active(), store);
        let ctx = ctx_with(
            llm.clone(),
            Arc::new(ScriptedTools::ok(r#"{"ok": true}"#)),
        );
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(true));
        let applied = out.applied_default_assumptions.expect("applied");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0]["suggested_default"], json!("postgres"));
        // 2 chiamate LLM: detection + planner.
        assert_eq!(*llm.calls.lock().unwrap(), 2);
    }

    // ── Fallback playbook ───────────────────────────────────────────────────

    #[tokio::test]
    async fn fallback_playbook_deterministico() {
        // L'LLM non emette mai nexus_todo_write (primario e fallback) -> usa i
        // playbook_steps deterministici per costruire il todo_block.
        // clarifying OFF per non consumare chiamate LLM nel branching.
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "t1",
            TodoStatus::Pending,
            1,
        )]));
        let node = node_with(cfg, store);
        let llm = Arc::new(ScriptedLlm::never_tool());
        let tools = Arc::new(ScriptedTools::ok(r#"{"ok": true}"#));
        let ctx = ctx_with(llm.clone(), tools.clone());
        let mut st = eligible_state();
        st.playbook_steps = Some(vec!["passo 1".to_string(), "passo 2".to_string()]);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(true));
        // Primario + fallback tool-robust = 2 chiamate, poi playbook (no terza LLM).
        assert_eq!(*llm.calls.lock().unwrap(), 2);
        // Il tool nexus_todo_write e' stato eseguito coi todos del playbook.
        let seen = tools.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let todos = seen[0].input["todos"].as_array().expect("todos");
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0]["content"], json!("passo 1"));
    }

    #[tokio::test]
    async fn skip_no_tool_use_emitted() {
        // Nessun tool + nessun playbook -> skip no_tool_use_emitted.
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let node = node_with(cfg, store);
        let llm = Arc::new(ScriptedLlm::never_tool());
        let ctx = ctx_with(llm, Arc::new(ScriptedTools::ok("{}")));
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(false));
        assert_eq!(
            out.plan_phase_skip_reason.as_deref(),
            Some("no_tool_use_emitted")
        );
    }

    #[tokio::test]
    async fn skip_no_capable_provider() {
        // Provider planner sentinella -> skip no_capable_provider.
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let node = PlannerNode::new(
            cfg,
            "__no_capable_provider__".to_string(),
            "x".to_string(),
            "openai".to_string(),
            "y".to_string(),
            store,
            Arc::new(crate::runtime::test_doubles::StubMetaStepStore::default()),
            Arc::new(crate::runtime::StubMetaReasonerPort),
        );
        let llm = Arc::new(ScriptedLlm::never_tool());
        let ctx = ctx_with(llm, Arc::new(ScriptedTools::ok("{}")));
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(false));
        assert!(out
            .plan_phase_skip_reason
            .as_deref()
            .unwrap()
            .starts_with("no_capable_provider:"));
    }

    #[tokio::test]
    async fn skip_tool_returned_error() {
        // Il tool nexus_todo_write ritorna {ok:false} -> skip tool_returned_error.
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let node = node_with(cfg, store);
        let llm = Arc::new(ScriptedLlm::always_tool(
            "nexus_todo_write",
            json!({"action": "create", "todos": [{"content": "X"}]}),
        ));
        let ctx = ctx_with(llm, Arc::new(ScriptedTools::ok(r#"{"ok": false}"#)));
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(
            out.plan_phase_skip_reason.as_deref(),
            Some("tool_returned_error")
        );
    }

    // ── Continuita' tool_use/tool_result (planner_node.py:586-602) ───────────

    /// Estrae gli ULTIMI due messaggi del delta-mergiato (assistant + tool) e
    /// verifica la coppia tool_use/tool_result SIMMETRICA: il `Message::Ai` porta
    /// una `ToolUse` di `nexus_todo_write` il cui id COINCIDE col `tool_call_id`
    /// del `Message::Tool` che segue. Senza, il tool_result e' orfano e le API
    /// Anthropic-compat rifiutano (400) la sequenza al turno successivo.
    fn assert_simmetria_tool_use_result(out: &AgentState) -> String {
        let n = out.messages.len();
        assert!(
            n >= 2,
            "attesi almeno assistant + tool in coda, trovati {n}"
        );
        let (tool_use_id, name) = match &out.messages[n - 2] {
            Message::Ai { tool_calls, .. } => {
                assert_eq!(
                    tool_calls.len(),
                    1,
                    "il Message::Ai deve portare 1 tool_use"
                );
                (tool_calls[0].id.clone(), tool_calls[0].name.clone())
            }
            other => panic!("penultimo messaggio non e' Message::Ai: {other:?}"),
        };
        assert_eq!(name, "nexus_todo_write", "la tool_use e' nexus_todo_write");
        let result_id = match &out.messages[n - 1] {
            Message::Tool { tool_call_id, .. } => tool_call_id.clone(),
            other => panic!("ultimo messaggio non e' Message::Tool: {other:?}"),
        };
        assert_eq!(
            tool_use_id, result_id,
            "tool_use.id deve coincidere col tool_result.tool_call_id (coppia valida)"
        );
        assert!(!tool_use_id.is_empty(), "id non vuoto");
        tool_use_id
    }

    #[tokio::test]
    async fn continuity_tool_use_result_ramo_modello() {
        // Ramo modello: il tool_use porta l'id REALE emesso dal modello (tc-1) e il
        // tool_result lo referenzia -> coppia valida.
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "t1",
            TodoStatus::Pending,
            1,
        )]));
        let node = node_with(cfg, store);
        let llm = Arc::new(ScriptedLlm::always_tool(
            "nexus_todo_write",
            json!({"action": "create", "todos": [{"content": "X"}]}),
        ));
        let ctx = ctx_with(llm, Arc::new(ScriptedTools::ok(r#"{"ok": true}"#)));
        let st = eligible_state();
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(true));
        // Id reale dal modello (vedi `tool_resp`: id = "tc-1").
        let id = assert_simmetria_tool_use_result(&out);
        assert_eq!(id, "tc-1", "ramo modello: id reale del tool_use_block");
    }

    #[tokio::test]
    async fn continuity_tool_use_result_ramo_playbook() {
        // Ramo fallback playbook: il todo_block sintetico NON ha id -> il planner
        // genera un uuid (parita' `tool_use_id = todo_block.get("id") or uuid4`).
        // La coppia tool_use/tool_result deve restare simmetrica (stesso uuid).
        let mut cfg = cfg_active();
        cfg.clarifying_questions_enabled = false;
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "t1",
            TodoStatus::Pending,
            1,
        )]));
        let node = node_with(cfg, store);
        let llm = Arc::new(ScriptedLlm::never_tool());
        let ctx = ctx_with(llm, Arc::new(ScriptedTools::ok(r#"{"ok": true}"#)));
        let mut st = eligible_state();
        st.playbook_steps = Some(vec!["passo 1".to_string(), "passo 2".to_string()]);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run"));
        assert_eq!(out.plan_phase_active, Some(true));
        // Id sintetico (uuid generato): non "tc-1", ma coincidente tra Ai e Tool.
        let id = assert_simmetria_tool_use_result(&out);
        assert_ne!(id, "tc-1", "ramo playbook: id NON dal modello");
        assert!(
            Uuid::parse_str(&id).is_ok(),
            "ramo playbook: id e' un uuid sintetico valido, era {id}"
        );
    }

    // ── plan_reuse_decision puro ─────────────────────────────────────────────

    #[test]
    fn plan_reuse_pura() {
        assert_eq!(
            plan_reuse_decision(None, Some("code"), Some("auto")),
            PlanReuse::NoPlan
        );
        // Legacy (campi None) -> riuso storico.
        assert_eq!(
            plan_reuse_decision(Some(&PlanRow::default()), Some("code"), Some("auto")),
            PlanReuse::Reuse
        );
        // Intent divergente tracciato -> stale.
        assert_eq!(
            plan_reuse_decision(
                Some(&PlanRow {
                    user_intent: Some("docs".into()),
                    behavior_mode: None
                }),
                Some("code"),
                Some("auto"),
            ),
            PlanReuse::Stale
        );
        // Stesso intent/mode -> reuse.
        assert_eq!(
            plan_reuse_decision(
                Some(&PlanRow {
                    user_intent: Some("code".into()),
                    behavior_mode: Some("auto".into()),
                }),
                Some("code"),
                Some("auto"),
            ),
            PlanReuse::Reuse
        );
    }

    // ── clarifying_branch puro ───────────────────────────────────────────────

    #[test]
    fn clarifying_branch_pura() {
        use crate::state::AutomationMode;
        // Nessuna domanda -> proceed (a prescindere dal mode).
        assert_eq!(
            clarifying_branch(&[], Some(AutomationMode::Confirm)),
            ClarifyingBranch::Proceed
        );
        let q = vec![json!({"id": "q1", "question": "x"})];
        // None/confirm/study -> halt.
        assert!(matches!(
            clarifying_branch(&q, None),
            ClarifyingBranch::Halt { .. }
        ));
        assert!(matches!(
            clarifying_branch(&q, Some(AutomationMode::Confirm)),
            ClarifyingBranch::Halt { .. }
        ));
        assert!(matches!(
            clarifying_branch(&q, Some(AutomationMode::None)),
            ClarifyingBranch::Halt { .. }
        ));
        // automatic/continuous -> apply defaults.
        assert!(matches!(
            clarifying_branch(&q, Some(AutomationMode::Automatic)),
            ClarifyingBranch::ApplyDefaults { .. }
        ));
    }

    // ── parse_tool_result + build_tool_input ─────────────────────────────────

    #[test]
    fn parse_tool_result_gate() {
        assert_eq!(parse_tool_result(r#"{"ok": true}"#), ToolResultOutcome::Ok);
        assert_eq!(
            parse_tool_result(r#"{"ok": false}"#),
            ToolResultOutcome::Error
        );
        assert_eq!(parse_tool_result(r#"{}"#), ToolResultOutcome::Error);
        assert_eq!(parse_tool_result("non-json"), ToolResultOutcome::Error);
    }

    #[test]
    fn build_tool_input_forza_campi() {
        let block = json!({"input": {"action": "create", "todos": []}});
        let out = build_tool_input(
            &block,
            "RID",
            "anthropic",
            "m1",
            Some("code"),
            Some("bilanciata"),
        );
        assert_eq!(out["run_id"], json!("RID"));
        assert_eq!(out["planner_model"], json!("anthropic/m1"));
        assert_eq!(out["user_intent"], json!("code"));
        assert_eq!(out["behavior_mode"], json!("bilanciata"));
        // setdefault: se planner_model gia' presente, NON sovrascrive.
        let block2 = json!({"input": {"planner_model": "gia-presente", "todos": []}});
        let out2 = build_tool_input(&block2, "RID", "p", "m", None, None);
        assert_eq!(out2["planner_model"], json!("gia-presente"));
    }

    // ── posizione del focus del turno in hinted_system ───────────────────────

    /// Stato eleggibile col task del turno FISSATO come lo fissa
    /// `build_initial_state`: senza quel dato il focus non nasce, e un test che
    /// non lo fissa misurerebbe un prompt senza la parte che vuole guardare.
    fn stato_con_task(task: &str) -> AgentState {
        let mut st = eligible_state();
        st.extra.insert(
            crate::decisions::turn_task::ORIGINAL_TASK_KEY.to_string(),
            Value::String(task.to_string()),
        );
        st
    }

    /// CONTRATTO: il focus del turno non apre il prompt del planner.
    ///
    /// Il focus e' ricalcolato dallo stato del run: anteposto, sposta i primi
    /// caratteri del system e taglia il prefisso riusabile di tutto cio' che lo
    /// segue. Misurato su questo nodo: la testa in comune fra due run scendeva da
    /// ~5860 caratteri (il `planner_system_text` fino al RUN_ID) a ~75.
    ///
    /// Guarda la CONSEGUENZA in due punti, perche' i lettori sono due: il
    /// fornitore a cache automatica legge i primi caratteri del testo, il
    /// `prompt_cache_key` del gateway legge la `parte_stabile`. Un test sul solo
    /// numero di caratteri in comune non basterebbe: il preambolo del focus e'
    /// fisso e ne vale ~100, quindi due prompt entrambi difettosi ne
    /// condividerebbero piu' del system stesso.
    #[test]
    fn il_focus_del_turno_non_apre_il_prompt_del_planner() {
        let cfg = cfg_active();
        let testa = cfg.planner_system_text.clone();
        let node = node_with(cfg, Arc::new(StubTodoStore::with_todos(vec![])));

        let a = stato_con_task("crea index.html con la pagina di login");
        let b = stato_con_task("correggi il calcolo del totale in spese.ts");
        let ha = node.build_hinted_system(&a, "run-1");
        let hb = node.build_hinted_system(&b, "run-2");

        // Il focus c'e' davvero: senza questo il resto del test passerebbe anche
        // su un prompt in cui la directive non e' mai stata iniettata.
        assert!(
            ha.contains("crea index.html"),
            "il focus non e' stato iniettato: {ha}"
        );
        assert!(hb.contains("correggi il calcolo del totale"));

        // (1) Il primo carattere che il fornitore vede e' il system del planner.
        assert!(
            ha.starts_with(&testa),
            "un blocco variabile apre il prompt: {ha:.200}"
        );
        assert!(hb.starts_with(&testa));

        // (2) La parte su cui il gateway costruisce l'identita' del prefisso non
        // porta il focus, quindi due run non ricevono chiavi diverse per averlo
        // cambiato.
        let stabile = nexus_types::system_prompt::parte_stabile(&ha);
        assert!(
            !stabile.contains("crea index.html"),
            "il focus e' finito nella parte stabile: {stabile}"
        );
        assert!(
            stabile.starts_with(&testa),
            "la parte stabile deve partire dal system del planner: {stabile:.200}"
        );

        // Idempotenza del punto unico: il confine resta uno.
        assert_eq!(
            ha.matches(nexus_types::system_prompt::CONFINE_DI_TURNO)
                .count(),
            1
        );
    }

    /// Il verso opposto, che tiene onesto il test sopra: col focus spento il
    /// prompt non porta ne' la directive ne' il confine, cioe' resta bit-identico
    /// a quello di un run senza turn_focus.
    #[test]
    fn focus_spento_non_lascia_traccia_nel_prompt_del_planner() {
        let cfg = PlannerConfig {
            turn_focus_enabled: false,
            ..cfg_active()
        };
        let node = node_with(cfg, Arc::new(StubTodoStore::with_todos(vec![])));
        let h = node.build_hinted_system(&stato_con_task("crea index.html"), "run-1");
        assert!(!h.contains("FOCUS DEL TURNO"));
        assert!(!h.contains(nexus_types::system_prompt::CONFINE_DI_TURNO));
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA + rami ON
    //! del planner. Lo script `scripts/gen_golden_planner.py` importa/replica le
    //! funzioni reali (`is_eligible`, decision di riuso piano, branching
    //! clarifying, tool catalog, fallback chain decision, `build_hinted_system`
    //! sui soli rami ON, `build_tool_input`, parse) e salva `{case_id, function,
    //! input, output}` in `/tmp/golden_planner.json`. Qui ricostruiamo l'input,
    //! chiamiamo la funzione Rust corrispondente e verifichiamo
    //! `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_planner.py
    //!   cargo test -p nexus-agent-graph --lib golden_planner_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::{
        build_tool_input, clarifying_branch, parse_tool_result, plan_reuse_decision, tool_catalog,
        ClarifyingBranch, PlanReuse, PlannerConfig, PlannerNode, ToolResultOutcome,
    };
    use crate::runtime::ports::PlanRow;
    use crate::state::{AgentState, Message, MessageContent};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Config dai campi dell'input golden (i 4 gate di is_eligible + i rami ON).
    fn cfg_from(input: &Value) -> PlannerConfig {
        let mut cfg = PlannerConfig::default();
        if let Some(b) = input.get("plan_phase_enabled").and_then(Value::as_bool) {
            cfg.plan_phase_enabled = b;
        }
        if let Some(a) = input.get("plan_behavior_modes").and_then(Value::as_array) {
            cfg.plan_behavior_modes = a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        if let Some(a) = input.get("plan_intents").and_then(Value::as_array) {
            cfg.plan_intents = a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        if let Some(n) = input.get("plan_min_token_budget").and_then(Value::as_i64) {
            cfg.plan_min_token_budget = n;
        }
        if let Some(s) = input.get("planner_system_text").and_then(Value::as_str) {
            cfg.planner_system_text = s.to_string();
        }
        if let Some(b) = input.get("turn_focus_enabled").and_then(Value::as_bool) {
            cfg.turn_focus_enabled = b;
        }
        cfg
    }

    /// Ricostruisce uno stato minimale per build_hinted_system: messages (solo
    /// testo) + context_brief.
    fn state_from(input: &Value) -> AgentState {
        let mut msgs: Vec<Message> = Vec::new();
        if let Some(arr) = input.get("messages").and_then(Value::as_array) {
            for m in arr {
                let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
                let text = m.get("content").and_then(Value::as_str).unwrap_or("");
                let content = MessageContent::text(text);
                msgs.push(match role {
                    "assistant" | "ai" => Message::Ai {
                        content,
                        tool_calls: vec![],
                        reasoning: None,
                        thinking_signature: None,
                    },
                    _ => Message::Human { content },
                });
            }
        }
        let mut st = AgentState {
            messages: msgs,
            ..Default::default()
        };
        if let Some(b) = input.get("context_brief").and_then(Value::as_str) {
            st.context_brief = Some(b.to_string());
        }
        st
    }

    fn opt_str(input: &Value, key: &str) -> Option<String> {
        input.get(key).and_then(Value::as_str).map(str::to_string)
    }

    fn plan_reuse_label(r: PlanReuse) -> &'static str {
        match r {
            PlanReuse::NoPlan => "no_plan",
            PlanReuse::Stale => "stale",
            PlanReuse::Reuse => "reuse",
        }
    }

    fn clarifying_label(b: &ClarifyingBranch) -> Value {
        match b {
            ClarifyingBranch::Proceed => json!({"branch": "proceed"}),
            ClarifyingBranch::Halt { questions } => {
                json!({"branch": "halt", "questions": questions})
            }
            ClarifyingBranch::ApplyDefaults { assumptions } => {
                json!({"branch": "apply_defaults", "assumptions": assumptions})
            }
        }
    }

    #[test]
    #[ignore = "richiede /tmp/golden_planner.json generato da gen_golden_planner.py"]
    fn golden_planner_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_planner.json", "gen_golden_planner.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(
            cases.len() >= 25,
            "attesi >=25 casi golden, trovati {}",
            cases.len()
        );

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "is_eligible" => {
                    let cfg = cfg_from(&c.input);
                    let bm = opt_str(&c.input, "behavior_mode");
                    let it = opt_str(&c.input, "intent");
                    let tb = c
                        .input
                        .get("token_budget")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    json!(cfg.is_eligible(bm.as_deref(), it.as_deref(), tb))
                }
                "plan_reuse" => {
                    // existing: null | {user_intent?, behavior_mode?}
                    let existing = c.input.get("existing").and_then(|v| {
                        if v.is_null() {
                            None
                        } else {
                            Some(PlanRow {
                                user_intent: v
                                    .get("user_intent")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                behavior_mode: v
                                    .get("behavior_mode")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            })
                        }
                    });
                    let it = opt_str(&c.input, "intent");
                    let bm = opt_str(&c.input, "behavior_mode");
                    json!(plan_reuse_label(plan_reuse_decision(
                        existing.as_ref(),
                        it.as_deref(),
                        bm.as_deref()
                    )))
                }
                "clarifying_branch" => {
                    use crate::state::AutomationMode;
                    let questions = c
                        .input
                        .get("questions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let auto_mode = c
                        .input
                        .get("automation_mode")
                        .and_then(Value::as_str)
                        .and_then(|s| match s {
                            "confirm" => Some(AutomationMode::Confirm),
                            "none" => Some(AutomationMode::None),
                            "automatic" => Some(AutomationMode::Automatic),
                            "continuous" => Some(AutomationMode::Continuous),
                            _ => None,
                        });
                    clarifying_label(&clarifying_branch(&questions, auto_mode))
                }
                "tool_catalog" => {
                    // La lista di un solo tool; confronto strutturale JSON.
                    json!(tool_catalog())
                }
                "build_hinted_system" => {
                    let cfg = cfg_from(&c.input);
                    let st = state_from(&c.input);
                    let run_id = c.input.get("run_id").and_then(Value::as_str).unwrap_or("");
                    // build_hinted_system e' un metodo del nodo: serve un'istanza.
                    let node = PlannerNode::new(
                        cfg,
                        "p".to_string(),
                        "m".to_string(),
                        "fp".to_string(),
                        "fm".to_string(),
                        std::sync::Arc::new(
                            crate::runtime::test_doubles::StubTodoStore::with_todos(vec![]),
                        ),
                        std::sync::Arc::new(
                            crate::runtime::test_doubles::StubMetaStepStore::default(),
                        ),
                        std::sync::Arc::new(crate::runtime::StubMetaReasonerPort),
                    );
                    json!(node.build_hinted_system(&st, run_id))
                }
                "build_tool_input" => {
                    let block = c.input.get("todo_block").cloned().unwrap_or(json!({}));
                    let run_id = c.input.get("run_id").and_then(Value::as_str).unwrap_or("");
                    let up = c
                        .input
                        .get("used_provider")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let um = c
                        .input
                        .get("used_model")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let it = opt_str(&c.input, "intent");
                    let bm = opt_str(&c.input, "behavior_mode");
                    build_tool_input(&block, run_id, up, um, it.as_deref(), bm.as_deref())
                }
                "parse_tool_result" => {
                    let rj = c
                        .input
                        .get("result_json")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    json!(matches!(parse_tool_result(rj), ToolResultOutcome::Ok))
                }
                other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
            };

            assert!(
                got == c.output,
                "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
                c.case_id,
                c.function,
                got,
                c.output
            );
            checked += 1;
        }
        println!("golden planner: {checked} casi verificati, tutti verdi");
    }
}
