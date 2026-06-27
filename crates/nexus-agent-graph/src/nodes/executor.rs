//! `ExecutorNode` — porta il CUORE del grafo, `executor_node`
//! (`brain/agents/nodes/__init__.py:1648-3513`).
//!
//! E' la META' del loop agentico che CHIAMA il modello: dato lo stato (history +
//! tool dichiarati + system) esegue UN turno, producendo i `pending_tool_uses`
//! (forma tool_use) + `stop_reason`, oppure una chiusura (end_turn/abort/G1).
//! L'altra meta' (`ToolDispatchNode`) esegue i pending e ritorna i tool_result;
//! il LOOP e' guidato dal RUNTIME (edge `tool_dispatch -> executor`, edge
//! `g1_continue -> executor`): QUESTO nodo fa il SINGOLO turno, NON instrada.
//! La decisione post-executor vive in [`crate::routing::route_after_executor`]
//! (gia' portata 1:1, NON duplicata qui).
//!
//! ## Riuso (regola L, nessuna logica deterministica ri-implementata qui)
//!
//! - `decisions::loop_signatures`: [`build_signature`], [`detect_signature_loop`]
//!   ([`LoopDetection`], cap [`RECENT_SIGNATURES_CAP`], soglia [`LOOP_THRESHOLD`]),
//!   [`exploration_counter_update`].
//! - `routing::signals`: TUTTI i detector pre-LLM/G1 — [`detect_repeated_failed_command`],
//!   [`detect_repeated_action`], [`count_recent_request_port`],
//!   [`has_active_resources_in_history`], [`detect_recent_tool_error`],
//!   [`detect_unfulfilled_intent`], [`unfulfilled_signal`],
//!   [`has_tool_calls_in_history`], lista [`EXPLORATION_ONLY_TOOLS`].
//! - `decisions::g1_accounting`: [`g1_accounting`] (CONTEGGIO re-entry/cap G1).
//! - `decisions::escalation`: [`pick_escalation_model`] (SELEZIONE pura del modello
//!   di auto-escalation, Tier 1 catena intra-provider + Tier 2 cross-provider,
//!   cooldown-aware) — usata dal signature-loop e dal cap G1.
//! - `decisions::progress_controller`: [`pc_decide`] (DECISIONE escalation/abort/
//!   guide/force_diagnose) — punto unico dei nudge anti-stallo.
//! - `decisions::helpers`: [`should_force_tool_choice`],
//!   [`provider_style_supports_forcing`], [`turn_action_oriented`],
//!   [`structural_unfulfilled_signal`].
//! - `decisions::turn_focus`: [`build_turn_focus_directive`] + [`inject_turn_focus`]
//!   (via primitiva + marker, da `context_reduction`).
//! - `decisions::context_reduction`: [`should_compress_now`] + dedup/drop/compress/
//!   token_brake + i 5 `inject_*` (lang/turn_focus/verification/forced_rag).
//! - `decisions::m16`: [`parse_discovered_tools`] (non usato qui: il parsing e' nel
//!   dispatch) — qui si usa solo l'INIEZIONE dei discovered come native.
//! - `decisions::tool_dispatch`: [`estimate_context_chars`] /
//!   [`current_context_token_estimate`] / [`ContextMessage`] (stime) + costanti.
//! - Trait (`runtime::ports`): [`LlmGateway`] (chiamata LLM con
//!   force_tool_choice/system_text/max_tokens), [`RunControlStore`]
//!   (is_superseded/heartbeat/set_effective_model), [`AgentStepStore`],
//!   [`MetaStepStore`], [`EventSink`], [`EscalationPort`] (input I/O dell'auto-
//!   escalation: catena DB + cooldown + cross-provider), [`NextActionsDeriver`]
//!   (derivazione scelte di proseguimento), [`BillingCooldownPort`] (lista
//!   provider in cooldown billing per il fail-fast), [`ModelUpscalePort`] (window
//!   modello + selezione upscale catalog). Sono CAMPI del nodo (coerente con
//!   `ToolDispatchNode`/`FinalGateNode`).
//!
//! ## Ordine 1:1 (TESTA -> NUDGE -> LLM -> POST), load-bearing
//!
//! TESTA (gate/early-return, ordine esatto del Python):
//!  1. `_check_superseded` -> early return `stop_reason=superseded` (`:1669`).
//!  2. `declared_outcome=done` & `declared_done_count>=3` -> end_turn (`:1683`).
//!  3. M16 merge discovered (iniezione native, cap DB) + strategia discovery-only
//!     search (`:1714-1759`).
//!  4. worker-mode (`:1792`) + DAG parallelo (`:1832`): rami GENUINAMENTE OFF di
//!     default (`worker_mode_enabled`/`dag_parallel_enabled` false),
//!     SubagentDispatcher = PR-J. TODO esplicito (coi flag OFF NON divergono).
//!  5. G1 cap/reentry (`:1882-2042`): [`g1_accounting`] (conteggio) + decisione cap.
//!     Al cap: auto-escalation orchestratore (`:1962-1993`) via [`pick_escalation_model`]
//!     (input dalla porta [`EscalationPort`]) -> se promuove, sticky al modello
//!     escalato + nudge "ESEGUI subito" + `g1_escalated` + reroute azzerato (NON
//!     ri-chiama l'LLM: il self-loop rientra in executor); a escalation esaurita,
//!     cap secco `g1_cap_reached`.
//!  6. NUDGE pre-LLM IN ORDINE (CRITICO): esplorazione -> comando-fallito ->
//!     repeated_action -> resource_reallocation -> G1. Replica ESATTA dell'ordine
//!     e delle mutazioni (messaggi/tools/system_text/force_action_hard).
//!
//! RISOLUZIONE provider/model: sticky > override > routing-risolto-a-monte (regola
//! G: il `routing_*` arriva GIA' deciso nella `ExecutorConfig`); sentinelle
//! `__router_unavailable__`/`__no_capable_provider__` -> `NodeError`.
//!
//! CONTEXT_REDUCTION: [`should_compress_now`] + dedup/drop/compress/token_brake +
//! i 5 `inject_*`. Le parti I/O (summarizer/offload/continuity-trim/rolling/
//! smart-upscale + model_context_window) sono ON/attive in produzione - NON
//! portate (richiedono trait dedicati) - DIVERGONO al wiring - da completare in
//! PR-J PRIMA del cutover (vedi classe (B) in "Cosa NON porta").
//!
//! LLM CALL: `max_tokens = max(8192, min(token_budget*4, 16384))`;
//! [`LlmGateway::complete`] con `force_tool_choice` (da [`should_force_tool_choice`]),
//! `system_text`, `max_tokens`. Retry-senza-forcing su risposta malformed (il nodo
//! gestisce il retry; cascade/sticky e' lato gateway concreto). Costruisce il
//! `Message::Ai` con `assistant_content` (blocchi tool_use) coerente coi pending.
//!
//! POST: [`build_signature`] sui pending + [`detect_signature_loop`] (coda cap 12);
//! auto-escalation nel signature loop (`:3159-3284`) PORTATA: alla rilevazione del
//! loop l'orchestratore PROMUOVE il modello via [`pick_escalation_model`] (punto
//! unico puro, input dalla porta [`EscalationPort`]) e RI-ESEGUE il turno col
//! modello promosso (ramo DOMINANTE `tried_escalation`: `auto_escalations`+1,
//! `provider`/`model` riassegnati, pending dalla 2a risposta, stop_reason di quella
//! risposta — NON `loop_detected`). Solo a escalation NON disponibile (catena
//! esaurita / provider in cooldown senza cross / `auto_escalations >= 3`) si chiude
//! secco con `loop_detected` (ramo MINORITARIO `not tried_escalation`).
//! [`exploration_counter_update`]; meta_step `executor_call` (EventSink +
//! MetaStepStore gata Real); delta con iterations+1, pending, stop_reason, messages,
//! provider_used/model_used, recent_tool_signatures, auto_escalations, ecc.
//!
//! ## SHADOW (`ExecMode::Replay`)
//!
//! Scritture gated shadow no-op in Replay: heartbeat/set_effective_model
//! (RunControlStore), persist meta_step (MetaStepStore). La chiamata LLM in shadow
//! (sia il turno principale sia la RI-chiamata dell'auto-escalation del
//! signature-loop) segue il pattern del crate: oggi [`LlmGateway`] NON ha
//! `ExecMode` (come per reflection/planner/clarify) — la pendenza e' documentata,
//! il gateway concreto la gestira' (un run shadow non emette eventi: `EventSink`
//! no-op nel ctx shadow). La porta [`EscalationPort`] e' SOLA LETTURA (catena +
//! cooldown + cross-provider): nessun gate `mode`.
//!
//! ## Cosa NON porta — DUE classi (etichettatura onesta, regola H)
//!
//! ### (A) Rami GENUINAMENTE OFF di default — coi default NON divergono
//!
//! - worker-mode + DAG parallelo ([`SubagentDispatcher`] = PR-J): default OFF
//!   (`worker_mode_enabled`/`dag_parallel_enabled` false) NON divergono.
//! - `closure_judge.judge` (LLM): default OFF (`agent.closure_judge.active=false`).
//! - `plan_rationale` injection (`:1766`): default OFF (`plan_rationale_enabled`) —
//!   portato come iniezione pura solo quando il flag e' ON.
//!
//! ### (B) Rami ON/SEEDATI in produzione
//!
//! PORTATI in PR-J2 (parte DETERMINISTICA pura + I/O dietro trait):
//!  - `next_actions.derive` (`:3379-3402`): la RIMOZIONE del blocco
//!    `<suggested_actions>` e' pura ([`strip_suggested_actions`], SEMPRE applicata
//!    a end_turn); la DERIVAZIONE scelte e' I/O dietro [`NextActionsDeriver`]
//!    (best-effort) + emissione/persistenza meta_step `next_actions`.
//!  - unfulfilled-report (`:3404-3429`): gate puro
//!    ([`should_substitute_unfulfilled_report`]: NON autonoma + unfulfilled + NON
//!    action-oriented) + resoconto deterministico ([`build_unfulfilled_report`]).
//!  - `_billing_exhausted_providers` fail-fast (`:2072-2092`): DECISIONE pura
//!    ([`billing_fail_fast_message`]: soglia esplorazione + provider esausti ->
//!    `loop_abort`); la lista provider e' I/O dietro [`BillingCooldownPort`]
//!    (fail-open). Posizione 1:1: PRE-LLM, prima del 2x soglia.
//!  - smart-upscale / model_context_window (`:2812-2830`): DECISIONE pura
//!    ([`should_upscale`] >=90% window + [`upscale_required_tokens`]); il lookup
//!    window + la SELEZIONE catalog tier-based sono I/O dietro [`ModelUpscalePort`]
//!    (best-effort). Posizione 1:1: PRE-token-brake, dopo la risoluzione provider.
//!
//! ANCORA NON portati (I/O context, DIVERGONO al wiring, da completare prima del cutover):
//!  - summarizer / offload / continuity-trim / rolling-summary (riduzione contesto
//!    I/O): le parti PURE sono in `context_reduction`; le parti LLM/embeddings
//!    restano TODO -> trait dedicati.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::context_reduction::{
    self as ctxr, CompressParams, CtxMgmtConfig, HistoryMessage, TokenBrakeConfig,
};
use crate::decisions::end_turn::{
    billing_fail_fast_message, build_unfulfilled_report, should_substitute_unfulfilled_report,
    should_upscale, strip_suggested_actions, upscale_required_tokens,
};
use crate::decisions::escalation::pick_escalation_model;
use crate::decisions::g1_accounting::{g1_accounting, G1Signals};
use crate::decisions::helpers::{
    provider_style_supports_forcing, should_force_tool_choice, turn_action_oriented,
};
use crate::decisions::loop_signatures::{
    build_signature, detect_signature_loop, exploration_counter_update,
};
use crate::decisions::progress_controller::{self as pc, Action, ProgressSignals};
use crate::decisions::tool_dispatch::{
    current_context_token_estimate, estimate_context_chars, ContextMessage,
};
use crate::decisions::turn_focus::build_turn_focus_directive;
use crate::routing::signals::{
    count_recent_request_port, detect_recent_tool_error, detect_repeated_action,
    detect_repeated_failed_command, detect_unfulfilled_intent, has_active_resources_in_history,
    has_tool_calls_in_history, EXPLORATION_ONLY_TOOLS,
};
use crate::runtime::ports::{
    AgentStepStore, BillingCooldownPort, EscalationPort, LlmMessage, LlmRequest, MetaStepStore,
    ModelUpscalePort, NextActionsDeriver, RunControlStore, SseEvent,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{
    AgentState, ContentBlock, Message, MessageContent, StateDelta, StopReason, ToolUse,
};

/// Tool brain-only `task_complete` (vedi `tool_dispatch`): conta nel gate
/// dichiarazione `done` ripetuta.
const TASK_COMPLETE_TOOL_NAME: &str = "task_complete";

/// Sentinelle del gate ADR 0020 (provider non disponibile): `route_model` /
/// `purpose_model` possono ritornarle quando tutti i capable sono in cooldown.
const SENTINELS: &[&str] = &["__router_unavailable__", "__no_capable_provider__"];

/// Meta-tool di discovery M16 (`_DISCOVERY_META` Python): set esposto al turno
/// di scoperta (solo `nexus_mcp_tool_search` resta forzato).
const DISCOVERY_META: &[&str] = &["nexus_mcp_tool_search", "nexus_mcp_tool_call"];

/// Config DB-driven dell'`ExecutorNode`, PASSATA (regola G: nessuna lettura DB nel
/// nodo, nessun fallback hardcoded nella logica decisionale).
///
/// Mappa i settings letti dal brain inline nell'executor + il provider/model
/// RISOLTI A MONTE dalla routing matrix (regola G: il nodo li riceve gia' decisi,
/// non chiama il router). Con i default vale il comportamento di OGGI (rami OFF).
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutorConfig {
    /// Cap massimo di re-routing G1 (`agent.g1_max_nudges`, default 3).
    pub g1_max_nudges: i64,
    /// Soglia di esplorazione consecutiva (`agent.exploration_loop_threshold`,
    /// default 6): a soglia nudge anti-esplorazione, a 2x abort/guide.
    pub exploration_loop_threshold: i64,
    /// `true` se il progress_controller e' attivo
    /// (`agent.progress_controller.enabled`).
    pub progress_controller_enabled: bool,
    /// Soglia ripetizione azione produttiva (`agent.repeated_action_threshold`,
    /// default 2).
    pub repeated_action_threshold: i64,
    /// `true` abilita lo stadio force_diagnose per repeated_action
    /// (`agent.repeated_action.force_diagnose_enabled`).
    pub repeated_action_force_diagnose_enabled: bool,
    /// Soglia riallocazione porte (`agent.reallocation_threshold`, default 3).
    pub reallocation_threshold: i64,
    /// `true` abilita il tool_choice forcing (`agent.tool_choice_forcing.enabled`).
    pub tool_choice_forcing_enabled: bool,
    /// Iterazione massima oltre cui il forcing "early action" non si applica
    /// (`agent.tool_choice_forcing.max_iteration`).
    pub tool_choice_forcing_max_iteration: i64,
    /// Stile tool_choice della capability del modello del turno (vista 0318,
    /// regola G): risolto a monte. `None` = forcing non supportato.
    pub tool_choice_style: Option<String>,
    /// Cap dei tool scoperti iniettati come native (`agent.tools.discovery_max_injected`,
    /// default 20).
    pub discovery_max_injected: usize,
    /// `true` se il run e' il primo turno agente (`is_first_agent_turn`): risolto
    /// a monte (richiederebbe la logica `_schema_utils`); abilita la strategia
    /// M16 discovery-only search.
    pub is_first_agent_turn: bool,
    /// Config del context management (compress phases), DB-driven (mig 0199).
    pub ctx_mgmt: CtxMgmtConfig,
    /// Reminder lingua (`_load_language_reminder`): `(enabled, text)`.
    pub language_reminder_enabled: bool,
    /// Testo del reminder lingua risolto a monte.
    pub language_reminder_text: String,
    /// `true` se il turn_focus e' attivo (`agent.context.turn_focus_enabled`).
    pub turn_focus_enabled: bool,
    /// Direttiva auto-verifica: `(enabled, text)` (`_load_verification_directive`).
    pub verification_directive_enabled: bool,
    /// Testo della direttiva di verifica risolto a monte.
    pub verification_directive_text: String,
    /// `true` se l'utente ha chiesto auto-verifica (`_detect_verification_request`,
    /// lessicale): risolto a monte (il detection e' un punto fuori nodo).
    pub verification_requested: bool,
    /// Reminder forced-RAG: `(text, ratio)` (`_load_forced_rag_reminder`).
    pub forced_rag_reminder_text: String,
    /// Ratio del forced-RAG reminder (`forced_rag_threshold_ratio`).
    pub forced_rag_ratio: f64,
    /// Config del freno token (max_context_ratio / aggressive_*).
    pub token_brake: TokenBrakeConfig,
    /// Context window (token) del modello del turno (catalogo, regola G). `0` =
    /// window ignoto -> token_brake/forced_rag no-op.
    pub context_window: i64,
    /// Provider del purpose `agent_tier_*` RISOLTO A MONTE (regola G): vuoto o
    /// sentinella -> `NodeError` (no provider). Usato solo se niente sticky/override.
    pub routing_provider: String,
    /// Modello RISOLTO A MONTE (regola G).
    pub routing_model: String,
    /// Cap globale di iterazioni del run (`iteration_budget`/`MAX_AGENT_ITERATIONS`):
    /// soglia forced-text = `cap - 5`.
    pub iteration_cap: i64,
    /// `true` se lo smart-upscale e' attivo (`agent.upscale.enabled`, default ON
    /// in produzione): promuove a un modello con window piu' grande se il contesto
    /// stimato supera il window del modello corrente (PRIMA della chiamata LLM).
    pub upscale_enabled: bool,
    /// Ratio di overhead per il window richiesto all'upscale
    /// (`agent.upscale.target_overhead_ratio`, default 1.2): `required =
    /// est_tokens * ratio`. Il tier e la query catalog vivono nell'impl della porta.
    pub upscale_overhead_ratio: f64,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        // Default IDENTICI ai safe-default del brain (valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica). Rami OFF.
        Self {
            g1_max_nudges: 3,
            exploration_loop_threshold: 6,
            progress_controller_enabled: false,
            repeated_action_threshold: 2,
            repeated_action_force_diagnose_enabled: false,
            reallocation_threshold: 3,
            tool_choice_forcing_enabled: false,
            tool_choice_forcing_max_iteration: 2,
            tool_choice_style: None,
            discovery_max_injected: 20,
            is_first_agent_turn: true,
            ctx_mgmt: CtxMgmtConfig {
                compress_start_iter: 5,
                compress_phase_boundaries: vec![5, 10, 20, 50],
                compress_phase_keep_recent: vec![8, 5, 3, 2],
                compress_phase_max_chars: vec![2000, 1000, 500, 150],
            },
            language_reminder_enabled: false,
            language_reminder_text: String::new(),
            turn_focus_enabled: true,
            verification_directive_enabled: false,
            verification_directive_text: String::new(),
            verification_requested: false,
            forced_rag_reminder_text: String::new(),
            forced_rag_ratio: 0.0,
            token_brake: TokenBrakeConfig {
                max_context_ratio: 0.70,
                aggressive_keep_recent: 3,
                aggressive_max_chars: 200,
            },
            context_window: 0,
            routing_provider: String::new(),
            routing_model: String::new(),
            iteration_cap: 60,
            // Default safe-DB-down: upscale OFF (il wiring mcp-core passa il valore
            // reale `agent.upscale.enabled`, ON in produzione). Coerente con la nota
            // "rami OFF coi safe-default" sopra: con questo default lo smart-upscale
            // non scatta (parita' col Python quando enabled=false).
            upscale_enabled: false,
            upscale_overhead_ratio: 1.2,
        }
    }
}

/// Nodo executor. Le porte I/O (`RunControlStore`, `AgentStepStore`,
/// `MetaStepStore`) sono CAMPI del nodo (come `ToolDispatchNode`); LLM e
/// EventSink arrivano dal `AgentNodeCtx`. La config DB-driven (incluso
/// provider/model RISOLTI A MONTE, regola G) e' nella [`ExecutorConfig`].
pub struct ExecutorNode {
    /// Config DB-driven (regola G: passata, mai letta dal nodo).
    cfg: ExecutorConfig,
    /// Controllo run condiviso (superseded + heartbeat + modello effettivo).
    /// PUNTO UNICO (regola L) con il tool_dispatch.
    run_control: Arc<dyn RunControlStore>,
    /// Persistenza meta-step (`executor_call` heartbeat), gata Real.
    meta_steps: Arc<dyn MetaStepStore>,
    /// Persistenza step incrementale (non usata nel turno LLM: gli step tool si
    /// persistono nel dispatch; tenuto per simmetria/uso futuro). Gata Real.
    #[allow(dead_code)]
    steps: Arc<dyn AgentStepStore>,
    /// Porta I/O dell'auto-escalation (catena DB + cooldown + cross-provider).
    /// La SELEZIONE e' del modulo puro [`pick_escalation_model`] (regola L); qui
    /// la porta fornisce solo gli input gia' risolti.
    escalation: Arc<dyn EscalationPort>,
    /// Porta I/O della derivazione scelte di proseguimento (`next_actions`). La
    /// RIMOZIONE deterministica del blocco `<suggested_actions>` e' del modulo puro
    /// [`strip_suggested_actions`] (regola L); qui la porta deriva le scelte
    /// (parse/fallback/LLM). Best-effort: errore -> nessuna scelta.
    next_actions: Arc<dyn NextActionsDeriver>,
    /// Porta I/O della lista provider in cooldown billing (fail-fast esplorazione).
    /// La DECISIONE (gate soglia + messaggio) e' del modulo puro
    /// [`billing_fail_fast_message`] (regola L). Fail-open: errore -> nessun esausto.
    billing: Arc<dyn BillingCooldownPort>,
    /// Porta I/O dello smart-upscale (window corrente + selezione modello target).
    /// La DECISIONE (`should_upscale` + `required`) e' del modulo puro (regola L).
    /// Best-effort: errore -> nessun upscale.
    upscale: Arc<dyn ModelUpscalePort>,
}

impl ExecutorNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta e le porte I/O.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: ExecutorConfig,
        run_control: Arc<dyn RunControlStore>,
        meta_steps: Arc<dyn MetaStepStore>,
        steps: Arc<dyn AgentStepStore>,
        escalation: Arc<dyn EscalationPort>,
        next_actions: Arc<dyn NextActionsDeriver>,
        billing: Arc<dyn BillingCooldownPort>,
        upscale: Arc<dyn ModelUpscalePort>,
    ) -> Self {
        Self {
            cfg,
            run_control,
            meta_steps,
            steps,
            escalation,
            next_actions,
            billing,
            upscale,
        }
    }
}

/// Risultato della risoluzione provider/model (sticky > override > routing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderResolution {
    /// Provider/model risolti.
    Resolved(String, String),
    /// Sentinella/nessun provider: il run si ferma (parita' col `raise` Python).
    NoProvider(String),
}

/// Esito dei gate di TESTA dell'executor (primo che scatta, priorita' esatta del
/// Python: superseded -> declared-done>=3 -> G1 cap). `Proceed` = nessun gate di
/// testa, si prosegue ai nudge/LLM. PURO (punto unico della decisione di testa,
/// regola L: il `run` delega qui, il golden la verifica).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadGate {
    /// `_check_superseded` -> stop_reason superseded (py:1669).
    Superseded,
    /// `declared_outcome=done` & count>=3 -> chiusura d'autorita' end_turn (py:1683).
    DeclaredDone,
    /// G1 cap raggiunto (escalation esaurita) -> g1_cap_reached (py:1945).
    G1Cap,
    /// Nessun gate di testa: prosegue ai nudge/LLM.
    Proceed,
}

/// Decisione PURA dei gate di TESTA (priorita' 1:1 col Python). `g1_cap_reached`
/// arriva gia' calcolato da [`g1_accounting`] (punto unico). PURA.
pub(crate) fn head_gate(
    superseded: bool,
    declared_done: bool,
    declared_done_count: i64,
    g1_cap_reached: bool,
) -> HeadGate {
    if superseded {
        return HeadGate::Superseded;
    }
    if declared_done && declared_done_count >= 3 {
        return HeadGate::DeclaredDone;
    }
    if g1_cap_reached {
        return HeadGate::G1Cap;
    }
    HeadGate::Proceed
}

/// Risolve provider/model: sticky > override > routing-risolto-a-monte
/// (py:2460-2521). PURA (punto unico, regola L): il metodo del nodo e il golden
/// la chiamano entrambi. Sticky/override saltano il gate sentinella (scelta utente
/// vincolante); la risoluzione automatica e' soggetta al gate ADR 0020.
pub(crate) fn resolve_provider_model(
    sticky_provider: Option<&str>,
    sticky_model: Option<&str>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    routing_provider: &str,
    routing_model: &str,
) -> ProviderResolution {
    let sp = sticky_provider.filter(|s| !s.is_empty());
    let sm = sticky_model.filter(|s| !s.is_empty());
    let prov = sp.or_else(|| provider_override.filter(|s| !s.is_empty()));
    let modl = sm.or_else(|| model_override.filter(|s| !s.is_empty()));
    if let (Some(p), Some(m)) = (prov, modl) {
        return ProviderResolution::Resolved(p.to_string(), m.to_string());
    }
    let p = prov.map(str::to_string).unwrap_or_else(|| routing_provider.to_string());
    let m = modl.map(str::to_string).unwrap_or_else(|| routing_model.to_string());
    if p.is_empty() || SENTINELS.contains(&p.as_str()) || SENTINELS.contains(&m.as_str()) {
        return ProviderResolution::NoProvider(p);
    }
    ProviderResolution::Resolved(p, m)
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ExecutorNode {
    fn id(&self) -> NodeId {
        NodeId::Executor
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        let run_id = state.thread_id.clone().unwrap_or_default();
        let mode = ctx.exec_mode();
        let iters_in = state.iterations.unwrap_or(0);

        // ── (1)+(2) Gate di TESTA (superseded -> declared-done>=3) ────────────
        // Punto unico [`head_gate`] (regola L): qui g1_cap non e' ancora noto
        // (conteggio sotto), quindi passiamo `false`; il G1 cap e' valutato dopo
        // il conteggio con la stessa funzione (priorita' 1:1 col Python).
        let superseded = ctx.cancel.is_cancelled()
            || self
                .run_control
                .is_superseded(&run_id)
                .await
                .unwrap_or(false);
        let declared_done = state
            .declared_outcome
            .as_ref()
            .and_then(|d| d.get("outcome").and_then(Value::as_str))
            == Some("done");
        let done_count = state.declared_done_count.unwrap_or(0);
        match head_gate(superseded, declared_done, done_count, false) {
            HeadGate::Superseded => {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    thread = %run_id,
                    "run superato/cancellato, uscita cooperativa (no chiamata modello)"
                );
                return Ok(StateDelta {
                    stop_reason: Some(Some(StopReason::Superseded)),
                    ..Default::default()
                }
                .into_opaque());
            }
            HeadGate::DeclaredDone => {
                return Ok(self.close_declared_done(state, iters_in));
            }
            // G1Cap qui non puo' scattare (passato false); Proceed -> prosegue.
            HeadGate::G1Cap | HeadGate::Proceed => {}
        }

        // Stato di lavoro mutabile (replica le variabili locali del Python che i
        // nudge mutano: messages / tools_json / system_text).
        let mut messages: Vec<Message> = state.messages.clone();
        let mut tools_json: Vec<Value> = state.tools_json.clone().unwrap_or_default();
        let mut system_text: String = state.system_text.clone().unwrap_or_default();
        let _ = &mut system_text; // mutato dai rami plan_rationale / delega (sotto)

        // ── plan_rationale injection (py:1766, ramo ON se presente) ───────────
        if let Some(rat) = state.plan_rationale.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let mut block = vec!["<piano_razionale>".to_string(), rat.to_string()];
            let constraints = state.plan_constraints.clone().unwrap_or_default();
            if !constraints.is_empty() {
                block.push(format!("Vincoli/non-goal: {}", constraints.join("; ")));
            }
            let alternatives = state.plan_alternatives.clone().unwrap_or_default();
            let alts: Vec<String> = alternatives
                .iter()
                .filter_map(|a| {
                    let o = a.as_object()?;
                    let opt = o.get("option").and_then(Value::as_str).unwrap_or("?");
                    let rej = o.get("rejected_because").and_then(Value::as_str).unwrap_or("?");
                    Some(format!("{opt} (scartata: {rej})"))
                })
                .collect();
            if !alts.is_empty() {
                block.push(format!("Alternative scartate: {}", alts.join("; ")));
            }
            block.push("</piano_razionale>".to_string());
            system_text = format!("{}\n\n{system_text}", block.join("\n"));
        }

        // ── (3) M16: merge tool scoperti come native (py:1714-1736) ───────────
        let discovered: Vec<Value> = state
            .discovered_tools_run
            .clone()
            .filter(|v| !v.is_empty())
            .or_else(|| state.discovered_tools_next_turn.clone())
            .unwrap_or_default();
        if !discovered.is_empty() && !tools_json.is_empty() {
            let mut existing: HashSet<String> = tools_json
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
                .collect();
            let mut added = 0usize;
            for dt in &discovered {
                if added >= self.cfg.discovery_max_injected {
                    break;
                }
                if let Some(name) = dt.get("name").and_then(Value::as_str) {
                    if !name.is_empty() && !existing.contains(name) {
                        tools_json.push(dt.clone());
                        existing.insert(name.to_string());
                        added += 1;
                    }
                }
            }
            if added > 0 {
                tracing::info!(
                    target: "nexus_agent_graph::executor",
                    added,
                    tools = tools_json.len(),
                    "M16: iniettati tool scoperti come native"
                );
            }
        }

        // ── M16 strategia "solo search" al turno di scoperta (py:1748-1759) ───
        if discovered.is_empty() && !tools_json.is_empty() {
            let names: HashSet<String> = tools_json
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
                .collect();
            let meta_set: HashSet<String> = DISCOVERY_META.iter().map(|s| s.to_string()).collect();
            if !names.is_empty() && names.is_subset(&meta_set) && self.cfg.is_first_agent_turn {
                tools_json.retain(|t| t.get("name").and_then(Value::as_str) == Some("nexus_mcp_tool_search"));
                tracing::info!(
                    target: "nexus_agent_graph::executor",
                    "M16: turno di scoperta -> espongo solo nexus_mcp_tool_search"
                );
            }
        }

        // ── (4) worker-mode + DAG parallelo: TODO (PR-J, SubagentDispatcher) ──
        // Rami OFF di default (worker_mode_enabled / dag_parallel_enabled false):
        // coi safe-default DB il Python NON li attraversa, quindi NON portarli ora
        // non divergono. Quando ON richiederanno il sotto-sistema sub-agenti.

        // ── (5) G1 cap/reentry: conteggio (g1_accounting) + decisione ─────────
        // ── (4c) CAP ASSOLUTO iterazioni: safety net finale anti-runaway ────
        // Chiude DETERMINISTICAMENTE il turno se il run ha raggiunto il tetto di
        // iterazioni (iteration_cap, DB-driven), anche quando ogni altro meccanismo
        // (G1 cap, progress_controller, forced-text) ha fallito: es. un modello che
        // ignora tool_choice=required e continua a descrivere senza agire. Evita il
        // runaway di iterazioni/costi osservato con gemini (45+ giri).
        if iters_in >= self.cfg.iteration_cap {
            let cap_text = format!(
                "Raggiunto il numero massimo di iterazioni ({}) senza completare il \
compito. Interrompo per evitare un ciclo infinito: riformula la richiesta in modo \
piu' specifico, oppure riprova con un modello piu' capace.",
                self.cfg.iteration_cap
            );
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                iters = iters_in,
                cap = self.cfg.iteration_cap,
                "CAP ASSOLUTO iterazioni raggiunto -> chiusura deterministica"
            );
            return Ok(StateDelta {
                messages: Some(vec![Message::Ai {
                    content: MessageContent::text(cap_text.clone()),
                    tool_calls: vec![],
                }]),
                result: Some(Some(cap_text)),
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::EndTurn)),
                iterations: Some(Some(iters_in + 1)),
                forced_close_unverified: Some(Some(true)),
                ..Default::default()
            }
            .into_opaque());
        }

        let mut g1_reroute_count = state.g1_reroute_count.unwrap_or(0);
        // Segnali derivati dai punti unici (regola L: non ricalcolati a mano).
        let unfulfilled_for_g1 = match closure_verdict_fulfilled(state) {
            Some(fulfilled) => !fulfilled,
            None => detect_unfulfilled_intent(last_assistant_text(&messages).as_deref()),
        };
        let g1 = g1_accounting(&G1Signals {
            prev_stop_reason: state.stop_reason.map(stop_reason_str),
            iterations: iters_in,
            has_pending: state
                .pending_tool_uses
                .as_ref()
                .map(|p| !p.is_empty())
                .unwrap_or(false),
            action_oriented: turn_action_oriented(state.action_oriented),
            unfulfilled: unfulfilled_for_g1,
            recent_error: detect_recent_tool_error(&messages, 4),
            current_count: g1_reroute_count,
            max_nudges: self.cfg.g1_max_nudges,
        });
        let is_g1_reentry = g1.is_reentry;
        g1_reroute_count = g1.updated_count;
        if is_g1_reentry {
            tracing::info!(
                target: "nexus_agent_graph::executor",
                reroute = g1_reroute_count,
                max = self.cfg.g1_max_nudges,
                "re-entry G1 rilevata"
            );
        }
        // G1 cap tramite il punto unico [`head_gate`] (regola L): qui superseded/
        // declared-done sono gia' esclusi (gestiti in testa), conta solo il cap.
        if matches!(head_gate(false, false, 0, g1.cap_reached), HeadGate::G1Cap) {
            // ESCALATION orchestratore (py:1962-1993): prima di arrenderci, l'
            // orchestratore PROMUOVE il turno a un modello piu' capace (catena DB
            // intra-provider + cross-provider loop_fallback_default), azzerando il
            // contatore reroute cosi' il nuovo modello ha il suo budget. La
            // SELEZIONE e' il punto unico puro [`pick_escalation_model`] (regola L);
            // gli input (catena/cooldown/cross) arrivano dalla porta. Solo a catena
            // ESAURITA (o auto_escalations >= 3) chiudiamo davvero al cap secco
            // (ramo `not _g1_picked`).
            let g1_cur_provider = state
                .provider_used
                .clone()
                .or_else(|| state.sticky_provider.clone())
                .or_else(|| state.provider_override.clone());
            let g1_cur_model = state
                .model_used
                .clone()
                .or_else(|| state.sticky_model.clone())
                .or_else(|| state.model_override.clone());
            let g1_escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
            // `_g1_picked = _pick(...) if _g1_escal < 3 else None` (py:1962-1966).
            let g1_picked = if g1_escal < 3 {
                let inputs = self
                    .escalation
                    .escalation_inputs(
                        state.user_intent.as_deref(),
                        g1_cur_provider.as_deref(),
                        g1_cur_model.as_deref(),
                    )
                    .await
                    .unwrap_or_default();
                pick_escalation_model(
                    &inputs.chain,
                    g1_cur_provider.as_deref(),
                    g1_cur_model.as_deref(),
                    g1_escal,
                    inputs.provider_in_cooldown,
                    inputs.cross_provider.as_ref(),
                )
            } else {
                None
            };
            if let Some(pick) = g1_picked {
                // Escalation orchestratore: sticky al modello promosso, reroute
                // azzerato, nudge "ESEGUI subito" (py:1967-1993). Il SELF-LOOP del
                // grafo rientra in executor che usera' lo sticky (NON ri-chiamiamo
                // l'LLM qui, parita' col return immediato del Python).
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    reroute = g1_reroute_count,
                    from_provider = g1_cur_provider.as_deref().unwrap_or(""),
                    to_provider = %pick.provider,
                    to_model = %pick.model,
                    "G1 cap, ESCALATION orchestratore -> azzero reroute e ri-do il turno"
                );
                let esc_nudge = human_msg(
                    "Il modello precedente ha solo descritto le azioni senza eseguirle \
dopo i tentativi previsti. Ora rispondi tu, che sei un modello piu' capace: NON \
descrivere, ESEGUI subito il prossimo step concreto con un tool call.",
                );
                let mut extra_out = state.extra.clone();
                extra_out.insert("auto_escalations".to_string(), json!(g1_escal + 1));
                return Ok(StateDelta {
                    messages: Some(vec![esc_nudge]),
                    sticky_provider: Some(Some(pick.provider)),
                    sticky_model: Some(Some(pick.model)),
                    g1_reroute_count: Some(Some(0)),
                    action_nudge_count: Some(Some(0)),
                    pending_tool_uses: Some(Some(vec![])),
                    stop_reason: Some(Some(StopReason::G1Escalated)),
                    iterations: Some(Some(iters_in + 1)),
                    extra: Some(extra_out),
                    ..Default::default()
                }
                .into_opaque());
            }
            // Catena esaurita / auto_escalations >= 3: cap secco G1 (py:1994-2003).
            let cap_text = format!(
                "Modello non risponde con azione dopo {} tentativi e anche i modelli \
piu' capaci provati in escalation non hanno agito. Fermo l'esecuzione: riformula \
la richiesta in modo piu' specifico.",
                self.cfg.g1_max_nudges
            );
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                reroute = g1_reroute_count,
                auto_escalations = g1_escal,
                "G1 cap raggiunto e catena escalation esaurita, interrompo"
            );
            return Ok(StateDelta {
                messages: Some(vec![Message::Ai {
                    content: MessageContent::text(cap_text.clone()),
                    tool_calls: vec![],
                }]),
                result: Some(Some(cap_text)),
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::G1CapReached)),
                forced_close_unverified: Some(Some(true)),
                iterations: Some(Some(iters_in + 1)),
                g1_reroute_count: Some(Some(g1_reroute_count)),
                action_nudge_count: Some(Some(state.action_nudge_count.unwrap_or(0))),
                ..Default::default()
            }
            .into_opaque());
        }

        // ── (6) NUDGE pre-LLM IN ORDINE (load-bearing) ────────────────────────
        // Stato dei contatori/flag/assi mutato dai nudge.
        let exploration_count = state.consecutive_exploration_calls.unwrap_or(0);
        let exploration_threshold = self.cfg.exploration_loop_threshold;
        let mut exploration_nudge_sent = state.exploration_nudge_sent.unwrap_or(false);
        let mut exploration_nudge_injected = false;
        let mut force_action_hard = false;
        let mut progress_guided: HashSet<String> =
            state.progress_guided_axes.clone().unwrap_or_default().into_iter().collect();
        let mut progress_diagnosed: HashSet<String> =
            state.progress_diagnosed_axes.clone().unwrap_or_default().into_iter().collect();
        let progress_on = self.cfg.progress_controller_enabled;

        // ── fail-fast billing-exhausted (py:2072-2092) ────────────────────────
        // Se l'esplorazione ha raggiunto la SOGLIA (non 2x) E i provider AI buoni
        // sono in cooldown billing/quota, NON insistere con nudge/escalation su un
        // modello di riserva che esplora senza concludere: la causa e' la ricarica
        // crediti, non il modello. Chiude SUBITO col messaggio onesto (loop_abort),
        // risparmiando i turni successivi. DECISIONE = punto unico puro
        // [`billing_fail_fast_message`] (regola L); la LISTA dei provider esausti
        // e' I/O dietro la porta [`BillingCooldownPort`] (fail-open). Posizione
        // 1:1 col Python: PRIMA del controllo esplorazione a 2x soglia.
        if exploration_count >= exploration_threshold {
            let exhausted = self
                .billing
                .billing_exhausted_providers()
                .await
                .unwrap_or_default();
            // Provider IN USO dal run (sticky se escalato, override, o ultimo
            // usato): il fail-fast billing scatta solo se QUESTO e' esausto.
            let current_provider = state
                .sticky_provider
                .as_deref()
                .or(state.provider_override.as_deref())
                .or(state.provider_used.as_deref())
                .unwrap_or("");
            if let Some(ff_msg) = billing_fail_fast_message(
                exploration_count,
                exploration_threshold,
                &exhausted,
                current_provider,
            ) {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    count = exploration_count,
                    esauriti = exhausted.len(),
                    "fail-fast esplorazione: provider billing esauriti, chiusura onesta"
                );
                return Ok(StateDelta {
                    messages: Some(vec![Message::Ai {
                        content: MessageContent::text(ff_msg.clone()),
                        tool_calls: vec![],
                    }]),
                    result: Some(Some(ff_msg)),
                    pending_tool_uses: Some(Some(vec![])),
                    stop_reason: Some(Some(StopReason::LoopAbort)),
                    iterations: Some(Some(iters_in + 1)),
                    consecutive_exploration_calls: Some(Some(exploration_count)),
                    ..Default::default()
                }
                .into_opaque());
            }
        }

        // (6a) ESPLORAZIONE a 2x soglia -> Guide / ESCALATE / abort
        // (py:2093-2159 + escalation dal loop di esplorazione). Prima di abortire,
        // se l'asse e' gia' stato guidato si tenta la PROMOZIONE del modello: stesso
        // pattern del cap G1 (punto unico pick_escalation_model + progress_controller
        // Action::Escalate). Cosi' la discovery ripetuta non chiude piu' secca senza
        // mai cambiare modello.
        if exploration_count >= 2 * exploration_threshold && progress_on {
            // Candidato di escalation: provider/model correnti + escalation gia'
            // fatte; gated a < 3 esattamente come il cap G1.
            let expl_escal = state
                .extra
                .get("auto_escalations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let expl_cur_provider = state
                .provider_used
                .clone()
                .or_else(|| state.sticky_provider.clone())
                .or_else(|| state.provider_override.clone());
            let expl_cur_model = state
                .model_used
                .clone()
                .or_else(|| state.sticky_model.clone())
                .or_else(|| state.model_override.clone());
            let expl_picked = if expl_escal < 3 {
                let inputs = self
                    .escalation
                    .escalation_inputs(
                        state.user_intent.as_deref(),
                        expl_cur_provider.as_deref(),
                        expl_cur_model.as_deref(),
                    )
                    .await
                    .unwrap_or_default();
                pick_escalation_model(
                    &inputs.chain,
                    expl_cur_provider.as_deref(),
                    expl_cur_model.as_deref(),
                    expl_escal,
                    inputs.provider_in_cooldown,
                    inputs.cross_provider.as_ref(),
                )
            } else {
                None
            };
            let dec = pc::decide(&ProgressSignals {
                exploration_count,
                exploration_threshold,
                already_guided: progress_guided.clone(),
                has_escalation_candidate: expl_picked.is_some(),
                escalations: expl_escal,
                max_escalations: 3,
                ..Default::default()
            });
            match dec.action {
                Action::Guide => {
                    force_action_hard = true;
                    progress_guided.insert("exploration".to_string());
                    if let Some(t) = &dec.nudge_text {
                        messages.push(human_msg(t));
                    }
                    tracing::warn!(
                        target: "nexus_agent_graph::executor",
                        count = exploration_count,
                        "progress_controller GUIDE esplorazione -> forza-azione"
                    );
                }
                Action::Escalate => {
                    // ESCALATION dal loop di esplorazione: prima era raggiungibile
                    // SOLO dal cap G1 (re-entry "descrive ma non agisce"), mai dalla
                    // discovery ripetuta. `expl_picked` e' Some (has_escalation_candidate).
                    let pick = expl_picked.expect("Escalate implica candidato presente");
                    tracing::warn!(
                        target: "nexus_agent_graph::executor",
                        count = exploration_count,
                        from_provider = expl_cur_provider.as_deref().unwrap_or(""),
                        to_provider = %pick.provider,
                        to_model = %pick.model,
                        "esplorazione: ESCALATION modello -> azzero contatori e ri-do il turno"
                    );
                    let esc_nudge = human_msg(
                        "Il modello precedente ha continuato a esplorare senza produrre \
un risultato. Ora rispondi tu, che sei un modello piu' capace: NON esplorare oltre, \
ESEGUI subito il prossimo step concreto con un tool call (modifica file o comando di \
esecuzione/verifica), oppure rispondi a parole se era una domanda.",
                    );
                    let mut extra_out = state.extra.clone();
                    extra_out.insert("auto_escalations".to_string(), json!(expl_escal + 1));
                    progress_guided.insert("exploration".to_string());
                    return Ok(StateDelta {
                        messages: Some(vec![esc_nudge]),
                        sticky_provider: Some(Some(pick.provider)),
                        sticky_model: Some(Some(pick.model)),
                        g1_reroute_count: Some(Some(0)),
                        action_nudge_count: Some(Some(0)),
                        pending_tool_uses: Some(Some(vec![])),
                        stop_reason: Some(Some(StopReason::G1Escalated)),
                        iterations: Some(Some(iters_in + 1)),
                        consecutive_exploration_calls: Some(Some(0)),
                        exploration_nudge_sent: Some(Some(false)),
                        progress_guided_axes: Some(Some(sorted(&progress_guided))),
                        extra: Some(extra_out),
                        ..Default::default()
                    }
                    .into_opaque());
                }
                _ => {
                    let expl_text = format!(
                        "Esplorazione ripetuta ({exploration_count} letture consecutive) senza \
produrre un risultato, anche dopo il sollecito ad agire e l'escalation del modello. \
Chiudo passando per la verifica del flusso."
                    );
                    tracing::warn!(
                        target: "nexus_agent_graph::executor",
                        count = exploration_count,
                        "progress_controller ABORT esplorazione"
                    );
                    return Ok(StateDelta {
                        messages: Some(vec![Message::Ai {
                            content: MessageContent::text(expl_text.clone()),
                            tool_calls: vec![],
                        }]),
                        result: Some(Some(expl_text)),
                        pending_tool_uses: Some(Some(vec![])),
                        stop_reason: Some(Some(stop_reason_from_str(dec.stop_reason.as_deref()))),
                        iterations: Some(Some(iters_in + 1)),
                        consecutive_exploration_calls: Some(Some(exploration_count)),
                        exploration_nudge_sent: Some(Some(exploration_nudge_sent)),
                        progress_guided_axes: Some(Some(sorted(&progress_guided))),
                        forced_close_unverified: Some(Some(true)),
                        ..Default::default()
                    }
                    .into_opaque());
                }
            }
        } else if exploration_count >= 2 * exploration_threshold {
            // Controller OFF: abort legacy secco (py:2137-2159).
            let expl_text = format!(
                "[LOOP RILEVATO] Il modello ha eseguito {exploration_count} esplorazioni/\
ricerche-tool consecutive senza produrre un risultato (ne' scrittura ne' risposta), \
ignorando il sollecito a procedere. Esecuzione interrotta per evitare stallo. \
Riformula la richiesta in modo piu' specifico o usa un modello piu' capace."
            );
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                count = exploration_count,
                "LOOP esplorativo, controller OFF -> abort legacy"
            );
            return Ok(StateDelta {
                messages: Some(vec![Message::Ai {
                    content: MessageContent::text(expl_text.clone()),
                    tool_calls: vec![],
                }]),
                result: Some(Some(expl_text)),
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::LoopDetected)),
                iterations: Some(Some(iters_in + 1)),
                consecutive_exploration_calls: Some(Some(exploration_count)),
                exploration_nudge_sent: Some(Some(exploration_nudge_sent)),
                ..Default::default()
            }
            .into_opaque());
        }

        // Grounding condiviso (py:2164): risorse attive note al run.
        let has_active_resources = has_active_resources_in_history(&messages, 24);

        // Nudge anti-esplorazione a SOGLIA (py:2165-2190).
        if exploration_count >= exploration_threshold && !exploration_nudge_sent {
            let port_hint = if has_active_resources {
                "per le porte NON allocarne di nuove: i servizi del progetto sono gia' attivi \
(vedi blocco RISORSE PROGETTO), riusa/riavvia con service_restart o punta i tool alle \
porte gia' allocate"
            } else {
                "usa request_port SOLO per un servizio NUOVO"
            };
            let nudge = format!(
                "Hai gia' raccolto sufficiente contesto / cercato abbastanza strumenti \
({exploration_count} esplorazioni). NON esplorare oltre e NON cercare altri tool. \
Procedi ORA in base alla richiesta: se devi MODIFICARE il progetto, scrivi i file con \
write_file ({port_hint}); se invece era una DOMANDA o una richiesta di proposte/opzioni, \
RISPONDI subito a parole con il risultato, senza altre tool call."
            );
            messages.push(human_msg(&nudge));
            exploration_nudge_sent = true;
            exploration_nudge_injected = true;
            tracing::info!(
                target: "nexus_agent_graph::executor",
                count = exploration_count,
                soglia = exploration_threshold,
                "nudge anti-esplorazione iniettato"
            );
        }

        // (6b) Comando ripetuto fallito (py:2200-2218).
        let (repeat_cmd, repeat_count) = detect_repeated_failed_command(&messages, 12);
        let mut repeated_cmd_nudge_sent = state.repeated_cmd_nudge_sent.unwrap_or(false);
        if let Some(cmd) = &repeat_cmd {
            if repeat_count >= 3 && !repeated_cmd_nudge_sent {
                let cmd_head: String = cmd.chars().take(120).collect();
                let cmd_text = format!(
                    "[LOOP RILEVATO] Hai eseguito `{cmd_head}` {repeat_count} volte consecutive \
con errore. Continuare a ripetere lo stesso comando non risolvera' il problema. CAMBIA \
STRATEGIA ORA: esamina l'output dell'errore, identifica la causa radice (dipendenza \
mancante? package rinominato? config errata?), e prova un approccio diverso (es. tool \
diverso, comando alternativo, lettura della doc, oppure chiedi all'utente)."
                );
                messages.push(human_msg(&cmd_text));
                repeated_cmd_nudge_sent = true;
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    count = repeat_count,
                    "nudge anti-loop-comando iniettato"
                );
            }
        }

        // (6c) repeated_action (py:2230-2309).
        if progress_on {
            let (ra_label, ra_count) = detect_repeated_action(&messages, 24);
            let ra_threshold = self.cfg.repeated_action_threshold;
            let matched = ra_label.as_ref().map(|_| ra_count >= ra_threshold).unwrap_or(false);
            if !matched {
                progress_guided.remove("repeated_action");
                progress_diagnosed.remove("repeated_action");
            } else if let Some(label) = ra_label {
                // Candidato escalation (stesso pattern di esplorazione/G1 cap):
                // prima di abortire su azione ripetuta, promuovi a un modello piu'
                // capace invece di arrenderti.
                let ra_escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
                let ra_cur_provider = state.provider_used.clone()
                    .or_else(|| state.sticky_provider.clone())
                    .or_else(|| state.provider_override.clone());
                let ra_cur_model = state.model_used.clone()
                    .or_else(|| state.sticky_model.clone())
                    .or_else(|| state.model_override.clone());
                let ra_picked = if ra_escal < 3 {
                    let inputs = self
                        .escalation
                        .escalation_inputs(
                            state.user_intent.as_deref(),
                            ra_cur_provider.as_deref(),
                            ra_cur_model.as_deref(),
                        )
                        .await
                        .unwrap_or_default();
                    pick_escalation_model(
                        &inputs.chain,
                        ra_cur_provider.as_deref(),
                        ra_cur_model.as_deref(),
                        ra_escal,
                        inputs.provider_in_cooldown,
                        inputs.cross_provider.as_ref(),
                    )
                } else {
                    None
                };
                let dec = pc::decide(&ProgressSignals {
                    repeated_action: Some((label.clone(), ra_count)),
                    already_guided: progress_guided.clone(),
                    already_diagnosed: progress_diagnosed.clone(),
                    force_diagnose_enabled: self.cfg.repeated_action_force_diagnose_enabled,
                    has_escalation_candidate: ra_picked.is_some(),
                    escalations: ra_escal,
                    max_escalations: 3,
                    ..Default::default()
                });
                match dec.action {
                    Action::Guide => {
                        progress_guided.insert("repeated_action".to_string());
                        if let Some(t) = &dec.nudge_text {
                            messages.push(human_msg(t));
                        }
                        tracing::warn!(target: "nexus_agent_graph::executor", "GUIDE repeated_action");
                    }
                    Action::ForceDiagnose => {
                        progress_diagnosed.insert("repeated_action".to_string());
                        if let Some(t) = &dec.nudge_text {
                            messages.push(human_msg(t));
                        }
                        tracing::warn!(target: "nexus_agent_graph::executor", "FORCE_DIAGNOSE repeated_action");
                    }
                    Action::Abort => {
                        // Recap M44 deterministico. modified_files_from_steps e' I/O
                        // (agent_steps): TODO -> "nessuno" finche' non c'e' la porta.
                        let ra_text = format!(
                            "ESITO: non completato.\nMi sono bloccato ripetendo la stessa azione \
({label}) {ra_count} volte senza che il risultato cambiasse; interrompo invece di \
insistere a vuoto.\nFile toccati: nessuno.\nProssimo passo: identificare la causa radice \
del fallimento di '{label}' dall'output/errore qui sopra e procedere con un approccio \
diverso; se sei bloccato da una dipendenza/credenziale/permesso/servizio mancante, \
indicalo esplicitamente."
                        );
                        tracing::warn!(target: "nexus_agent_graph::executor", "ABORT repeated_action");
                        return Ok(StateDelta {
                            messages: Some(vec![Message::Ai {
                                content: MessageContent::text(ra_text.clone()),
                                tool_calls: vec![],
                            }]),
                            result: Some(Some(ra_text)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(stop_reason_from_str(dec.stop_reason.as_deref()))),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
                            forced_close_unverified: Some(Some(true)),
                            ..Default::default()
                        }
                        .into_opaque());
                    }
                    Action::Escalate => {
                        // Azione ripetuta a vuoto dopo guide/diagnose: promuovi il
                        // modello e ri-do il turno (stesso pattern del cap G1). Il
                        // nudge copre anche il caso 'lavoro gia' fatto': invece di
                        // ripetere la verifica, concludi positivamente.
                        let pick = ra_picked.expect("Escalate implica candidato presente");
                        tracing::warn!(
                            target: "nexus_agent_graph::executor",
                            to_provider = %pick.provider,
                            to_model = %pick.model,
                            "ESCALATE repeated_action -> promuovo modello"
                        );
                        let esc_nudge = human_msg(
                            "Hai ripetuto la stessa azione senza progresso. Ora rispondi tu, \
che sei un modello piu' capace: cambia approccio ed ESEGUI il prossimo step concreto; \
se invece il lavoro e' gia' fatto e funzionante (es. l'app si avvia e risponde), NON \
ripetere la verifica: dichiaralo concludendo positivamente con un breve riepilogo.",
                        );
                        let mut extra_out = state.extra.clone();
                        extra_out.insert("auto_escalations".to_string(), json!(ra_escal + 1));
                        progress_guided.insert("repeated_action".to_string());
                        return Ok(StateDelta {
                            messages: Some(vec![esc_nudge]),
                            sticky_provider: Some(Some(pick.provider)),
                            sticky_model: Some(Some(pick.model)),
                            g1_reroute_count: Some(Some(0)),
                            action_nudge_count: Some(Some(0)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(StopReason::G1Escalated)),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
                            extra: Some(extra_out),
                            ..Default::default()
                        }
                        .into_opaque());
                    }
                    Action::Proceed => {}
                }
            }
        }

        // (6d) resource_reallocation (py:2321-2383).
        if progress_on {
            let rp_count = count_recent_request_port(&messages, 16);
            let rp_threshold = self.cfg.reallocation_threshold;
            if rp_count < rp_threshold {
                progress_guided.remove("resource_reallocation");
            } else {
                // Candidato escalation (stesso pattern di repeated_action/esplorazione).
                let rp_escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
                let rp_cur_provider = state.provider_used.clone()
                    .or_else(|| state.sticky_provider.clone())
                    .or_else(|| state.provider_override.clone());
                let rp_cur_model = state.model_used.clone()
                    .or_else(|| state.sticky_model.clone())
                    .or_else(|| state.model_override.clone());
                let rp_picked = if rp_escal < 3 {
                    let inputs = self
                        .escalation
                        .escalation_inputs(
                            state.user_intent.as_deref(),
                            rp_cur_provider.as_deref(),
                            rp_cur_model.as_deref(),
                        )
                        .await
                        .unwrap_or_default();
                    pick_escalation_model(
                        &inputs.chain,
                        rp_cur_provider.as_deref(),
                        rp_cur_model.as_deref(),
                        rp_escal,
                        inputs.provider_in_cooldown,
                        inputs.cross_provider.as_ref(),
                    )
                } else {
                    None
                };
                let dec = pc::decide(&ProgressSignals {
                    reallocation_count: rp_count,
                    reallocation_threshold: rp_threshold,
                    has_active_resources,
                    already_guided: progress_guided.clone(),
                    has_escalation_candidate: rp_picked.is_some(),
                    escalations: rp_escal,
                    max_escalations: 3,
                    ..Default::default()
                });
                match dec.action {
                    Action::Guide => {
                        progress_guided.insert("resource_reallocation".to_string());
                        if let Some(t) = &dec.nudge_text {
                            messages.push(human_msg(t));
                        }
                        tracing::warn!(target: "nexus_agent_graph::executor", "GUIDE resource_reallocation");
                    }
                    Action::Abort => {
                        let rp_text = format!(
                            "ESITO: non completato.\nMi sono bloccato richiedendo porte ({rp_count} \
chiamate request_port ravvicinate) invece di riusare i servizi gia' attivi del progetto; \
interrompo invece di insistere.\nFile toccati: nessuno.\nProssimo passo: usare \
list_active_services per vedere i servizi attivi e le porte gia' allocate, riusare la porta \
esistente del servizio richiesto (o riavviarlo con service_restart se spento) e puntare i \
tool/le richieste a quella porta, senza allocarne di nuove."
                        );
                        tracing::warn!(target: "nexus_agent_graph::executor", "ABORT resource_reallocation");
                        return Ok(StateDelta {
                            messages: Some(vec![Message::Ai {
                                content: MessageContent::text(rp_text.clone()),
                                tool_calls: vec![],
                            }]),
                            result: Some(Some(rp_text)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(stop_reason_from_str(dec.stop_reason.as_deref()))),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
                            forced_close_unverified: Some(Some(true)),
                            ..Default::default()
                        }
                        .into_opaque());
                    }
                    Action::Escalate => {
                        let pick = rp_picked.expect("Escalate implica candidato presente");
                        tracing::warn!(
                            target: "nexus_agent_graph::executor",
                            to_provider = %pick.provider,
                            to_model = %pick.model,
                            "ESCALATE resource_reallocation -> promuovo modello"
                        );
                        let esc_nudge = human_msg(
                            "Hai richiesto porte ripetutamente invece di riusare i servizi attivi. \
Ora rispondi tu, che sei un modello piu' capace: NON allocare nuove porte, usa \
list_active_services per i servizi gia' attivi e le porte allocate, riusa quella del \
servizio del tuo scopo (o riavvialo) ed ESEGUI il prossimo step.",
                        );
                        let mut extra_out = state.extra.clone();
                        extra_out.insert("auto_escalations".to_string(), json!(rp_escal + 1));
                        progress_guided.insert("resource_reallocation".to_string());
                        return Ok(StateDelta {
                            messages: Some(vec![esc_nudge]),
                            sticky_provider: Some(Some(pick.provider)),
                            sticky_model: Some(Some(pick.model)),
                            g1_reroute_count: Some(Some(0)),
                            action_nudge_count: Some(Some(0)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(StopReason::G1Escalated)),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            extra: Some(extra_out),
                            ..Default::default()
                        }
                        .into_opaque());
                    }
                    Action::ForceDiagnose | Action::Proceed => {}
                }
            }
        }

        // ── Forza-azione: rimuovi i read-only se oltre soglia esplorazione (py:2402) ─
        if !tools_json.is_empty() && exploration_count >= exploration_threshold {
            let productive: Vec<Value> = tools_json
                .iter()
                .filter(|t| {
                    t.get("name")
                        .and_then(Value::as_str)
                        .map(|n| !EXPLORATION_ONLY_TOOLS.contains(&n))
                        .unwrap_or(true)
                })
                .cloned()
                .collect();
            if !productive.is_empty() && productive.len() < tools_json.len() {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    rimossi = tools_json.len() - productive.len(),
                    restano = productive.len(),
                    "forza-azione: rimossi tool di sola lettura"
                );
                tools_json = productive;
            }
        }

        // ── Forced text response: svuota i tool nell'ultima finestra (py:2438-2453) ─
        let forced_text_threshold = self.cfg.iteration_cap - 5;
        if !tools_json.is_empty()
            && iters_in >= forced_text_threshold
            && state.stop_reason == Some(StopReason::ToolUse)
        {
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                iters = iters_in,
                threshold = forced_text_threshold,
                "forced text response: rimozione tool per forzare risposta testuale"
            );
            tools_json.clear();
        }

        // ── Risoluzione provider/model: sticky > override > routing (py:2460-2521) ─
        // Mutabili: l'auto-escalation nel signature-loop (py:3262-3263) li riassegna
        // al modello promosso prima del calcolo eff/sticky del delta finale.
        let mut provider;
        let mut model;
        match self.resolve_provider(state) {
            ProviderResolution::Resolved(p, m) => {
                provider = p;
                model = m;
            }
            ProviderResolution::NoProvider(p) => {
                return Err(NodeError::Failed {
                    node: "executor",
                    message: format!(
                        "Nessun provider AI disponibile (tutti i provider capable in cooldown \
billing/quota oppure gate di routing non risponde): provider={p}. Il run si ferma \
(ADR 0020)."
                    ),
                });
            }
        }
        tracing::info!(
            target: "nexus_agent_graph::executor",
            provider = %provider,
            model = %model,
            tools = tools_json.len(),
            "provider/model risolti"
        );

        // ── G1 nudge anti-descrittivo pre-LLM (py:2564-2644) ──────────────────
        let mut nudge_count = state.action_nudge_count.unwrap_or(0);
        let mut g1_nudge_injected = false;
        if !tools_json.is_empty() && iters_in >= 1 && nudge_count < 2 {
            let is_action_req = turn_action_oriented(state.action_oriented);
            let last_asst = last_assistant_text(&messages);
            let is_unfulfilled = detect_unfulfilled_intent(last_asst.as_deref());
            let no_tools_yet = !has_tool_calls_in_history(&messages);
            if (is_action_req && no_tools_yet) || is_unfulfilled {
                // _detect_polling_wait e' lessicale; non e' un punto unico portato:
                // usiamo il nudge generico (parita' col ramo non-polling, dominante).
                // TODO: portare _detect_polling_wait come detector lessicale se serve.
                let nudge_content =
                    "ERRORE: hai annunciato/descritto cosa avresti fatto, ma NON hai chiamato \
nessun tool. Questo non e' accettabile. AGISCI ADESSO — esegui l'azione che hai appena \
dichiarato usando un tool: shell_exec/run_command per comandi (docker, npm, dotnet, ss, \
ecc.), read_file/list_files per ispezionare, write_file/edit_file per creare o modificare \
file. Nessuna spiegazione: ESEGUI il prossimo step concreto con un tool call.";
                messages.push(human_msg(nudge_content));
                g1_nudge_injected = true;
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    iter = iters_in,
                    "G1 nudge iniettato"
                );
                // Convergenza progress_controller: affianca la forza-azione hard.
                if progress_on {
                    let dec = pc::decide(&ProgressSignals {
                        g1_over_cap: true,
                        already_guided: progress_guided.clone(),
                        ..Default::default()
                    });
                    if dec.action == Action::Guide && dec.force_action {
                        force_action_hard = true;
                        progress_guided.insert("g1_descriptive".to_string());
                        tracing::warn!(
                            target: "nexus_agent_graph::executor",
                            "GUIDE g1_descriptive -> forza tool_choice required"
                        );
                    }
                }
            }
        }

        // ── meta_step executor_call (heartbeat, EventSink + MetaStepStore) ────
        let calling_meta = json!({
            "kind": "executor_call",
            "title": format!("Sto interrogando {provider}/{model}"),
            "payload": {
                "provider": provider,
                "model": model,
                "intent": state.user_intent.as_deref().unwrap_or("chat"),
                "iteration": iters_in,
                "tools_count": tools_json.len(),
            },
        });
        ctx.emit.emit(SseEvent::MetaStep {
            kind: "executor_call".to_string(),
            title: format!("Sto interrogando {provider}/{model}"),
            payload: calling_meta.get("payload").cloned().unwrap_or(Value::Null),
        });
        let _ = self.meta_steps.persist_meta_step(calling_meta, mode).await;
        // Heartbeat best-effort (anti-recovery prematuro), gata Real.
        let _ = self.run_control.heartbeat(&run_id, mode).await;

        // ── CONTEXT REDUCTION (parte PURA, punti unici PR-D) ──────────────────
        // I/O (summarizer / continuity-trim / rolling-summary / smart-upscale /
        // system-offload) NON portati: TODO trait dedicati. Qui solo la parte pura.
        let mut hist: Vec<HistoryMessage> = messages.iter().map(message_to_history).collect();
        let compress_iter = iters_in;

        // Compressione a generazioni (cutoff fisso, py:2764-2810).
        let boundaries = &self.cfg.ctx_mgmt.compress_phase_boundaries;
        let phase_now = boundaries.iter().filter(|b| compress_iter >= **b).count() as i64;
        let prev_phase = state.compress_cutoff_phase.unwrap_or(0);
        let mut cutoff_idx = state.compress_cutoff_index.unwrap_or(0);
        let (do_compress, params): (bool, CompressParams) =
            ctxr::should_compress_now(compress_iter, &self.cfg.ctx_mgmt);
        let mut gen_cutoff_index: Option<i64> = None;
        let mut gen_cutoff_phase: Option<i64> = None;
        if phase_now > prev_phase {
            // CAMBIO FASE: dedup + drop base64 (rolling = I/O, TODO).
            hist = ctxr::dedup_tool_results_history(&hist);
            hist = ctxr::drop_unused_base64_payloads(&hist, ctxr_drop_age(), 2);
            cutoff_idx = std::cmp::max(0, hist.len() as i64 - params.keep_recent);
            gen_cutoff_index = Some(cutoff_idx);
            gen_cutoff_phase = Some(phase_now);
        }
        if do_compress && cutoff_idx > 0 {
            hist = ctxr::compress_old_tool_results(
                &hist,
                0,
                params.max_content_chars.max(0) as usize,
                Some(cutoff_idx as usize),
                &ctxr::degraded_marker,
            );
        } else if !do_compress
            && estimate_history_chars(&hist) > (ctxr::MAX_CONTEXT_CHARS as i64) / 2
        {
            // Safety net legacy (py:2803-2810): compressione con keep_recent=6.
            hist = ctxr::compress_old_tool_results(&hist, 6, 0, None, &ctxr::degraded_marker);
        }

        // ── smart upscale modello (py:2812-2830, ADR 0016 Fase C) ─────────────
        // Se il contesto stimato supera (>=90%) il window del modello attivo,
        // promuove a un modello con window maggiore PRIMA del brake (cosi' il brake
        // usa il window del modello effettivo). DECISIONE = punti unici puri
        // [`should_upscale`] / [`upscale_required_tokens`] (regola L); il lookup
        // window + la SELEZIONE catalog (tier-based) sono I/O dietro la porta
        // [`ModelUpscalePort`] (best-effort, fail-open). Riassegna provider/model
        // del turno (la risoluzione provider e' gia' avvenuta sopra). Niente switch
        // se la porta non promuove (parita' col Python: `_upscale_result is None`).
        let upscale_est_tokens = history_token_estimator(&hist);
        let upscale_window = self.upscale.context_window(&model).await.unwrap_or(0);
        if should_upscale(self.cfg.upscale_enabled, upscale_est_tokens, upscale_window) {
            let required = upscale_required_tokens(upscale_est_tokens, self.cfg.upscale_overhead_ratio);
            if let Ok(Some(pick)) = self.upscale.select_upscale_model(&model, required).await {
                tracing::info!(
                    target: "nexus_agent_graph::executor",
                    from_model = %model,
                    to_provider = %pick.provider,
                    to_model = %pick.model,
                    est = upscale_est_tokens,
                    reason = %pick.reason,
                    "smart upscale: promosso a modello con window maggiore"
                );
                provider = pick.provider;
                model = pick.model;
            }
        }

        // Token brake (py:2836): cap hard sotto window (token_estimator puro qui).
        if self.cfg.context_window > 0 {
            hist = ctxr::apply_token_brake(
                &hist,
                self.cfg.context_window,
                &self.cfg.token_brake,
                &history_token_estimator,
            );
        }

        // Iniezioni system_text (P3, idempotenti) nell'ordine del Python.
        system_text = ctxr::inject_language_reminder(
            &system_text,
            self.cfg.language_reminder_enabled,
            &self.cfg.language_reminder_text,
        );
        // turn_focus (py:2864-2878): build directive (punto unico) + inject.
        if self.cfg.turn_focus_enabled {
            if let Some(directive) = build_turn_focus_directive(&messages, false) {
                system_text = ctxr::inject_turn_focus(&system_text, &directive);
            }
        }
        system_text = ctxr::inject_verification_directive(
            &system_text,
            self.cfg.verification_requested,
            self.cfg.verification_directive_enabled,
            &self.cfg.verification_directive_text,
        );
        // forced_rag_reminder (py:2907-2911): appende un HumanMessage se sopra ratio.
        let rag_est = history_token_estimator(&hist);
        let (hist_rag, _) = ctxr::inject_forced_rag_reminder(
            &hist,
            &system_text,
            rag_est,
            self.cfg.context_window,
            self.cfg.forced_rag_ratio,
            &self.cfg.forced_rag_reminder_text,
        );
        hist = hist_rag;

        // ── tool_choice forcing (py:2913-2972, funzione pura) ─────────────────
        let names_tc: HashSet<String> = tools_json
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        let in_discovery = !names_tc.is_empty()
            && names_tc.iter().all(|n| n == "nexus_mcp_tool_search");
        let supports_forcing = provider_style_supports_forcing(self.cfg.tool_choice_style.as_deref());
        // Forza-azione hard (progress_controller GUIDE) OPPURE forcing "early
        // action" (funzione pura): in entrambi i casi `Some(true)`, py:2946-2969.
        let force_now = (force_action_hard && supports_forcing)
            || should_force_tool_choice(
                !tools_json.is_empty(),
                turn_action_oriented(state.action_oriented),
                iters_in,
                in_discovery,
                supports_forcing,
                self.cfg.tool_choice_forcing_enabled,
                self.cfg.tool_choice_forcing_max_iteration,
            );
        let force_tc: Option<bool> = if force_now { Some(true) } else { None };

        // ── LLM CALL (py:2974-3107) ───────────────────────────────────────────
        // max_tokens = max(8192, min(budget*4, 16384)) == clamp(8192, 16384).
        let max_tokens = (state.token_budget.unwrap_or(400) * 4).clamp(8192, 16384);
        let llm_messages = history_to_llm_messages(&hist);
        let req = LlmRequest {
            provider: provider.clone(),
            model: model.clone(),
            messages: llm_messages.clone(),
            tools: if tools_json.is_empty() { None } else { Some(tools_json.clone()) },
            force_tool_choice: force_tc,
            system_text: Some(system_text.clone()),
            max_tokens: Some(max_tokens),
            run_id: if run_id.is_empty() { None } else { Some(run_id.clone()) },
            iteration: Some(iters_in),
            intent: state.user_intent.clone(),
            // Nodo chiamante = executor: il decorator di replay (shadow) rigioca
            // la sequenza di tool del primario su questo purpose (regola L). Il
            // gateway concreto (GatewayLlmAdapter) lo IGNORA.
            purpose: Some("executor".into()),
        };

        // `gateway_errored`: il `complete` ha sollevato (Err). Il Python NON fa
        // early-return nel ramo `except` (py:3104-3107): imposta solo result_text/
        // stop_reason="error" e PROSEGUE al return UNIFICATO (py:3457-3513),
        // persistendo TUTTI i contatori mutati nel turno (g1_reroute_count,
        // *_nudge_sent, progress_*_axes, recent_tool_signatures, sticky, ...).
        // Per parita' sintetizziamo qui una `resp` vuota equivalente
        // (content=err_text, nessun tool_call, stop_reason="error") e lasciamo
        // CONVERGERE al delta finale unificato. Il flag salta il retry-senza-
        // forcing (che e' il retry interno malformed del Python, NON l'errore
        // provider, gia' gestito da generate_agent_turn_sync) e la cascade-detect.
        let mut gateway_errored = false;
        let mut resp = match ctx.llm.complete(req).await {
            Ok(r) => r,
            Err(err) => {
                // try/except onnicomprensivo Python: result="[Errore provider ...]",
                // stop_reason="error" (NON NodeError: il run prosegue al routing).
                tracing::error!(
                    target: "nexus_agent_graph::executor",
                    error = %err,
                    "agent_turn fallita"
                );
                gateway_errored = true;
                // Riepilogo del lavoro svolto PRIMA dell'interruzione: anche se il
                // provider e' caduto (es. cooldown), l'utente deve sapere cosa e'
                // stato fatto, non solo l'errore. Punto unico summarize_actions_in_history.
                let err_text = match crate::routing::signals::summarize_actions_in_history(&messages) {
                    Some(w) => format!(
                        "[Errore provider {provider}: {err}]\n\nInterrotto dopo {iters_in} iterazioni. Lavoro svolto finora: {w}."
                    ),
                    None => format!("[Errore provider {provider}: {err}]"),
                };
                // provider_used/model_used None: nessuna cascade -> eff = richiesto
                // (cascade_did_fallback=false -> sticky invariato, == Python).
                crate::runtime::ports::LlmResponse {
                    content: err_text,
                    stop_reason: Some("error".to_string()),
                    ..Default::default()
                }
            }
        };

        // Retry-senza-forcing su risposta malformed (py:2991-3022): se il forcing
        // ha prodotto un errore di function-call, ritenta UNA volta senza forcing.
        // NON in caso di errore gateway (la `resp` e' sintetica, non un malformed
        // recuperabile: il Python non ritenta nell'except).
        if !gateway_errored && force_tc == Some(true) && is_forcing_failure(&resp) {
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                "tool_choice forcing ha causato errore, retry SENZA forcing"
            );
            let retry = LlmRequest {
                provider: provider.clone(),
                model: model.clone(),
                // clone: `llm_messages` serve anche all'eventuale ri-chiamata di
                // auto-escalation del signature-loop (sotto).
                messages: llm_messages.clone(),
                tools: if tools_json.is_empty() { None } else { Some(tools_json.clone()) },
                force_tool_choice: Some(false),
                system_text: Some(system_text.clone()),
                max_tokens: Some(max_tokens),
                run_id: if run_id.is_empty() { None } else { Some(run_id.clone()) },
                iteration: Some(iters_in),
                intent: state.user_intent.clone(),
                // Retry-senza-forcing dello stesso turno executor: stesso purpose
                // (in shadow consuma lo stesso gruppo-iterazione del replay).
                purpose: Some("executor".into()),
            };
            if let Ok(r) = ctx.llm.complete(retry).await {
                resp = r;
            }
        }

        // Pensiero intermedio del modello (reasoning aggregato dal gateway): emesso
        // come ThinkingDelta cosi' il ThinkingBlock della chat mostra il ragionamento
        // di OGNI interrogazione (ripristino visibilita' pre-porting). Solo su risposta
        // valida (non sull'errore sintetico). Riusa il canale gia' tradotto da event_sink.
        if !gateway_errored {
            // Pensiero del modello: il reasoning aggregato (modelli "thinking")
            // OPPURE, per i modelli non-thinking (gemini) che non lo emettono, il
            // content testuale dei turni CON tool_call (ragionamento pre-azione) —
            // cosi' il ThinkingBlock mostra SEMPRE cosa sta ragionando l'agente,
            // senza duplicare la risposta conversazionale finale (turni senza tool).
            let from_reasoning = resp
                .reasoning
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let thinking = match from_reasoning {
                Some(r) => Some(r),
                None if !resp.tool_calls.is_empty() => {
                    let c = resp.content.trim();
                    if c.is_empty() { None } else { Some(c.to_string()) }
                }
                None => None,
            };
            if let Some(t) = thinking {
                ctx.emit.emit(SseEvent::ThinkingDelta { delta: t });
            }
        }

        // NB: il calcolo dei provider/model EFFETTIVI (cascade) + set_effective_model
        // e' RIMANDATO a DOPO l'auto-escalation del signature-loop (py:3457+): se il
        // loop scala il modello, `provider`/`model` vengono riassegnati e il calcolo
        // eff/cascade deve vedere il NUOVO modello.

        let mut result_text = resp.content.clone();
        let mut stop_reason_str_resp = resp.stop_reason.clone();
        // pending_tool_uses = i blocchi tool_use della risposta (forma Value).
        let mut pending_tool_uses: Vec<Value> = resp
            .tool_calls
            .iter()
            .map(|t| json!({"type": "tool_use", "id": t.id, "name": t.name, "input": t.input}))
            .collect();

        // ── Costruzione del Message::Ai con assistant_content (continuita') ───
        // Se il gateway ha riportato i blocchi assistant_content (testo + tool_use),
        // li usiamo per ricostruire il Message::Ai a blocchi (continuita'
        // tool_use/tool_result come planner). Altrimenti: content testuale + i
        // tool_calls in `tool_calls` (forma OpenAI-compat).
        let mut assistant_msg = build_assistant_message(&resp, &result_text);

        // ── POST: loop detection per signature (py:3138-3287) ─────────────────
        let mut new_signatures: Vec<String> = pending_tool_uses
            .iter()
            .map(|tu| {
                let name = tu.get("name").and_then(Value::as_str).unwrap_or("");
                let input = tu.get("input").cloned().unwrap_or(json!({}));
                build_signature(name, &input)
            })
            .collect();
        let recent: Vec<String> = state.recent_tool_signatures.clone().unwrap_or_default();
        let det = detect_signature_loop(&recent, &new_signatures);
        // `escalations` (auto_escalations) cresce di 1 quando il loop scala il
        // modello (py:3284); resta invariato altrimenti. Tracciato per il delta.
        let mut escalations = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
        let mut loop_close_result: Option<String> = None;
        if let Some(loop_sig) = &det.loop_signature {
            let tool_name = loop_sig.split_once('|').map(|(t, _)| t).unwrap_or(loop_sig);
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                tool = tool_name,
                "LOOP detected per signature ripetuta"
            );
            // Auto-escalation intra-provider (py:3159-3284): al primo loop l'
            // orchestratore PROMUOVE automaticamente un modello piu' capace (catena
            // DB + cross-provider loop_fallback_default, cooldown-aware) e RI-ESEGUE
            // lo stesso turno. SELEZIONE = punto unico puro [`pick_escalation_model`]
            // (regola L); gli input arrivano dalla porta. Cap `escalations < 3`
            // (py:3166). Solo a escalation NON disponibile chiudiamo secco
            // loop_detected (ramo `not tried_escalation`).
            let mut tried_escalation = false;
            if escalations < 3 && !tools_json.is_empty() {
                let inputs = self
                    .escalation
                    .escalation_inputs(state.user_intent.as_deref(), Some(&provider), Some(&model))
                    .await
                    .unwrap_or_default();
                if let Some(pick) = pick_escalation_model(
                    &inputs.chain,
                    Some(&provider),
                    Some(&model),
                    escalations,
                    inputs.provider_in_cooldown,
                    inputs.cross_provider.as_ref(),
                ) {
                    // Hint anti-loop nel system_text (py:3227-3234): chiede di NON
                    // ripetere la stessa tool call.
                    let anti_loop_hint = format!(
                        "\n\n[ANTI-LOOP] Hai appena ripetuto la stessa tool call ('{tool_name}' \
con stesso input) piu' volte. Non ripetere la stessa tool call con lo stesso input. \
Se mancano informazioni, fai UNA richiesta piu' specifica oppure cambia strategia e \
riassumi lo stato."
                    );
                    let system_text2 = format!("{system_text}{anti_loop_hint}");
                    tracing::info!(
                        target: "nexus_agent_graph::executor",
                        from_provider = %provider,
                        to_provider = %pick.provider,
                        to_model = %pick.model,
                        from_chain = pick.from_chain,
                        "LOOP -> auto-escalation, ri-eseguo il turno col modello promosso"
                    );
                    // Ri-esegue lo STESSO turno col provider/model promosso
                    // (py:3241-3248). Trasporto unico (regola L): passa dal gateway
                    // come gli altri agent turn.
                    let esc_req = LlmRequest {
                        provider: pick.provider.clone(),
                        model: pick.model.clone(),
                        messages: llm_messages.clone(),
                        tools: if tools_json.is_empty() {
                            None
                        } else {
                            Some(tools_json.clone())
                        },
                        // Parita' col Python (py:3241-3248): la ri-chiamata escalata
                        // NON passa `force_tool_choice` -> default `None` -> `auto`.
                        // La primaria mantiene `force_tc`; qui forzare il tool
                        // contraddirebbe l'`anti_loop_hint` ("cambia strategia,
                        // riassumi lo stato"), che ammette anche risposta testuale.
                        force_tool_choice: None,
                        system_text: Some(system_text2),
                        max_tokens: Some(max_tokens),
                        run_id: if run_id.is_empty() { None } else { Some(run_id.clone()) },
                        iteration: Some(iters_in),
                        intent: state.user_intent.clone(),
                        // Auto-escalation dello stesso turno executor: stesso purpose.
                        purpose: Some("executor".into()),
                    };
                    // Best-effort: se la ri-chiamata fallisce, NON promuoviamo
                    // (parita' con l'except Python py:3266-3267 -> not tried_escalation).
                    if let Ok(resp2) = ctx.llm.complete(esc_req).await {
                        resp = resp2;
                        result_text = resp.content.clone();
                        stop_reason_str_resp = resp.stop_reason.clone();
                        pending_tool_uses = resp
                            .tool_calls
                            .iter()
                            .map(|t| json!({"type": "tool_use", "id": t.id, "name": t.name, "input": t.input}))
                            .collect();
                        assistant_msg = build_assistant_message(&resp, &result_text);
                        provider = pick.provider;
                        model = pick.model;
                        new_signatures = vec![]; // reset accumulator dopo escalation (py:3265)
                        tried_escalation = true;
                    }
                }
            }
            if tried_escalation {
                escalations += 1; // py:3284
            } else {
                // Catena esaurita / tutti in cooldown / ri-chiamata fallita: chiude
                // secco con loop_detected (py:3269-3281).
                let loop_msg = format!(
                    "[LOOP RILEVATO] Il modello {provider}/{model} ha ripetuto il tool '{tool_name}' \
con stesso input 3+ volte senza progresso. Esecuzione interrotta per evitare stallo. \
Suggerimento: usa un modello piu' capace (es. anthropic/claude) o riformula il prompt in \
modo piu' specifico."
                );
                assistant_msg = Message::Ai {
                    content: MessageContent::text(loop_msg.clone()),
                    tool_calls: vec![],
                };
                pending_tool_uses = vec![];
                stop_reason_str_resp = Some("loop_detected".to_string());
                loop_close_result = Some(loop_msg);
                new_signatures = vec![]; // reset accumulator (py:3281)
            }
        }
        let stop_reason_final = stop_reason_str_resp.as_deref();

        // Provider/model EFFETTIVI (cascade interno del gateway), calcolati DOPO l'
        // eventuale escalation cosi' il confronto e' col NUOVO modello promosso
        // (py:3457+). set_effective_model best-effort (gata Real) -> modello reale UI.
        let eff_provider = resp.provider_used.clone().unwrap_or_else(|| provider.clone());
        let eff_model = resp.model_used.clone().unwrap_or_else(|| model.clone());
        let cascade_did_fallback = eff_provider != provider || eff_model != model;
        if cascade_did_fallback {
            let _ = self
                .run_control
                .set_effective_model(&run_id, &eff_provider, &eff_model, mode)
                .await;
        }
        // ── usage del turno (py:3087-3125, 3320, 3476-3480) ───────────────────
        // I token vengono dalla risposta LLM (post-escalation: `resp` e' gia' quella
        // promossa se il signature-loop ha scalato). Il Python li EMETTE per-turno con
        // reducer overwrite (last-write, non additivo: vedi state.py — solo messages/
        // meta_steps sono `add`), quindi qui replichiamo: valori del turno, non
        // cumulativi (la cumulazione e' del finalize/ledger). Senza questa
        // propagazione l'esito nativo aveva SEMPRE total_tokens=0 nel DB (secondo
        // bug osservato sul primario Rust, oltre all'hollow). `total_tokens` segue la
        // formula del Python (prompt+completion+cache_*). Su turno error (gateway_errored)
        // l'usage e' default (zero), parita' col Python che non somma nulla nell'except.
        let usage = &resp.usage;
        let turn_prompt_tokens = usage.prompt_tokens;
        let turn_completion_tokens = usage.completion_tokens;
        let turn_cache_creation = usage.cache_creation_tokens.unwrap_or(0);
        let turn_cache_read = usage.cache_read_tokens.unwrap_or(0);
        let turn_total_tokens =
            turn_prompt_tokens + turn_completion_tokens + turn_cache_creation + turn_cache_read;
        let turn_total_cost = usage.total_cost_usd;

        // ── Emissione Usage live (barra contesto / TokenUsageBar) ─────────────
        // Il grafo nativo non emetteva mai SseEvent::Usage, quindi la barra
        // contesto restava invisibile durante il run nativo (a differenza del
        // path Python che la aggiorna a ogni turno). Emettiamo qui, subito dopo
        // aver letto i token del turno dalla risposta del gateway. I valori sono
        // del TURNO (reducer overwrite last-write, come lo stato: la cumulazione
        // e' del finalize/ledger, non del grafo) -> parita' 1:1 col Python che
        // emette per-turno. Su turno error l'usage e' zero (gateway_errored) e
        // l'emissione e' un no-op informativo (best-effort, l'emit e' infallibile).
        ctx.emit.emit(SseEvent::Usage {
            prompt_tokens: turn_prompt_tokens,
            completion_tokens: turn_completion_tokens,
            total_tokens: turn_total_tokens,
        });

        // Coda aggiornata cap 12: con loop chiuso new_signatures e' [] -> recent only.
        let updated_signatures = detect_signature_loop(&recent, &new_signatures).updated_signatures;

        // ── exploration counter update (py:3296-3317) ─────────────────────────
        let pending_names: Vec<String> = pending_tool_uses
            .iter()
            .filter_map(|tu| tu.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        let expl = exploration_counter_update(
            &pending_names,
            state.consecutive_exploration_calls.unwrap_or(0),
            if exploration_nudge_injected {
                exploration_nudge_sent
            } else {
                state.exploration_nudge_sent.unwrap_or(false)
            },
            EXPLORATION_ONLY_TOOLS,
        );
        if !pending_tool_uses.is_empty() {
            // Reset coordinato g1_descriptive: il modello ha emesso una tool call.
            progress_guided.remove("g1_descriptive");
            if expl.reset_exploration_axis {
                progress_guided.remove("exploration");
            }
        }

        // ── Costruzione del delta finale (py:3457-3513) ───────────────────────
        let mut final_result = loop_close_result.unwrap_or(result_text);
        let stop_reason_enum = stop_reason_from_str(stop_reason_final);

        // ── POST end_turn: next_actions + unfulfilled-report (py:3379-3429) ───
        // Entrambi i rami si applicano SOLO a turno realmente concluso (end_turn
        // senza tool pendenti e `result` non vuoto): la risposta assistant e'
        // completa e visibile. Mutano `final_result` (testo visibile) + il
        // `assistant_msg` (entrambi finiscono nel delta sotto), 1:1 col Python.
        let turn_concluded = stop_reason_enum == StopReason::EndTurn
            && pending_tool_uses.is_empty()
            && !final_result.trim().is_empty();
        if turn_concluded {
            // (1) next_actions (py:3379-3402): RIMOZIONE deterministica del blocco
            // <suggested_actions> dal testo visibile (punto unico puro, SEMPRE
            // applicata) + DERIVAZIONE delle scelte via porta (best-effort: errore
            // -> nessuna scelta, ma il testo resta comunque ripulito).
            let cleaned = strip_suggested_actions(&final_result);
            if cleaned != final_result {
                final_result = cleaned.clone();
                assistant_msg = Message::Ai {
                    content: MessageContent::text(cleaned.clone()),
                    tool_calls: vec![],
                };
            }
            // Derivazione scelte sul testo ripulito (best-effort). Il meta_step si
            // EMETTE (live) e si PERSISTE (storico): entrambi via le porte esistenti.
            let choices = self.next_actions.derive(&cleaned).await.unwrap_or_default();
            if !choices.is_empty() {
                let payload = json!({
                    "choices": choices
                        .iter()
                        .map(|c| json!({"label": c.label, "prompt": c.prompt}))
                        .collect::<Vec<_>>(),
                });
                let meta = json!({
                    "kind": "next_actions",
                    "title": "Prossimi passi",
                    "payload": payload,
                });
                ctx.emit.emit(SseEvent::MetaStep {
                    kind: "next_actions".to_string(),
                    title: "Prossimi passi".to_string(),
                    payload,
                });
                let _ = self.meta_steps.persist_meta_step(meta, mode).await;
            }

            // (2) unfulfilled-report (py:3404-3429): in modalita' NON autonoma con
            // intento NON compiuto e turno NON action-oriented, SOSTITUISCE il
            // result con il resoconto onesto deterministico. Gate puro
            // [`should_substitute_unfulfilled_report`] + segnale unfulfilled dal
            // detector LESSICALE puro sul testo GIA' ripulito (1:1 col Python
            // :3413, che in QUESTO ramo report usa SOLO _detect_unfulfilled_intent
            // e NON consulta closure_verdict). La distinzione col ramo G1
            // (closure-first, py:1913-1917 mig 0422) e' LOAD-BEARING: il verdetto
            // closure NON va consultato qui, altrimenti al cutover closure_judge
            // il ramo report leggerebbe un verdetto (potenzialmente stale del turno
            // precedente) che il Python ignora deliberatamente in questo punto.
            let unfulfilled_post = detect_unfulfilled_intent(Some(final_result.as_str()));
            if should_substitute_unfulfilled_report(
                state.automation_mode,
                unfulfilled_post,
                turn_action_oriented(state.action_oriented),
            ) {
                let report = build_unfulfilled_report(Some(final_result.as_str()), &messages);
                tracing::info!(
                    target: "nexus_agent_graph::executor",
                    "intenzione non eseguita in modalita' non-autonoma -> resoconto onesto"
                );
                final_result = report.clone();
                assistant_msg = Message::Ai {
                    content: MessageContent::text(report),
                    tool_calls: vec![],
                };
            }
        }

        // sticky: aggiornato solo se cascade ha fatto fallback (o gia' presente).
        let sticky_provider = if cascade_did_fallback {
            Some(eff_provider.clone())
        } else {
            state.sticky_provider.clone()
        };
        let sticky_model = if cascade_did_fallback {
            Some(eff_model.clone())
        } else {
            state.sticky_model.clone()
        };

        // action_nudge_count: +1 se il nudge G1 e' stato iniettato e non ha
        // prodotto tool call ancora (py:3494-3501).
        if g1_nudge_injected && pending_tool_uses.is_empty() {
            nudge_count += 1;
        }

        // ── SSE verso il frontend (parita' 1:1 con run_via_brain) ─────────────
        // L'executor ha deciso l'esito del turno: emette gli eventi che l'utente
        // si aspetta dal canale chat. Best-effort, infallibile (il sink no-op
        // dello shadow scarta tutto: in Replay nessun evento esce, garanzia gia'
        // assicurata dal `NullEventSink` iniettato nel ctx shadow da
        // `build_native_engine`; qui non si re-implementa alcun gate `shadow`).
        //  - ToolUse: un evento per ogni blocco tool_use deciso (mappa il `tool_use`
        //    del brain, che emette uno step Running per ogni tool richiesto).
        //  - EndTurn: turno concluso senza tool pendenti (il modello ha terminato
        //    la generazione). Il terminatore `Done` (is_final) NON e' dell'executor:
        //    lo emette il finalizzatore del run quando il grafo raggiunge End
        //    (l'executor puo' essere riattraversato in turni successivi), 1:1 con
        //    `run_via_brain` che mette `is_final=true` solo a fine retry loop.
        for tu in &pending_tool_uses {
            let id = tu.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let name = tu.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            ctx.emit.emit(SseEvent::ToolUse {
                id,
                name,
                input: tu.get("input").cloned().unwrap_or(Value::Null),
            });
        }
        if pending_tool_uses.is_empty() && stop_reason_enum == StopReason::EndTurn {
            ctx.emit.emit(SseEvent::EndTurn);
        }

        let mut delta = StateDelta {
            messages: Some(vec![assistant_msg]),
            result: Some(Some(final_result)),
            provider_used: Some(Some(eff_provider)),
            model_used: Some(Some(eff_model)),
            pending_tool_uses: Some(Some(pending_tool_uses)),
            stop_reason: Some(Some(stop_reason_enum)),
            recent_tool_signatures: Some(Some(updated_signatures)),
            consecutive_exploration_calls: Some(Some(expl.consecutive_exploration_calls)),
            exploration_nudge_sent: Some(Some(expl.exploration_nudge_sent)),
            progress_guided_axes: Some(Some(sorted(&progress_guided))),
            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
            repeated_cmd_nudge_sent: Some(Some(repeated_cmd_nudge_sent)),
            iterations: Some(Some(iters_in + 1)),
            action_nudge_count: Some(Some(nudge_count)),
            g1_reroute_count: Some(Some(g1_reroute_count)),
            sticky_provider: Some(sticky_provider),
            sticky_model: Some(sticky_model),
            // Usage del turno (py:3476-3480), overwrite last-write come il Python.
            prompt_tokens: Some(Some(turn_prompt_tokens)),
            completion_tokens: Some(Some(turn_completion_tokens)),
            cache_creation_tokens: Some(Some(turn_cache_creation)),
            cache_read_tokens: Some(Some(turn_cache_read)),
            total_tokens: Some(Some(turn_total_tokens)),
            total_cost_usd: Some(turn_total_cost),
            ..Default::default()
        };
        // Cutoff di generazione persistito solo al cambio fase (py:3458-3459).
        if let Some(ci) = gen_cutoff_index {
            delta.compress_cutoff_index = Some(Some(ci));
        }
        if let Some(cp) = gen_cutoff_phase {
            delta.compress_cutoff_phase = Some(Some(cp));
        }

        // auto_escalations nel delta (py:3475) — campo non tipizzato, vive in
        // `extra`. Valore Python: `escalations if loop_sig is not None else
        // int(state.get("auto_escalations") or 0)`. La variabile `escalations`
        // qui e' GIA' incrementata se il signature-loop ha promosso il modello
        // (ramo `tried_escalation`), e invariata altrimenti: coincide con il valore
        // Python in entrambi i rami. `extra` e' overwrite secco (regola di wrapping
        // delta.rs): preserviamo l'INTERA mappa extra dello stato e impostiamo solo
        // questa chiave per non azzerare gli altri campi runtime (project_id,
        // iteration_budget, ...).
        let mut extra_out = state.extra.clone();
        extra_out.insert("auto_escalations".to_string(), json!(escalations));
        delta.extra = Some(extra_out);

        // next_actions.derive (py:3379-3402) + unfulfilled-report (py:3404-3429):
        // PORTATI sopra (rimozione <suggested_actions> + derivazione scelte +
        // resoconto onesto). closure_judge.judge (py:3441-3455): genuinamente OFF
        // di default (agent.closure_judge.active=false) -> non diverge coi default.

        let _ = TASK_COMPLETE_TOOL_NAME; // dichiarazione done gestita nel dispatch

        Ok(delta.into_opaque())
    }
}

impl ExecutorNode {
    /// Risolve provider/model delegando al punto unico [`resolve_provider_model`]
    /// (regola L): legge sticky/override dallo stato + routing dalla config.
    fn resolve_provider(&self, state: &AgentState) -> ProviderResolution {
        resolve_provider_model(
            state.sticky_provider.as_deref(),
            state.sticky_model.as_deref(),
            state.provider_override.as_deref(),
            state.model_override.as_deref(),
            &self.cfg.routing_provider,
            &self.cfg.routing_model,
        )
    }

    /// Costruisce il delta di chiusura d'autorita' su `done` ripetuto >=3
    /// (py:1683-1705). PURO sullo stato (testo da summary/result/default).
    fn close_declared_done(&self, state: &AgentState, iters_in: i64) -> OpaqueDelta {
        let decl_summary = state
            .declared_outcome
            .as_ref()
            .and_then(|d| d.get("summary").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let close_text = decl_summary
            .or_else(|| {
                state
                    .result
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                "Lavoro dichiarato completato dal modello (task_complete ripetuto).".to_string()
            });
        tracing::warn!(
            target: "nexus_agent_graph::executor",
            done = state.declared_done_count.unwrap_or(0),
            "outcome=done dichiarato >=3 volte, chiusura d'autorita'"
        );
        StateDelta {
            messages: Some(vec![Message::Ai {
                content: MessageContent::text(close_text.clone()),
                tool_calls: vec![],
            }]),
            result: Some(Some(close_text)),
            pending_tool_uses: Some(Some(vec![])),
            stop_reason: Some(Some(StopReason::EndTurn)),
            iterations: Some(Some(iters_in + 1)),
            ..Default::default()
        }
        .into_opaque()
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Helper puri (mapping messaggi, segnali derivati)
// ──────────────────────────────────────────────────────────────────────────

/// Estrae il bool `fulfilled` dal `closure_verdict` (replica `_unfulfilled_signal`
/// e il ramo G1 del Python). PUNTO UNICO della lettura del verdetto: stesso campo
/// letto da `routing::signals::unfulfilled_signal` (qui per il conteggio G1).
fn closure_verdict_fulfilled(state: &AgentState) -> Option<bool> {
    match &state.closure_verdict {
        Some(Value::Object(map)) => match map.get("fulfilled") {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        },
        _ => None,
    }
}

/// Ultimo testo dell'assistente (in reverse) con content testuale non vuoto
/// (`_last_assistant_text`). `None` se nessuno. Usa `flatten_text` (regola L).
fn last_assistant_text(messages: &[Message]) -> Option<String> {
    for m in messages.iter().rev() {
        if let Message::Ai { content, .. } = m {
            let flat = content.flatten_text();
            if !flat.trim().is_empty() {
                return Some(flat);
            }
        }
    }
    None
}

/// Stringa snake_case dello `StopReason` (per il conteggio G1 che confronta
/// `end_turn`/`stop`). Riusa la serde rename dell'enum (punto unico).
fn stop_reason_str(sr: StopReason) -> String {
    match serde_json::to_value(sr) {
        Ok(Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Mappa una stringa stop_reason del provider/decisione al `StopReason` enum.
/// `None`/sconosciuto -> `EndTurn` (default Python `meta.get("stop_reason") or
/// "end_turn"`). Le stringhe del progress_controller (`loop_abort`) e del provider
/// (`tool_use`/`end_turn`/`stop`/`error`) mappano 1:1.
fn stop_reason_from_str(s: Option<&str>) -> StopReason {
    match s {
        Some("tool_use") => StopReason::ToolUse,
        Some("stop") => StopReason::Stop,
        Some("loop_detected") => StopReason::LoopDetected,
        Some("loop_abort") => StopReason::LoopAbort,
        Some("g1_escalated") => StopReason::G1Escalated,
        Some("g1_cap_reached") => StopReason::G1CapReached,
        Some("superseded") => StopReason::Superseded,
        Some("error") => StopReason::Error,
        // None | "end_turn" | qualunque altro -> end_turn (default Python).
        _ => StopReason::EndTurn,
    }
}

/// `true` se la `LlmResponse` indica un fallimento del forcing (function-call
/// malformata / tool_choice non supportato): replica il check Python su
/// `stop_reason=="error"` + substring nell'errore. Qui usiamo lo `stop_reason`
/// normalizzato `error` come segnale (il dettaglio dell'errore vive nel gateway
/// concreto, che puo' arricchire `stop_reason`). Conservativo: vero solo su error.
fn is_forcing_failure(resp: &crate::runtime::ports::LlmResponse) -> bool {
    resp.stop_reason.as_deref() == Some("error")
}

/// Crea un `Message::Human` con testo (per i nudge iniettati).
fn human_msg(text: &str) -> Message {
    Message::Human {
        content: MessageContent::text(text),
    }
}

/// Lista ordinata (come `sorted(set)` Python) per i campi `progress_*_axes`.
fn sorted(set: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort();
    v
}

/// Costruisce il `Message::Ai` finale del turno (continuita' tool_use/tool_result).
///
/// Se il gateway ha riportato `assistant_content` (blocchi anthropic_content:
/// testo + tool_use), li deserializza in `ContentBlock` per ricostruire il
/// messaggio a BLOCCHI (forma autoritativa Rust, == `additional_kwargs[
/// "anthropic_content"]` Python): i tool_use restano riferibili dai tool_result
/// del turno successivo. Se assenti: content testuale + i tool_calls in
/// `tool_calls` (forma OpenAI-compat), come fa il planner col solo tool_use.
fn build_assistant_message(resp: &crate::runtime::ports::LlmResponse, result_text: &str) -> Message {
    if !resp.assistant_content.is_empty() {
        let blocks: Vec<ContentBlock> = resp
            .assistant_content
            .iter()
            .map(|b| {
                serde_json::from_value::<ContentBlock>(b.clone()).unwrap_or_else(|_| {
                    ContentBlock::Text {
                        text: match b {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        },
                    }
                })
            })
            .collect();
        return Message::Ai {
            content: MessageContent::Blocks(blocks),
            tool_calls: vec![],
        };
    }
    // Forma minimale: testo + tool_calls (OpenAI-compat).
    Message::Ai {
        content: MessageContent::text(result_text),
        tool_calls: resp.tool_calls.clone(),
    }
}

/// Mappa un [`Message`] del canale interno in [`HistoryMessage`] (forma su cui
/// operano le primitive di context_reduction): `is_human` dal ruolo, `content`
/// testo o blocchi, `anthropic_content` i blocchi se presenti.
fn message_to_history(m: &Message) -> HistoryMessage {
    match m {
        Message::Human { content } => history_from_content(content, true),
        Message::Ai { content, tool_calls } => {
            // Se l'AI porta tool_calls (forma OpenAI-compat) ma content testuale,
            // espandiamo i tool_use in anthropic_content per la dedup/compress.
            let mut hm = history_from_content(content, false);
            if hm.anthropic_content.is_null() && !tool_calls.is_empty() {
                hm.anthropic_content = Value::Array(
                    tool_calls
                        .iter()
                        .map(|t| json!({"type": "tool_use", "id": t.id, "name": t.name, "input": t.input}))
                        .collect(),
                );
            }
            hm
        }
        // Il `ToolMessage` (risultato) preserva ruolo e id: `history_to_llm_messages`
        // ne ricostruisce il `role="tool"` + `tool_call_id` per il wire (continuita'
        // tool_use/tool_result, bug 2026-06-26). Senza questi campi il messaggio
        // verrebbe degradato ad assistant testuale e Anthropic risponderebbe HTTP
        // 400 (`tool_use ids without tool_result`). La compressione che lo riscrive
        // azzera questi flag (vedi `HistoryMessage::rebuilt_human`).
        Message::Tool { content, tool_call_id } => {
            let mut hm = history_from_content(content, false);
            hm.is_tool = true;
            hm.tool_call_id = Some(tool_call_id.clone());
            hm
        }
    }
}

fn history_from_content(c: &MessageContent, is_human: bool) -> HistoryMessage {
    match c {
        MessageContent::Text(s) => HistoryMessage {
            is_human,
            content: Value::String(s.clone()),
            anthropic_content: Value::Null,
            ..Default::default()
        },
        MessageContent::Blocks(blocks) => {
            let arr: Vec<Value> = blocks
                .iter()
                .map(|b| serde_json::to_value(b).unwrap_or(Value::Null))
                .collect();
            HistoryMessage {
                is_human,
                content: Value::Null,
                anthropic_content: Value::Array(arr),
                ..Default::default()
            }
        }
    }
}

/// Converte le [`HistoryMessage`] ridotte nei [`LlmMessage`] del wire,
/// PRESERVANDO la continuita' tool_use/tool_result (bug 2026-06-26, regola L: un
/// solo formato messaggio end-to-end). Un singolo `HistoryMessage` puo' espandersi
/// in PIU' [`LlmMessage`] (un turno human che porta N blocchi `tool_result`
/// diventa N messaggi `role="tool"`), quindi usiamo `flat_map`.
///
/// Il `tool_dispatch` produce i risultati come `Message::Human` con blocchi
/// `ContentBlock::ToolResult` (forma autoritativa Rust == HumanMessage +
/// anthropic_content Python); l'assistant-con-tool porta i `tool_use` nei blocchi.
/// Il server (`to_anthropic_messages`) riconosce la coppia tool_use/tool_result
/// SOLO da `role="tool"`+`tool_call_id` e da `assistant`+`tool_calls`: i blocchi
/// `tool_result`/`tool_use` lasciati DENTRO il content di un user verrebbero
/// serializzati come stringa JSON (Anthropic non li vede) -> HTTP 400 (`tool_use
/// ids without tool_result`). Qui li ESTRAIAMO nei campi giusti.
fn history_to_llm_messages(hist: &[HistoryMessage]) -> Vec<LlmMessage> {
    hist.iter().flat_map(history_msg_to_wire).collect()
}

/// Espande UN [`HistoryMessage`] nei [`LlmMessage`] del wire (vedi
/// [`history_to_llm_messages`]).
fn history_msg_to_wire(m: &HistoryMessage) -> Vec<LlmMessage> {
    // 1) `Message::Tool` esplicito: un solo messaggio `role="tool"` + id.
    if m.is_tool {
        let content = if m.content.is_null() {
            tool_result_content(&m.anthropic_content)
        } else {
            m.content.clone()
        };
        return vec![LlmMessage {
            role: "tool".to_string(),
            content,
            tool_call_id: m.tool_call_id.clone(),
            ..Default::default()
        }];
    }

    // 2) Messaggio a blocchi: separa tool_result (-> messaggi tool), tool_use
    //    (-> tool_calls dell'assistant) e testo. Copre sia l'assistant-con-tool
    //    sia l'human che trasporta i tool_result del tool_dispatch.
    if let Some(blocks) = m.anthropic_content.as_array() {
        if !blocks.is_empty() {
            let tool_results = extract_tool_results(blocks);
            let tool_uses = extract_tool_uses(blocks);
            if !tool_results.is_empty() || !tool_uses.is_empty() {
                let mut out: Vec<LlmMessage> = Vec::new();
                // I tool_result diventano messaggi `role="tool"` (round-trip id).
                for (tool_use_id, content) in tool_results {
                    out.push(LlmMessage {
                        role: "tool".to_string(),
                        content,
                        tool_call_id: Some(tool_use_id),
                        ..Default::default()
                    });
                }
                // I tool_use diventano `tool_calls` di un assistant (+ testo).
                if !tool_uses.is_empty() {
                    out.push(LlmMessage {
                        role: "assistant".to_string(),
                        content: flatten_history_blocks_text(&m.anthropic_content),
                        tool_calls: Some(tool_uses),
                        ..Default::default()
                    });
                }
                return out;
            }
            // Blocchi senza tool (es. solo testo / immagini): preserva i blocchi.
            let role = if m.is_human { "user" } else { "assistant" };
            return vec![LlmMessage {
                role: role.to_string(),
                content: m.anthropic_content.clone(),
                ..Default::default()
            }];
        }
    }

    // 3) Forma minimale role/content (turno puramente testuale).
    let role = if m.is_human { "user" } else { "assistant" };
    vec![LlmMessage {
        role: role.to_string(),
        content: m.content.clone(),
        ..Default::default()
    }]
}

/// Estrae i blocchi `{type:"tool_use", id, name, input}` di un `anthropic_content`
/// in [`ToolUse`] (continuita' tool_use/tool_result). Blocchi non-tool_use ignorati.
fn extract_tool_uses(blocks: &[Value]) -> Vec<ToolUse> {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|b| {
            let id = b.get("id").and_then(Value::as_str)?.to_string();
            let name = b.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
            Some(ToolUse { id, name, input })
        })
        .collect()
}

/// Estrae i blocchi `{type:"tool_result", tool_use_id, content}` come coppie
/// `(tool_use_id, content-stringa)`. Il `tool_use_id` referenzia il `tool_use`
/// dell'assistant che lo ha richiesto (round-trip). Il content e' reso a stringa
/// (il server fa comunque `content_to_string` per il ruolo tool).
fn extract_tool_results(blocks: &[Value]) -> Vec<(String, Value)> {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|b| {
            let id = b.get("tool_use_id").and_then(Value::as_str)?.to_string();
            let content = match b.get("content") {
                Some(Value::String(s)) => Value::String(s.clone()),
                Some(other) => Value::String(serde_json::to_string(other).unwrap_or_default()),
                None => Value::String(String::new()),
            };
            Some((id, content))
        })
        .collect()
}

/// Concatena (separati da `\n`) i testi dei blocchi `{type:"text", text}` di un
/// `anthropic_content` in una `Value::String` (testo dell'assistant per il wire).
/// `Value::Null` (nessun blocco testo) per content vuoto.
fn flatten_history_blocks_text(anthropic_content: &Value) -> Value {
    let Some(arr) = anthropic_content.as_array() else {
        return Value::Null;
    };
    let text = arr
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    Value::String(text)
}

/// Estrae il payload del/dei blocco/i `{type:"tool_result", content}` di un
/// `anthropic_content` (forma Anthropic di un `Message::Tool` a blocchi) verso il
/// content stringa atteso dal server per il ruolo `tool`. Un solo tool_result
/// (caso comune): il suo content cosi' com'e' (stringa) o serializzato (struttura).
/// Piu' blocchi o blocchi non-tool_result: serializza l'intero array (il server
/// fa comunque `content_to_string`). `Value::Null` se non un array.
fn tool_result_content(anthropic_content: &Value) -> Value {
    let Some(arr) = anthropic_content.as_array() else {
        return Value::Null;
    };
    let results: Vec<&Value> = arr
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .collect();
    if let [single] = results.as_slice() {
        match single.get("content") {
            Some(Value::String(s)) => return Value::String(s.clone()),
            Some(other) => return Value::String(serde_json::to_string(other).unwrap_or_default()),
            None => {}
        }
    }
    // Fallback: l'intero array serializzato (il server lo stringifica comunque).
    Value::String(serde_json::to_string(arr).unwrap_or_default())
}

/// Stima chars del contesto a partire dalle [`HistoryMessage`] (riusa
/// [`estimate_context_chars`] sul mapping ContextMessage, regola L).
fn estimate_history_chars(hist: &[HistoryMessage]) -> i64 {
    let msgs: Vec<ContextMessage> = hist.iter().map(history_to_context).collect();
    estimate_context_chars(&msgs)
}

/// Token estimator PURO per il token_brake/forced_rag (parita' con la divisione
/// char/3.5 del crate). Confine I/O: il Python usa tiktoken (`_estimate_context_
/// tokens`); qui la stima deterministica char-based e' sufficiente per la
/// DECISIONE pura (la stima accurata tiktoken e' un TODO trait dedicato).
fn history_token_estimator(hist: &[HistoryMessage]) -> i64 {
    let msgs: Vec<ContextMessage> = hist.iter().map(history_to_context).collect();
    current_context_token_estimate(&msgs, "")
}

/// Mappa una [`HistoryMessage`] in [`ContextMessage`] per le stime.
fn history_to_context(m: &HistoryMessage) -> ContextMessage {
    ContextMessage {
        content: m.content.clone(),
        anthropic_content: m.anthropic_content.clone(),
    }
}

/// Età massima per il drop dei base64 (`drop_unused_base64_age`). DB-driven nel
/// Python (`_load_ctx_mgmt_config`); qui il safe-default documentato (mig 0199).
/// TODO: portarlo nella `ExecutorConfig` quando il wiring mcp-core lo richiedera'.
fn ctxr_drop_age() -> i64 {
    8
}

#[cfg(test)]
mod tests;
