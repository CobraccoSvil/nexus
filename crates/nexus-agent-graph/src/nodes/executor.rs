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
//!   [`detect_repeated_action_detailed`], [`count_recent_request_port`],
//!   [`has_active_resources_in_history`], [`detect_recent_tool_error`],
//!   [`detect_pending_steps_report_with`] (ADR 0018 fase 3: sostituto
//!   strutturale della detection lessicale rimossa), [`unfulfilled_signal`],
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
    provider_style_supports_forcing, should_force_tool_choice, structural_unfulfilled_signal,
    turn_action_oriented,
};
use crate::decisions::loop_signatures::{
    build_signature, detect_signature_loop_progress_aware_with, exploration_counter_update,
    LoopThresholds,
};
use crate::decisions::meta_reason::{build_stall_context, translate};
use crate::decisions::progress_controller::{self as pc, Action, Axis, ProgressSignals};
use crate::decisions::tool_dispatch::{
    current_context_token_estimate, estimate_context_chars, flatten_context_text,
    ContextMessage,
};
use crate::decisions::turn_focus::build_turn_focus_directive;
use crate::routing::signals::{
    count_recent_request_port, detect_recent_tool_error, detect_repeated_action_detailed,
    detect_pending_steps_report_with, detect_repeated_failed_command, has_active_resources_in_history,
    has_recent_productive_action, has_tool_calls_in_history, EXPLORATION_ONLY_TOOLS,
};
use crate::runtime::ports::{
    AgentStepStore, BillingCooldownPort, EscalationPort, LlmMessage, LlmRequest, MetaStepStore,
    ModelUpscalePort, NextActionsDeriver, RunControlStore, SseEvent, StallBudgetPort, SummaryStore,
    TokenCounter,
};
use crate::nodes::stall_recovery::{stall_move_key, STALL_CONTEXT_KEY};
use crate::runtime::ports::RecoveryMove;
use crate::runtime::AgentNodeCtx;
use crate::state::{
    put_extra, AgentState, ContentBlock, Message, MessageContent, StateDelta, StopReason, ToolUse,
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
    /// Soglia ripetizione azione di SOLA LETTURA
    /// (`agent.repeated_action_threshold.read_only`, default 4). Piu' alta di
    /// quella produttiva: una rilettura idempotente (read_file/list_files/grep)
    /// non "insiste a vuoto" come un build che fallisce, quindi non deve far
    /// scattare la macchina GUIDE->ABORT su un modello capace alla prima
    /// ripetizione accidentale (regola H, causa radice del falso-stallo lettura).
    pub repeated_action_threshold_read_only: i64,
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
    /// soglia forced-text = `cap - forced_text_offset`.
    pub iteration_cap: i64,
    /// Offset sottratto a `iteration_cap` per la soglia di forced-text
    /// (`agent.executor.forced_text_offset`, default 5): a `cap - offset`
    /// iterazioni il turno viene marcato FORZATO (chiusura imminente). Regola G:
    /// ex costante `iteration_cap - 5` hardcoded.
    pub forced_text_offset: i64,
    /// Budget massimo di escalation del run (`agent.executor.max_escalations`,
    /// default 3): cap UNICO (regola L) condiviso dal gate ESCALATE del
    /// progress_controller (`ProgressSignals.max_escalations`) e dall'
    /// auto-escalation intra-turno al signature-loop (`auto_escalations`). Regola
    /// G: ex literal `3` sparsi nei costrutti `ProgressSignals` e nel cap 2616.
    pub max_escalations: i64,
    /// Soglie DB-driven della rilevazione loop-by-signature
    /// (`agent.loop.signature_threshold` / `agent.loop.recent_signatures_cap`).
    /// Regola G: ex costanti `LOOP_THRESHOLD` / `RECENT_SIGNATURES_CAP`.
    pub loop_thresholds: LoopThresholds,
    /// Soglia (`agent.loop.repeated_user_question_threshold`, mig 0510, default 2)
    /// oltre la quale (`>=`) scatta l'asse `RepeatedUserQuestion` (loop
    /// clarification CROSS-RUN, chiude il loop email). Il conteggio arriva da
    /// `AgentState::repeated_clarify_count` (detector `ClarifyHistoryPort`,
    /// calcolato all'avvio del run). Regola G: DB-driven, mai hardcoded. Default 2.
    pub repeated_user_question_threshold: i64,
    /// `true` se lo smart-upscale e' attivo (`agent.upscale.enabled`, default ON
    /// in produzione): promuove a un modello con window piu' grande se il contesto
    /// stimato supera il window del modello corrente (PRIMA della chiamata LLM).
    pub upscale_enabled: bool,
    /// Ratio di overhead per il window richiesto all'upscale
    /// (`agent.upscale.target_overhead_ratio`, default 1.2): `required =
    /// est_tokens * ratio`. Il tier e la query catalog vivono nell'impl della porta.
    pub upscale_overhead_ratio: f64,
    /// `true` se il rolling-summary e' attivo (`agent.context.rolling_summary_enabled`):
    /// al cambio-fase RIASSUME (non solo tronca) i messaggi vecchi chiamando il
    /// modello economico via [`SummaryStore`]. Il modello e i turni di finestra
    /// vivono nell'impl della porta (regola G). Default safe-DB-down: OFF.
    pub rolling_summary_enabled: bool,
    /// `keep_recent` (numero di messaggi recenti da preservare) per il rolling
    /// summary, da `agent.context.rolling_keep_recent_turns`. I messaggi
    /// `hist[..len-keep_recent]` (aggiustati per il pairing) vengono riassunti.
    pub rolling_keep_recent: i64,
    /// Hard cap del contesto (ADR 0016 D2, `agent.context.hard_cap_ratio`): se
    /// DOPO upscale+brake la stima resta `>= ratio*window`, il run termina
    /// fail-fast con errore strutturato invece di chiamare l'LLM. `0.0` = gate
    /// OFF (default safe-DB-down).
    pub hard_cap_ratio: f64,
    /// Template del messaggio overflow risolto a monte dal DB
    /// (`agent.context.overflow_message_key` -> `nexus_prompt_templates`).
    /// Vuoto = messaggio deterministico coi soli numeri (il testo redazionale
    /// vive SOLO nel DB, regola G).
    pub overflow_message_template: String,
    /// Rilevamento "report con passi pendenti" attivo
    /// (`agent.closure.pending_steps_detection_enabled`, stessa chiave della
    /// RoutingConfig — ADR 0018 fase 3: e' il sostituto strutturale del vecchio
    /// fallback lessicale nei rami G1/report dell'executor).
    pub pending_steps_detection_enabled: bool,
    /// Item minimi dell'elenco pendenti (`agent.closure.pending_steps_min_items`).
    pub pending_steps_min_items: i64,
    /// `true` se il meta-reasoner LLM di recovery-da-stallo e' attivo
    /// (`agent.stall_recovery.enabled`, mig 0510, default `false`). Con OFF il
    /// gate di EMISSIONE dello `StallReason` non scatta MAI: la gerarchia fissa
    /// `progress_controller::decide` decide come oggi (comportamento BIT-IDENTICO,
    /// regola G: opt-in DB, nessun fallback hardcoded). Con ON, prima delle mosse
    /// costose (ForceDiagnose/ChangeStrategy/Escalate/Abort) l'executor instrada
    /// al nodo dedicato `StallRecovery` (superstep isolato, replay-safe).
    pub stall_recovery_enabled: bool,
    /// Budget di consultazioni del meta-reasoner per-SESSIONE
    /// (`agent.stall_recovery.max_moves_per_session`, mig 0510, default 6): cap
    /// duro oltre cui il gate NON emette piu' `StallReason` e ricade sulla
    /// gerarchia fissa (rete di sicurezza anti meta-loop / anti-costo). Regola G:
    /// DB-driven, mai hardcoded. Rilevante solo a `stall_recovery_enabled=true`.
    pub stall_recovery_max_moves_per_session: i64,
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
            repeated_action_threshold_read_only: 4,
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
            // Default safe-DB-down: ex costanti hardcoded (regola G). Valgono SOLO
            // se il DB non fornisce i setting; il wiring mcp-core passa i valori reali.
            forced_text_offset: 5,
            max_escalations: 3,
            loop_thresholds: LoopThresholds::default(),
            // Default safe-DB-down = seed mig 0510 (2). Con nessuna storia di
            // clarify ripetuti l'asse non scatta mai -> comportamento invariato.
            repeated_user_question_threshold: 2,
            // Default safe-DB-down: upscale OFF (il wiring mcp-core passa il valore
            // reale `agent.upscale.enabled`, ON in produzione). Coerente con la nota
            // "rami OFF coi safe-default" sopra: con questo default lo smart-upscale
            // non scatta (parita' col Python quando enabled=false).
            upscale_enabled: false,
            upscale_overhead_ratio: 1.2,
            // Default safe-DB-down: rolling-summary OFF (il wiring mcp-core passa
            // `agent.context.rolling_summary_enabled`). Con questo default il
            // summarizer non scatta: la riduzione resta deterministica (compress).
            rolling_summary_enabled: false,
            // Default coerente con `agent.context.rolling_keep_recent_turns` (2).
            rolling_keep_recent: 2,
            // Default safe-DB-down: hard cap OFF (il wiring mcp-core passa
            // `agent.context.hard_cap_ratio`, 0.95 in produzione).
            hard_cap_ratio: 0.0,
            overflow_message_template: String::new(),
            // Default identici ai safe-default della RoutingConfig (stesse
            // chiavi DB agent.closure.pending_steps_*).
            pending_steps_detection_enabled: true,
            pending_steps_min_items: 2,
            // Default safe-DB-down = seed mig 0510: meta-reasoner OFF, budget 6.
            // Con OFF il gate di emissione StallReason non scatta mai -> il motore
            // resta bit-identico a oggi (il wiring mcp-core passa i valori reali).
            stall_recovery_enabled: false,
            stall_recovery_max_moves_per_session: 6,
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
    /// Contatore token REALE opzionale (ADR 0016 D1, porta CPU-only): `None` =
    /// stima char-based storica. Iniettato dal wiring col builder
    /// [`Self::with_token_counter`] in base a `agent.context.tokenizer`.
    token_counter: Option<Arc<dyn TokenCounter>>,
    /// Porta I/O del rolling-summary (chiamata LLM al modello economico). La
    /// DECISIONE (`select_rolling_summary_cutoff` + serializzazione + applicazione)
    /// e' del modulo puro [`crate::decisions::context_reduction`] (regola L).
    /// Best-effort: errore -> history invariata (degrado). Gata Real.
    summary_store: Arc<dyn SummaryStore>,
    /// Porta I/O del budget CROSS-RUN del meta-reasoner (consultazioni per
    /// SESSIONE). OPZIONALE (`None` -> solo cap per-run via `extra`, comportamento
    /// storico): iniettata dal wiring col builder [`Self::with_stall_budget`]. La
    /// DECISIONE (cap raggiunto?) resta nel gate di emissione (regola L: la porta
    /// fornisce solo il conteggio). Fail-open: guasto -> conteggio 0, non blocca.
    stall_budget: Option<Arc<dyn StallBudgetPort>>,
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
        summary_store: Arc<dyn SummaryStore>,
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
            summary_store,
            token_counter: None,
            stall_budget: None,
        }
    }

    /// Inietta il contatore token REALE (ADR 0016 D1, es. tiktoken cl100k via
    /// mcp-token). Senza iniezione lo stimatore resta il char-based storico:
    /// stesso comportamento dei test e del fallback DB-down (mai un panico).
    pub fn with_token_counter(mut self, counter: Arc<dyn TokenCounter>) -> Self {
        self.token_counter = Some(counter);
        self
    }

    /// Inietta la porta del budget CROSS-RUN del meta-reasoner (consultazioni per
    /// SESSIONE). Senza iniezione (default) il cap resta il solo per-run
    /// (`extra["stall_moves_used"]`), comportamento storico bit-identico. Con la
    /// porta iniettata il gate di emissione somma il cross-run al per-run e rispetta
    /// il cap `agent.stall_recovery.max_moves_per_session` per-sessione.
    pub fn with_stall_budget(mut self, budget: Arc<dyn StallBudgetPort>) -> Self {
        self.stall_budget = Some(budget);
        self
    }

    /// PUNTO UNICO (regola L) della stima token della history nell'executor:
    /// upscale, brake, hard-cap e forced-RAG leggono TUTTI da qui. Con la porta
    /// iniettata conta i token REALI sul testo appiattito (stesso perimetro del
    /// char-based: `flatten_context_text`); senza, delega alla stima char/3.5
    /// storica (`current_context_token_estimate`).
    fn estimate_history_tokens(&self, hist: &[HistoryMessage]) -> i64 {
        match &self.token_counter {
            Some(counter) => {
                let msgs: Vec<ContextMessage> = hist.iter().map(history_to_context).collect();
                counter.count(&flatten_context_text(&msgs, ""))
            }
            None => history_token_estimator(hist),
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

/// Punto unico (regola L) della GRAZIA POST-ESCALATION: indice minimo (nel
/// prefisso persistito di `messages`) da cui i detector di stallo basati sui
/// messaggi (repeated_action 6c, resource_reallocation 6d) contano le azioni.
/// Scritto in `extra["repeat_scan_floor"]` da OGNI ramo che promuove un
/// modello: le azioni del modello precedente non contano piu' come stallo,
/// cosi' il promosso ha una finestra pulita e fa ALMENO una chiamata prima di
/// qualunque nuova decisione (incidente run c4fa064b). Clampato a `len`.
fn repeat_scan_floor(state: &AgentState, len: usize) -> usize {
    state
        .extra
        .get("repeat_scan_floor")
        .and_then(Value::as_i64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0)
        .min(len)
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

        // ── CONSUMO meta-reasoner (rientro dal nodo StallRecovery) ────────────
        // Se rientriamo con StallResolved (self-loop `StallRecovery -> executor`),
        // la RecoveryMove e' persistita in extra: la consumiamo QUI (blocco #6
        // punto 3). Ok(Some) -> applica la mossa (nudge/escalate/needs_input/blocked)
        // e ritorna; None -> Fallback (mossa assente/non traducibile) -> prosegue
        // la gerarchia fissa (rete di sicurezza). A flag OFF nessun detector emette
        // StallReason, quindi StallResolved non arriva MAI qui -> bit-identico.
        if state.stop_reason == Some(StopReason::StallResolved) {
            if let Some(delta) = self.consume_recovery_move(state, iters_in, ctx, mode).await {
                return Ok(delta);
            }
        }

        // ── Chiusura del turno DICHIARATIVO forzato (ADR 0034) ────────────────
        // Se un ramo di chiusura coordinata ha FORZATO la dichiarazione
        // (`outcome_declaration_forced`) e il modello HA dichiarato via
        // task_complete (`declared_outcome` presente, flag `force_outcome_
        // declaration` gia' consumato dal turno dichiarativo), il run chiude
        // QUI d'autorita' col summary dichiarato: nessun turno LLM aggiuntivo.
        // L'esito canonico a valle viene dal segnale MACCHINA (outcome/blocker/
        // refusal, regola M), non dal testo.
        let declaration_was_forced = state
            .extra
            .get("outcome_declaration_forced")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let declaration_pending = state
            .extra
            .get("force_outcome_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let declaration_closed = state
            .extra
            .get("outcome_declaration_closed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // `!declaration_closed`: la chiusura d'autorita' scatta UNA SOLA VOLTA.
        // Senza il guard, il rientro dal final_gate FAILED (che chiede di
        // applicare un fix) veniva ri-chiuso subito col summary stantio,
        // neutralizzando il ciclo di correzione oggettiva.
        if declaration_was_forced && !declaration_pending && !declaration_closed {
            if let Some(decl) = &state.declared_outcome {
                let outcome_kind = decl
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("sconosciuto")
                    .to_string();
                let close_text = decl
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Esito dichiarato: {outcome_kind}."));
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    outcome = %outcome_kind,
                    "chiusura post-dichiarazione forzata (ADR 0034): esito strutturato"
                );
                self.emit_phase(
                    ctx,
                    mode,
                    "outcome_declared",
                    format!("Esito dichiarato: {outcome_kind}"),
                    json!({"outcome": outcome_kind}),
                )
                .await;
                let mut extra_out = state.extra.clone();
                extra_out.insert("outcome_declaration_closed".to_string(), json!(true));
                return Ok(StateDelta {
                    messages: Some(vec![Message::Ai {
                        content: MessageContent::text(close_text.clone()),
                        tool_calls: vec![],
                        reasoning: None,
                    }]),
                    result: Some(Some(close_text)),
                    pending_tool_uses: Some(Some(vec![])),
                    stop_reason: Some(Some(StopReason::EndTurn)),
                    iterations: Some(Some(iters_in + 1)),
                    extra: Some(extra_out),
                    ..Default::default()
                }
                .into_opaque());
            }
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
                    reasoning: None,
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
        // ADR 0018 fase 3: il fallback lessicale (blacklist NARRAZIONE) e' stato
        // rimosso — senza verdetto closure il segnale e' il report STRUTTURALE
        // di passi pendenti (stesse chiavi DB agent.closure.pending_steps_*).
        let unfulfilled_for_g1 = match closure_verdict_fulfilled(state) {
            Some(fulfilled) => !fulfilled,
            None => detect_pending_steps_report_with(
                last_assistant_text(&messages).as_deref(),
                self.cfg.pending_steps_detection_enabled,
                self.cfg.pending_steps_min_items,
            ),
        };
        let g1_recent_error = detect_recent_tool_error(&messages, 4);
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
            recent_error: g1_recent_error,
            // Errore "stale": persiste oltre il doppio del budget di nudge G1 ->
            // il modello e' bloccato in loop sull'errore, non ci sta reagendo a uno
            // nuovo. Contarlo fa scattare il cap G1 + escalation invece di bruciare
            // iterazioni fino a iteration_cap. Soglia derivata dal cap DB-driven
            // g1_max_nudges (niente hardcoded, regola G).
            error_is_stale: g1_recent_error
                && iters_in >= self.cfg.g1_max_nudges.saturating_mul(2),
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
        // Oltre al cap re-entry "pulito" (g1.cap_reached), scatta anche su loop G1
        // CONCLAMATO: molte iterazioni e output ancora non compiuto. Cattura il loop
        // DESCRITTIVO con tool-call inutili intervallate (modello debole che descrive
        // o chiama tool a vuoto), dove il pattern end_turn/tool_use alternato NON
        // viene contato come re-entry da g1_accounting (e nudge_count non cresce
        // perche' pending_tool_uses non e' vuoto). Cosi' scatta l'escalation a un
        // modello piu' capace invece di bruciare iterazioni fino a iteration_cap.
        // Soglia DB-driven (g1_max_nudges, regola G); 4x = ben oltre il budget nudge,
        // quindi un run legittimo che progredisce non viene escalato per errore.
        // La soglia cresce col numero di escalation gia' fatte (auto_escalations):
        // ogni modello promosso riceve un nuovo budget di iterazioni prima del cap
        // successivo, evitando escalation "a raffica" che non darebbero a nessun
        // modello una chance reale di convergere (la prima escalation azzera i
        // contatori re-entry ma non iters_in, quindi una soglia fissa ri-scatterebbe
        // subito). Cap assoluto iteration_cap resta la safety net finale.
        let g1_escal_now = state
            .extra
            .get("auto_escalations")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let g1_loop_threshold = self
            .cfg
            .g1_max_nudges
            .saturating_mul(4)
            .saturating_mul(g1_escal_now + 1);
        // Anti falso-negativo (regola H): il ramo "loop conclamato" si basa su
        // `unfulfilled_for_g1`, che senza `closure_verdict` (non popolato nel grafo
        // nativo) usa il report STRUTTURALE di passi pendenti (ADR 0018 fase 3: la
        // vecchia blacklist lessicale della narrazione e' stata rimossa).
        // La guardia resta comunque: un run che ha PRODOTTO lavoro (es. installato
        // system-deps + browser e fatto passare i test E2E) non va abortito a
        // `iters>=soglia` solo per la forma dell'ultimo testo. NON chiudere come loop
        // se nelle ultime iterazioni c'e' stato lavoro produttivo (tool non-esplorazione).
        // Il cap re-entry "pulito" (`g1.cap_reached`) resta intatto; la safety net
        // finale e' sempre `iteration_cap`. Lookback in messaggi (~5-6 iterazioni:
        // AI tool_use + tool_result), ampio da non scattare su un run che ha appena
        // agito, stretto da non mascherare un loop davvero a vuoto.
        const G1_LOOP_PRODUCTIVE_LOOKBACK: usize = 16;
        let g1_recent_productive =
            has_recent_productive_action(&messages, G1_LOOP_PRODUCTIVE_LOOKBACK);
        // `!declaration_pending` (ADR 0034): con un turno dichiarativo PENDENTE
        // i gate di chiusura pre-LLM lasciano passare — al rientro dal delta
        // dichiarativo la clausola "loop conclamato" si ri-soddisfaceva sugli
        // stessi segnali immutati e consumava la finestra una-tantum SENZA che
        // il modello ricevesse mai la chiamata dichiarativa.
        let g1_cap_effective = (g1.cap_reached
            || (unfulfilled_for_g1 && iters_in >= g1_loop_threshold && !g1_recent_productive))
            && !declaration_pending;
        if matches!(head_gate(false, false, 0, g1_cap_effective), HeadGate::G1Cap) {
            // ESCALATION orchestratore (py:1962-1993): prima di arrenderci, l'
            // orchestratore PROMUOVE il turno a un modello piu' capace (catena DB
            // intra-provider + cross-provider loop_fallback_default), azzerando il
            // contatore reroute cosi' il nuovo modello ha il suo budget. La
            // SELEZIONE e' il punto unico puro [`pick_escalation_model`] (regola L);
            // gli input (catena/cooldown/cross) arrivano dalla porta. Solo a catena
            // ESAURITA (o auto_escalations >= 3) chiudiamo davvero al cap secco
            // (ramo `not _g1_picked`).
            // Coppia corrente = risoluzione del turno (punto unico, regola L):
            // dopo una promozione sticky il "corrente" e' il promosso, anche se
            // non ha ancora fatto una chiamata (vedi escalation_current_pair).
            let (g1_cur_provider, g1_cur_model) = self.escalation_current_pair(state);
            let g1_escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
            // `_g1_picked = _pick(...) if _g1_escal < 3 else None` (py:1962-1966).
            let mut g1_cooldown_flag = false;
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
                g1_cooldown_flag = inputs.provider_in_cooldown;
                // Indice catena 0 (non g1_escal): la catena della porta e'
                // RELATIVA al corrente (chain_for filtra rank > corrente) e il
                // corrente AVANZA via sticky a ogni promozione; l'indice storico
                // (pensato per la catena assoluta per base_model del Python)
                // saltava sistematicamente un tier a ogni escalation successiva.
                // Il CAP resta su auto_escalations < 3 (qui sopra).
                pick_escalation_model(
                    &inputs.chain,
                    g1_cur_provider.as_deref(),
                    g1_cur_model.as_deref(),
                    0,
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
                self.emit_phase(
                    ctx,
                    mode,
                    "escalation",
                    format!(
                        "Passo a {}/{} (il modello descrive senza agire)",
                        pick.provider, pick.model
                    ),
                    json!({
                        "to_provider": pick.provider,
                        "to_model": pick.model,
                        "reason": "g1_cap",
                        // Stato STRUTTURATO del provider di partenza (ADR 0037 B):
                        // la causa dello switch e' "descrive senza agire", il flag
                        // aggiunge se il provider era anche in cooldown (ADR 0020).
                        "cooldown": g1_cooldown_flag,
                    }),
                )
                .await;
                let esc_nudge = human_msg(
                    "Il modello precedente ha solo descritto le azioni senza eseguirle \
dopo i tentativi previsti. Ora rispondi tu, che sei un modello piu' capace: NON \
descrivere, ESEGUI subito il prossimo step concreto con un tool call.",
                );
                let mut extra_out = state.extra.clone();
                extra_out.insert("auto_escalations".to_string(), json!(g1_escal + 1));
                // Grazia post-escalation: il promosso riparte con finestra pulita
                // sull'asse repeated_action (vedi floor nel check 6c).
                extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                return Ok(StateDelta {
                    messages: Some(vec![esc_nudge]),
                    sticky_provider: Some(Some(pick.provider)),
                    sticky_model: Some(Some(pick.model)),
                    // FIX-A (scale-controller): tier del modello promosso propagato
                    // dal pick (regola M: campo strutturato, non parsing). INERTE
                    // finche' nessun decisore lo legge (PR-B2/B3) -> bit-identico.
                    current_tier: Some(pick.tier),
                    // Finestra pulita anche per il signature-loop: le firme del
                    // modello precedente non pesano sul promosso.
                    recent_tool_signatures: Some(Some(vec![])),
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
            // ADR 0034: prima della chiusura di sistema, UN turno dichiarativo
            // forzato — l'esito del run diventa la dichiarazione strutturata
            // del modello invece del testo sintetico qui sotto.
            if let Some(delta) = self.forced_declaration_delta(state, iters_in, ctx, mode).await {
                return Ok(delta);
            }
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
            self.emit_phase(
                ctx,
                mode,
                "loop_break",
                "Interrompo: il modello non agisce dopo i solleciti".to_string(),
                json!({"reason": "g1_cap", "reroute": g1_reroute_count}),
            )
            .await;
            return Ok(StateDelta {
                messages: Some(vec![Message::Ai {
                    content: MessageContent::text(cap_text.clone()),
                    tool_calls: vec![],
                    reasoning: None,
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
        let mut progress_strategy: HashSet<String> =
            state.progress_strategy_axes.clone().unwrap_or_default().into_iter().collect();
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
                        reasoning: None,
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

        // (6z) LOOP CLARIFICATION CROSS-RUN (asse RepeatedUserQuestion, blocco #5).
        // La stessa domanda-chiarimento e' gia' stata posta all'utente >= soglia
        // volte nella SESSIONE (segnale strutturato dai meta_step `kind='clarify'`,
        // calcolato all'avvio del run dal detector `ClarifyHistoryPort` e portato in
        // `state.repeated_clarify_count`, regola M). E' il loop che condannava
        // l'incidente email (la loop-detection copriva solo le firme di TOOL). Con
        // reasoner OFF: GUIDE fissa SOFT (nudge "non ri-chiedere lo stesso dato,
        // usalo com'e' o dichiara il blocco") — Fase 1 migliorativa senza LLM. Con
        // reasoner ON (blocco #6/#7) sara' quest'ultimo a scegliere una mossa piu'
        // ricca. `!declaration_pending`: il turno dichiarativo non va corto-circuitato.
        // Default (count 0 / soglia 2): l'asse non scatta -> comportamento invariato.
        let repeated_clarify_count = state.repeated_clarify_count.unwrap_or(0);
        if progress_on && !declaration_pending {
            let dec = pc::decide(&ProgressSignals {
                repeated_user_question_count: repeated_clarify_count,
                repeated_user_question_threshold: self.cfg.repeated_user_question_threshold,
                already_guided: progress_guided.clone(),
                ..Default::default()
            });
            if matches!(dec.axis, Some(pc::Axis::RepeatedUserQuestion))
                && matches!(dec.action, Action::Guide)
            {
                // GUIDE SOFT (dec.force_action=false per questo asse): NON forziamo
                // la tool call (non setto force_action_hard), inietto solo il nudge
                // fisso e registro l'asse (evita ripetizione del nudge nei turni
                // successivi dello stesso run).
                if let Some(t) = &dec.nudge_text {
                    messages.push(human_msg(t));
                }
                progress_guided.insert(pc::Axis::RepeatedUserQuestion.as_str().to_string());
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    count = repeated_clarify_count,
                    threshold = self.cfg.repeated_user_question_threshold,
                    "progress_controller GUIDE repeated_user_question (loop clarify cross-run)"
                );
            }
        }

        // (6a) ESPLORAZIONE a 2x soglia -> Guide / ESCALATE / abort
        // (py:2093-2159 + escalation dal loop di esplorazione). Prima di abortire,
        // se l'asse e' gia' stato guidato si tenta la PROMOZIONE del modello: stesso
        // pattern del cap G1 (punto unico pick_escalation_model + progress_controller
        // Action::Escalate). Cosi' la discovery ripetuta non chiude piu' secca senza
        // mai cambiare modello.
        // `!declaration_pending`: il turno dichiarativo non va corto-circuitato
        // dai gate di stallo (ADR 0034, vedi guard sul G1 cap).
        if exploration_count >= 2 * exploration_threshold && progress_on && !declaration_pending {
            // Candidato di escalation: provider/model correnti + escalation gia'
            // fatte; gated a < 3 esattamente come il cap G1.
            let expl_escal = state
                .extra
                .get("auto_escalations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            // Coppia corrente = risoluzione del turno (punto unico, regola L).
            let (expl_cur_provider, expl_cur_model) = self.escalation_current_pair(state);
            let mut expl_cooldown_flag = false;
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
                expl_cooldown_flag = inputs.provider_in_cooldown;
                // Indice catena 0: catena RELATIVA al corrente, che avanza via
                // sticky (vedi ramo G1); il cap resta su auto_escalations < 3.
                pick_escalation_model(
                    &inputs.chain,
                    expl_cur_provider.as_deref(),
                    expl_cur_model.as_deref(),
                    0,
                    inputs.provider_in_cooldown,
                    inputs.cross_provider.as_ref(),
                )
            } else {
                None
            };
            let expl_signals = ProgressSignals {
                exploration_count,
                exploration_threshold,
                already_guided: progress_guided.clone(),
                has_escalation_candidate: expl_picked.is_some(),
                escalations: expl_escal,
                max_escalations: self.cfg.max_escalations,
                ..Default::default()
            };
            // GATE meta-reasoner (blocco #6): vedi ramo repeated_action.
            if let Some(delta) = self
                .maybe_stall_reason_delta(
                    state,
                    Axis::Exploration,
                    &expl_signals,
                    iters_in,
                    &messages,
                    ctx,
                )
                .await
            {
                return Ok(delta);
            }
            let dec = pc::decide(&expl_signals);
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
                    self.emit_phase(
                        ctx,
                        mode,
                        "escalation",
                        format!(
                            "Passo a {}/{} (esplorazione senza risultato)",
                            pick.provider, pick.model
                        ),
                        json!({
                            "to_provider": pick.provider,
                            "to_model": pick.model,
                            "reason": "exploration",
                            // Stato STRUTTURATO del provider di partenza (ADR 0037 B):
                            // causa dello switch = esplorazione senza risultato; il
                            // flag aggiunge se era anche in cooldown (ADR 0020).
                            "cooldown": expl_cooldown_flag,
                        }),
                    )
                    .await;
                    let esc_nudge = human_msg(
                        "Il modello precedente ha continuato a esplorare senza produrre \
un risultato. Ora rispondi tu, che sei un modello piu' capace: NON esplorare oltre, \
ESEGUI subito il prossimo step concreto con un tool call (modifica file o comando di \
esecuzione/verifica), oppure rispondi a parole se era una domanda.",
                    );
                    let mut extra_out = state.extra.clone();
                    extra_out.insert("auto_escalations".to_string(), json!(expl_escal + 1));
                    // Grazia post-escalation (vedi floor nel check 6c).
                    extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                    progress_guided.insert("exploration".to_string());
                    return Ok(StateDelta {
                        messages: Some(vec![esc_nudge]),
                        sticky_provider: Some(Some(pick.provider)),
                        sticky_model: Some(Some(pick.model)),
                        // FIX-A (scale-controller): tier del modello promosso dal pick.
                        current_tier: Some(pick.tier),
                        // Finestra pulita anche per il signature-loop.
                        recent_tool_signatures: Some(Some(vec![])),
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
                            reasoning: None,
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
                    reasoning: None,
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

        // (6c) repeated_action (py:2230-2309). `!declaration_pending`: il turno
        // dichiarativo non va corto-circuitato dai gate di stallo (ADR 0034).
        if progress_on && !declaration_pending {
            // Punto unico (regola L): la rilevazione e' sensibile al CONTENUTO
            // (signature = name+path+hash(old_string)), cosi' due edit_file sullo
            // stesso file con old_string DIVERSI sono azioni distinte (count 1) e
            // non scattano falsi stalli. `hit.failed` discrimina l'edit/write
            // FALLITO per scegliere il nudge specifico "copia l'old_string esatto".
            //
            // GRAZIA POST-ESCALATION (incidente run c4fa064b): le azioni PRECEDENTI
            // all'ultima promozione di modello NON contano piu' come stallo. Senza
            // questo floor, al rientro da un Action::Escalate (G1Continue) il
            // detector rivedeva gli STESSI messaggi immutati e ri-decideva subito:
            // l'intero budget escalation (3) veniva bruciato in pochi millisecondi
            // senza che il modello promosso facesse nemmeno UNA chiamata, e il run
            // chiudeva in ABORT "il modello non riesce". Il floor (indice nel
            // prefisso PERSISTITO di `messages`) e' scritto da ogni ramo che
            // promuove; i nudge transitori del turno stanno dopo quel prefisso,
            // quindi lo slice resta coerente.
            let ra_scan_from = repeat_scan_floor(state, messages.len());
            let ra_hit = detect_repeated_action_detailed(&messages[ra_scan_from..], 24);
            let ra_label = ra_hit.as_ref().map(|h| h.label.clone());
            let ra_count = ra_hit.as_ref().map(|h| h.count).unwrap_or(0);
            let ra_edit_failed = ra_hit
                .as_ref()
                .map(|h| h.failed && matches!(h.tool_name.as_str(), "edit_file" | "write_file"))
                .unwrap_or(false);
            // Tool di SOLA LETTURA ripetuto identico (read_file/list_files/grep &
            // co.): la GUIDE guida a CONCLUDERE con testo invece di forzare un altro
            // read-only (NON-convergenza, regola H). Il set e' allineato a
            // EXPLORATION_ONLY_TOOLS (punto unico, regola L): un tool ripetibile e'
            // read-only se rientra fra quelli di sola esplorazione.
            let ra_read_only = ra_hit
                .as_ref()
                .map(|h| EXPLORATION_ONLY_TOOLS.contains(&h.tool_name.as_str()))
                .unwrap_or(false);
            // Avvio di un SERVIZIO long-running FALLITO ripetuto (run_service/
            // service_restart): il servizio parte e muore subito, l'agente lo rilancia
            // identico -> falso stallo. Il controller lo guida a leggere i log del
            // servizio e correggere la causa (nudge specifico, force-action OFF cosi'
            // i tool di lettura log restano), invece di forzare un rilancio cieco o
            // arrendersi con ABORT (regola H, gemello dell'edit fallito).
            let ra_service_failed = ra_hit
                .as_ref()
                .map(|h| h.failed && matches!(h.tool_name.as_str(), "run_service" | "service_restart"))
                .unwrap_or(false);
            // Fallimento STRUTTURATO generico (exit_code/is_error, regola M): una
            // qualsiasi azione ripetuta che fallisce DAVVERO (es. `run_command:
            // curl` con exit 7 = server non in ascolto) e' una causa radice da
            // diagnosticare, non un loop da abortire. Instrada a FORCE_DIAGNOSE
            // invece che a escalation/ABORT "il modello non riesce". I casi
            // specifici (edit/service falliti) mantengono i loro nudge dedicati.
            let ra_failed = ra_hit.as_ref().map(|h| h.failed).unwrap_or(false);
            // Soglia dedicata per le LETTURE idempotenti (piu' alta): una
            // rilettura accidentale non deve innescare subito GUIDE->ABORT su un
            // modello capace. Le azioni produttive (build/test/edit) mantengono la
            // soglia bassa (2) perche' ripeterle a vuoto e' davvero uno stallo.
            let ra_threshold = if ra_read_only {
                self.cfg.repeated_action_threshold_read_only
            } else {
                self.cfg.repeated_action_threshold
            };
            let matched = ra_label.as_ref().map(|_| ra_count >= ra_threshold).unwrap_or(false);
            if !matched {
                progress_guided.remove("repeated_action");
                progress_diagnosed.remove("repeated_action");
                progress_strategy.remove("repeated_action");
            } else if let Some(label) = ra_label {
                // Candidato escalation (stesso pattern di esplorazione/G1 cap):
                // prima di abortire su azione ripetuta, promuovi a un modello piu'
                // capace invece di arrenderti.
                let ra_escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
                // Coppia corrente = risoluzione del turno (punto unico, regola L).
                let (ra_cur_provider, ra_cur_model) = self.escalation_current_pair(state);
                let mut ra_cooldown_flag = false;
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
                    ra_cooldown_flag = inputs.provider_in_cooldown;
                    // Indice catena 0: catena RELATIVA al corrente, che avanza
                    // via sticky (vedi ramo G1); cap su auto_escalations < 3.
                    pick_escalation_model(
                        &inputs.chain,
                        ra_cur_provider.as_deref(),
                        ra_cur_model.as_deref(),
                        0,
                        inputs.provider_in_cooldown,
                        inputs.cross_provider.as_ref(),
                    )
                } else {
                    None
                };
                let ra_signals = ProgressSignals {
                    repeated_action: Some((label.clone(), ra_count)),
                    repeated_action_edit_failed: ra_edit_failed,
                    repeated_action_read_only: ra_read_only,
                    repeated_action_service_failed: ra_service_failed,
                    repeated_action_failed: ra_failed,
                    // Biforca il nudge read-only: su un task di fix orienta all'EDIT
                    // (no rinuncia), su una domanda concludi con testo (punto unico).
                    action_oriented: turn_action_oriented(state.action_oriented),
                    already_guided: progress_guided.clone(),
                    already_diagnosed: progress_diagnosed.clone(),
                    already_strategy_shifted: progress_strategy.clone(),
                    force_diagnose_enabled: self.cfg.repeated_action_force_diagnose_enabled,
                    has_escalation_candidate: ra_picked.is_some(),
                    escalations: ra_escal,
                    max_escalations: self.cfg.max_escalations,
                    ..Default::default()
                };
                // GATE meta-reasoner (blocco #6): PRIMA delle mosse costose (dopo il
                // livello-1 GUIDE cheap), se abilitato + budget + l'asse richiede
                // meta-ragionamento, instrada al nodo StallRecovery. A flag OFF -> None
                // -> prosegue la gerarchia fissa (bit-identico).
                if let Some(delta) = self
                    .maybe_stall_reason_delta(
                        state,
                        Axis::RepeatedAction,
                        &ra_signals,
                        iters_in,
                        &messages,
                        ctx,
                    )
                    .await
                {
                    return Ok(delta);
                }
                let dec = pc::decide(&ra_signals);
                match dec.action {
                    Action::Guide => {
                        progress_guided.insert("repeated_action".to_string());
                        // Forza una tool call correttiva (dec.force_action ora true per
                        // repeated_action): rimuove i read-only e impone tool_choice, cosi'
                        // il modello DEVE agire invece di ripetere o descrivere/arrendersi.
                        if dec.force_action {
                            force_action_hard = true;
                        }
                        if let Some(t) = &dec.nudge_text {
                            messages.push(human_msg(t));
                        }
                        tracing::warn!(target: "nexus_agent_graph::executor", "GUIDE repeated_action (force-action)");
                    }
                    Action::ForceDiagnose => {
                        progress_diagnosed.insert("repeated_action".to_string());
                        // La diagnosi deve sfociare in un edit, non in testo/resa.
                        if dec.force_action {
                            force_action_hard = true;
                        }
                        if let Some(t) = &dec.nudge_text {
                            messages.push(human_msg(t));
                        }
                        tracing::warn!(target: "nexus_agent_graph::executor", "FORCE_DIAGNOSE repeated_action (force-action)");
                    }
                    Action::ChangeStrategy => {
                        // Livello 1.9: prima di cambiare MODELLO, il modello
                        // CORRENTE cambia STRADA (strumento alternativo / piu'
                        // contesto / passo piu' piccolo). Una tantum per asse.
                        progress_strategy.insert("repeated_action".to_string());
                        if dec.force_action {
                            force_action_hard = true;
                        }
                        if let Some(t) = &dec.nudge_text {
                            messages.push(human_msg(t));
                        }
                        self.emit_phase(
                            ctx,
                            mode,
                            "strategy_shift",
                            format!("Cambio strategia su '{label}'"),
                            json!({"label": label, "count": ra_count}),
                        )
                        .await;
                        tracing::warn!(target: "nexus_agent_graph::executor", "CHANGE_STRATEGY repeated_action (force-action)");
                    }
                    Action::Abort => {
                        // Recap ONESTO (regola H): elenca i file REALMENTE modificati
                        // (edit/write riusciti) invece dell'hardcoded "nessuno". Se
                        // l'agente HA prodotto lavoro, NON dichiarare fallimento: chiudi
                        // con EndTurn verso il final_gate, che valuta l'esito reale
                        // (build/test); il messaggio falso "File toccati: nessuno" su un
                        // task gia' risolto era la causa di "il sistema risolve ma dice
                        // di aver fallito". Solo a 0 file modificati resta l'abort secco.
                        let touched =
                            crate::routing::signals::modified_files_from_messages(&messages, 40);
                        // ADR 0034: sui sottocasi di FALLIMENTO (0 file toccati,
                        // non read-only) prova PRIMA il turno dichiarativo: la
                        // chiusura diventa l'esito strutturato del modello
                        // (outcome/blocker/summary) invece del testo di sistema.
                        // I sottocasi con lavoro prodotto o read-only instradano
                        // gia' al final_gate (esito oggettivo): restano invariati.
                        if touched.is_empty() && !ra_read_only {
                            if let Some(delta) = self.forced_declaration_delta(state, iters_in, ctx, mode).await {
                                return Ok(delta);
                            }
                        }
                        let (ra_text, ra_stop) = if !touched.is_empty() {
                            (
                                format!(
                                    "ESITO: modifiche applicate ai file: {}.\nMi sono fermato \
perche' '{label}' si ripeteva senza ulteriore progresso; verifica i risultati (build/test) \
per confermare la correzione.",
                                    touched.join(", ")
                                ),
                                StopReason::EndTurn,
                            )
                        } else if ra_failed {
                            // Azione che FALLISCE per segnale STRUTTURATO
                            // (exit_code/is_error, regola M), non un loop a vuoto:
                            // dopo diagnosi/escalation la causa reale non e' stata
                            // risolta. NON e' incapacita' del modello: chiudi
                            // ONESTAMENTE nominando il fallimento reale e il blocker,
                            // instradando al final_gate (esito reale).
                            (
                                format!(
                                    "L'azione '{label}' continua a fallire con un errore REALE \
(vedi exit code / output qui sopra), non e' una ripetizione a vuoto. La causa radice non e' \
stata risolta: se e' codice/configurazione va corretta (es. bind/porta del servizio, \
dipendenza, variabile d'ambiente); se dipende da qualcosa di esterno \
(credenziale/permesso/servizio non disponibile), dichiaralo come blocco esplicito."
                                ),
                                StopReason::EndTurn,
                            )
                        } else if ra_read_only {
                            // Lettura idempotente ripetuta: il contenuto e' GIA' nel
                            // contesto. NON e' un fallimento del modello (era la causa
                            // del falso "il modello non riesce" su modelli capaci):
                            // chiudi in modo ONESTO instradando al final_gate, che
                            // valuta l'esito reale, invece dell'abort "non completato".
                            (
                                format!(
                                    "Ho gia' raccolto il contenuto necessario: '{label}' e' stato \
letto e il risultato e' nel contesto qui sopra. Concludo con quanto raccolto invece di \
rileggere di nuovo lo stesso bersaglio. Se manca un dato specifico per completare, indica \
UN bersaglio DIVERSO da esaminare, altrimenti rispondi con il risultato."
                                ),
                                StopReason::EndTurn,
                            )
                        } else {
                            (
                                format!(
                                    "ESITO: non completato.\nMi sono bloccato ripetendo la stessa \
azione ({label}) {ra_count} volte senza che il risultato cambiasse; interrompo invece di \
insistere a vuoto.\nFile toccati: nessuno.\nProssimo passo: identificare la causa radice \
del fallimento di '{label}' dall'output/errore qui sopra e procedere con un approccio \
diverso; se sei bloccato da una dipendenza/credenziale/permesso/servizio mancante, \
indicalo esplicitamente."
                                ),
                                stop_reason_from_str(dec.stop_reason.as_deref()),
                            )
                        };
                        tracing::warn!(
                            target: "nexus_agent_graph::executor",
                            touched = touched.len(),
                            "ABORT/CLOSE repeated_action (recap onesto)"
                        );
                        self.emit_phase(
                            ctx,
                            mode,
                            "loop_break",
                            format!("Interrompo: '{label}' ripetuto senza progresso"),
                            json!({"label": label, "count": ra_count, "reason": "repeated_action"}),
                        )
                        .await;
                        return Ok(StateDelta {
                            messages: Some(vec![Message::Ai {
                                content: MessageContent::text(ra_text.clone()),
                                tool_calls: vec![],
                                reasoning: None,
                            }]),
                            result: Some(Some(ra_text)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(ra_stop)),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
                            progress_strategy_axes: Some(Some(sorted(&progress_strategy))),
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
                        self.emit_phase(
                            ctx,
                            mode,
                            "escalation",
                            format!(
                                "Passo a {}/{} (stallo su '{label}')",
                                pick.provider, pick.model
                            ),
                            json!({
                                "to_provider": pick.provider,
                                "to_model": pick.model,
                                "reason": "repeated_action",
                                // Stato STRUTTURATO del provider di partenza (ADR
                                // 0037 B): causa dello switch = stallo su una stessa
                                // azione; il flag aggiunge se era anche in cooldown.
                                "cooldown": ra_cooldown_flag,
                            }),
                        )
                        .await;
                        let esc_nudge = human_msg(
                            "Hai ripetuto la stessa azione senza progresso. Ora rispondi tu, \
che sei un modello piu' capace: cambia approccio ed ESEGUI il prossimo step concreto; \
se invece il lavoro e' gia' fatto e funzionante (es. l'app si avvia e risponde), NON \
ripetere la verifica: dichiaralo concludendo positivamente con un breve riepilogo.",
                        );
                        let mut extra_out = state.extra.clone();
                        extra_out.insert("auto_escalations".to_string(), json!(ra_escal + 1));
                        // Grazia post-escalation: senza questo floor il rientro
                        // rivedeva le stesse azioni e ri-decideva in pochi ms,
                        // bruciando il budget senza chiamare il promosso.
                        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                        progress_guided.insert("repeated_action".to_string());
                        return Ok(StateDelta {
                            messages: Some(vec![esc_nudge]),
                            sticky_provider: Some(Some(pick.provider)),
                            sticky_model: Some(Some(pick.model)),
                            // FIX-A (scale-controller): tier del modello promosso dal pick.
                            current_tier: Some(pick.tier),
                            // Finestra pulita anche per il signature-loop.
                            recent_tool_signatures: Some(Some(vec![])),
                            g1_reroute_count: Some(Some(0)),
                            action_nudge_count: Some(Some(0)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(StopReason::G1Escalated)),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
                            progress_strategy_axes: Some(Some(sorted(&progress_strategy))),
                            extra: Some(extra_out),
                            ..Default::default()
                        }
                        .into_opaque());
                    }
                    Action::Proceed => {}
                    // AskUser/DeclareBlocked sono prodotte SOLO dal meta-reasoner
                    // (nodo StallRecovery, consumo dedicato), MAI da pc::decide in
                    // questo ramo della gerarchia fissa: no-op qui. Arm esplicito
                    // (niente `_`) cosi' il compilatore forza la copertura (regola L).
                    Action::AskUser | Action::DeclareBlocked => {}
                }
            }
        }

        // (6d) resource_reallocation (py:2321-2383). `!declaration_pending`:
        // vedi guard 6c (ADR 0034).
        if progress_on && !declaration_pending {
            // Grazia post-escalation ANCHE su questo asse: senza il floor, al
            // rientro da un Escalate i request_port pre-promozione erano ancora
            // nella finestra e l'asse ri-decideva subito (stessa classe
            // dell'incidente c4fa064b, spostata dal 6c al 6d).
            let rp_scan_from = repeat_scan_floor(state, messages.len());
            let rp_count = count_recent_request_port(&messages[rp_scan_from..], 16);
            let rp_threshold = self.cfg.reallocation_threshold;
            if rp_count < rp_threshold {
                progress_guided.remove("resource_reallocation");
            } else {
                // Candidato escalation (stesso pattern di repeated_action/esplorazione).
                let rp_escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
                // Coppia corrente = risoluzione del turno (punto unico, regola L).
                let (rp_cur_provider, rp_cur_model) = self.escalation_current_pair(state);
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
                    // Indice catena 0: catena RELATIVA al corrente, che avanza
                    // via sticky (vedi ramo G1); cap su auto_escalations < 3.
                    pick_escalation_model(
                        &inputs.chain,
                        rp_cur_provider.as_deref(),
                        rp_cur_model.as_deref(),
                        0,
                        inputs.provider_in_cooldown,
                        inputs.cross_provider.as_ref(),
                    )
                } else {
                    None
                };
                let rp_signals = ProgressSignals {
                    reallocation_count: rp_count,
                    reallocation_threshold: rp_threshold,
                    has_active_resources,
                    already_guided: progress_guided.clone(),
                    has_escalation_candidate: rp_picked.is_some(),
                    escalations: rp_escal,
                    max_escalations: self.cfg.max_escalations,
                    ..Default::default()
                };
                // GATE meta-reasoner (blocco #6): vedi ramo repeated_action.
                if let Some(delta) = self
                    .maybe_stall_reason_delta(
                        state,
                        Axis::ResourceReallocation,
                        &rp_signals,
                        iters_in,
                        &messages,
                        ctx,
                    )
                    .await
                {
                    return Ok(delta);
                }
                let dec = pc::decide(&rp_signals);
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
                                reasoning: None,
                            }]),
                            result: Some(Some(rp_text)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(stop_reason_from_str(dec.stop_reason.as_deref()))),
                            iterations: Some(Some(iters_in + 1)),
                            progress_guided_axes: Some(Some(sorted(&progress_guided))),
                            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
                            progress_strategy_axes: Some(Some(sorted(&progress_strategy))),
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
                        // Grazia post-escalation (vedi floor nel check 6c).
                        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                        progress_guided.insert("resource_reallocation".to_string());
                        return Ok(StateDelta {
                            messages: Some(vec![esc_nudge]),
                            sticky_provider: Some(Some(pick.provider)),
                            sticky_model: Some(Some(pick.model)),
                            // FIX-A (scale-controller): tier del modello promosso dal pick.
                            current_tier: Some(pick.tier),
                            // Finestra pulita anche per il signature-loop.
                            recent_tool_signatures: Some(Some(vec![])),
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
                    // ChangeStrategy non si applica all'asse reallocation (il
                    // livello 1.9 e' emesso solo per repeated_action).
                    Action::ForceDiagnose | Action::ChangeStrategy | Action::Proceed => {}
                    // AskUser/DeclareBlocked: prodotte solo dal meta-reasoner
                    // (consumo dedicato), mai da pc::decide qui -> no-op (regola L).
                    Action::AskUser | Action::DeclareBlocked => {}
                }
            }
        }

        // ── Forza-azione: rimuovi i read-only oltre soglia esplorazione (py:2402)
        // OPPURE quando il progress_controller ha forzato l'azione (`force_action_hard`:
        // GUIDE/ForceDiagnose di `repeated_action` read-only su turno operativo). Causa
        // radice della non-convergenza sui fix (regola H): senza questa estensione il
        // forcing imponeva `tool_choice=required` ma LASCIAVA i read-only nella lista ->
        // il modello, obbligato a chiamare UN tool, ri-chiamava `read_file` invece di
        // `edit_file`/`run_command`, riaprendo il loop (read 2 volte -> ABORT a 0 file
        // modificati). Rimuovendoli sotto force-action, l'unico tool disponibile diventa
        // PRODUTTIVO e l'agente APPLICA la correzione. ─
        //
        // SOLO su turno OPERATIVO (`action_oriented`): su un turno INFORMATIVO
        // ("elenca i file e dimmi il totale") la chiusura corretta e' una risposta
        // testuale dopo le letture — strippare i read-only lascia solo tool di
        // scrittura e l'agente, obbligato dal `tool_choice=required`, degenera in
        // edit_file su un task di sola lettura (loop -> ABORT hollow, incidente
        // 2026-07-02 TEST E2E). La biforcazione per action_oriented e' la stessa
        // del progress_controller (regola L); qui si applica al punto di strip.
        // `report_only == Some(true)` (classifier: nessuna modifica autorizzata)
        // disattiva lo strip anche quando action_oriented=true: un listing
        // richiede tool (action=true) ma resta di sola lettura (report=true).
        if !tools_json.is_empty()
            && (exploration_count >= exploration_threshold || force_action_hard)
            && turn_action_oriented(state.action_oriented)
            && state.report_only != Some(true)
        {
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
        // `forced_text_turn` marca il turno come FORZATO: una risposta VUOTA a
        // un turno forzato e' un esito NON verificato e non puo' chiudere il run
        // come 'completed' (incidente run b07c7e78: Gemini rispose con stringa
        // vuota alla finestra forced-text -> final_answer NULL, chat muta).
        let forced_text_threshold = self.cfg.iteration_cap - self.cfg.forced_text_offset;
        let mut forced_text_turn = false;
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
            forced_text_turn = true;
        }

        // ── Turno DICHIARATIVO forzato (ADR 0034): catalogo = solo task_complete ─
        // Richiesto da un ramo di chiusura coordinata (forced_declaration_delta):
        // il modello DEVE dichiarare l'esito strutturato. Ricostruito da
        // state.tools_json (non da tools_json locale) cosi' vince su ogni strip
        // precedente (forza-azione/forced-text). Il flag e' consumato nel delta
        // finale del turno (una sola finestra dichiarativa).
        let declaring_turn = state
            .extra
            .get("force_outcome_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if declaring_turn {
            let decl_only: Vec<Value> = state
                .tools_json
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|t| {
                    t.get("name").and_then(Value::as_str) == Some(TASK_COMPLETE_TOOL_NAME)
                })
                .collect();
            if !decl_only.is_empty() {
                tools_json = decl_only;
                force_action_hard = true;
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    "turno dichiarativo (ADR 0034): catalogo ridotto a task_complete + tool choice forzata"
                );
            }
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
            // ADR 0018 fase 3: la vecchia detection lessicale della narrazione e'
            // rimossa — il segnale e' STRUTTURALE (punto unico, lo stesso del
            // forcing early-action piu' sotto): turno precedente chiuso SENZA
            // tool call pur con tool disponibili e richiesta d'azione.
            let prev_acted = state.stop_reason == Some(StopReason::ToolUse);
            let is_unfulfilled = structural_unfulfilled_signal(
                !tools_json.is_empty(),
                !prev_acted && iters_in >= 1,
                is_action_req,
                iters_in,
                self.cfg.tool_choice_forcing_max_iteration,
            );
            let no_tools_yet = !has_tool_calls_in_history(&messages);
            if (is_action_req && no_tools_yet) || is_unfulfilled {
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
        // I/O (continuity-trim / system-offload) NON portati: TODO trait dedicati.
        // Il ROLLING-SUMMARY (riassume i vecchi via LLM economico) e' agganciato al
        // cambio-fase qui sotto via la porta [`SummaryStore`] (best-effort, gata Real).
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
            // CAMBIO FASE: dedup + drop base64.
            hist = ctxr::dedup_tool_results_history(&hist);
            hist = ctxr::drop_unused_base64_payloads(&hist, ctxr_drop_age(), 2);

            // ROLLING-SUMMARY (intervento 3): RIASSUME il prefisso vecchio invece di
            // limitarsi a comprimere/troncare. DECISIONE pura (punto unico, regola L):
            // cutoff -> serialize -> SummaryStore.summarize (I/O, gata Real) -> apply.
            // BEST-EFFORT: su guasto (LLM down, cooldown, Replay no-op) la history
            // resta INVARIATA e si prosegue (compress/token_brake fanno il resto).
            if self.cfg.rolling_summary_enabled {
                if let Some(cut) =
                    ctxr::select_rolling_summary_cutoff(&hist, self.cfg.rolling_keep_recent)
                {
                    let prefix_text = ctxr::serialize_prefix_for_summary(&hist, cut);
                    match self.summary_store.summarize(prefix_text, mode).await {
                        Ok(summary) if !summary.trim().is_empty() => {
                            let before = hist.len();
                            hist = ctxr::apply_rolling_summary(&hist, cut, &summary);
                            tracing::info!(
                                target: "nexus_agent_graph::executor",
                                run_id = %run_id,
                                phase = phase_now,
                                msgs_before = before,
                                msgs_after = hist.len(),
                                cutoff = cut,
                                "rolling summary: prefisso conversazione riassunto"
                            );
                        }
                        Ok(_) => {
                            // Summary vuoto: degrada (history invariata).
                            tracing::warn!(
                                target: "nexus_agent_graph::executor",
                                run_id = %run_id,
                                "rolling summary: risposta vuota, degrado a history invariata"
                            );
                        }
                        Err(e) => {
                            // Guasto LLM / Replay no-op: degrado best-effort.
                            tracing::warn!(
                                target: "nexus_agent_graph::executor",
                                run_id = %run_id,
                                error = %e,
                                "rolling summary non disponibile, degrado a history invariata"
                            );
                        }
                    }
                }
            }

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
        let upscale_est_tokens = self.estimate_history_tokens(&hist);
        let upscale_window = self.upscale.context_window(&model).await.unwrap_or(0);
        // Window effettivo del turno: quello del modello richiesto (config, regola
        // G) finche' l'upscale non promuove; dopo la promozione, quello del modello
        // promosso (dal catalog via porta), cosi' brake e hard-cap a valle usano il
        // window del modello EFFETTIVO come dichiarato dall'intento di questa fase.
        let mut effective_window = self.cfg.context_window;
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
                if let Ok(w) = self.upscale.context_window(&model).await {
                    if w > 0 {
                        effective_window = w;
                    }
                }
            }
        }

        // Token brake (py:2836): cap hard sotto window (token_estimator puro qui).
        if effective_window > 0 {
            hist = ctxr::apply_token_brake(
                &hist,
                effective_window,
                &self.cfg.token_brake,
                &|h: &[HistoryMessage]| self.estimate_history_tokens(h),
            );
        }

        // ── hard cap post-brake (ADR 0016 fase D2): fail-fast strutturato ─────
        // Se DOPO upscale+brake la stima resta oltre `hard_cap_ratio*window`, il
        // brake non e' riuscito a rientrare (es. singolo messaggio enorme): la
        // chiamata LLM fallirebbe comunque. Meglio terminare il run con errore
        // STRUTTURATO (meta_step `context_overflow` + `extra.error_class`, regola
        // M) e messaggio dal template DB (`system.context_overflow`, regola G)
        // che mandare una richiesta cieca destinata al 400 del provider.
        if self.cfg.hard_cap_ratio > 0.0 {
            let post_brake_est = self.estimate_history_tokens(&hist);
            if ctxr::check_hard_cap(post_brake_est, effective_window, self.cfg.hard_cap_ratio) {
                let text = if self.cfg.overflow_message_template.is_empty() {
                    // Fallback deterministico coi soli numeri: il testo
                    // redazionale vive SOLO nel template DB (regola G).
                    format!(
                        "[context_overflow] stima {post_brake_est} token oltre il limite \
della finestra {effective_window} del modello {provider}/{model}"
                    )
                } else {
                    ctxr::render_overflow_message(
                        &self.cfg.overflow_message_template,
                        post_brake_est,
                        effective_window,
                    )
                };
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    est = post_brake_est,
                    window = effective_window,
                    ratio = self.cfg.hard_cap_ratio,
                    provider = %provider,
                    model = %model,
                    "HARD CAP contesto raggiunto dopo brake -> fail-fast (ADR 0016 D2)"
                );
                let overflow_meta = json!({
                    "kind": "context_overflow",
                    "title": "Contesto oltre il limite del modello",
                    "payload": {
                        "estimated_tokens": post_brake_est,
                        "max_window": effective_window,
                        "hard_cap_ratio": self.cfg.hard_cap_ratio,
                        "provider": provider,
                        "model": model,
                    },
                });
                ctx.emit.emit(SseEvent::MetaStep {
                    kind: "context_overflow".to_string(),
                    title: "Contesto oltre il limite del modello".to_string(),
                    payload: overflow_meta.get("payload").cloned().unwrap_or(Value::Null),
                });
                let _ = self.meta_steps.persist_meta_step(overflow_meta, mode).await;
                // `extra` nel delta e' overwrite: merge con lo stato per non
                // perdere le chiavi esistenti.
                let mut extra = state.extra.clone();
                extra.insert("error_class".to_string(), json!("context_overflow"));
                return Ok(StateDelta {
                    messages: Some(vec![Message::Ai {
                        content: MessageContent::text(text.clone()),
                        tool_calls: vec![],
                        reasoning: None,
                    }]),
                    result: Some(Some(text)),
                    pending_tool_uses: Some(Some(vec![])),
                    stop_reason: Some(Some(StopReason::Error)),
                    iterations: Some(Some(iters_in + 1)),
                    extra: Some(extra),
                    ..Default::default()
                }
                .into_opaque());
            }
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
        let rag_est = self.estimate_history_tokens(&hist);
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
        // Forcing "early action" TRANSITORIO (NON-convergenza, regola H): il
        // forcing `tool_choice=required` non si applica a OGNI iterazione iniziale
        // (cio' costringeva il modello a chiamare un tool anche quando il task era
        // gia' soddisfatto -> ripetizione list_files/read_file -> escalation ->
        // overflow su un task read-only banale). Si applica SOLO quando il turno
        // PRECEDENTE non ha agito pur dovendo (il caso BUG-e: tool disponibili +
        // action_oriented + nessuna tool call), segnalato dal punto unico
        // [`structural_unfulfilled_signal`] (regola L) sullo `stop_reason` in
        // ingresso (= esito del turno precedente). Cosi':
        //   - turno precedente ToolUse -> ha agito -> NIENTE forcing early: il
        //     modello PUO' chiudere con una risposta testuale quando il task e' fatto;
        //   - turno precedente end_turn/stop con tool+action_oriented (BUG-e:
        //     descrive senza agire) -> forcing early ON, esattamente come prima.
        // Il primo turno (iters_in=0, nessun precedente) NON forza: `no_tool_call`
        // sotto e' `prev != ToolUse` (vero a iter 0), ma `had_tools` del turno
        // precedente non esiste -> il segnale strutturale resta governato da
        // iters_in>=1 nel ramo BUG-e reale (turno gia' speso senza agire). La
        // forza-azione HARD del progress_controller (`force_action_hard`, stalli
        // esplorazione/repeated_action/g1) resta SEMPRE attiva: NON e' toccata qui.
        let prev_acted = state.stop_reason == Some(StopReason::ToolUse);
        let early_action_bug_e = structural_unfulfilled_signal(
            !tools_json.is_empty(),
            !prev_acted && iters_in >= 1,
            turn_action_oriented(state.action_oriented),
            iters_in,
            self.cfg.tool_choice_forcing_max_iteration,
        );
        // Forza-azione hard (progress_controller GUIDE) OPPURE forcing "early
        // action" condizionato a BUG-e: in entrambi i casi `Some(true)`, py:2946-2969.
        let force_now = (force_action_hard && supports_forcing)
            || (early_action_bug_e
                && should_force_tool_choice(
                    !tools_json.is_empty(),
                    turn_action_oriented(state.action_oriented),
                    iters_in,
                    in_discovery,
                    supports_forcing,
                    self.cfg.tool_choice_forcing_enabled,
                    self.cfg.tool_choice_forcing_max_iteration,
                ));
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
                // FAILOVER cross-provider sul provider caduto/indisponibile (regola H
                // + regola L): se il gateway ha segnalato in modo STRUTTURATO che il
                // provider scelto NON e' disponibile ([`PortError::ProviderUnavailable`],
                // = 500 `PROVIDER_ERROR`: tutti i provider risolti per QUESTA richiesta
                // in cooldown), NON chiudere il run con `StopReason::Error`: RIPIEGA su
                // un provider SANO delegando al PUNTO UNICO del routing iniziale
                // ([`EscalationPort::failover_provider`] -> `select_agentic_model`), la
                // STESSA selezione che il rilancio manuale userebbe. NON usa piu' il
                // `loop_fallback_default` (un solo candidato statico, senza filtro
                // cooldown, che non faceva cascata: o coincideva col corrente -> None,
                // o puntava a un provider a crediti zero -> loop fino a Error). Se c'e'
                // un provider sano, promuoviamo lo sticky e usciamo con `G1Escalated`:
                // il self-loop rientra nell'executor col provider nuovo (stesso pattern
                // del ramo G1). I provider gia' provati sono accumulati in
                // `failover_tried` cosi' la cascata ne sceglie sempre uno diverso. Solo
                // quando NESSUN provider sano resta cadiamo nella chiusura `Error`
                // (onesta). Gated `auto_escalations < 3` (no escalation a raffica).
                if matches!(err, crate::runtime::ports::PortError::ProviderUnavailable(_)) {
                    let cd_escal = state
                        .extra
                        .get("auto_escalations")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    if cd_escal < 3 {
                        // CASCATA cross-provider: accumuliamo i provider gia' provati
                        // in questo run (incluso quello appena caduto) cosi' ogni salto
                        // sceglie un provider SANO DIVERSO, invece di insistere su un
                        // candidato fisso (era il difetto del vecchio
                        // `loop_fallback_default`: un solo provider statico, senza
                        // filtro cooldown -> o coincideva col corrente -> None, o
                        // puntava a un provider a crediti zero -> loop fino a Error).
                        let mut tried: Vec<String> = state
                            .extra
                            .get("failover_tried")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !tried.iter().any(|p| p == &provider) {
                            tried.push(provider.clone());
                        }
                        // Punto unico (regola L): il MIGLIOR provider agentico SANO
                        // escludendo i gia' provati. DELEGA alla STESSA selezione del
                        // routing iniziale (`select_agentic_model`, che esclude da se'
                        // anche i cooldown) -> la rete scala IN-RUN, senza che l'utente
                        // debba ri-lanciare. Fail-open: errore -> None -> chiusura Error.
                        if let Ok(Some(pick)) =
                            self.escalation.failover_provider(&tried).await
                        {
                            tracing::warn!(
                                target: "nexus_agent_graph::executor",
                                from_provider = %provider,
                                to_provider = %pick.provider,
                                to_model = %pick.model,
                                tried = tried.len(),
                                "provider caduto -> FAILOVER cross-provider via routing (cascata)"
                            );
                            self.emit_phase(
                                ctx,
                                mode,
                                "escalation",
                                format!(
                                    "Provider {provider} non disponibile: passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                                json!({
                                    "from_provider": provider,
                                    "to_provider": pick.provider,
                                    "to_model": pick.model,
                                    "reason": "provider_failover",
                                    // Causa STRUTTURATA (ADR 0037 arricchimento B):
                                    // questo ramo entra SOLO su ProviderUnavailable,
                                    // derivato dal CODICE errore del gateway
                                    // (PROVIDER_ERROR), non dal testo (regola M). Lo
                                    // switch e' dovuto a cooldown/quota del provider
                                    // di partenza: il frontend puo' colorare la banda
                                    // "Cambio provider" come cooldown senza euristiche.
                                    "cooldown": true,
                                    "cause": "cooldown",
                                }),
                            )
                            .await;
                            let esc_nudge = human_msg(
                                "Il provider precedente non e' disponibile (in cooldown). \
Riprendi tu, su un provider sano: esegui il prossimo step concreto del compito.",
                            );
                            // Marca il provider scelto come provato: se cadesse anche
                            // lui, il giro dopo lo esclude e ne sceglie un altro sano.
                            tried.push(pick.provider.clone());
                            let mut extra_out = state.extra.clone();
                            extra_out
                                .insert("auto_escalations".to_string(), json!(cd_escal + 1));
                            extra_out
                                .insert("failover_tried".to_string(), json!(tried));
                            // Grazia post-escalation (vedi floor nel check 6c).
                            extra_out.insert(
                                "repeat_scan_floor".to_string(),
                                json!(state.messages.len()),
                            );
                            return Ok(StateDelta {
                                messages: Some(vec![esc_nudge]),
                                sticky_provider: Some(Some(pick.provider)),
                                sticky_model: Some(Some(pick.model)),
                                // FIX-A (scale-controller): tier del modello di
                                // failover, risolto dal catalog nell'adapter
                                // (`failover_provider`); `None` se ignoto -> default a
                                // valle (bit-identico, nessun decisore legge ancora).
                                current_tier: Some(pick.tier),
                                // Finestra pulita anche per il signature-loop.
                                recent_tool_signatures: Some(Some(vec![])),
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
                    }
                    tracing::warn!(
                        target: "nexus_agent_graph::executor",
                        provider = %provider,
                        auto_escalations = cd_escal,
                        "provider caduto ma nessun provider sano disponibile: chiusura Error"
                    );
                }
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

        // Thinking di QUESTO turno executor (FIX D4): catturato qui per essere
        // accumulato nel delta finale (reasoning_acc), oltre che emesso LIVE.
        let mut turn_thinking: Option<String> = None;
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
                // Accumula per la persistenza (FIX D4): il blocco "Ragionamento"
                // deve sopravvivere al refresh. Lo stesso testo viaggia LIVE come
                // SseEvent::ThinkingDelta e finisce nel reasoning_acc dello stato.
                turn_thinking = Some(t.clone());
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
        // PROGRESS-AWARE (regola L, stessa esclusione del detector repeated_action):
        // le riletture read-only che seguono un'azione produttiva (edit/build) sono
        // DEBUGGING, non stallo — senza questa esclusione il loop-detector uccideva
        // un modello capace mentre convergeva su una build rossa (run b833a83d).
        let det = detect_signature_loop_progress_aware_with(
            &recent,
            &new_signatures,
            |n| EXPLORATION_ONLY_TOOLS.contains(&n),
            self.cfg.loop_thresholds,
        );
        // `escalations` (auto_escalations) cresce di 1 quando il loop scala il
        // modello (py:3284); resta invariato altrimenti. Tracciato per il delta.
        let mut escalations = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
        // `true` quando il signature-loop ha promosso e ri-eseguito il turno col
        // modello promosso: rende la promozione STICKY nel delta finale (prima era
        // one-shot: il turno successivo ricadeva sul modello di routing debole, che
        // rientrava subito in loop — incidente run c4fa064b) e fa partire la grazia
        // post-escalation (floor repeated_action).
        let mut signature_loop_promoted = false;
        // FIX-A (scale-controller): tier del modello promosso dal signature-loop,
        // catturato dal pick (regola M) per scriverlo in `current_tier` insieme allo
        // sticky nel delta finale del turno. `None` finche' non c'e' promozione.
        let mut signature_loop_tier: Option<String> = None;
        let mut loop_close_result: Option<String> = None;
        // OUTPUT-PROGRESSO (regola M/H): una firma ripetuta i cui ULTIMI due
        // esiti TESTUALI differiscono sta PROGREDENDO (es. build rilanciata dopo
        // ogni correzione che fallisce con errori via via diversi): NON e' un
        // loop. Confronto STRUTTURALE degli output (punto unico
        // repeated_signature_output_progress), mai semantica del testo.
        let loop_sig_effective = det.loop_signature.as_ref().filter(|sig| {
            let progress =
                crate::routing::signals::repeated_signature_output_progress(&messages, sig, 24);
            if progress {
                tracing::info!(
                    target: "nexus_agent_graph::executor",
                    sig = %sig,
                    "signature ripetuta ma con esiti DIVERSI (output-progresso): non e' un loop"
                );
            }
            !progress
        });
        if let Some(loop_sig) = loop_sig_effective {
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
            if escalations < self.cfg.max_escalations && !tools_json.is_empty() {
                let inputs = self
                    .escalation
                    .escalation_inputs(state.user_intent.as_deref(), Some(&provider), Some(&model))
                    .await
                    .unwrap_or_default();
                // Indice catena 0: catena RELATIVA al corrente (vedi ramo G1);
                // il cap resta su escalations < 3 (qui sopra).
                if let Some(pick) = pick_escalation_model(
                    &inputs.chain,
                    Some(&provider),
                    Some(&model),
                    0,
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
                    self.emit_phase(
                        ctx,
                        mode,
                        "escalation",
                        format!(
                            "Passo a {}/{} (tool call ripetuta identica)",
                            pick.provider, pick.model
                        ),
                        json!({
                            "from_provider": provider,
                            "to_provider": pick.provider,
                            "to_model": pick.model,
                            "reason": "signature_loop",
                            // Stato STRUTTURATO del provider di partenza (ADR 0037
                            // arricchimento B): gia' risolto in `inputs` senza query
                            // extra. La causa dello switch e' il loop di firma, ma il
                            // flag segnala se il provider era anche in cooldown
                            // (billing/quota, gate ADR 0020) — segnale, non testo.
                            "cooldown": inputs.provider_in_cooldown,
                        }),
                    )
                    .await;
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
                        // FIX-A: cattura il tier del promosso prima che il pick esca
                        // di scope; scritto in `current_tier` col delta finale.
                        signature_loop_tier = pick.tier;
                        new_signatures = vec![]; // reset accumulator dopo escalation (py:3265)
                        tried_escalation = true;
                        signature_loop_promoted = true;
                    }
                }
            }
            if tried_escalation {
                escalations += 1; // py:3284
            } else {
                // Catena esaurita / tutti in cooldown / ri-chiamata fallita.
                // ADR 0034: prima della chiusura di sistema, UN turno dichiarativo
                // forzato — l'esito del run diventa la dichiarazione strutturata
                // del modello (outcome/blocker/summary) invece del testo sintetico.
                // La risposta corrente (la tool call ripetuta identica) viene
                // scartata: non va ne' eseguita ne' persistita.
                if let Some(delta) = self.forced_declaration_delta(state, iters_in, ctx, mode).await {
                    return Ok(delta);
                }
                // Chiusura secca loop_detected (py:3269-3281). Messaggio ONESTO:
                // niente suggerimenti hardcoded di modelli (regola G) — il loop a
                // vuoto e' uno stallo del RUN, non un verdetto sul modello.
                self.emit_phase(
                    ctx,
                    mode,
                    "loop_break",
                    format!("Interrompo: '{tool_name}' ripetuto identico senza progresso"),
                    json!({"tool": tool_name, "reason": "signature_loop"}),
                )
                .await;
                let loop_msg = format!(
                    "[LOOP RILEVATO] Il run e' stato interrotto: il tool '{tool_name}' e' stato \
ripetuto con lo stesso input 3+ volte senza alcun progresso in mezzo ({provider}/{model}). \
Riformula la richiesta in modo piu' specifico oppure indica un punto di partenza diverso."
                );
                assistant_msg = Message::Ai {
                    content: MessageContent::text(loop_msg.clone()),
                    tool_calls: vec![],
                    reasoning: None,
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
        // (Solo la coda: la variante progress-aware calcola la stessa coda.)
        let updated_signatures = detect_signature_loop_progress_aware_with(
            &recent,
            &new_signatures,
            |n| EXPLORATION_ONLY_TOOLS.contains(&n),
            self.cfg.loop_thresholds,
        )
        .updated_signatures;

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
        // La chiusura secca del signature-loop e' un forced-close anti-loop: il
        // segnale STRUTTURATO (regola M) deve sopravvivere alla riscrittura di
        // stop_reason operata dal final_gate (senza, il run chiudeva "completed"
        // col messaggio di sistema come risposta — run b833a83d).
        let loop_forced_close = loop_close_result.is_some();
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

        // ── Risposta VUOTA a un turno FORZATO (incidente run b07c7e78) ────────
        // Alla finestra forced-text (tool rimossi per imporre il resoconto) o al
        // turno dichiarativo ADR 0034 il modello puo' chiudere con testo VUOTO
        // (Gemini: outputTokens=0, stopReason=end_turn). Quell'esito NON e'
        // verificato: senza questo ramo il run finiva status='completed' con
        // final_answer NULL e ZERO messaggi assistant in chat. Rimedio a due
        // livelli: (a) se il turno dichiarativo e' ancora disponibile si rientra
        // chiedendo la dichiarazione strutturata (regola M: l'esito dal segnale
        // macchina, non da un testo che non c'e'); (b) altrimenti il delta viene
        // marcato `forced_close_unverified` (stesso segnale autoritativo di
        // 9ece276) -> il finalizzatore mappa FailedDiagnosed e produce il recap
        // deterministico, mai un 'completed' muto.
        let empty_forced_reply = (forced_text_turn || declaring_turn)
            && matches!(stop_reason_enum, StopReason::EndTurn | StopReason::Stop)
            && pending_tool_uses.is_empty()
            && final_result.trim().is_empty();
        if empty_forced_reply {
            if let Some(d) = self.forced_declaration_delta(state, iters_in, ctx, mode).await {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    forced_text = forced_text_turn,
                    "risposta VUOTA al turno forzato -> retry col turno dichiarativo (ADR 0034)"
                );
                return Ok(d);
            }
        }

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
                    // Testo ripulito (rimozione <suggested_actions>): turno di chiusura
                    // testuale, nessun reasoning da ri-passare.
                    reasoning: None,
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

            // (2) unfulfilled-report: in modalita' NON autonoma con esito NON
            // compiuto e turno NON action-oriented, SOSTITUISCE il result con il
            // resoconto onesto deterministico. Gate puro
            // [`should_substitute_unfulfilled_report`]; il segnale e' il report
            // STRUTTURALE di passi pendenti sul testo finale (ADR 0018 fase 3:
            // la detection lessicale della narrazione e' stata rimossa; l'origine
            // Python di questo ramo e' dismessa col brain). Il closure_verdict
            // resta deliberatamente NON consultato qui: potrebbe essere stale
            // del turno precedente.
            let unfulfilled_post = detect_pending_steps_report_with(
                Some(final_result.as_str()),
                self.cfg.pending_steps_detection_enabled,
                self.cfg.pending_steps_min_items,
            );
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
                    // Resoconto onesto sostitutivo: turno testuale, nessun reasoning.
                    reasoning: None,
                };
            }
        }

        // sticky: aggiornato se cascade ha fatto fallback O se il signature-loop
        // ha promosso il modello in questo turno. Prima la promozione del
        // signature-loop era one-shot (eff==provider dopo la riassegnazione ->
        // cascade_did_fallback=false -> sticky invariato): il turno successivo
        // ricadeva sul modello di routing debole, vanificando l'escalation.
        let sticky_promote = cascade_did_fallback || signature_loop_promoted;
        let sticky_provider = if sticky_promote {
            Some(eff_provider.clone())
        } else {
            state.sticky_provider.clone()
        };
        let sticky_model = if sticky_promote {
            Some(eff_model.clone())
        } else {
            state.sticky_model.clone()
        };
        // FIX-A (scale-controller): `current_tier` deve descrivere il modello STICKY
        // EFFETTIVO di questo delta (regola M/H), mai un tier disallineato. Il tier e'
        // noto SENZA lookup solo se il signature-loop ha promosso E nessun cascade
        // interno del gateway ha poi cambiato il modello (eff_model == model del pick):
        // li' vale il tier del pick. Se lo sticky cambia (cascade fallback, con o senza
        // promozione) verso un modello di cui non conosciamo il tier senza I/O (vietato
        // nel path turno/replay, regola H), si AZZERA (`Some(None)`) invece di affermare
        // il tier di un modello diverso da quello sticky: il consumatore (PR-B) ricade
        // sul default. Se lo sticky NON cambia, no-op (`None`): `current_tier` resta
        // quello valido (routing iniziale o turni precedenti).
        let current_tier_delta: Option<Option<String>> = if sticky_promote {
            if signature_loop_promoted && eff_model == model {
                Some(signature_loop_tier.clone())
            } else {
                Some(None)
            }
        } else {
            None
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

        // FIX D4: accumula il thinking di questo turno nel reasoning del run.
        // Self-loop dell'executor: ogni iterazione concatena al valore portato
        // dallo stato. Persistito a fine run nel metadata.reasoning del messaggio
        // assistant (sopravvive al refresh). Se questo turno non ha prodotto
        // thinking, il campo NON viene scritto (`None`) e resta invariato.
        let reasoning_acc_delta: Option<Option<String>> = turn_thinking.as_ref().map(|t| {
            let prev = state.reasoning_acc.as_deref().unwrap_or("");
            let merged = if prev.is_empty() {
                t.clone()
            } else {
                format!("{prev}\n\n{t}")
            };
            Some(merged)
        });

        let mut delta = StateDelta {
            messages: Some(vec![assistant_msg]),
            result: Some(Some(final_result)),
            reasoning_acc: reasoning_acc_delta,
            provider_used: Some(Some(eff_provider)),
            model_used: Some(Some(eff_model)),
            pending_tool_uses: Some(Some(pending_tool_uses)),
            stop_reason: Some(Some(stop_reason_enum)),
            recent_tool_signatures: Some(Some(updated_signatures)),
            consecutive_exploration_calls: Some(Some(expl.consecutive_exploration_calls)),
            exploration_nudge_sent: Some(Some(expl.exploration_nudge_sent)),
            progress_guided_axes: Some(Some(sorted(&progress_guided))),
            progress_diagnosed_axes: Some(Some(sorted(&progress_diagnosed))),
            progress_strategy_axes: Some(Some(sorted(&progress_strategy))),
            repeated_cmd_nudge_sent: Some(Some(repeated_cmd_nudge_sent)),
            iterations: Some(Some(iters_in + 1)),
            action_nudge_count: Some(Some(nudge_count)),
            g1_reroute_count: Some(Some(g1_reroute_count)),
            sticky_provider: Some(sticky_provider),
            sticky_model: Some(sticky_model),
            current_tier: current_tier_delta,
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
        if signature_loop_promoted {
            // Grazia post-escalation: il floor parte dal prefisso persistito
            // corrente, cosi' l'assistant del promosso (appeso da questo delta)
            // e le sue azioni successive contano da zero nel detector
            // repeated_action, mentre lo storico pre-promozione non conta piu'.
            extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
        }
        if declaring_turn {
            // Finestra dichiarativa consumata (ADR 0034): una sola, che il
            // modello abbia dichiarato o no. `outcome_declaration_forced` resta
            // true: la testa dell'executor chiude col summary se la dichiarazione
            // c'e'; i rami di chiusura non ri-forzano (una tantum).
            extra_out.remove("force_outcome_declaration");
        }
        delta.extra = Some(extra_out);
        if loop_forced_close {
            // Segnale autoritativo di chiusura anti-loop (mig 0386): il
            // finalizzatore mappa a FailedDiagnosed anche se il final_gate
            // riscrive stop_reason sul ramo forced_close.
            delta.forced_close_unverified = Some(Some(true));
        }
        if empty_forced_reply {
            // Risposta vuota al turno forzato e turno dichiarativo NON
            // disponibile (gia' consumato o task_complete assente dal catalogo):
            // esito non verificato -> mai 'completed' (incidente run b07c7e78).
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                forced_text = forced_text_turn,
                declaring = declaring_turn,
                "risposta VUOTA al turno forzato senza turno dichiarativo -> forced_close_unverified"
            );
            delta.forced_close_unverified = Some(Some(true));
        }

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

    /// NARRAZIONE LIVE di una FASE del run: delega al punto unico
    /// [`crate::nodes::emit_phase_meta`] (regola L) con le porte del nodo.
    async fn emit_phase(
        &self,
        ctx: &AgentNodeCtx,
        mode: crate::runtime::ports::ExecMode,
        kind: &str,
        title: String,
        payload: Value,
    ) {
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            mode,
            kind,
            title,
            payload,
        )
        .await;
    }

    // ──────────────────────────────────────────────────────────────────────
    //  Meta-reasoner di recovery-da-stallo (blocco #6): gate di EMISSIONE +
    //  CONSUMO al rientro. PUNTO UNICO (regola L): i 3 detector pre-LLM
    //  (repeated_action / resource_reallocation / esplorazione) delegano a
    //  `maybe_stall_reason_delta` invece di re-implementare il gate; il rientro
    //  `StallResolved` delega a `consume_recovery_move`. La costruzione dello
    //  StallContext e la traduzione della mossa vivono nel modulo puro
    //  `decisions::meta_reason` (build_stall_context / translate).
    // ──────────────────────────────────────────────────────────────────────

    /// Mosse del meta-reasoner gia' consumate nel RUN corrente (gamba PER-RUN del
    /// budget, mig 0510). Il contatore vive in `extra["stall_moves_used"]`,
    /// checkpointato con lo stato -> REPLAY-SAFE e coerente tra i supersteps
    /// `StallReason -> StallRecovery -> StallResolved -> executor` dello STESSO run.
    /// `extra` si azzera tra run DIVERSI: la gamba CROSS-RUN e' fornita dalla porta
    /// [`StallBudgetPort`] (persistita per sessione). Il gate di emissione
    /// (`maybe_stall_reason_delta`) SOMMA le due gambe e confronta col cap
    /// `stall_recovery_max_moves_per_session` (regola G), cosi' il budget e'
    /// effettivo per-SESSIONE, chiudendo anche il loop email cross-run.
    /// Complementare al cap strutturale `already_asked_user`.
    fn stall_moves_used(state: &AgentState) -> i64 {
        state
            .extra
            .get("stall_moves_used")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    /// `true` se una `AskUser` e' gia' stata emessa (cap anti ri-domanda per asse):
    /// per l'asse `RepeatedUserQuestion` deriva dal segnale cross-run
    /// `repeated_clarify_count > 0` (regola M: la ri-domanda ripetuta e' gia' un
    /// segnale strutturato dai meta_step clarify); per gli altri assi da un flag
    /// per-run in extra impostato al consumo di una `AskUser`. Passato true fa
    /// scegliere al reasoner `DeclareBlocked` invece di ri-chiedere (chiude il loop).
    fn already_asked_user(state: &AgentState, axis: Axis) -> bool {
        if matches!(axis, Axis::RepeatedUserQuestion)
            && state.repeated_clarify_count.unwrap_or(0) > 0
        {
            return true;
        }
        state
            .extra
            .get("stall_asked_user_axes")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|v| v.as_str() == Some(axis.as_str())))
            .unwrap_or(false)
    }

    /// Esito STRUTTURATO dell'ultimo tool nella coda messaggi come stringa stabile
    /// per lo StallContext (regola M: dal segnale `tool_result_outcome_after` /
    /// exit_code / is_error, MAI dal parsing della prosa). `Some("error")` /
    /// `Some("ok")` / `None` (nessun tool_result nella coda). Il segnale
    /// `redaction_rejected` dello StallContext e' cablato a parte in
    /// `maybe_stall_reason_delta` dal codice strutturato [REDACTION_REJECTED]
    /// (`recent_redaction_rejected`, regola M): la fonte lo codifica nel
    /// tool_result, qui lo si legge come segnale, mai dal testo del placeholder.
    fn last_tool_outcome(messages: &[Message]) -> Option<&'static str> {
        // Ultimo indice con un tool_use nell'AI: cerchiamo l'esito che lo segue.
        // Punto unico della lettura esito: `tool_result_outcome_after` (regola L).
        for idx in (0..messages.len()).rev() {
            if !crate::routing::signals::message_tool_uses(&messages[idx]).is_empty() {
                return match crate::routing::signals::tool_result_outcome_after(messages, idx, 3) {
                    Some(true) => Some("error"),
                    Some(false) => Some("ok"),
                    None => None,
                };
            }
        }
        None
    }

    /// Budget consultazioni CROSS-RUN del meta-reasoner per la SESSIONE, letto dalla
    /// porta [`StallBudgetPort`] (fail-open: guasto -> 0, non blocca). `0` anche se la
    /// porta non e' iniettata (solo cap per-run, comportamento storico). SOLA
    /// LETTURA: nessun side-effect.
    async fn stall_moves_cross_run(&self, ctx: &AgentNodeCtx) -> i64 {
        let Some(budget) = &self.stall_budget else {
            return 0;
        };
        budget
            .consultations_in_session(ctx.session_id)
            .await
            .unwrap_or(0)
    }

    /// Gate di EMISSIONE dello `StallReason` (blocco #6, punto 2). Chiamato dai 3
    /// detector pre-LLM DOPO aver costruito i `signals` e PRIMA di applicare la
    /// gerarchia fissa `pc::decide`. Ritorna `Some(delta)` — che instrada al nodo
    /// `StallRecovery` — SOLO se TUTTE le condizioni valgono:
    ///   1. `stall_recovery_enabled` (flag OFF di default -> `None` -> bit-identico);
    ///   2. budget per-SESSIONE non esaurito: il cap
    ///      `stall_recovery_max_moves_per_session` (regola G) e' confrontato con la
    ///      somma del per-run (`extra["stall_moves_used"]`, si azzera tra run) E del
    ///      CROSS-RUN letto da [`StallBudgetPort`] (persistito per sessione, chiude il
    ///      loop email cross-run). Fail-open: porta assente/guasta -> solo cap per-run.
    ///   3. l'asse richiede META-ragionamento, cioe' la gerarchia fissa starebbe
    ///      per fare una mossa COSTOSA (ForceDiagnose/ChangeStrategy/Escalate/Abort).
    ///      Il livello-1 GUIDE cheap (e Proceed) resta gestito dalla gerarchia
    ///      fissa: non si spreca una LLM-call per un nudge assertivo.
    /// Costruisce lo `StallContext` (modulo puro `build_stall_context`, segnali gia'
    /// risolti) e lo serializza in `extra[STALL_CONTEXT_KEY]` col clone-whole-map
    /// (`put_extra`, regola L: `extra` e' OVERWRITE totale). NON chiama l'LLM (lo fa
    /// il nodo dedicato, replay-safe).
    async fn maybe_stall_reason_delta(
        &self,
        state: &AgentState,
        axis: Axis,
        signals: &ProgressSignals,
        iters_in: i64,
        messages: &[Message],
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        // (1) Flag OFF -> il gate non scatta MAI (comportamento bit-identico).
        if !self.cfg.stall_recovery_enabled {
            return None;
        }
        // (2) Budget per-SESSIONE esaurito -> rete di sicurezza (gerarchia fissa).
        // Somma il per-run (extra, si azzera tra run) al CROSS-RUN (porta, persistito
        // per sessione): il cap e' cosi' effettivo per-sessione, non solo per-run.
        let moves_used_session = self.stall_moves_cross_run(ctx).await;
        let moves_total = Self::stall_moves_used(state) + moves_used_session;
        if moves_total >= self.cfg.stall_recovery_max_moves_per_session {
            return None;
        }
        // (3) Solo se la gerarchia fissa farebbe una mossa COSTOSA (non GUIDE/Proceed):
        // il meta-ragionamento subentra DOPO il livello-1 GUIDE cheap.
        let fixed = pc::decide(signals);
        let needs_meta = !matches!(fixed.action, Action::Guide | Action::Proceed);
        if !needs_meta {
            return None;
        }
        // work_epoch STABILE (chiave idempotenza/replay): avanza solo sui cambi
        // macroscopici. `todo_seq` ~ iterazioni del run; escalation e floor da extra.
        let escalations = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
        let floor = state.extra.get("repeat_scan_floor").and_then(Value::as_i64).unwrap_or(0);
        let epoch = crate::decisions::meta_reason::work_epoch(iters_in, escalations, floor);
        // (4) ANTI META-LOOP (idempotenza per epoca): se per questo (axis, epoch) la
        // mossa e' GIA' stata decisa+consumata (chiave-cache presente in extra) o e'
        // gia' stata risolta a Fallback (marcatore), NON ri-emettere. Senza questa
        // guardia, dopo un consumo che ri-fa il turno (nudge) o dopo un Fallback, il
        // detector ri-scatterebbe con lo STESSO epoch e ri-consulterebbe l'LLM in
        // loop. La chiave-cache e' il punto unico `stall_move_key` (regola L).
        if state.extra.contains_key(&stall_move_key(axis.as_str(), epoch))
            || state
                .extra
                .get("stall_fallback_epochs")
                .and_then(Value::as_array)
                .map(|a| a.iter().any(|v| v.as_i64() == Some(epoch)))
                .unwrap_or(false)
        {
            return None;
        }
        // Segnali cross-cutting per lo StallContext (regola M: tutti strutturati).
        let recent_tool_signatures = state.recent_tool_signatures.clone().unwrap_or_default();
        let last_outcome = Self::last_tool_outcome(messages);
        let modified = crate::routing::signals::modified_files_from_messages(messages, 40);
        let already_asked = Self::already_asked_user(state, axis);
        // redaction_rejected dal SEGNALE STRUTTURATO (regola M): la fonte
        // (`mcp-core::security::redaction_guard`) antepone al tool_result il codice
        // macchina [REDACTION_REJECTED] quando rifiuta un input contenente un
        // placeholder di redazione; il punto unico `recent_redaction_rejected`
        // (regola L) lo LEGGE, MAI un `contains("[REDACTED:")` sul placeholder
        // umano. E' il segnale che riconosce il blocco ambientale del loop email
        // (email ri-oscurata che il modello continua a copiare).
        let redaction_rejected =
            crate::routing::signals::recent_redaction_rejected(messages, 16);
        let stall = build_stall_context(
            axis,
            signals,
            &recent_tool_signatures,
            last_outcome,
            redaction_rejected,
            state.repeated_clarify_count.unwrap_or(0),
            state.user_intent.as_deref(),
            &modified,
            already_asked,
            epoch,
        );
        let value = serde_json::to_value(&stall).ok()?;
        let extra = put_extra(state, STALL_CONTEXT_KEY, value);
        tracing::info!(
            target: "nexus_agent_graph::executor",
            axis = axis.as_str(),
            work_epoch = epoch,
            moves_used_run = Self::stall_moves_used(state),
            moves_used_session,
            "meta-reasoner: emetto StallReason -> nodo StallRecovery"
        );
        Some(
            StateDelta {
                extra: Some(extra),
                stop_reason: Some(Some(StopReason::StallReason)),
                iterations: Some(Some(iters_in + 1)),
                ..Default::default()
            }
            .into_opaque(),
        )
    }

    /// CONSUMO della `RecoveryMove` al rientro dal nodo `StallRecovery`
    /// (`StopReason::StallResolved`, blocco #6 punto 3). Legge lo `StallContext`
    /// e la mossa persistita in `extra[stall_move_key(axis, work_epoch)]`, la
    /// traduce col punto unico `translate` e la applica riusando gli STESSI
    /// meccanismi di `pc::decide`:
    ///   - `Guide`/`ChangeStrategy`/`ForceDiagnose` -> nudge (human_msg) + eventuale
    ///     force-action, poi RI-DA il turno (`G1Escalated` self-loop, come i rami
    ///     nudge della gerarchia fissa);
    ///   - `Escalate` -> ramo escalation esistente (`pick_escalation_model` +
    ///     sticky), promuovendo il modello;
    ///   - `AskUser`/`DeclareBlocked` -> chiusura DIRETTA con esito strutturato
    ///     `needs_input`/`blocked` (ADR 0034, `normalize_declared_outcome` punto
    ///     unico), il consumo REALE delle 2 nuove azioni;
    ///   - `Fallback` / assenza mossa -> marca l'epoca come fallback-risolta e RI-DA
    ///     il turno (re-entry pulito): al re-entry il gate NON ri-emette (guardia
    ///     anti meta-loop) e la gerarchia fissa procede (rete di sicurezza). Ritorna
    ///     `None` solo se manca del tutto lo StallContext (guasto a monte -> prosegue).
    /// Incrementa il budget su consultazione EFFETTIVA (una mossa applicata),
    /// preservando l'intero `extra` (clone-whole-map): il PER-RUN in
    /// `extra["stall_moves_used"]` E il CROSS-RUN via [`StallBudgetPort`]
    /// (`record_consultation`, gata Real, best-effort).
    async fn consume_recovery_move(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
        mode: crate::runtime::ports::ExecMode,
    ) -> Option<OpaqueDelta> {
        // Lo StallContext dice quale (axis, work_epoch) leggere: la chiave-cache e'
        // il punto unico `stall_move_key` (stessa formula del nodo produttore).
        // Assente -> guasto a monte: prosegui la gerarchia fissa senza marcatura.
        let stall: crate::runtime::ports::StallContext = state
            .extra
            .get(STALL_CONTEXT_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())?;
        let key = stall_move_key(&stall.axis, stall.work_epoch);
        // Mossa assente (nodo degradato a resolved-only: reasoner Ok(None)/errore) o
        // Fallback (mossa non traducibile): marca l'epoca come risolta a fallback per
        // rompere il meta-loop, poi RI-DA il turno pulito -> al re-entry il gate non
        // ri-emette e la gerarchia fissa decide (rete di sicurezza). NON incrementa
        // il budget (nessuna mossa applicata).
        let translated = state
            .extra
            .get(&key)
            .and_then(|v| serde_json::from_value::<RecoveryMove>(v.clone()).ok())
            .and_then(|mv| translate(&mv));
        let Some(dec) = translated else {
            let mut extra_out = state.extra.clone();
            Self::mark_fallback_epoch(&mut extra_out, stall.work_epoch);
            tracing::debug!(
                target: "nexus_agent_graph::executor",
                axis = %stall.axis,
                work_epoch = stall.work_epoch,
                "meta-reasoner: nessuna mossa valida (fallback), prosegue gerarchia fissa"
            );
            return Some(
                StateDelta {
                    recent_tool_signatures: Some(Some(vec![])),
                    pending_tool_uses: Some(Some(vec![])),
                    stop_reason: Some(Some(StopReason::G1Escalated)),
                    iterations: Some(Some(iters_in + 1)),
                    extra: Some(extra_out),
                    ..Default::default()
                }
                .into_opaque(),
            );
        };

        // Budget consumato su consultazione EFFETTIVA (mossa applicata). Due gambe:
        //  - PER-RUN: `extra["stall_moves_used"]` (checkpointato, si azzera tra run);
        //  - CROSS-RUN: la porta [`StallBudgetPort`] persiste la consultazione per
        //    SESSIONE (append, gata Real via `mode`), cosi' il cap e' effettivo
        //    per-sessione anche sul loop email cross-run. Best-effort (fail-open):
        //    un guasto di persistenza non deve rompere il turno.
        let moves_used = Self::stall_moves_used(state) + 1;
        let mut extra_out = state.extra.clone();
        extra_out.insert("stall_moves_used".to_string(), json!(moves_used));
        if let Some(budget) = &self.stall_budget {
            if let Err(err) = budget.record_consultation(ctx.session_id, mode).await {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    error = %err,
                    "meta-reasoner: registrazione budget cross-run fallita (best-effort)"
                );
            }
        }

        match dec.action {
            // Nudge-based: inietta il nudge del reasoner + eventuale force-action e
            // RI-DA il turno (self-loop G1Escalated, come i rami nudge fissi).
            Action::Guide | Action::ChangeStrategy | Action::ForceDiagnose => {
                let mut messages_out: Vec<Message> = vec![];
                if let Some(t) = &dec.nudge_text {
                    messages_out.push(human_msg(t));
                }
                // Il nudge del reasoner riparte con finestra pulita sui detector di
                // ripetizione (stessa grazia dei rami Escalate: il modello promosso/
                // ri-orientato non deve ereditare le firme che hanno causato lo stallo).
                extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                self.emit_phase(
                    ctx,
                    mode,
                    "stall_recovery",
                    format!("Recovery: {}", dec.reason),
                    json!({"axis": stall.axis, "action": action_str(dec.action)}),
                )
                .await;
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    axis = %stall.axis,
                    action = action_str(dec.action),
                    "meta-reasoner: applico mossa nudge -> ri-do il turno"
                );
                Some(
                    StateDelta {
                        messages: Some(messages_out),
                        recent_tool_signatures: Some(Some(vec![])),
                        g1_reroute_count: Some(Some(0)),
                        action_nudge_count: Some(Some(0)),
                        pending_tool_uses: Some(Some(vec![])),
                        stop_reason: Some(Some(StopReason::G1Escalated)),
                        iterations: Some(Some(iters_in + 1)),
                        extra: Some(extra_out),
                        ..Default::default()
                    }
                    .into_opaque(),
                )
            }
            // Escalate: promuovi il modello riusando il ramo escalation esistente
            // (pick_escalation_model + sticky). Se nessun candidato -> rete di
            // sicurezza (gerarchia fissa, che a budget escalation esaurito abortisce).
            Action::Escalate => {
                let escal = state.extra.get("auto_escalations").and_then(Value::as_i64).unwrap_or(0);
                let (cur_provider, cur_model) = self.escalation_current_pair(state);
                // `cooldown_flag` sollevato fuori dal blocco cosi' e' disponibile al
                // payload dell'escalation (ADR 0037 arricchimento B): nessuna query
                // extra, e' lo stesso `inputs` gia' letto per la selezione.
                let mut cooldown_flag = false;
                let picked = if escal < self.cfg.max_escalations {
                    let inputs = self
                        .escalation
                        .escalation_inputs(
                            state.user_intent.as_deref(),
                            cur_provider.as_deref(),
                            cur_model.as_deref(),
                        )
                        .await
                        .unwrap_or_default();
                    cooldown_flag = inputs.provider_in_cooldown;
                    pick_escalation_model(
                        &inputs.chain,
                        cur_provider.as_deref(),
                        cur_model.as_deref(),
                        0,
                        inputs.provider_in_cooldown,
                        inputs.cross_provider.as_ref(),
                    )
                } else {
                    None
                };
                let pick = picked?;
                self.emit_phase(
                    ctx,
                    mode,
                    "escalation",
                    format!("Passo a {}/{} (meta-reasoner)", pick.provider, pick.model),
                    json!({
                        "to_provider": pick.provider,
                        "to_model": pick.model,
                        "reason": "stall_recovery",
                        // Stato STRUTTURATO del provider di partenza (segnale, non
                        // testo): la causa dello switch e' lo stallo, il flag dice se
                        // il provider era anche in cooldown billing/quota (ADR 0020).
                        "cooldown": cooldown_flag,
                    }),
                )
                .await;
                let esc_nudge = human_msg(
                    "Il modello precedente si e' bloccato senza progresso. Ora rispondi tu, \
che sei un modello piu' capace: cambia approccio ed ESEGUI il prossimo step concreto \
con un tool call.",
                );
                extra_out.insert("auto_escalations".to_string(), json!(escal + 1));
                extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    to_provider = %pick.provider,
                    to_model = %pick.model,
                    "meta-reasoner: ESCALATE -> promuovo modello"
                );
                Some(
                    StateDelta {
                        messages: Some(vec![esc_nudge]),
                        sticky_provider: Some(Some(pick.provider)),
                        sticky_model: Some(Some(pick.model)),
                        // FIX-A (scale-controller): tier del modello promosso dal pick.
                        current_tier: Some(pick.tier),
                        recent_tool_signatures: Some(Some(vec![])),
                        g1_reroute_count: Some(Some(0)),
                        action_nudge_count: Some(Some(0)),
                        pending_tool_uses: Some(Some(vec![])),
                        stop_reason: Some(Some(StopReason::G1Escalated)),
                        iterations: Some(Some(iters_in + 1)),
                        extra: Some(extra_out),
                        ..Default::default()
                    }
                    .into_opaque(),
                )
            }
            // AskUser / DeclareBlocked: consumo REALE delle 2 nuove azioni. Chiudono
            // il run con esito STRUTTURATO (ADR 0034), NON con un turno LLM libero:
            // e' cio' che spezza il loop email (l'agente non ri-chiede all'infinito,
            // dichiara needs_input/blocked una volta). L'outcome e' costruito col
            // punto unico `normalize_declared_outcome` (regola L) e letto a valle da
            // `route_after_executor` (gate 7: needs_input/blocked -> chiusura).
            Action::AskUser => {
                let question = dec.nudge_text.clone().unwrap_or_default();
                // Marca l'asse come "gia' chiesto" (cap anti ri-domanda per-run).
                Self::mark_asked_user(&mut extra_out, &stall.axis);
                let outcome = crate::decisions::tool_dispatch::normalize_declared_outcome(&json!({
                    "outcome": "needs_input",
                    "summary": question,
                    "next_step": question,
                }))
                .unwrap_or_else(|| json!({"outcome": "needs_input", "summary": question}));
                self.emit_phase(
                    ctx,
                    mode,
                    "outcome_declared",
                    "Esito dichiarato: needs_input (meta-reasoner)".to_string(),
                    json!({"outcome": "needs_input", "axis": stall.axis}),
                )
                .await;
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    axis = %stall.axis,
                    "meta-reasoner: AskUser -> chiusura strutturata needs_input"
                );
                Some(
                    StateDelta {
                        messages: Some(vec![Message::Ai {
                            content: MessageContent::text(question.clone()),
                            tool_calls: vec![],
                            reasoning: None,
                        }]),
                        result: Some(Some(question)),
                        declared_outcome: Some(Some(outcome)),
                        pending_tool_uses: Some(Some(vec![])),
                        stop_reason: Some(Some(StopReason::EndTurn)),
                        iterations: Some(Some(iters_in + 1)),
                        extra: Some(extra_out),
                        ..Default::default()
                    }
                    .into_opaque(),
                )
            }
            Action::DeclareBlocked => {
                // `nudge_text` porta il `blocker` (dal `translate`): validato ADR 0034.
                let blocker = dec.nudge_text.clone().unwrap_or_default();
                let summary = format!(
                    "Non posso proseguire: blocco dichiarato ({blocker}). La causa e' esterna \
al mio controllo e va risolta prima di continuare."
                );
                let outcome = crate::decisions::tool_dispatch::normalize_declared_outcome(&json!({
                    "outcome": "blocked",
                    "summary": summary,
                    "blocker": blocker,
                }))
                .unwrap_or_else(|| json!({"outcome": "blocked", "summary": summary}));
                self.emit_phase(
                    ctx,
                    mode,
                    "outcome_declared",
                    format!("Esito dichiarato: blocked ({blocker})"),
                    json!({"outcome": "blocked", "blocker": blocker, "axis": stall.axis}),
                )
                .await;
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    axis = %stall.axis,
                    blocker = %blocker,
                    "meta-reasoner: DeclareBlocked -> chiusura strutturata blocked"
                );
                let close_summary = outcome
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("Blocco dichiarato.")
                    .to_string();
                Some(
                    StateDelta {
                        messages: Some(vec![Message::Ai {
                            content: MessageContent::text(close_summary.clone()),
                            tool_calls: vec![],
                            reasoning: None,
                        }]),
                        result: Some(Some(close_summary)),
                        declared_outcome: Some(Some(outcome)),
                        pending_tool_uses: Some(Some(vec![])),
                        stop_reason: Some(Some(StopReason::EndTurn)),
                        iterations: Some(Some(iters_in + 1)),
                        extra: Some(extra_out),
                        ..Default::default()
                    }
                    .into_opaque(),
                )
            }
            // `translate` non produce mai Proceed/Abort (Fallback -> None gia' sopra):
            // arm esplicito per esaustivita' (regola L, niente `_`).
            Action::Proceed | Action::Abort => None,
        }
    }

    /// Registra `axis` fra gli assi per cui una `AskUser` e' gia' stata emessa
    /// (cap anti ri-domanda per-run). Muta la mappa extra gia' clonata dal chiamante.
    fn mark_asked_user(extra_out: &mut serde_json::Map<String, Value>, axis: &str) {
        let mut axes: Vec<Value> = extra_out
            .get("stall_asked_user_axes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !axes.iter().any(|v| v.as_str() == Some(axis)) {
            axes.push(Value::String(axis.to_string()));
        }
        extra_out.insert("stall_asked_user_axes".to_string(), Value::Array(axes));
    }

    /// Registra `epoch` fra le epoche gia' risolte a Fallback dal reasoner (guardia
    /// anti meta-loop): il gate di emissione salta un epoch marcato, cosi' dopo un
    /// Fallback il detector non ri-consulta l'LLM per lo stesso stallo. Muta la mappa
    /// extra gia' clonata dal chiamante (clone-whole-map preservato).
    fn mark_fallback_epoch(extra_out: &mut serde_json::Map<String, Value>, epoch: i64) {
        let mut epochs: Vec<Value> = extra_out
            .get("stall_fallback_epochs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !epochs.iter().any(|v| v.as_i64() == Some(epoch)) {
            epochs.push(json!(epoch));
        }
        extra_out.insert("stall_fallback_epochs".to_string(), Value::Array(epochs));
    }

    /// Turno DICHIARATIVO forzato (ADR 0034): prima di una chiusura di sistema
    /// (abort anti-loop / cap G1 a catalogo esaurito) il modello riceve UN turno
    /// col catalogo ridotto a solo `task_complete` e tool choice forzata, cosi'
    /// l'esito del run e' la SUA dichiarazione strutturata (outcome/blocker/
    /// summary, regola M) invece di un testo sintetico di sistema.
    ///
    /// `None` (il chiamante procede con la chiusura storica) se:
    /// - il turno dichiarativo e' GIA' stato concesso in questo run
    ///   (`outcome_declaration_forced`, una tantum: se il modello non dichiara
    ///   nemmeno sotto forcing, la chiusura successiva e' quella secca);
    /// - il catalogo del run NON contiene `task_complete` (senza definizione il
    ///   modello non puo' chiamarlo: niente turno a vuoto).
    async fn forced_declaration_delta(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
        mode: crate::runtime::ports::ExecMode,
    ) -> Option<OpaqueDelta> {
        let already = state
            .extra
            .get("outcome_declaration_forced")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if already {
            return None;
        }
        let has_tool = state
            .tools_json
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some(TASK_COMPLETE_TOOL_NAME));
        if !has_tool {
            return None;
        }
        // Narrazione live: la chat spiega perche' il prossimo turno e' "strano"
        // (catalogo ridotto a task_complete).
        self.emit_phase(
            ctx,
            mode,
            "declaration_request",
            "Chiedo al modello di dichiarare l'esito del lavoro".to_string(),
            json!({}),
        )
        .await;
        let nudge = human_msg(
            "Il turno sta per chiudere senza un esito dichiarato. Chiama ORA il tool \
task_complete dichiarando l'esito REALE del lavoro: outcome=done SOLO se il lavoro e' \
completo e verificato; blocked (con blocker valorizzato) se una causa esterna ti ferma; \
partial se hai completato solo una parte (indica in next_step cosa resta); needs_input se \
serve una decisione dell'utente. Nel summary scrivi il resoconto finale per l'utente.",
        );
        let mut extra_out = state.extra.clone();
        extra_out.insert("force_outcome_declaration".to_string(), json!(true));
        extra_out.insert("outcome_declaration_forced".to_string(), json!(true));
        // Grazia sui detector di stallo: il turno dichiarativo non deve essere
        // corto-circuitato dalle azioni pre-chiusura (stesso floor del fix
        // escalation, punto unico repeat_scan_floor). I gate di chiusura pre-LLM
        // hanno inoltre il guard `!declaration_pending` (testa del run).
        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
        tracing::warn!(
            target: "nexus_agent_graph::executor",
            "chiusura di sistema -> turno dichiarativo forzato (ADR 0034): il modello dichiara l'esito"
        );
        // NB: g1_reroute_count/action_nudge_count NON vengono azzerati (a
        // differenza dei rami Escalate): il turno dichiarativo non e' un nuovo
        // budget di lavoro — se il modello non dichiara, il gate 7 del routing
        // trova i contatori al cap e si chiude secco senza un nuovo ciclo di
        // nudge G1.
        Some(
            StateDelta {
                messages: Some(vec![nudge]),
                recent_tool_signatures: Some(Some(vec![])),
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::G1Escalated)),
                iterations: Some(Some(iters_in + 1)),
                extra: Some(extra_out),
                ..Default::default()
            }
            .into_opaque(),
        )
    }

    /// Coppia (provider, model) CORRENTE ai fini dell'escalation (punto unico,
    /// regola L). Priorita' A COPPIE COERENTI (mai provider di una fonte +
    /// model di un'altra): sticky > (provider_used, model_used) > override >
    /// routing.
    ///
    /// - `sticky` per primo: una promozione appena scritta NON ha ancora
    ///   chiamato, quindi `model_used` e' stantio; il vecchio ordine
    ///   `model_used`-first faceva credere al selettore di essere ancora sul
    ///   modello debole e ri-proponeva lo stesso candidato, bruciando il
    ///   budget escalation (incidente run c4fa064b: 2 ESCALATE identici
    ///   back-to-back -> ABORT).
    /// - `model_used` prima di override/routing: riflette l'ULTIMA chiamata
    ///   REALE, upscale e cascade inclusi — il filtro finestra-aware della
    ///   catena deve ancorarsi alla finestra del modello davvero in uso, non
    ///   a quella del modello di routing pre-upscale.
    fn escalation_current_pair(&self, state: &AgentState) -> (Option<String>, Option<String>) {
        let pair = |p: &Option<String>, m: &Option<String>| -> Option<(String, String)> {
            match (p.as_deref(), m.as_deref()) {
                (Some(p), Some(m)) if !p.is_empty() && !m.is_empty() => {
                    Some((p.to_string(), m.to_string()))
                }
                _ => None,
            }
        };
        let picked = pair(&state.sticky_provider, &state.sticky_model)
            .or_else(|| pair(&state.provider_used, &state.model_used))
            .or_else(|| pair(&state.provider_override, &state.model_override))
            .or_else(|| {
                pair(
                    &Some(self.cfg.routing_provider.clone()),
                    &Some(self.cfg.routing_model.clone()),
                )
            });
        match picked {
            Some((p, m)) => (Some(p), Some(m)),
            None => (None, None),
        }
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
                reasoning: None,
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

/// Stringa stabile dell'`Action` del progress_controller (== serde rename).
/// Punto unico (regola L): usato dal consumo meta-reasoner per i log/payload e
/// dai test (`nudge_order`), non duplicato.
pub(crate) fn action_str(a: Action) -> &'static str {
    match a {
        Action::Proceed => "proceed",
        Action::Guide => "guide",
        Action::ForceDiagnose => "force_diagnose",
        Action::ChangeStrategy => "change_strategy",
        Action::Escalate => "escalate",
        Action::Abort => "abort",
        Action::AskUser => "ask_user",
        Action::DeclareBlocked => "declare_blocked",
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
            // Reasoning del turno (DeepSeek thinking mode): preservato per il
            // round-trip al gateway (vincolo HTTP 400). Vuoto -> None.
            reasoning: resp.reasoning.as_ref().filter(|r| !r.is_empty()).cloned(),
        };
    }
    // Forma minimale: testo + tool_calls (OpenAI-compat).
    Message::Ai {
        content: MessageContent::text(result_text),
        tool_calls: resp.tool_calls.clone(),
        reasoning: resp.reasoning.as_ref().filter(|r| !r.is_empty()).cloned(),
    }
}

/// Mappa un [`Message`] del canale interno in [`HistoryMessage`] (forma su cui
/// operano le primitive di context_reduction): `is_human` dal ruolo, `content`
/// testo o blocchi, `anthropic_content` i blocchi se presenti.
fn message_to_history(m: &Message) -> HistoryMessage {
    match m {
        Message::Human { content } => history_from_content(content, true),
        Message::Ai { content, tool_calls, reasoning } => {
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
            // Reasoning DeepSeek del turno: preservato per il round-trip al gateway.
            hm.reasoning = reasoning.clone();
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
                        // Round-trip reasoning DeepSeek del turno assistant.
                        reasoning: m.reasoning.clone(),
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
                // Round-trip reasoning DeepSeek: solo sugli assistant (mai sugli user).
                reasoning: if m.is_human { None } else { m.reasoning.clone() },
                ..Default::default()
            }];
        }
    }

    // 3) Forma minimale role/content (turno puramente testuale).
    let role = if m.is_human { "user" } else { "assistant" };
    vec![LlmMessage {
        role: role.to_string(),
        content: m.content.clone(),
        // Round-trip reasoning DeepSeek: solo sugli assistant (mai sugli user).
        reasoning: if m.is_human { None } else { m.reasoning.clone() },
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
