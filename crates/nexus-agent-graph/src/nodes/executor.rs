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
//!  2. `declared_outcome=done` & `declared_done_count>=3` -> end_turn (`:1683`),
//!     SALVO rientro da final_gate in correzione (`final_gate_cycle>0`: il gate
//!     ha bocciato e chiede fix; la chiusura d'autorita' neutralizzerebbe il ciclo).
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
//! MetaStepStore); delta con iterations+1, pending, stop_reason, messages,
//! provider_used/model_used, recent_tool_signatures, auto_escalations, ecc.
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
use crate::decisions::escalation::{cap_candidates_one_step, pick_escalation_model};
use crate::decisions::switch_reason::SwitchReason;
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
use crate::decisions::orchestration_reason::context_pressure_from_tokens;
use crate::decisions::progress_controller::{self as pc, Action, Axis, ProgressSignals};
use crate::decisions::scale_reason::{
    build_scale_context, effective_ctx_mgmt, effective_g1_threshold, effective_rolling,
    effective_token_brake, resolve_sizing_overrides, scale_cache_key, scale_trigger,
    ScaleHysteresisConfig, ScaleSizingConfig, ScaleTriggerConfig, SizingBaseline,
};
use crate::decisions::text_repetition::{detect_repetition_collapse, RepetitionThresholds};
use crate::decisions::tool_dispatch::{
    current_context_token_estimate, estimate_context_chars, flatten_context_text, ContextMessage,
};
use crate::decisions::turn_focus::build_turn_focus_directive;
use crate::nodes::scale_control::{
    SCALE_CONTEXT_KEY, SCALE_HYSTERESIS_CFG_KEY, SCALE_MOVE_CACHE_KEY_KEY, SCALE_SIZING_CFG_KEY,
    SCALE_SIZING_OVERRIDES_KEY,
};
use crate::nodes::final_gate::FINAL_GATE_ESCALATION_KEY;
use crate::nodes::stall_recovery::{stall_move_key, STALL_CONTEXT_KEY};
use crate::routing::signals::{
    count_recent_request_port, detect_pending_steps_report_with, detect_recent_tool_error,
    detect_repeated_action_detailed, detect_repeated_failed_command,
    has_active_resources_in_history, has_recent_productive_action, has_tool_calls_in_history,
    modified_files_from_messages, tool_error_stats, EXPLORATION_ONLY_TOOLS,
};
use crate::runtime::ports::RecoveryMove;
use crate::runtime::ports::{
    AgentStepStore, BillingCooldownPort, ContextOffload, EmbeddingStore, EscalationPort,
    LlmMessage, LlmRequest, LlmResponse, MetaStepStore, ModelUpscalePort, NextActionsDeriver,
    OffloadKind, PortError, RunControlStore, ScaleMove, ScaleTier, SizingOverrides, SseEvent,
    StallBudgetPort, SummaryStore, TokenCounter,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{
    put_extra, AgentState, ContentBlock, Message, MessageContent, MetaStep, StateDelta, StopReason,
    ToolUse,
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
    /// Vocabolario DB-driven (regola G) dei `code` di `client_error` (4xx)
    /// PROVIDER-SPECIFICI recuperabili su un ALTRO provider: un 400 con uno di
    /// questi code (es. Google `invalid_argument`/`thought_signature`) fa failover
    /// cross-provider invece di chiudere il run; ogni altro 4xx di formato/history
    /// CONDIVISA (es. Mistral `invalid_request_message_order`) resta chiusura onesta
    /// (bug 2, incidente f0ad0337). Vuoto = nessun ClientError recuperabile
    /// (conservativo: senza segnale non si fa failover cieco).
    pub recoverable_client_error_codes: Vec<String>,
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
    /// Budget TOKEN cumulativo del run (`agent.run_token_budget`, default 400000):
    /// safety net anti-runaway COMPLEMENTARE a `iteration_cap` (che conta iterazioni).
    /// Somma dei token (input+output, dal segnale strutturato `LlmUsage.total_tokens`,
    /// regola M) di TUTTE le risposte LLM del run. Al raggiungimento (`>=`) il run
    /// chiude deterministicamente (forced_close_unverified). `0` = disabilitato ->
    /// comportamento bit-identico a oggi (retro-compatibile). Chiude il buco: un
    /// modello che ignora `force_tool_choice` produce turni solo-testo che non
    /// triggerano il signature-loop (contano ITERAZIONI, non TOKEN) e bruciano
    /// ~16k token/turno (osservato un run a 1.8M token senza convergere). Regola G:
    /// DB-driven, mai hardcoded.
    pub run_token_budget: u64,
    /// BACKSTOP di ultima istanza (catastrofe) sul budget token cumulativo del run
    /// (`agent.run_token_hard_cap`, default ~2x `run_token_budget`). Quando il
    /// meta-reasoner e' ACCESO (`stall_recovery_enabled=true`) il `run_token_budget`
    /// NON chiude piu' direttamente: diventa la soglia-TRIGGER che consulta il
    /// giudice agentico (`StallReason` -> nodo `StallRecovery` -> `RecoveryMove`).
    /// Il giudice puo' decidere di PROSEGUIRE (es. e' vicino a chiudere): senza un
    /// tetto duro un modello patologico brucerebbe token all'infinito. Questo
    /// hard-cap e' la rete di sicurezza NON-negoziabile: al raggiungimento (`>=`)
    /// l'executor chiude deterministicamente (`close_runaway`) SENZA consultare il
    /// giudice, come il vecchio comportamento di 822e083. `0` = disabilitato. Con
    /// `stall_recovery_enabled=false` questo hard-cap e' irrilevante: il
    /// `run_token_budget` chiude gia' prima (retro-compat 822e083). Regola G:
    /// DB-driven, mai hardcoded.
    pub run_token_hard_cap: u64,
    /// Tetto di spesa in DOLLARI dell'INTERO run (`agent.run_cost_budget_usd`, default
    /// 0 = disabilitato). Freno complementare al budget token: quando il costo
    /// cumulativo REALE del run (`AgentState::run_cost_cumulative_usd`, somma dei costi
    /// per-turno ciascuno col prezzo del proprio modello -> esatto anche dopo
    /// un'escalation cross-tier) raggiunge (`>=`) questo valore, l'executor chiude
    /// d'autorita' (`close_runaway`) come l'hard-cap token. A DIFFERENZA del budget
    /// token NON si resetta all'escalation: e' il tetto dell'intero run, uniforme in
    /// dollari invece che in token (400k token costano ~64x su gpt-5.5 vs
    /// deepseek-flash). Regola G: DB-driven, mai hardcoded.
    pub run_cost_budget_usd: f64,
    /// Deadline in SECONDI dell'INTERO run (`agent.run_time_budget_s`, default 0
    /// = disabilitato -> bit-identico). Terzo asse del budget accanto a token e
    /// dollari (fase 3 paradigma orchestrazione): tempo di parete dal via del
    /// run primario (`AgentState::run_started_at_epoch_s`, checkpointato ->
    /// sopravvive ai resume, misura il run INTERO e non l'ultimo spezzone). Al
    /// raggiungimento (`>=`) l'executor chiude d'autorita' (`close_runaway`) con
    /// reason canonico `time_budget` (regola M/N), come il cap di spesa. NON si
    /// resetta all'escalation. Regola G: DB-driven, mai hardcoded.
    pub run_time_budget_s: u64,
    /// Percentuale di [`Self::run_time_budget_s`] oltre cui (`>=`) un run con un
    /// CANALE DI RUOLO ancora muto (figura del consiglio senza `advisory_verdict`,
    /// avvocato senza `debate_position`) riceve UN turno di grazia per dichiarare
    /// col proprio canale, invece di essere ucciso muto allo scadere
    /// (`agent.time_grace_pct`, default 70 = al 70% del budget). `0` = disabilitato
    /// -> comportamento bit-identico (si chiude solo a budget esaurito).
    ///
    /// Perche' una PERCENTUALE e non un tempo fisso: il residuo che serve per
    /// chiudere e' proporzionale al budget (una figura da 300s ha bisogno di piu'
    /// margine di una da 60s). Il valore giusto dipende dalla latenza reale delle
    /// chiamate: se una singola chiamata dura quanto il residuo, il turno di grazia
    /// non fa in tempo e va abbassata (regola G: si tara dal DB, non nel codice).
    pub time_grace_pct: u64,
    /// Turni CONSECUTIVI falliti al gateway sulla STESSA coppia provider/model
    /// con causa DETERMINISTICA (risposta degenere `empty_completion`,
    /// `client_error` fuori dalla whitelist di recupero) oltre cui (`>=`) il run
    /// chiude con esito onesto invece di ritentare
    /// (`agent.gateway_deterministic_streak_max`, default 3; `0` = disabilitato).
    ///
    /// Senza questo tetto, quando il failover cross-provider non scatta (cap
    /// escalation raggiunto, nessun sostituto sano, causa non recuperabile) il
    /// turno d'errore sintetico lascia provider e model sticky INVARIATI: il
    /// giro dopo rifa' la stessa chiamata e riceve la stessa risposta, fino al
    /// budget. Misurato sul run 2abb30db del 20/07: retry deterministici a
    /// oltranza (~7s l'uno su empty, ~500ms su 400), invisibili al DB perche'
    /// i retry intra-iterazione non emettono meta-step. Le cause TRANSITORIE
    /// (cooldown, transient) restano fuori: possono risolversi da sole.
    pub gateway_deterministic_streak_max: u64,
    /// Turni solo-testo CONSECUTIVI oltre cui (`>=`) il run chiude deterministicamente
    /// (`agent.max_consecutive_text_only_turns`, default 3): fast-fail sul modello che
    /// DESCRIVE senza AGIRE (pattern gemini che ignora `force_tool_choice`). Un turno
    /// e' "solo-testo" quando la risposta NON contiene tool_use mentre il loop si
    /// aspetta azioni (segnale strutturato `LlmResponse.tool_calls`/`pending_tool_uses`,
    /// regola M — mai dal parsing del testo). `0` = disabilitato -> bit-identico.
    /// Regola G: DB-driven, mai hardcoded.
    pub max_consecutive_text_only_turns: u32,
    /// 3.4 (difesa strutturale provider-no-progress): quando il cap solo-testo scatta
    /// (provider che non produce output utile per N turni), invece di chiudere il run
    /// col backstop, PROVA prima a CAMBIARE PROVIDER via failover (`failover_provider`,
    /// escludendo i gia' provati). Cosi' un provider bloccato non affossa il run se un
    /// altro puo' procedere. `false` = disabilitato -> comportamento bit-identico
    /// (chiusura backstop come oggi). Gata da `agent.provider_no_progress.enabled`
    /// (regola G). Riusa il cap solo-testo come trigger e `auto_escalations < 3` per
    /// non ciclare fra provider.
    pub provider_no_progress_switch_enabled: bool,
    /// Soglie DB-driven della rilevazione loop-by-signature
    /// (`agent.loop.signature_threshold` / `agent.loop.recent_signatures_cap`).
    /// Regola G: ex costanti `LOOP_THRESHOLD` / `RECENT_SIGNATURES_CAP`.
    pub loop_thresholds: LoopThresholds,
    /// Soglie DB-driven (`agent.anti_repetition.*`) del rilevamento
    /// repetition-collapse del TESTO di un turno assistant: un muro di testo con
    /// la stessa sottostringa ripetuta N+ volte (collasso dei modelli piccoli) NON
    /// e' una risposta valida ne' un esito verificato. Complementare al
    /// signature-loop (che guarda le tool call, non il testo). `scan_tail_cap=0`
    /// disabilita -> comportamento bit-identico. Regola G: mai hardcoded.
    pub repetition: RepetitionThresholds,
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
    /// GOVERNANCE costo/beneficio del rolling-summary (opt-in,
    /// `agent.governance.rolling_summary_adaptive`, default OFF): quando ON, il
    /// rolling-summary viene SALTATO se il prefisso da riassumere e' sotto
    /// [`Self::governance_rolling_summary_min_prefix`] (il beneficio non giustifica
    /// il costo della chiamata LLM). Decisione PURA
    /// [`crate::decisions::governance::rolling_summary_worthwhile`]. OFF =
    /// comportamento storico (bit-identico: decide solo `select_rolling_summary_cutoff`).
    pub governance_rolling_summary_adaptive: bool,
    /// Soglia minima di messaggi del prefisso sotto cui il rolling-summary NON vale
    /// il costo (`agent.governance.rolling_summary_min_prefix`, regola G). Usata solo
    /// se [`Self::governance_rolling_summary_adaptive`] e' ON.
    pub governance_rolling_summary_min_prefix: i64,
    /// `true` se il continuity-trim SEMANTICO e' attivo
    /// (`agent.context.continuity_trim_enabled`): al cambio-fase scarta dal prefisso
    /// vecchio gli atomi (turno+tool_result) semanticamente irrilevanti al focus del
    /// turno, via [`EmbeddingStore`] + coseno. Richiede la porta iniettata
    /// ([`Self::with_embedding_store`]). Default safe-DB-down: OFF (bit-identico).
    pub continuity_trim_enabled: bool,
    /// Soglia coseno sotto la quale un atomo e' "irrilevante" e viene scartato dal
    /// continuity-trim (`agent.context.continuity_trim_min_score`). Default 0.25.
    pub continuity_trim_min_score: f32,
    /// Cap massimo di messaggi scartabili dal continuity-trim in una passata
    /// (`agent.context.continuity_trim_max_drop`), rete di sicurezza. Default 8.
    pub continuity_trim_max_drop: i64,
    /// `true` se i tool_result compressi vengono OFFLOADATI su RAG (recuperabili via
    /// `ref` nel marker) invece di essere solo degradati
    /// (`agent.context.compress_offload_enabled`). Richiede la porta
    /// [`ContextOffload`] iniettata. Default safe-DB-down: OFF (bit-identico).
    pub compress_offload_enabled: bool,
    /// `true` se gli originali del rolling-summary vengono indicizzati su RAG
    /// (`chat_history`, recuperabili per sessione) prima di essere sostituiti dal
    /// riassunto (`agent.context.rolling_summary_offload_enabled`). Richiede la porta
    /// [`ContextOffload`] iniettata. Default safe-DB-down: OFF (bit-identico).
    pub rolling_summary_offload_enabled: bool,
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
    /// Config dello SCALE-CONTROLLER bidirezionale (mig 0516, opt-in DB, regola G).
    /// Con `scale.enabled=false` (default) il detector-emissione dell'executor NON
    /// valuta MAI: nessun `ScaleReason` emesso, nodo `ScaleControl` irraggiungibile
    /// -> comportamento BIT-IDENTICO a oggi (vincolo primario PR-B3). Tutte le
    /// soglie/flag arrivano dai settings `agent.scale.*` (mai hardcoded).
    pub scale: ScaleConfig,
}

/// Config dello SCALE-CONTROLLER letta dai settings `agent.scale.*` (mig 0516).
/// Tutti i default sono conservativi/OFF (regola G: default = comportamento piu'
/// cauto, mai magic-value): con `enabled=false` (default) il controller e' inerte.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleConfig {
    /// `agent.scale.enabled`: kill-switch dello scale-controller. `false` (default)
    /// -> il detector salta PRIMA di ogni lavoro (zero overhead, bit-identico).
    pub enabled: bool,
    /// `agent.scale.downscale_enabled`: abilita il DOWNSCALE. `false` (default) ->
    /// solo up-consolidation (rollout: prima up, poi down).
    pub downscale_enabled: bool,
    /// `agent.scale.eval_every_iters` (default 4): cadenza di valutazione + base
    /// della chiave-cache `floor(iterations/N)`.
    pub eval_every_iters: i64,
    /// `agent.scale.min_tail_iters` (default 6): gate break-even. Coda residua
    /// minima per attivare il controller (costo netto zero su run corti).
    pub min_tail_iters: i64,
    /// `agent.scale.min_confidence` (default 0.70): soglia sotto cui KeepTier.
    pub min_confidence: f64,
    /// `agent.scale.change_cooldown_turns` (default 2): cooldown post-cambio-tier.
    pub change_cooldown_turns: i64,
    /// `agent.scale.downscale_clean_window` (default 3): streak pulita richiesta per
    /// il downscale (banda-morta asimmetrica).
    pub downscale_clean_window: i64,
    /// `agent.scale.max_reversals` (default 2): oltre -> pin al tier PIU' ALTO.
    pub max_reversals: i64,
    /// `agent.scale.max_tier_changes_per_run` (default 3): cambi-tier massimi/run.
    pub max_tier_changes_per_run: i64,
    /// `agent.scale.max_evals_per_run` (default 6): cap consultazioni LLM/run.
    pub max_evals_per_run: i64,
    /// `agent.scale.window_overhead_ratio` (default 1.3): overhead per il vincolo
    /// finestra nel downscale (FIX-B): `required = est_tokens * ratio`.
    pub window_overhead_ratio: f64,
    /// `agent.scale.sizing_enabled` (default false): kill-switch NESTED del SIZING
    /// agentico (mig 0524). Con `scale.enabled=true` ma sizing OFF il flusso TIER
    /// resta bit-identico (il detector non popola i segnali sizing, il gate degrada
    /// ogni `AdjustSizing` a `KeepTier`). Regola G, opt-in DB.
    pub sizing_enabled: bool,
    /// `agent.scale.sizing_cooldown_turns` (default 3): turni minimi tra due cambi di
    /// POSTURA di sizing (anti-thrash del sizing, DISTINTO dal cooldown tier).
    pub sizing_cooldown_turns: i64,
    /// `agent.scale.sizing_aggressiveness` in `[0,1]` (default 0.5): quanto una
    /// postura spinge le soglie di dimensionamento (UNICA manopola DB del
    /// trasformatore proporzionale; i floor/ceil sono invarianti dell'algoritmo).
    pub sizing_aggressiveness: f64,
}

impl Default for ScaleConfig {
    fn default() -> Self {
        // Default safe-DB-down = seed conservativi mig 0516 (OFF). Valgono SOLO se il
        // DB e' irraggiungibile; il wiring mcp-core passa i valori reali. Con
        // enabled=false il detector non scatta -> bit-identico.
        Self {
            enabled: false,
            downscale_enabled: false,
            eval_every_iters: 4,
            min_tail_iters: 6,
            min_confidence: 0.70,
            change_cooldown_turns: 2,
            downscale_clean_window: 3,
            max_reversals: 2,
            max_tier_changes_per_run: 3,
            max_evals_per_run: 6,
            window_overhead_ratio: 1.3,
            // Sizing agentico OFF di default (mig 0524): con scale ON ma sizing OFF il
            // flusso tier resta bit-identico.
            sizing_enabled: false,
            sizing_cooldown_turns: 3,
            sizing_aggressiveness: 0.5,
        }
    }
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
            // Safe-default (vale solo se il DB e' irraggiungibile): i code
            // provider-specifici notoriamente recuperabili con un altro provider.
            recoverable_client_error_codes: vec![
                "invalid_argument".to_string(),
                "thought_signature".to_string(),
                "failed_precondition".to_string(),
            ],
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
            // Default safe-DB-down = seed mig 0520. Il wiring mcp-core passa i valori
            // reali dai settings agent.run_token_budget / agent.max_consecutive_text_only_turns.
            run_token_budget: 400_000,
            // Default safe-DB-down = seed mig 0521 (~2x budget). Backstop di
            // catastrofe: rilevante solo a stall_recovery_enabled=true (altrimenti
            // run_token_budget chiude gia' prima -> retro-compat 822e083).
            run_token_hard_cap: 800_000,
            // 0 = disabilitato (bit-identico): il freno di spesa in dollari e' attivato
            // dal setting DB agent.run_cost_budget_usd (mig 0533).
            run_cost_budget_usd: 0.0,
            // 0 = disabilitato (bit-identico): la deadline di run e' attivata
            // dal setting DB agent.run_time_budget_s (mig 0604).
            run_time_budget_s: 0,
            // Turno di grazia al 70% del budget: il default vive nel DB (mig 0614),
            // qui e' solo la rete di sicurezza documentata del costruttore.
            time_grace_pct: 70,
            gateway_deterministic_streak_max: 3,
            max_consecutive_text_only_turns: 3,
            // 3.4 default OFF -> comportamento bit-identico (chiusura backstop).
            provider_no_progress_switch_enabled: false,
            loop_thresholds: LoopThresholds::default(),
            repetition: RepetitionThresholds::default(),
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
            // Governance rolling-summary OFF di default (opt-in): con OFF il gate
            // costo/beneficio non si applica -> comportamento storico bit-identico.
            governance_rolling_summary_adaptive: false,
            governance_rolling_summary_min_prefix: 6,
            // Default safe-DB-down: continuity-trim/offload OFF (il wiring mcp-core
            // passa i settaggi `agent.context.*`). Con questi default il
            // comportamento e' bit-identico a oggi (nessun embed, nessun offload).
            continuity_trim_enabled: false,
            continuity_trim_min_score: 0.25,
            continuity_trim_max_drop: 8,
            compress_offload_enabled: false,
            rolling_summary_offload_enabled: false,
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
            // Default safe-DB-down = seed mig 0516 (scale OFF). Con enabled=false il
            // detector-emissione salta PRIMA di ogni lavoro -> bit-identico.
            scale: ScaleConfig::default(),
        }
    }
}

/// Granularita' con cui lo Stop utente diventa visibile MENTRE una chiamata al
/// modello e' in volo. Non e' una soglia di business (regola G): e' il passo di
/// poll del segnale di cancellazione DB. 2s bilancia reattivita' (lo Stop si
/// riflette entro ~2s) e carico DB (una SELECT booleana per chiamata attiva ogni
/// 2s). Iniettato (mai letto qui) cosi' i test lo stringono a millisecondi.
const CANCEL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// PUNTO UNICO (regola L) della cancellazione COOPERATIVA di una chiamata al
/// modello. Corre `complete_fut` (la chiamata al gateway) contro un poll
/// periodico di [`RunControlStore::is_superseded`] (il segnale DB scritto dallo
/// Stop utente `user_cancel` o dal supersede last-wins).
///
/// Perche' esiste: il gate di TESTA dell'executor legge la cancellazione SOLO a
/// inizio iterazione (`head_gate`), mai durante la chiamata. Senza questa corsa,
/// uno Stop arrivato mentre la chiamata e' in volo non viene visto finche' la
/// chiamata non rientra (fino a 90-150s sotto carico), e se il modello poi
/// conclude il run finalizza 'completed' ignorando lo Stop (incidente 18/07, run
/// 53dac032: Stop a +2min, chiuso 'completed'). Qui, appena il DB segnala la
/// cancellazione, la corsa ritorna `None` e `complete_fut` viene DROPPATO
/// (reqwest annulla la richiesta HTTP in volo).
///
/// `biased`: a parita' di prontezza la CHIAMATA vince, cosi' una risposta gia'
/// arrivata non viene persa per un poll concomitante. Fail-open (regola di
/// sicurezza, coerente con [`RunControlStore::is_superseded`]): un errore di
/// lettura del segnale NON cancella (il run prosegue; il gate di testa
/// ricontrollera'), mai un abort per un guasto infrastrutturale.
///
/// Ritorna `Some(esito)` se la chiamata e' rientrata (successo o errore del
/// gateway); `None` se lo Stop e' stato rilevato durante la chiamata (il
/// chiamante chiude il run `Superseded` senza attendere che la chiamata rientri).
async fn complete_or_cancel(
    complete_fut: impl std::future::Future<Output = Result<LlmResponse, PortError>>,
    run_control: &dyn RunControlStore,
    run_id: &str,
    poll_interval: std::time::Duration,
) -> Option<Result<LlmResponse, PortError>> {
    tokio::select! {
        biased;
        r = complete_fut => Some(r),
        _ = poll_until_superseded(run_control, run_id, poll_interval) => None,
    }
}

/// Attende finche' il segnale di cancellazione DB (`is_superseded`) non e' vero,
/// pollandolo ogni `interval`. Completa SOLO alla cancellazione: usato come ramo
/// perdente del `select!` di [`complete_or_cancel`] (se la chiamata rientra prima,
/// questo future viene droppato). Fail-open: un errore di lettura non e' una
/// cancellazione (il run prosegue).
async fn poll_until_superseded(
    run_control: &dyn RunControlStore,
    run_id: &str,
    interval: std::time::Duration,
) {
    loop {
        tokio::time::sleep(interval).await;
        if run_control.is_superseded(run_id).await.unwrap_or(false) {
            return;
        }
    }
}

/// Delta di uscita cooperativa per un run superato/cancellato: `stop_reason =
/// Superseded`, nient'altro. PUNTO UNICO (regola L) condiviso dal gate di TESTA
/// (Stop visto a inizio iterazione) e dalla cancellazione DURANTE la chiamata
/// ([`complete_or_cancel`]), cosi' i due percorsi chiudono il run in modo
/// identico e il delta non e' costruito inline in due punti.
fn superseded_delta() -> OpaqueDelta {
    StateDelta {
        stop_reason: Some(Some(StopReason::Superseded)),
        ..Default::default()
    }
    .into_opaque()
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
    /// Persistenza meta-step (`executor_call` heartbeat).
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
    /// Porta I/O embedding per il continuity-trim SEMANTICO. OPZIONALE (`None` ->
    /// continuity-trim disattivo, bit-identico: solo troncamento posizionale):
    /// iniettata dal wiring col builder [`Self::with_embedding_store`]. La DECISIONE
    /// (coseno, chi scartare) resta nel modulo puro `context_reduction` (regola L).
    /// Gata anche dal flag `cfg.continuity_trim_enabled`.
    embedding_store: Option<Arc<dyn EmbeddingStore>>,
    /// Porta I/O di offload RAG del contesto (tool_result compressi + originali
    /// rolling-summary). OPZIONALE (`None` -> nessun offload: degrado a marker /
    /// rolling non recuperabile, come oggi): iniettata col builder
    /// [`Self::with_context_offload`]. Gata dai flag `cfg.compress_offload_enabled`
    /// / `cfg.rolling_summary_offload_enabled`.
    offload: Option<Arc<dyn ContextOffload>>,
}


/// Payload STRUTTURATO del cambio provider (punto unico dei tre emettitori:
/// failover in cascata, signature_loop, no_progress). Le chiavi sono il
/// contratto wire con la card "CAMBIO PROVIDER" del frontend; `from_model` e'
/// il modello CADUTO -- senza, la card mostrava "Mistral / ?" (il frontend
/// ripiega su prev.model, assente sul primo segmento).
fn switch_payload(
    from_provider: &str,
    from_model: &str,
    to_provider: &str,
    to_model: &str,
    reason: SwitchReason,
    cooldown: Option<bool>,
    cause: Option<&str>,
) -> Value {
    let mut p = serde_json::Map::new();
    p.insert("from_provider".into(), from_provider.into());
    p.insert("from_model".into(), from_model.into());
    p.insert("to_provider".into(), to_provider.into());
    p.insert("to_model".into(), to_model.into());
    // `reason` = identificatore canonico, invariato: e' cio' che la logica e i
    // test confrontano. `reason_description` e' il canale per l'occhio, additivo
    // (un frontend che non lo conosce continua a funzionare come prima).
    // Prima il motivo era un `&str` libero e la card mostrava il codice grezzo:
    // "Motivo: final_gate_nonconvergence".
    p.insert("reason".into(), reason.code().into());
    p.insert("reason_description".into(), reason.descrizione().into());
    if let Some(c) = cooldown {
        p.insert("cooldown".into(), c.into());
    }
    if let Some(c) = cause {
        p.insert("cause".into(), c.into());
    }
    Value::Object(p)
}

/// Payload di un cambio provider da STALLO (g1_cap / exploration /
/// repeated_action): la coppia corrente arriva come `Option` da
/// `escalation_current_pair` (None -> stringa vuota, il frontend ripiega su
/// prev.model). Wrapper dei tre emettitori sul punto unico `switch_payload`
/// (regola L): senza, ricostruivano il payload a mano OMETTENDO `from_model`
/// -> la card mostrava "Mistral / ?".
fn stall_switch_payload(
    cur_provider: &Option<String>,
    cur_model: &Option<String>,
    to_provider: &str,
    to_model: &str,
    reason: SwitchReason,
) -> Value {
    switch_payload(
        cur_provider.as_deref().unwrap_or(""),
        cur_model.as_deref().unwrap_or(""),
        to_provider,
        to_model,
        reason,
        None,
        None,
    )
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
            embedding_store: None,
            offload: None,
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

    /// Inietta la porta embedding per il continuity-trim SEMANTICO. Senza iniezione
    /// (default) il continuity-trim e' disattivo (bit-identico: solo troncamento
    /// posizionale). Scatta solo se ANCHE il flag `continuity_trim_enabled` e' attivo
    /// (regola G: la porta abilita l'infra, il flag governa il comportamento).
    pub fn with_embedding_store(mut self, store: Arc<dyn EmbeddingStore>) -> Self {
        self.embedding_store = Some(store);
        self
    }

    /// Inietta la porta di offload RAG del contesto (tool_result compressi +
    /// originali del rolling-summary). Senza iniezione (default) niente offload: la
    /// compressione degrada a marker e il rolling-summary non e' recuperabile. Scatta
    /// solo se ANCHE i flag `compress_offload_enabled`/`rolling_summary_offload_enabled`
    /// sono attivi (regola G).
    pub fn with_context_offload(mut self, offload: Arc<dyn ContextOffload>) -> Self {
        self.offload = Some(offload);
        self
    }

    /// Costruisce la mappa `content -> pointer` per il compress-offload: offloada su
    /// RAG (best-effort, gata dal flag `compress_offload_enabled` + porta) i
    /// tool_result che [`ctxr::compress_old_tool_results`] comprimera'
    /// (SELEZIONE pura [`ctxr::contents_eligible_for_offload`], regola L). Mappa
    /// VUOTA se disabilitato, senza porta, o su guasto -> il marker degrada a
    /// [`ctxr::degraded_marker`] (bit-identico a oggi).
    async fn build_compress_offload_map(
        &self,
        hist: &[HistoryMessage],
        cutoff_index: usize,
        threshold: usize,
        run_id: &str,
    ) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        if !self.cfg.compress_offload_enabled {
            return map;
        }
        let Some(offload) = self.offload.as_ref() else {
            return map;
        };
        let session = if run_id.is_empty() {
            None
        } else {
            Some(run_id.to_string())
        };
        for content in ctxr::contents_eligible_for_offload(hist, cutoff_index, threshold) {
            if map.contains_key(&content) {
                continue;
            }
            if let Ok(ptr) = offload
                .offload_to_rag(
                    serde_json::Value::String(content.clone()),
                    OffloadKind::ToolResult,
                    session.clone(),
                    None,
                )
                .await
            {
                map.insert(content, ptr);
            }
            // Su Err: questo contenuto degrada a degraded_marker (nessun ref).
        }
        if !map.is_empty() {
            tracing::info!(
                target: "nexus_agent_graph::executor",
                run_id = %run_id,
                offloaded = map.len(),
                "compress-offload: tool_result indicizzati (recuperabili via ref)"
            );
        }
        map
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
    /// Soppresso quando `final_gate_cycle>0` (correzione oggettiva in corso).
    DeclaredDone,
    /// G1 cap raggiunto (escalation esaurita) -> g1_cap_reached (py:1945).
    G1Cap,
    /// Nessun gate di testa: prosegue ai nudge/LLM.
    Proceed,
}

/// Decisione PURA dei gate di TESTA (priorita' 1:1 col Python). `g1_cap_reached`
/// arriva gia' calcolato da [`g1_accounting`] (punto unico). `final_gate_correction_active`
/// true quando `final_gate_cycle > 0`: il gate ha bocciato e chiede fix; in quel
/// turno la chiusura d'autorita' su `done>=3` e' soppressa (incidente run 97cbaa45).
pub(crate) fn head_gate(
    superseded: bool,
    declared_done: bool,
    declared_done_count: i64,
    g1_cap_reached: bool,
    final_gate_correction_active: bool,
) -> HeadGate {
    if superseded {
        return HeadGate::Superseded;
    }
    if declared_done
        && declared_done_count >= 3
        && !final_gate_correction_active
    {
        return HeadGate::DeclaredDone;
    }
    if g1_cap_reached {
        return HeadGate::G1Cap;
    }
    HeadGate::Proceed
}

/// Finestra forced-text (ultime N iterazioni): svuota il catalogo tool per
/// obbligare un resoconto testuale. Soppressa durante la correzione post-
/// `final_gate` (`final_gate_cycle>0`): in quel turno servono i tool di scrittura
/// per applicare i fix richiesti dal gate (incidente run cee89699).
pub(crate) fn forced_text_turn_active(
    iters_in: i64,
    forced_text_threshold: i64,
    stop_reason: Option<StopReason>,
    final_gate_correction_active: bool,
    tools_available: bool,
) -> bool {
    tools_available
        && iters_in >= forced_text_threshold
        && stop_reason == Some(StopReason::ToolUse)
        && !final_gate_correction_active
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
    let p = prov
        .map(str::to_string)
        .unwrap_or_else(|| routing_provider.to_string());
    let m = modl
        .map(str::to_string)
        .unwrap_or_else(|| routing_model.to_string());
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
        let final_gate_correction_active = state.final_gate_cycle.unwrap_or(0) > 0;
        match head_gate(
            superseded,
            declared_done,
            done_count,
            false,
            final_gate_correction_active,
        ) {
            HeadGate::Superseded => {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    thread = %run_id,
                    "run superato/cancellato, uscita cooperativa (no chiamata modello)"
                );
                return Ok(superseded_delta());
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
            if let Some(delta) = self.consume_recovery_move(state, iters_in, ctx).await {
                return Ok(delta);
            }
        }

        // ── CONSUMO scale-controller (rientro dal nodo ScaleControl) ──────────
        // Speculare al rientro StallResolved: se rientriamo con ScaleResolved
        // (self-loop `ScaleControl -> executor`), la ScaleMove e' persistita in
        // extra: la consumiamo QUI (PR-B3). Some -> applica il cambio-tier (sticky +
        // current_tier) e ri-fa il turno; None -> KeepTier / cambio annullato ->
        // prosegue il turno normale. A flag OFF nessun detector emette ScaleReason,
        // quindi ScaleResolved non arriva MAI qui -> bit-identico.
        if state.stop_reason == Some(StopReason::ScaleResolved) {
            if let Some(delta) = self.consume_scale_move(state, iters_in, ctx).await {
                return Ok(delta);
            }
        }

        // ── CONSUMO trigger di ESCALATION da NON-CONVERGENZA del final_gate ───
        // Rientro DEDICATO dal final_gate (`ToolUse` + flag [`FINAL_GATE_ESCALATION_KEY`]
        // in extra, posato quando il gate esaurisce `max_cycles` con criteri OGGETTIVI
        // ancora falliti): PRIMA di ridare il turno allo STESSO modello scadente,
        // PROMUOVI a uno piu' capace via il PUNTO UNICO [`Self::maybe_escalate_nonconvergence`]
        // (regola L: il gate non ha la porta di escalation, l'executor si'). Precede i
        // cap generici (iteration_cap/budget) perche' e' un segnale gia' diagnosticato
        // dal gate, non un runaway grezzo.
        //   - Some -> delta di promozione (sticky + reset contatori + budget del turno
        //     azzerato + flag CONSUMATO dentro maybe_escalate_nonconvergence); il
        //     modello promosso riparte con cicli di gate freschi.
        //   - None -> catena esaurita / tutti in cooldown (raro: il gate ha gia'
        //     verificato auto_escalations < max, ma un cooldown puo' sopraggiungere):
        //     chiusura FailedDiagnosed via il PUNTO UNICO [`Self::close_runaway`]
        //     (backstop identico a budget_token). Il flag residuo e' innocuo (il run
        //     chiude verso learner/gate-forced, non rientra in questo nodo).
        if state
            .extra
            .get(FINAL_GATE_ESCALATION_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(delta) = self
                .maybe_escalate_nonconvergence(
                    state,
                    iters_in,
                    SwitchReason::FinalGateNonconvergence,
                    ctx,
                    false,
                )
                .await
            {
                return Ok(delta);
            }
            let auto_escalations = state
                .extra
                .get("auto_escalations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let close_text = "La verifica finale non e' stata superata entro i tentativi \
previsti e non e' disponibile un modello piu' capace a cui passare (catena di \
escalation esaurita). Interrompo: riformula la richiesta in modo piu' specifico \
oppure riprova piu' tardi."
                .to_string();
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                auto_escalations,
                "non-convergenza final_gate: nessun modello di escalation disponibile -> chiusura (backstop)"
            );
            return Ok(self.close_runaway(
                iters_in,
                close_text,
                "final_gate_nonconvergence_no_escalation",
                json!({ "auto_escalations": auto_escalations }),
            ));
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
        // Turno dichiarativo pendente: quello dell'ESITO (ADR 0034) oppure quello
        // di RUOLO (turno di grazia forzante). In entrambi i casi i gate di
        // chiusura pre-LLM non devono corto-circuitare il turno prima che il
        // modello riceva la richiesta di dichiarare (incidente gia' documentato
        // sul gemello: vedi il commento a `!declaration_pending` piu' sotto).
        let declaration_pending = state
            .extra
            .get("force_outcome_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || state
                .extra
                .get("force_role_declaration")
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
                        thinking_signature: None,
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
        if let Some(rat) = state
            .plan_rationale
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
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
                    let rej = o
                        .get("rejected_because")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
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
                tools_json.retain(|t| {
                    t.get("name").and_then(Value::as_str) == Some("nexus_mcp_tool_search")
                });
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
            // Niente di fisso (regola H): il cap ASSOLUTO iterazioni non chiude piu'
            // secco. Come 4d/4e (token/text_only), e' un TRIGGER del giudice
            // tier-agentico (nodo StallRecovery): decide su segnali strutturati
            // (proseguire guidato / escalare modello / dichiarare blocked). None
            // (giudice OFF / budget consultazioni esaurito / anti-meta-loop) ->
            // backstop sotto (chiusura EndTurn, bit-identico al pre-fix).
            if let Some(delta) = self
                .maybe_runaway_stall_delta(
                    state,
                    crate::decisions::meta_reason::AXIS_ITERATION_CAP,
                    iters_in,
                    iters_in,
                    &messages,
                    ctx,
                )
                .await
            {
                return Ok(delta);
            }
            // Non-convergenza (regola H, simmetria col ramo budget_token): esaurito il
            // giudice meta-reasoner, PRIMA del backstop di chiusura prova l'ESCALATION
            // AGENTICA a un modello piu' capace (punto unico maybe_escalate_nonconvergence).
            // `reset_iterations=true`: il cap E' sulle iterazioni, quindi il promosso
            // riparte con un ciclo pieno (altrimenti rientrerebbe subito qui senza mai
            // lavorare). None (catena esaurita / max escalation / cooldown) -> backstop
            // sotto (bit-identico al pre-fix). Bound: auto_escalations + hard-cap
            // token/costo.
            if let Some(delta) = self
                .maybe_escalate_nonconvergence(state, iters_in, SwitchReason::IterationCap, ctx, true)
                .await
            {
                return Ok(delta);
            }
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
                    thinking_signature: None,
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

        // ── (4d) BUDGET TOKEN cumulativo: safety net anti-runaway per COSTO ────
        // Complementare a `iteration_cap` (che conta ITERAZIONI): un modello che
        // ignora `force_tool_choice` produce turni solo-testo che non triggerano il
        // signature-loop (contano ripetizioni di TOOL, non testo) e bruciano
        // ~16k token/turno (osservato un run a 1.8M token / $2.42 senza convergere).
        // Il conteggio arriva dal segnale STRUTTURATO dell'usage del gateway
        // (regola M: `LlmUsage.total_tokens`, sommato a fine turno nello stato).
        // `run_token_budget=0` = disabilitato -> bit-identico. Chiude PRIMA della
        // chiamata LLM, come il cap iterazioni.
        let tokens_used_total = state.tokens_used_total.unwrap_or(0).max(0) as u64;
        // BACKSTOP di catastrofe (regola H: rete di sicurezza non-negoziabile): oltre
        // l'hard-cap si chiude SEMPRE direttamente, senza consultare il giudice (un
        // modello patologico brucerebbe token all'infinito se il giudice dicesse
        // "prosegui"). Con stall_recovery_enabled=false questo ramo e' irrilevante
        // (run_token_budget chiude gia' prima). `0` = disabilitato.
        if self.cfg.run_token_hard_cap > 0 && tokens_used_total >= self.cfg.run_token_hard_cap {
            let hard_text = format!(
                "Raggiunto il tetto DURO di token del run ({} token, hard-cap {}). \
Interrompo d'autorita' per evitare un consumo incontrollato: riformula la richiesta \
in modo piu' specifico, oppure riprova con un modello piu' capace.",
                tokens_used_total, self.cfg.run_token_hard_cap
            );
            tracing::error!(
                target: "nexus_agent_graph::executor",
                tokens_used = tokens_used_total,
                hard_cap = self.cfg.run_token_hard_cap,
                "HARD-CAP token raggiunto -> chiusura d'autorita' (backstop, no giudice)"
            );
            return Ok(self.close_runaway(
                iters_in,
                hard_text,
                "token_hard_cap",
                json!({
                    "tokens_used_total": tokens_used_total,
                    "run_token_hard_cap": self.cfg.run_token_hard_cap,
                }),
            ));
        }
        // Freno di spesa in DOLLARI del RUN (audit selezione costi): il costo
        // cumulativo REALE (ogni turno col prezzo del proprio modello, esatto anche
        // dopo un'escalation cross-tier) ha superato il tetto in dollari. A differenza
        // del budget token (per-turno, TRIGGER del giudice, si resetta all'escalation)
        // questo e' il tetto dell'INTERO run e NON si resetta -> hard stop d'autorita'
        // come l'hard-cap token. `0` = disabilitato (bit-identico). Prezzo ignoto ->
        // costo non accumulato -> non scatta (best-effort, mai un cap spurio).
        if self.cfg.run_cost_budget_usd > 0.0 {
            let cost_used = state.run_cost_cumulative_usd.unwrap_or(0.0).max(0.0);
            if cost_used >= self.cfg.run_cost_budget_usd {
                let cost_text = format!(
                    "Raggiunto il tetto di spesa del run (${:.2}, budget ${:.2}). \
Interrompo d'autorita' per evitare un costo incontrollato: riformula la richiesta in \
modo piu' specifico, oppure riprova con un modello piu' economico.",
                    cost_used, self.cfg.run_cost_budget_usd
                );
                tracing::error!(
                    target: "nexus_agent_graph::executor",
                    cost_used_usd = cost_used,
                    cost_budget_usd = self.cfg.run_cost_budget_usd,
                    "CAP di spesa del run raggiunto -> chiusura d'autorita' (costo reale cumulativo)"
                );
                return Ok(self.close_runaway(
                    iters_in,
                    cost_text,
                    "cost_budget_usd",
                    json!({
                        "run_cost_cumulative_usd": cost_used,
                        "run_cost_budget_usd": self.cfg.run_cost_budget_usd,
                    }),
                ));
            }
        }
        // Deadline dell'INTERO run in tempo di parete (fase 3 paradigma
        // orchestrazione): epoch di avvio CHECKPOINTATO nello stato -> la misura
        // sopravvive ai resume e copre il run intero. `0` = disabilitato; run
        // senza epoch (avviati prima della fase 3) -> nessun enforcement, mai un
        // default inventato. Chiusura PULITA con reason canonico `time_budget`
        // (regola M/N), gemella del cap di spesa qui sopra.
        if self.cfg.run_time_budget_s > 0 {
            if let Some(started) = state.run_started_at_epoch_s {
                let now_s = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(started);
                let elapsed_s = now_s.saturating_sub(started).max(0) as u64;
                // SOLLECITO DI CHIUSURA prima del kill (interattivo, regola H): col
                // budget quasi esaurito e il canale di ruolo ancora muto, invece di
                // uccidere si CHIEDE alla figura di chiudere col parere che ha. Cosi'
                // una figura che scade non muore n/d: dichiara un parere reale, magari
                // parziale, che il consiglio puo' comporre. Punto unico riusato
                // (regola L): lo stesso `maybe_advisory_grace_delta` del backstop
                // text-only, gia' one-shot e gia' con gli azzeramenti dei detector.
                // `None` (non e' un ruolo / grazia gia' concessa) -> si prosegue al
                // ramo di chiusura sotto, bit-identico.
                if let Some(delta) = self
                    .maybe_time_grace_delta(state, iters_in, ctx, elapsed_s)
                    .await
                {
                    return Ok(delta);
                }
                if elapsed_s >= self.cfg.run_time_budget_s {
                    let time_text = format!(
                        "Raggiunta la deadline del run ({elapsed_s}s trascorsi, budget {}s). \
Interrompo d'autorita' per rispettare il tempo massimo: riformula la richiesta in modo \
piu' specifico, oppure alza agent.run_time_budget_s se il task richiede piu' tempo.",
                        self.cfg.run_time_budget_s
                    );
                    tracing::error!(
                        target: "nexus_agent_graph::executor",
                        elapsed_s,
                        run_time_budget_s = self.cfg.run_time_budget_s,
                        "DEADLINE del run raggiunta -> chiusura d'autorita' (tempo di parete)"
                    );
                    return Ok(self.close_runaway(
                        iters_in,
                        time_text,
                        "time_budget",
                        json!({
                            "elapsed_s": elapsed_s,
                            "run_time_budget_s": self.cfg.run_time_budget_s,
                        }),
                    ));
                }
            }
        }
        if self.cfg.run_token_budget > 0 && tokens_used_total >= self.cfg.run_token_budget {
            // Con il meta-reasoner ACCESO il budget morbido e' un TRIGGER del giudice
            // agentico (non piu' un veto fisso): instrada al nodo StallRecovery che
            // sceglie la mossa (proseguire guidato / escalare modello / dichiarare
            // blocked). Se il gate ritorna None (flag OFF / budget consultazioni
            // esaurito / anti-meta-loop) ricade sul backstop close_runaway sotto
            // (comportamento bit-identico a 822e083 con flag OFF).
            if let Some(delta) = self
                .maybe_runaway_stall_delta(
                    state,
                    crate::decisions::meta_reason::AXIS_TOKEN_OVERFLOW,
                    tokens_used_total as i64,
                    iters_in,
                    &messages,
                    ctx,
                )
                .await
            {
                return Ok(delta);
            }
            // Non-convergenza (regola H, "niente di fisso"): PRIMA del backstop di
            // chiusura, prova l'ESCALATION AGENTICA a un modello piu' capace e sano
            // (selezione tier+telemetria, poi reset del budget per il promosso). Se
            // non ne esiste uno (catena esaurita / max escalation / tutti in cooldown)
            // -> backstop sotto. Chiude il cerchio: la non-convergenza fa SALIRE il
            // modello invece di chiudere secco.
            if let Some(delta) = self
                .maybe_escalate_nonconvergence(state, iters_in, SwitchReason::BudgetToken, ctx, false)
                .await
            {
                return Ok(delta);
            }
            let budget_text = format!(
                "Raggiunto il budget massimo di token del run ({} token, tetto {}). \
Interrompo per evitare un consumo incontrollato: riformula la richiesta in modo \
piu' specifico, oppure riprova con un modello piu' capace.",
                tokens_used_total, self.cfg.run_token_budget
            );
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                tokens_used = tokens_used_total,
                budget = self.cfg.run_token_budget,
                "BUDGET TOKEN cumulativo esaurito -> chiusura deterministica (backstop)"
            );
            return Ok(self.close_runaway(
                iters_in,
                budget_text,
                "budget_token_esaurito",
                json!({
                    "tokens_used_total": tokens_used_total,
                    "run_token_budget": self.cfg.run_token_budget,
                }),
            ));
        }

        // ── (4e) FAST-FAIL turni SOLO-TESTO consecutivi ───────────────────────
        // Rileva il modello che DESCRIVE senza AGIRE (pattern gemini che ignora
        // `force_tool_choice`): N turni consecutivi in cui la risposta NON conteneva
        // tool_use mentre il loop si aspettava azioni. Il contatore e' aggiornato a
        // fine turno dal segnale STRUTTURATO `LlmResponse.tool_calls` (regola M), non
        // dal testo. `max_consecutive_text_only_turns=0` = disabilitato -> bit-identico.
        let text_only_streak = state.consecutive_text_only_turns.unwrap_or(0).max(0) as u32;
        if self.cfg.max_consecutive_text_only_turns > 0
            && text_only_streak >= self.cfg.max_consecutive_text_only_turns
        {
            // Con il meta-reasoner ACCESO lo streak solo-testo e' un TRIGGER del
            // giudice (non piu' un veto fisso): instrada al nodo StallRecovery, che
            // tipicamente sceglie escalate_model o shift_strategy (il modello
            // descrive senza agire). None -> backstop close_runaway (bit-identico
            // 822e083 con flag OFF).
            if let Some(delta) = self
                .maybe_runaway_stall_delta(
                    state,
                    crate::decisions::meta_reason::AXIS_TEXT_ONLY,
                    text_only_streak as i64,
                    iters_in,
                    &messages,
                    ctx,
                )
                .await
            {
                return Ok(delta);
            }
            // 3.4 (difesa strutturale, dietro flag): prima del backstop close, prova a
            // CAMBIARE PROVIDER (un provider fermo non deve affossare il run se un altro
            // puo' procedere). `None` -> chiusura backstop sotto (bit-identico, flag OFF).
            if let Some(delta) = self
                .maybe_switch_provider_on_no_progress(state, iters_in, ctx, text_only_streak)
                .await
            {
                return Ok(delta);
            }
            // Turno di grazia figura (una-tantum): una figura del consiglio senza parere
            // non deve chiudere n/d al backstop text-only. Un turno mirato la spinge a
            // emettere advisory_verdict (parere reale). Copre il percorso in cui il
            // meta-reasoner NON e' intervenuto (budget stall esaurito) e si andrebbe
            // dritti a close_runaway: e' il caso reale fe4dc12c (functional_analyst
            // deepseek, it=60). `None` (non-figura / grazia gia' concessa) -> close sotto.
            if let Some(delta) = self.maybe_advisory_grace_delta(state, iters_in, ctx).await {
                return Ok(delta);
            }
            let stall_text = format!(
                "Il modello ha prodotto {} risposte consecutive di solo testo senza \
eseguire alcuna azione (tetto {}). Interrompo: il compito non sta avanzando. \
Riformula la richiesta, oppure riprova con un modello piu' capace di usare i tool.",
                text_only_streak, self.cfg.max_consecutive_text_only_turns
            );
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                text_only_streak,
                threshold = self.cfg.max_consecutive_text_only_turns,
                "TURNI SOLO-TESTO consecutivi oltre soglia -> chiusura deterministica (backstop)"
            );
            return Ok(self.close_runaway(
                iters_in,
                stall_text,
                "text_only_stallo",
                json!({
                    "consecutive_text_only_turns": text_only_streak,
                    "max_consecutive_text_only_turns": self.cfg.max_consecutive_text_only_turns,
                }),
            ));
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
            error_is_stale: g1_recent_error && iters_in >= self.cfg.g1_max_nudges.saturating_mul(2),
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
        // Override sizing STICKY del run (mig 0524): letti UNA volta, riusati dal gate
        // g1-loop e dal blocco di riduzione contesto piu' avanti nello stesso turno.
        // `None` (sizing OFF / nessuna postura applicata) -> gli helper `effective_*`
        // lasciano invariate le soglie fisse -> BIT-IDENTICO. Read-only, replay-safe
        // (lo stato e' checkpointato).
        let sizing_ov = Self::read_sizing_overrides(state);
        let g1_escal_now = state
            .extra
            .get("auto_escalations")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        // Soglia g1-loop resa ADATTIVA dal sizing (mig 0524, decisione #5): base *
        // moltiplicatore. Senza override il moltiplicatore e' 1.0 -> base invariata.
        // Valuta i fattori INSIEME col guard `!g1_recent_productive` piu' sotto: un
        // modello medio che PROGREDISCE ottiene piu' respiro (soglia alzata) invece di
        // essere escalato a una soglia geometrica fissa.
        let g1_loop_threshold = effective_g1_threshold(
            self.cfg
                .g1_max_nudges
                .saturating_mul(4)
                .saturating_mul(g1_escal_now + 1),
            sizing_ov.as_ref(),
        );
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
        if matches!(
            head_gate(false, false, 0, g1_cap_effective, false),
            HeadGate::G1Cap
        ) {
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
            let g1_escal = state
                .extra
                .get("auto_escalations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
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
                // Indice catena 0 (non g1_escal): la catena della porta e'
                // RELATIVA al corrente (chain_for filtra rank > corrente) e il
                // corrente AVANZA via sticky a ogni promozione; l'indice storico
                // (pensato per la catena assoluta per base_model del Python)
                // saltava sistematicamente un tier a ogni escalation successiva.
                // Il CAP resta su auto_escalations < 3 (qui sopra).
                pick_escalation_model(
                    &inputs.candidates,
                    g1_cur_provider.as_deref(),
                    g1_cur_model.as_deref(),
                    &inputs.policy,
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
                    "escalation",
                    format!(
                        "Passo a {}/{} (il modello descrive senza agire)",
                        pick.provider, pick.model
                    ),
                    stall_switch_payload(
                        &g1_cur_provider,
                        &g1_cur_model,
                        &pick.provider,
                        &pick.model,
                        SwitchReason::G1Cap,
                    ),
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
            if let Some(delta) = self
                .forced_declaration_delta(state, iters_in, ctx)
                .await
            {
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
                    thinking_signature: None,
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
        let mut progress_guided: HashSet<String> = state
            .progress_guided_axes
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut progress_diagnosed: HashSet<String> = state
            .progress_diagnosed_axes
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut progress_strategy: HashSet<String> = state
            .progress_strategy_axes
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect();
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
                        thinking_signature: None,
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
                // Indice catena 0: catena RELATIVA al corrente, che avanza via
                // sticky (vedi ramo G1); il cap resta su auto_escalations < 3.
                pick_escalation_model(
                    &inputs.candidates,
                    expl_cur_provider.as_deref(),
                    expl_cur_model.as_deref(),
                    &inputs.policy,
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
                        "escalation",
                        format!(
                            "Passo a {}/{} (esplorazione senza risultato)",
                            pick.provider, pick.model
                        ),
                        stall_switch_payload(
                            &expl_cur_provider,
                            &expl_cur_model,
                            &pick.provider,
                            &pick.model,
                            SwitchReason::Exploration,
                        ),
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
                            thinking_signature: None,
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
                    thinking_signature: None,
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
                .map(|h| {
                    h.failed && matches!(h.tool_name.as_str(), "run_service" | "service_restart")
                })
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
            let matched = ra_label
                .as_ref()
                .map(|_| ra_count >= ra_threshold)
                .unwrap_or(false);
            if !matched {
                progress_guided.remove("repeated_action");
                progress_diagnosed.remove("repeated_action");
                progress_strategy.remove("repeated_action");
            } else if let Some(label) = ra_label {
                // Candidato escalation (stesso pattern di esplorazione/G1 cap):
                // prima di abortire su azione ripetuta, promuovi a un modello piu'
                // capace invece di arrenderti.
                let ra_escal = state
                    .extra
                    .get("auto_escalations")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                // Coppia corrente = risoluzione del turno (punto unico, regola L).
                let (ra_cur_provider, ra_cur_model) = self.escalation_current_pair(state);
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
                    // Indice catena 0: catena RELATIVA al corrente, che avanza
                    // via sticky (vedi ramo G1); cap su auto_escalations < 3.
                    pick_escalation_model(
                        &inputs.candidates,
                        ra_cur_provider.as_deref(),
                        ra_cur_model.as_deref(),
                        &inputs.policy,
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
                            if let Some(delta) = self
                                .forced_declaration_delta(state, iters_in, ctx)
                                .await
                            {
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
                                thinking_signature: None,
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
                            "escalation",
                            format!(
                                "Passo a {}/{} (stallo su '{label}')",
                                pick.provider, pick.model
                            ),
                            stall_switch_payload(
                                &ra_cur_provider,
                                &ra_cur_model,
                                &pick.provider,
                                &pick.model,
                                SwitchReason::RepeatedAction,
                            ),
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
                        extra_out
                            .insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
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
                let rp_escal = state
                    .extra
                    .get("auto_escalations")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
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
                        &inputs.candidates,
                        rp_cur_provider.as_deref(),
                        rp_cur_model.as_deref(),
                        &inputs.policy,
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
                                thinking_signature: None,
                            }]),
                            result: Some(Some(rp_text)),
                            pending_tool_uses: Some(Some(vec![])),
                            stop_reason: Some(Some(stop_reason_from_str(
                                dec.stop_reason.as_deref(),
                            ))),
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
                        extra_out
                            .insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
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
        if forced_text_turn_active(
            iters_in,
            forced_text_threshold,
            state.stop_reason,
            final_gate_correction_active,
            !tools_json.is_empty(),
        ) {
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
                .filter(|t| t.get("name").and_then(Value::as_str) == Some(TASK_COMPLETE_TOOL_NAME))
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

        // ── Turno DICHIARATIVO DI RUOLO: catalogo = solo il tool del canale ──────
        // Gemello del blocco sopra per i canali di ruolo (advisory_verdict /
        // debate_position). Richiesto dal turno di grazia: la sola direttiva in
        // prosa aveva efficacia misurata 1/5 — lo stesso tipo di segnale che il
        // modello muto sta gia' ignorando. Con il catalogo ridotto a UN tool,
        // tool_choice=required equivale a forzare QUEL tool su ogni dialetto:
        // l'obbligo del quorum diventa un vincolo di macchina.
        let declaring_role_turn = state
            .extra
            .get("force_role_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if declaring_role_turn {
            if let Some(chan) = pending_role_channel(state) {
                let only: Vec<Value> = state
                    .tools_json
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| t.get("name").and_then(Value::as_str) == Some(chan.tool))
                    .collect();
                if !only.is_empty() {
                    tools_json = only;
                    force_action_hard = true;
                    tracing::warn!(
                        target: "nexus_agent_graph::executor",
                        tool = chan.tool,
                        "turno dichiarativo di RUOLO: catalogo ridotto al canale + tool choice forzata"
                    );
                }
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
            correlation_id: None,
        });
        let _ = self.meta_steps.persist_meta_step(calling_meta).await;
        // Heartbeat best-effort (anti-recovery prematuro).
        let _ = self.run_control.heartbeat(&run_id).await;

        // ── CONTEXT REDUCTION (parte PURA, punti unici PR-D) ──────────────────
        // I/O (continuity-trim / system-offload) NON portati: TODO trait dedicati.
        // Il ROLLING-SUMMARY (riassume i vecchi via LLM economico) e' agganciato al
        // cambio-fase qui sotto via la porta [`SummaryStore`] (best-effort).
        let mut hist: Vec<HistoryMessage> = messages.iter().map(message_to_history).collect();
        let compress_iter = iters_in;

        // Config di DIMENSIONAMENTO EFFETTIVA (mig 0524): base DB-driven + eventuali
        // override sizing STICKY (`sizing_ov`). Con override assenti -> clone della
        // base INVARIATA (BIT-IDENTICO). Rende ADATTIVE (non piu' soglie fisse
        // geometriche) la compressione (fasi/keep_recent/max_chars + compress_start),
        // il freno token e il rolling-summary (punto unico del merge = scale_reason).
        let eff_ctx_mgmt = effective_ctx_mgmt(&self.cfg.ctx_mgmt, sizing_ov.as_ref());
        let (eff_rolling_enabled, eff_rolling_keep) = effective_rolling(
            self.cfg.rolling_summary_enabled,
            self.cfg.rolling_keep_recent,
            sizing_ov.as_ref(),
        );
        let eff_token_brake = effective_token_brake(&self.cfg.token_brake, sizing_ov.as_ref());

        // Compressione a generazioni (cutoff fisso, py:2764-2810).
        let boundaries = &eff_ctx_mgmt.compress_phase_boundaries;
        let phase_now = boundaries.iter().filter(|b| compress_iter >= **b).count() as i64;
        let prev_phase = state.compress_cutoff_phase.unwrap_or(0);
        let mut cutoff_idx = state.compress_cutoff_index.unwrap_or(0);
        let (do_compress, params): (bool, CompressParams) =
            ctxr::should_compress_now(compress_iter, &eff_ctx_mgmt);
        let mut gen_cutoff_index: Option<i64> = None;
        let mut gen_cutoff_phase: Option<i64> = None;
        if phase_now > prev_phase {
            // CAMBIO FASE: dedup + drop base64.
            hist = ctxr::dedup_tool_results_history(&hist);
            hist = ctxr::drop_unused_base64_payloads(&hist, ctxr_drop_age(), 2);

            // ROLLING-SUMMARY (intervento 3): RIASSUME il prefisso vecchio invece di
            // limitarsi a comprimere/troncare. DECISIONE pura (punto unico, regola L):
            // cutoff -> serialize -> SummaryStore.summarize (I/O) -> apply.
            // BEST-EFFORT: su guasto (LLM down, cooldown) la history
            // resta INVARIATA e si prosegue (compress/token_brake fanno il resto).
            if eff_rolling_enabled {
                if let Some(cut) = ctxr::select_rolling_summary_cutoff(&hist, eff_rolling_keep) {
                    // GOVERNANCE costo/beneficio (opt-in, decisione PURA regola L):
                    // salta il rolling-summary se il prefisso da riassumere e' troppo
                    // piccolo per giustificare il costo della chiamata LLM. Flag OFF
                    // (default) -> comportamento storico bit-identico.
                    let governance_skip = self.cfg.governance_rolling_summary_adaptive
                        && !crate::decisions::governance::rolling_summary_worthwhile(
                            cut as i64,
                            self.cfg.governance_rolling_summary_min_prefix,
                        );
                    if governance_skip {
                        tracing::debug!(
                            target: "nexus_agent_graph::executor",
                            run_id = %run_id,
                            cutoff = cut,
                            min_prefix = self.cfg.governance_rolling_summary_min_prefix,
                            "rolling summary: saltato per governance costo/beneficio (prefisso sotto soglia)"
                        );
                    } else {
                        let prefix_text = ctxr::serialize_prefix_for_summary(&hist, cut);
                        match self
                            .summary_store
                            .summarize(prefix_text.clone())
                            .await
                        {
                            Ok(summary) if !summary.trim().is_empty() => {
                                // OFFLOAD retrievable degli ORIGINALI (chat_history) PRIMA di
                                // sostituirli col riassunto: restano recuperabili per sessione
                                // via search_semantic. Best-effort, gata dal flag + porta.
                                if self.cfg.rolling_summary_offload_enabled {
                                    if let Some(offload) = self.offload.as_ref() {
                                        match offload
                                            .offload_to_rag(
                                                serde_json::Value::String(prefix_text.clone()),
                                                OffloadKind::ChatHistory,
                                                if run_id.is_empty() {
                                                    None
                                                } else {
                                                    Some(run_id.clone())
                                                },
                                                None,
                                            )
                                            .await
                                        {
                                            Ok(ptr) => tracing::info!(
                                                target: "nexus_agent_graph::executor",
                                                run_id = %run_id,
                                                pointer = %ptr,
                                                "rolling summary: originali indicizzati su RAG (recuperabili)"
                                            ),
                                            Err(e) => tracing::warn!(
                                                target: "nexus_agent_graph::executor",
                                                run_id = %run_id,
                                                error = %e,
                                                "rolling summary offload non disponibile (non blocca)"
                                            ),
                                        }
                                    }
                                }
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
                                // HEARTBEAT: il rolling summary e' lavoro LUNGO (una
                                // chiamata LLM + l'offload su RAG con embedding) e
                                // sta FRA due battiti (il precedente e' a :2916,
                                // prima del context reduction; il prossimo e' alla
                                // prossima iterazione). Un run che comprime il
                                // contesto sta lavorando, e deve poterlo dimostrare
                                // al reaper invece di sembrare fermo.
                                let _ = self.run_control.heartbeat(&run_id).await;
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
                                // Guasto LLM: degrado best-effort.
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
            }

            // CONTINUITY-TRIM SEMANTICO: scarta dal prefisso vecchio gli atomi (turno
            // assistant + i suoi tool_result) semanticamente IRRILEVANTI al FOCUS del
            // turno, invece del solo troncamento posizionale. DECISIONE pura (regola L:
            // select/decide/apply in context_reduction); EMBEDDING via porta.
            // BEST-EFFORT: su guasto embedder la history resta invariata.
            if self.cfg.continuity_trim_enabled {
                if let Some(embedder) = self.embedding_store.as_ref() {
                    let candidates = ctxr::select_continuity_trim_candidates(
                        &hist,
                        self.cfg.rolling_keep_recent,
                    );
                    let focus_text = ctxr::continuity_focus_text(&hist);
                    if !candidates.is_empty() && !focus_text.trim().is_empty() {
                        // Un solo embed: [focus] ++ testo dei candidati (ordine preservato).
                        let mut texts: Vec<String> = Vec::with_capacity(candidates.len() + 1);
                        texts.push(focus_text);
                        texts.extend(candidates.iter().map(|c| c.text.clone()));
                        match embedder.embed(texts).await {
                            Ok(vecs) if vecs.len() == candidates.len() + 1 => {
                                let focus_vec = &vecs[0];
                                let cand_vecs = &vecs[1..];
                                let drops = ctxr::decide_continuity_drops(
                                    focus_vec,
                                    cand_vecs,
                                    &candidates,
                                    self.cfg.continuity_trim_min_score,
                                    self.cfg.continuity_trim_max_drop.max(0) as usize,
                                );
                                if !drops.is_empty() {
                                    let before = hist.len();
                                    hist = ctxr::apply_continuity_trim(&hist, &drops);
                                    tracing::info!(
                                        target: "nexus_agent_graph::executor",
                                        run_id = %run_id,
                                        phase = phase_now,
                                        dropped = drops.len(),
                                        msgs_before = before,
                                        msgs_after = hist.len(),
                                        "continuity trim: atomi irrilevanti scartati (compressione semantica)"
                                    );
                                }
                            }
                            Ok(_) => tracing::warn!(
                                target: "nexus_agent_graph::executor",
                                run_id = %run_id,
                                "continuity trim: embed disallineato, degrado a history invariata"
                            ),
                            Err(e) => tracing::warn!(
                                target: "nexus_agent_graph::executor",
                                run_id = %run_id,
                                error = %e,
                                "continuity trim: embedder non disponibile, degrado a history invariata"
                            ),
                        }
                    }
                }
            }

            cutoff_idx = std::cmp::max(0, hist.len() as i64 - params.keep_recent);
            gen_cutoff_index = Some(cutoff_idx);
            gen_cutoff_phase = Some(phase_now);
        }
        if do_compress && cutoff_idx > 0 {
            // COMPRESS-OFFLOAD: se abilitato, offloada su RAG i tool_result che
            // verranno compressi PRIMA di comprimerli, cosi' il marker porta un `ref`
            // recuperabile invece del solo "[... compresso ...]". SELEZIONE pura
            // (regola L: contents_eligible_for_offload), I/O gata da flag + porta.
            // Su guasto la mappa resta vuota -> degraded_marker (bit-identico a oggi).
            let max_chars = params.max_content_chars.max(0) as usize;
            let offload_map = self
                .build_compress_offload_map(&hist, cutoff_idx as usize, max_chars, &run_id)
                .await;
            let marker_fn = |content: &str| -> String {
                match offload_map.get(content) {
                    Some(ptr) => format!(
                        "\n[... compresso: {} char originali, recuperabili via \
                         nexus_search_semantic ref={ptr} ...]",
                        content.chars().count()
                    ),
                    None => ctxr::degraded_marker(content),
                }
            };
            hist = ctxr::compress_old_tool_results(
                &hist,
                0,
                max_chars,
                Some(cutoff_idx as usize),
                &marker_fn,
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
            let required =
                upscale_required_tokens(upscale_est_tokens, self.cfg.upscale_overhead_ratio);
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
                &eff_token_brake,
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
                // Niente di fisso (regola H): il cap di CONTESTO non fa piu' fail-fast
                // secco. E' un TRIGGER del giudice tier-agentico (nodo StallRecovery):
                // col contesto oltre finestra anche dopo brake, il giudice tipicamente
                // sceglie EscalateModel (finestra piu' grande) o DeclareBlocked. None
                // (giudice OFF / budget esaurito / anti-meta-loop) -> backstop sotto
                // (fail-fast strutturato context_overflow, bit-identico al pre-fix).
                if let Some(delta) = self
                    .maybe_runaway_stall_delta(
                        state,
                        crate::decisions::meta_reason::AXIS_CONTEXT_CAP,
                        post_brake_est,
                        iters_in,
                        &messages,
                        ctx,
                    )
                    .await
                {
                    return Ok(delta);
                }
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
                    correlation_id: None,
                });
                let _ = self.meta_steps.persist_meta_step(overflow_meta).await;
                // `extra` nel delta e' overwrite: merge con lo stato per non
                // perdere le chiavi esistenti.
                let mut extra = state.extra.clone();
                extra.insert("error_class".to_string(), json!("context_overflow"));
                return Ok(StateDelta {
                    messages: Some(vec![Message::Ai {
                        content: MessageContent::text(text.clone()),
                        tool_calls: vec![],
                        reasoning: None,
                        thinking_signature: None,
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
        let in_discovery =
            !names_tc.is_empty() && names_tc.iter().all(|n| n == "nexus_mcp_tool_search");
        let supports_forcing =
            provider_style_supports_forcing(self.cfg.tool_choice_style.as_deref());
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

        // ── DETECTOR-EMISSIONE scale-controller (PR-B3, FIX-A: PRE-LLM) ──────
        // GEMELLO del detector stallo (`maybe_stall_reason_delta`, call site
        // 1382/1707/2000): valutato PRIMA della chiamata LLM del turno (non a fine
        // turno). Cosi':
        //   - IDEMPOTENTE AL RESUME: il superstep di emissione NON chiama `complete`,
        //     quindi un resume da checkpoint non ripete una chiamata LLM del turno.
        //   - NIENTE TURNO SCARTATO (F4/F6): se la scala cambia il tier, il rientro
        //     applica sticky+current_tier e si prosegue il turno col modello GIUSTO
        //     (la `complete` sotto usa il nuovo modello). Su KeepTier / cambio
        //     annullato il rientro ritorna None e si prosegue con UNA sola
        //     `complete` (nessuna chiamata LLM produttiva scartata e rifatta).
        // A flag `agent.scale.enabled=false` (default) `maybe_scale_reason_delta`
        // ritorna SUBITO None (guard primario, zero overhead): bit-identico. Con
        // flag ON, valuta solo su un run agentico che PROSEGUE (`requires_tool_use`
        // resta true nel contesto). La precedenza stallo (FIX-E) usa il solo
        // segnale disponibile pre-LLM: `detect_recent_tool_error` (l'ultimo
        // tool_result e' errore -> asse stallo attivo, niente scale questo turno).
        if self.cfg.scale.enabled {
            let scale_escalations = state
                .extra
                .get("auto_escalations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let scale_stall_active = detect_recent_tool_error(&messages, 4);
            let scale_est_tokens = self.estimate_history_tokens(&hist);
            if let Some(delta) = self.maybe_scale_reason_delta(
                state,
                iters_in,
                &messages,
                scale_escalations,
                scale_est_tokens,
                effective_window,
                scale_stall_active,
            ) {
                return Ok(delta);
            }
        }

        // ── LLM CALL (py:2974-3107) ───────────────────────────────────────────
        // max_tokens = max(8192, min(budget*4, 16384)) == clamp(8192, 16384).
        let max_tokens = (state.token_budget.unwrap_or(400) * 4).clamp(8192, 16384);
        let llm_messages = history_to_llm_messages(&hist);
        let req = LlmRequest {
            provider: provider.clone(),
            model: model.clone(),
            messages: llm_messages.clone(),
            tools: if tools_json.is_empty() {
                None
            } else {
                Some(tools_json.clone())
            },
            force_tool_choice: force_tc,
            system_text: Some(system_text.clone()),
            max_tokens: Some(max_tokens),
            response_format: None,
            thinking: None,
            run_id: if run_id.is_empty() {
                None
            } else {
                Some(run_id.clone())
            },
            iteration: Some(iters_in),
            intent: state.user_intent.clone(),
            // Nodo chiamante = executor. Il gateway concreto (GatewayLlmAdapter)
            // lo IGNORA quando il modello e' gia' risolto (regola L).
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
        // Streak dei fallimenti gateway deterministici (sotto soglia): va
        // persistito nel delta finale; su turno riuscito viene rimosso.
        let mut det_streak_val: Option<Value> = None;
        // Cancellazione COOPERATIVA (regola L, punto unico complete_or_cancel):
        // uno Stop arrivato DURANTE questa chiamata la interrompe subito e chiude
        // il run 'superseded', senza attendere che rientri (fino a 90-150s sotto
        // carico). Il gate di testa la vede solo a inizio iterazione: qui colmiamo
        // la finestra in cui la chiamata e' in volo.
        let complete_result = match complete_or_cancel(
            ctx.llm.complete(req),
            self.run_control.as_ref(),
            &run_id,
            CANCEL_POLL_INTERVAL,
        )
        .await
        {
            None => {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    thread = %run_id,
                    "Stop rilevato durante la chiamata al modello: interruzione cooperativa"
                );
                return Ok(superseded_delta());
            }
            Some(r) => r,
        };
        let mut resp = match complete_result {
            Ok(r) => r,
            Err(err) => {
                // FAILOVER cross-provider sul provider caduto/indisponibile (regola H
                // + regola L): se il gateway ha segnalato in modo STRUTTURATO che il
                // provider scelto NON e' disponibile ([`PortError::ProviderUnavailable`],
                // = 500 `PROVIDER_ERROR`: tutti i provider risolti per QUESTA richiesta
                // in cooldown), NON chiudere il run con `StopReason::Error`: RIPIEGA su
                // un SOSTITUTO scelto AGENTICAMENTE dalla porta
                // ([`EscalationPort::failover_provider`] -> `pick_failover_model`):
                // TUTTI i candidati agentici sani (ogni tier, niente pavimento ne'
                // catena), ordinati salute -> likelihood da telemetria, col tier del
                // modello caduto come INDICAZIONE (mai un filtro). Se c'e'
                // un provider sano, promuoviamo lo sticky e usciamo con `G1Escalated`:
                // il self-loop rientra nell'executor col provider nuovo (stesso pattern
                // del ramo G1). I provider gia' provati sono accumulati in
                // `failover_tried` cosi' la cascata ne sceglie sempre uno diverso. Solo
                // quando NESSUN provider sano resta cadiamo nella chiusura `Error`
                // (onesta). Gated `auto_escalations < 3` (no escalation a raffica).
                if let crate::runtime::ports::PortError::ProviderUnavailable(pu) = &err {
                    // Bug 2: un ClientError PROVIDER-SPECIFICO recuperabile (code
                    // strutturato in whitelist DB-driven, es. Google invalid_argument/
                    // thought_signature) PUO' fare failover cross-provider; ogni altro
                    // ClientError (code assente o history condivisa Mistral) resta
                    // chiusura onesta (f0ad0337). Punto unico: allows_cross_provider_failover.
                    if !pu.allows_cross_provider_failover(&self.cfg.recoverable_client_error_codes) {
                        tracing::warn!(
                            target: "nexus_agent_graph::executor",
                            provider = %provider,
                            model = %model,
                            code = pu.code.as_deref().unwrap_or("none"),
                            "provider client_error non recuperabile: niente failover cross-provider, chiusura onesta"
                        );
                    } else {
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
                        // Punto unico (regola L): selezione AGENTICA del SOSTITUTO
                        // (tutti i tier, telemetria strutturata, esclusi i gia' provati
                        // e i cooldown). Il tier del modello caduto viaggia come
                        // INDICAZIONE, non come filtro -> la rete scala IN-RUN, senza
                        // che l'utente debba ri-lanciare. Fail-open: errore -> None ->
                        // chiusura Error.
                        if let Ok(Some(pick)) = self
                            .escalation
                            .failover_provider(
                                Some(&provider),
                                Some(&model),
                                state.current_tier.as_deref(),
                                // Causa tipizzata (regola M): l'impl filtra la finestra
                                // SOLO per ContextTooLong; per EmptyCompletion & co. non
                                // deve escludere sostituti a finestra minore.
                                pu.cause,
                                &tried,
                            )
                            .await
                        {
                            tracing::warn!(
                                target: "nexus_agent_graph::executor",
                                from_provider = %provider,
                                to_provider = %pick.provider,
                                to_model = %pick.model,
                                cause = pu.cause.as_str(),
                                tried = tried.len(),
                                "provider caduto -> FAILOVER cross-provider via routing (cascata)"
                            );
                            // HEARTBEAT: un cambio di provider e' la PROVA che il
                            // run sta lavorando — non e' appeso, sta girando la
                            // cascata. Senza questo battito la liveness si misura
                            // una volta per ITERAZIONE (executor.rs:2916, prima del
                            // context reduction), e una prima iterazione con piu'
                            // failover puo' superare la soglia del reaper (900s,
                            // mig 0392) mentre lavora: il run verrebbe ucciso
                            // proprio perche' si sta sforzando di sopravvivere.
                            // MISURATO (16/07): un run reale ha impiegato 1145s con
                            // 5 provider su 6 in errore — 245s oltre la soglia. E'
                            // sopravvissuto solo perche' le iterazioni successive
                            // battevano. La mig 0392 dichiara il limite: "il battito
                            // si ferma durante un tool sincrono lungo".
                            let _ = self.run_control.heartbeat(&run_id).await;
                            // Motivo ONESTO dello switch (regola M): la causa
                            // tipizzata arriva dal body strutturato del gateway
                            // (details.primary_cause / POLICY_TIER_EXCLUDED), mai
                            // dal testo. PRIMA era hardcoded "cooldown" per tutto:
                            // un 4xx del provider o un'esclusione di policy
                            // venivano raccontati come instabilita' del provider
                            // (incidente run 48793fde, 2026-07-06).
                            use crate::runtime::ports::ProviderFailureCause as Cause;
                            let title = match pu.cause {
                                Cause::Billing => format!(
                                    "Credito esaurito su {provider}: passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                                Cause::ClientError => format!(
                                    "{provider} ha rifiutato la richiesta: passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                                Cause::PolicyTierExcluded => format!(
                                    "Contenuto riservato: {provider} escluso dalla policy, \
passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                                Cause::EmptyCompletion => format!(
                                    "{provider} non ha prodotto output: passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                                Cause::ContextTooLong => format!(
                                    "Richiesta troppo grande per {provider}: passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                                Cause::Cooldown | Cause::Unknown => format!(
                                    "Provider {provider} non disponibile: passo a {}/{}",
                                    pick.provider, pick.model
                                ),
                            };
                            self.emit_phase(
                                ctx,
                                "escalation",
                                title,
switch_payload(
                                    &provider,
                                    &model,
                                    &pick.provider,
                                    &pick.model,
                                    SwitchReason::ProviderFailover,
                                    Some(pu.cause.is_cooldown_like()),
                                    Some(pu.cause.as_str()),
                                ),
                            )
                            .await;
                            let esc_nudge = human_msg(match pu.cause {
                                Cause::ClientError => {
                                    "Il provider precedente ha rifiutato la richiesta \
(errore lato provider sulla richiesta, non un cooldown). Riprendi tu, sul nuovo provider: \
esegui il prossimo step concreto del compito."
                                }
                                Cause::PolicyTierExcluded => {
                                    "Il provider precedente e' stato escluso dalla policy per \
contenuto riservato (sensitivity tier). Riprendi tu, sul provider ammesso: esegui il \
prossimo step concreto del compito."
                                }
                                Cause::Billing => {
                                    "Il provider precedente ha il credito esaurito. Riprendi \
tu, su un provider sano: esegui il prossimo step concreto del compito."
                                }
                                Cause::EmptyCompletion => {
                                    "Il provider precedente ha risposto senza produrre output \
(nessun testo ne' azione: budget consumato nel ragionamento). Riprendi tu, sul nuovo provider: \
esegui il prossimo step concreto del compito."
                                }
                                Cause::ContextTooLong => {
                                    "Il provider precedente ha rifiutato la richiesta perche' \
troppo grande per la sua finestra/limite (non un cooldown). Riprendi tu, sul nuovo provider a \
finestra piu' ampia: esegui il prossimo step concreto del compito."
                                }
                                Cause::Cooldown | Cause::Unknown => {
                                    "Il provider precedente non e' disponibile (in cooldown). \
Riprendi tu, su un provider sano: esegui il prossimo step concreto del compito."
                                }
                            });
                            // Marca il provider scelto come provato: se cadesse anche
                            // lui, il giro dopo lo esclude e ne sceglie un altro sano.
                            tried.push(pick.provider.clone());
                            let mut extra_out = state.extra.clone();
                            extra_out.insert("auto_escalations".to_string(), json!(cd_escal + 1));
                            extra_out.insert("failover_tried".to_string(), json!(tried));
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
                                // Fresh start su provider sano: azzera anche lo streak
                                // solo-testo (il failover NON e' un turno descrittivo
                                // del modello; il tetto non deve scattare per un
                                // cooldown di provider). tokens_used_total NON cambia:
                                // la chiamata e' FALLITA (0 token effettivi).
                                consecutive_text_only_turns: Some(Some(0)),
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
                }
                // TETTO sui fallimenti DETERMINISTICI (punto unico
                // deterministic_streak_gate): oltre la soglia si chiude con
                // esito onesto invece di consumare il budget in retry invisibili.
                match deterministic_streak_gate(state, &self.cfg, &err, &provider, &model, iters_in)
                {
                    DetGate::Close(delta) => return Ok(delta),
                    DetGate::Under(v) => det_streak_val = Some(v),
                    DetGate::NonDeterministico => {}
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
                //
                // Il testo per l'utente viene dal punto unico di presentazione
                // (`nexus_types::error_presentation`), non da un taglio di
                // caratteri. Qui viveva `compact_provider_error`, che tagliava
                // alla prima graffa: funzionava sui body JSON e non vedeva NULLA
                // degli errori di trasporto, che graffe non ne hanno — ed e'
                // esattamente il caso che arrivava in chat come
                // "error sending request for url (...) <- io(ConnectionRefused,
                // os_error=10061)". Il dettaglio tecnico resta nel `tracing::error!`
                // qui sopra.
                let err_short = port_error_message(&err);
                let err_text = match crate::routing::signals::summarize_actions_in_history(&messages) {
                    Some(w) => format!(
                        "[Errore provider {provider}: {err_short}]\n\nInterrotto dopo {iters_in} iterazioni. Lavoro svolto finora: {w}."
                    ),
                    None => format!("[Errore provider {provider}: {err_short}]"),
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
                tools: if tools_json.is_empty() {
                    None
                } else {
                    Some(tools_json.clone())
                },
                force_tool_choice: Some(false),
                system_text: Some(system_text.clone()),
                max_tokens: Some(max_tokens),
                response_format: None,
                thinking: None,
                run_id: if run_id.is_empty() {
                    None
                } else {
                    Some(run_id.clone())
                },
                iteration: Some(iters_in),
                intent: state.user_intent.clone(),
                // Retry-senza-forcing dello stesso turno executor: stesso purpose.
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
                    if c.is_empty() {
                        None
                    } else {
                        Some(c.to_string())
                    }
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
        let mut escalations = state
            .extra
            .get("auto_escalations")
            .and_then(Value::as_i64)
            .unwrap_or(0);
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
                    .escalation_inputs(
                        state.user_intent.as_deref(),
                        Some(&provider),
                        Some(&model),
                    )
                    .await
                    .unwrap_or_default();
                // Indice catena 0: catena RELATIVA al corrente (vedi ramo G1);
                // il cap resta su escalations < 3 (qui sopra).
                if let Some(pick) = pick_escalation_model(
                    &inputs.candidates,
                    Some(&provider),
                    Some(&model),
                    &inputs.policy,
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
                        "escalation",
                        format!(
                            "Passo a {}/{} (tool call ripetuta identica)",
                            pick.provider, pick.model
                        ),
                        switch_payload(
                            &provider,
                            &model,
                            &pick.provider,
                            &pick.model,
                            SwitchReason::SignatureLoop,
                            None,
                            None,
                        ),
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
                        response_format: None,
                        thinking: None,
                        run_id: if run_id.is_empty() {
                            None
                        } else {
                            Some(run_id.clone())
                        },
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
                if let Some(delta) = self
                    .forced_declaration_delta(state, iters_in, ctx)
                    .await
                {
                    return Ok(delta);
                }
                // Chiusura secca loop_detected (py:3269-3281). Messaggio ONESTO:
                // niente suggerimenti hardcoded di modelli (regola G) — il loop a
                // vuoto e' uno stallo del RUN, non un verdetto sul modello.
                self.emit_phase(
                    ctx,
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
                    thinking_signature: None,
                };
                pending_tool_uses = vec![];
                stop_reason_str_resp = Some("loop_detected".to_string());
                loop_close_result = Some(loop_msg);
                new_signatures = vec![]; // reset accumulator (py:3281)
            }
        }

        // ── Anti repetition-collapse del TESTO (regola M) ─────────────────────
        // Complementare al signature-loop (che guarda le tool call): un turno con
        // UNA sola tool call ma un muro di testo ripetuto (stessa sottostringa
        // N+ volte) NON triggera il signature-loop eppure e' spazzatura degenere
        // (collasso dei modelli piccoli: codestral "Command failed" x898, run
        // de7477e9). Segnale STRUTTURALE (periodicita' della coda, punto unico
        // [`detect_repetition_collapse`]), mai semantica del testo.
        //
        // Chiusura come il signature-loop (regola L): impostiamo le variabili di
        // chiusura (assistant_msg col recap ONESTO, tool azzerati, loop_close_result)
        // e LASCIAMO proseguire il flusso normale. NON un early return: quello
        // bypasserebbe la coda del turno (contabilita' token da resp.usage,
        // emissione SseEvent::Usage, propagazione auto_escalations, set_effective_model)
        // -> total_tokens=0 e conteggio escalation perso (review adversariale).
        // Il testo degenere viene SCARTATO (sostituito dal recap); i TOKEN del turno
        // restano contati (sono stati consumati davvero). loop_close_result rende il
        // delta finale forced_close_unverified -> esito FailedDiagnosed, mai 'completed'.
        // Salta se il signature-loop ha gia' deciso la chiusura del turno.
        if loop_close_result.is_none() {
            if let Some(hit) = detect_repetition_collapse(&result_text, self.cfg.repetition) {
                let preview = hit.unit_preview(80);
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    provider = %provider,
                    model = %model,
                    repeats = hit.repeats,
                    span_len = hit.span_len,
                    "repetition-collapse rilevato nel testo del turno -> chiusura non-verificata"
                );
                self.emit_phase(
                    ctx,
                    "repetition_collapse",
                    format!("Risposta degenere di {provider}/{model}: interrompo (testo ripetuto)"),
                    json!({
                        "reason": "repetition_collapse",
                        "repeats": hit.repeats,
                        "span_len": hit.span_len,
                    }),
                )
                .await;
                let close_text = format!(
                    "[RISPOSTA DEGENERE] Il modello {provider}/{model} ha prodotto una \
risposta ripetitiva non valida: la sequenza \"{preview}\" e' stata ripetuta \
{}+ volte senza contenuto utile. Nessuna verifica e' stata eseguita e il \
compito NON e' stato completato. Riformula la richiesta oppure riprova con un \
modello piu' capace.",
                    hit.repeats
                );
                assistant_msg = Message::Ai {
                    content: MessageContent::text(close_text.clone()),
                    tool_calls: vec![],
                    reasoning: None,
                    thinking_signature: None,
                };
                pending_tool_uses = vec![];
                // Riusa il vocabolario stop_reason del forced-close anti-loop
                // (riconosciuto da stop_reason_from_str -> canonical "loop"); il
                // meta_step "repetition_collapse" sopra distingue nella timeline.
                stop_reason_str_resp = Some("loop_detected".to_string());
                loop_close_result = Some(close_text);
                new_signatures = vec![];
            }
        }

        let stop_reason_final = stop_reason_str_resp.as_deref();

        // Provider/model EFFETTIVI (cascade interno del gateway), calcolati DOPO l'
        // eventuale escalation cosi' il confronto e' col NUOVO modello promosso
        // (py:3457+). set_effective_model best-effort -> modello reale UI.
        let eff_provider = resp
            .provider_used
            .clone()
            .unwrap_or_else(|| provider.clone());
        let eff_model = resp.model_used.clone().unwrap_or_else(|| model.clone());
        let cascade_did_fallback = eff_provider != provider || eff_model != model;
        if cascade_did_fallback {
            let _ = self
                .run_control
                .set_effective_model(&run_id, &eff_provider, &eff_model)
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
        // NOTA (FIX-A, PR-B3): il detector-emissione scale-controller e' stato
        // spostato PRE-LLM (prima della `complete`, vedi call site sopra la LLM
        // CALL). Emetterlo qui a fine turno (post-LLM) disallineava il cursore
        // replay e scartava un turno LLM produttivo su KeepTier: entrambi chiusi
        // spostandolo prima della chiamata, come il gemello stallo.

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
        let empty_forced_reply = (forced_text_turn || declaring_turn || declaring_role_turn)
            && matches!(stop_reason_enum, StopReason::EndTurn | StopReason::Stop)
            && pending_tool_uses.is_empty()
            && final_result.trim().is_empty();
        if empty_forced_reply {
            if let Some(d) = self
                .forced_declaration_delta(state, iters_in, ctx)
                .await
            {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    forced_text = forced_text_turn,
                    "risposta VUOTA al turno forzato -> retry col turno dichiarativo (ADR 0034)"
                );
                return Ok(d);
            }
        }

        // ── CHIUSURA VOLONTARIA con canale di ruolo MUTO ──────────────────────
        // Terzo call site del turno di grazia (gli altri due: budget a tempo,
        // backstop text-only). Il caso reale che mancava: la figura fa il lavoro,
        // scrive la diagnosi IN PROSA e chiude con end_turn senza mai chiamare il
        // proprio tool — 14 dei 24 run muti storici, incluso il run 10:03 del
        // 20/07 (qwen3: analisi corretta, parere mai dichiarato, quorum saltato).
        // La prosa NON va persa: e' il resoconto del modello, la grazia le si
        // accoda (`preserving`). One-shot come gli altri call site: al secondo
        // end_turn muto si chiude come oggi, nessun loop.
        if matches!(stop_reason_enum, StopReason::EndTurn | StopReason::Stop)
            && pending_tool_uses.is_empty()
        {
            if let Some(d) = self
                .maybe_advisory_grace_delta_preserving(
                    state,
                    Some(assistant_msg.clone()),
                    iters_in,
                    ctx,
                )
                .await
            {
                tracing::warn!(
                    target: "nexus_agent_graph::executor",
                    "chiusura volontaria con canale di ruolo muto -> turno di grazia FORZANTE"
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
                    thinking_signature: None,
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
                    correlation_id: None,
                });
                let _ = self.meta_steps.persist_meta_step(meta).await;
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
                    thinking_signature: None,
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
        // si aspetta dal canale chat. Best-effort, infallibile.
        //  - ToolUse: un evento per ogni blocco tool_use deciso (mappa il `tool_use`
        //    del brain, che emette uno step Running per ogni tool richiesto).
        //  - EndTurn: turno concluso senza tool pendenti (il modello ha terminato
        //    la generazione). Il terminatore `Done` (is_final) NON e' dell'executor:
        //    lo emette il finalizzatore del run quando il grafo raggiunge End
        //    (l'executor puo' essere riattraversato in turni successivi), 1:1 con
        //    `run_via_brain` che mette `is_final=true` solo a fine retry loop.
        for tu in &pending_tool_uses {
            let id = tu
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = tu
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
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

        // ── Contatori anti-runaway CUMULATIVI (reducer overwrite last-write) ──
        // (1) tokens_used_total: LAVORO INCREMENTALE del run, dal segnale
        //     STRUTTURATO dell'usage (regola M): delta del prompt rispetto al
        //     turno precedente (solo il contesto NUOVO caricato) + output +
        //     cache_creation. NON il prompt lordo per-turno: la history viene
        //     ri-inviata a OGNI turno, quindi cumulare `turn_total_tokens`
        //     condannava matematicamente i run con contesto grande (run 8c4f5eea:
        //     history ~50k -> ~8 turni SANI bruciavano il budget 400k -> cascata
        //     di escalation "non-convergenza" fino al cap -> failed_diagnosed,
        //     con la correzione post-gate uccisa pre-LLM senza fare lavoro).
        //     Il runaway vero resta coperto: contesto che esplode = delta grandi,
        //     output ripetuto a raffica = completion cumulate, e il freno di
        //     spesa in dollari (`run_cost_cumulative_usd`) conta comunque il
        //     costo REALE lordo. Il ramo PRE-LLM del prossimo giro confronta
        //     questo totale con `run_token_budget`. Su turno error
        //     (gateway_errored) l'usage e' default (zero): delta 0 e output 0,
        //     il totale resta invariato, coerente col non-conteggio del ramo
        //     error. `state.prompt_tokens` porta il prompt dell'ULTIMO turno
        //     (reducer overwrite): dopo una compressione il prompt scende e il
        //     delta clampa a 0 (nessun rimborso, conservativo).
        // (2) consecutive_text_only_turns: azzerato se il modello ha emesso almeno
        //     un tool_use (`pending_tool_uses` non vuoto = segnale strutturato
        //     `resp.tool_calls`, regola M), altrimenti +1. Rileva il modello che
        //     descrive senza agire (pattern gemini). Un turno error non conta come
        //     "solo-testo" produttivo: e' un fallimento gestito dal ramo error, ma
        //     resta comunque privo di tool_use -> incrementa (conservativo: un
        //     provider che erra a raffica va chiuso anche via questo asse).
        let prev_tokens_total = state.tokens_used_total.unwrap_or(0).max(0);
        let prev_turn_prompt = state.prompt_tokens.unwrap_or(0).max(0);
        let incremental_prompt = (turn_prompt_tokens - prev_turn_prompt).max(0);
        let turn_budget_tokens = incremental_prompt
            .saturating_add(turn_completion_tokens.max(0))
            .saturating_add(turn_cache_creation.max(0));
        let new_tokens_total = prev_tokens_total.saturating_add(turn_budget_tokens);
        // Costo cumulativo REALE del run (freno di spesa in dollari): somma il costo
        // del turno (dall'usage, gia' col prezzo del modello del turno) al totale
        // portato dallo stato. Reducer overwrite (come i token). turn_total_cost None
        // (prezzo ignoto o turno error) -> +0.0, il cumulativo resta invariato.
        let prev_cost_total = state.run_cost_cumulative_usd.unwrap_or(0.0).max(0.0);
        let new_cost_total = prev_cost_total + turn_total_cost.unwrap_or(0.0).max(0.0);
        let new_text_only_streak = if pending_tool_uses.is_empty() {
            state.consecutive_text_only_turns.unwrap_or(0).max(0) + 1
        } else {
            0
        };

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
            // Contatori anti-runaway cumulativi (safety net token, mig 0520).
            tokens_used_total: Some(Some(new_tokens_total)),
            consecutive_text_only_turns: Some(Some(new_text_only_streak)),
            action_nudge_count: Some(Some(nudge_count)),
            g1_reroute_count: Some(Some(g1_reroute_count)),
            sticky_provider: Some(sticky_provider),
            sticky_model: Some(sticky_model),
            current_tier: current_tier_delta,
            // Finestra EFFETTIVA del turno (config o modello promosso dallo
            // smart-upscale): il ToolDispatchNode la usa per il predictive cap
            // al posto della finestra statica di config, cosi' il gate segue il
            // modello reale del turno (regola H, incidente 2026-07-06).
            effective_context_window: Some(Some(effective_window)),
            // Usage del turno (py:3476-3480), overwrite last-write come il Python.
            prompt_tokens: Some(Some(turn_prompt_tokens)),
            completion_tokens: Some(Some(turn_completion_tokens)),
            cache_creation_tokens: Some(Some(turn_cache_creation)),
            cache_read_tokens: Some(Some(turn_cache_read)),
            total_tokens: Some(Some(turn_total_tokens)),
            total_cost_usd: Some(turn_total_cost),
            // Costo cumulativo del run (freno di spesa in dollari, NON si resetta
            // all'escalation): reducer overwrite col totale accumulato.
            run_cost_cumulative_usd: Some(Some(new_cost_total)),
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
        match &det_streak_val {
            // Fallimento deterministico sotto soglia: lo streak sopravvive al turno.
            Some(v) => {
                extra_out.insert(GW_DET_STREAK_KEY.to_string(), v.clone());
            }
            // Turno senza fallimento deterministico: reset esplicito (oltre alla
            // contiguita', che gia' lo invaliderebbe).
            None => {
                extra_out.remove(GW_DET_STREAK_KEY);
            }
        }
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
        if declaring_role_turn {
            // Finestra di RUOLO consumata (gemella della precedente): una sola,
            // che il modello abbia dichiarato o no — l'anti-loop resta il
            // one-shot di ADVISORY_GRACE_USED_KEY.
            extra_out.remove("force_role_declaration");
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

/// Esito del gate CONDIVISO dei due detector-emissione dello `StallReason`
/// ([`ExecutorNode::stall_emit_gate`], regola L). Portato ai due chiamanti
/// ([`ExecutorNode::maybe_stall_reason_delta`] e il gemello runaway
/// `maybe_runaway_stall_delta`) quando l'emissione e' PERMESSA; ognuno vi aggiunge
/// i propri segnali e il proprio `build_*_context`.
struct StallEmitGate {
    /// work_epoch STABILE (chiave idempotenza/replay): avanza solo sui cambi
    /// macroscopici del run.
    epoch: i64,
    /// Escalation gia' fatte nel run: serve al `build_runaway_context` del gemello
    /// runaway (il ramo stall non la usa, destruttura con `..`).
    escalations: i64,
    /// Budget consultazioni CROSS-RUN gia' usate nella sessione (per il log di
    /// emissione, campo strutturato).
    moves_used_session: i64,
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
        kind: &str,
        title: String,
        payload: Value,
    ) {
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
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

    /// Gate CONDIVISO dei due detector-emissione dello `StallReason`
    /// ([`Self::maybe_stall_reason_delta`] e il gemello runaway
    /// `maybe_runaway_stall_delta`, regola L: STESSO gate budget+epoca+anti-meta-loop,
    /// prima duplicato verbatim nei due). Ritorna `Some(StallEmitGate)` se l'emissione
    /// e' PERMESSA per `(axis, epoch)`, `None` se va saltata:
    ///   1. `stall_recovery_enabled` OFF (flag, default) -> `None` (bit-identico);
    ///   2. budget per-SESSIONE esaurito: cap `stall_recovery_max_moves_per_session`
    ///      (regola G) confrontato con la somma di per-run (`extra["stall_moves_used"]`)
    ///      + cross-run ([`StallBudgetPort`], persistito per sessione, chiude il loop
    ///      email) -> `None`. Fail-open: porta assente/guasta -> solo cap per-run;
    ///   3. mossa gia' decisa o epoca gia' risolta a Fallback per `(axis, epoch)`
    ///      (anti-meta-loop, punto unico `stall_move_key`) -> `None`.
    /// SOLA LETTURA (nessun side-effect): il check `needs_meta` (solo stall) e la
    /// costruzione dello `StallContext` restano nei due chiamanti.
    async fn stall_emit_gate(
        &self,
        state: &AgentState,
        axis: &str,
        iters_in: i64,
        ctx: &AgentNodeCtx,
    ) -> Option<StallEmitGate> {
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
        // work_epoch STABILE (chiave idempotenza/replay): avanza solo sui cambi
        // macroscopici. `todo_seq` ~ iterazioni del run; escalation e floor da extra.
        let escalations = state
            .extra
            .get("auto_escalations")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let floor = state
            .extra
            .get("repeat_scan_floor")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let epoch = crate::decisions::meta_reason::work_epoch(iters_in, escalations, floor);
        // (3) ANTI-META-LOOP (idempotenza per epoca): mossa gia' decisa+consumata
        // (chiave-cache in extra) o epoca gia' risolta a Fallback (marcatore) per questo
        // (axis, epoch) -> non ri-emettere. La chiave-cache e' il punto unico
        // `stall_move_key` (regola L).
        if state.extra.contains_key(&stall_move_key(axis, epoch))
            || state
                .extra
                .get("stall_fallback_epochs")
                .and_then(Value::as_array)
                .map(|a| a.iter().any(|v| v.as_i64() == Some(epoch)))
                .unwrap_or(false)
        {
            return None;
        }
        Some(StallEmitGate {
            epoch,
            escalations,
            moves_used_session,
        })
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
        // Gate CONDIVISO col gemello runaway (regola L): flag + budget per-SESSIONE +
        // work_epoch + anti-meta-loop. `None` -> rete di sicurezza (gerarchia fissa).
        let StallEmitGate {
            epoch,
            moves_used_session,
            ..
        } = self
            .stall_emit_gate(state, axis.as_str(), iters_in, ctx)
            .await?;
        // Solo se la gerarchia fissa farebbe una mossa COSTOSA (non GUIDE/Proceed): il
        // meta-ragionamento subentra DOPO il livello-1 GUIDE cheap. Check STALL-ONLY
        // (gli assi runaway non sono in `ProgressSignals`, il gemello non lo ha) e
        // side-effect-free: eseguirlo DOPO il gate e' bit-identico all'ordine storico.
        let fixed = pc::decide(signals);
        if matches!(fixed.action, Action::Guide | Action::Proceed) {
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
        let redaction_rejected = crate::routing::signals::recent_redaction_rejected(messages, 16);
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

    /// Gate di EMISSIONE dello `StallReason` per un asse RUNAWAY pre-LLM
    /// (`token_overflow` / `text_only`): trasforma i limiti FISSI 4d/4e in TRIGGER
    /// del giudice agentico invece di chiudere direttamente con `close_runaway`.
    /// GEMELLO di [`Self::maybe_stall_reason_delta`] (stesso budget per-sessione,
    /// stessa `work_epoch`, stessa guardia anti-meta-loop, stessa costruzione
    /// StallContext dal punto unico `meta_reason`), ma SENZA il gate
    /// `pc::decide != Guide`: questi assi NON sono in [`Axis`]/`ProgressSignals`,
    /// quindi la gerarchia fissa non li conosce. Il runaway E' di per se' un evento
    /// costoso: se il gate scatta si consulta sempre il giudice (che puo' comunque
    /// decidere `ContinueGuided`/`Fallback`).
    ///
    /// Ritorna `Some(delta)` (-> `StallReason` -> nodo `StallRecovery`) SOLO se:
    ///   1. `stall_recovery_enabled` (flag OFF -> `None` -> il chiamante usa il
    ///      backstop `close_runaway`, comportamento bit-identico a 822e083);
    ///   2. budget per-SESSIONE non esaurito (per-run in `extra["stall_moves_used"]`
    ///      + cross-run [`StallBudgetPort`], come il gemello);
    ///   3. nessuna mossa gia' decisa / epoca gia' risolta a Fallback per questo
    ///      (axis, epoch) (anti-meta-loop, punto unico `stall_move_key`).
    /// `axis` e' [`AXIS_TOKEN_OVERFLOW`] o [`AXIS_TEXT_ONLY`]; `count` e' il valore
    /// 3.4 (difesa strutturale, dietro flag `provider_no_progress_switch_enabled`): sul
    /// cap solo-testo (un provider che DESCRIVE senza AGIRE per N turni), prova a
    /// CAMBIARE PROVIDER via failover invece di chiudere il run col backstop. `None`
    /// (-> il chiamante chiude, bit-identico) se: flag OFF, provider corrente assente,
    /// cap escalation esaurito (`auto_escalations >= 3`) o nessun sostituto sano resta.
    /// Riusa il punto unico `EscalationPort::failover_provider` (causa `EmptyCompletion`
    /// = "non produce output utile" -> finestra NON filtrata) e la cascata
    /// `failover_tried` del ramo errore-provider (regola L).
    async fn maybe_switch_provider_on_no_progress(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
        text_only_streak: u32,
    ) -> Option<OpaqueDelta> {
        if !self.cfg.provider_no_progress_switch_enabled {
            return None;
        }
        let provider = state
            .provider_used
            .clone()
            .or_else(|| state.provider_override.clone())
            .unwrap_or_default();
        let model = state
            .model_used
            .clone()
            .or_else(|| state.model_override.clone())
            .unwrap_or_default();
        if provider.trim().is_empty() {
            return None;
        }
        // Cap anti-raffica (stesso del ramo errore-provider): non ciclare fra provider.
        let cd_escal = state
            .extra
            .get("auto_escalations")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if cd_escal >= 3 {
            return None;
        }
        let mut tried: Vec<String> = state
            .extra
            .get("failover_tried")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !tried.iter().any(|p| p == &provider) {
            tried.push(provider.clone());
        }
        // "Non produce output utile": causa EmptyCompletion (il failover causa-aware NON
        // filtra la finestra -> ripiega su qualunque provider sano).
        let pick = self
            .escalation
            .failover_provider(
                Some(&provider),
                Some(&model),
                state.current_tier.as_deref(),
                crate::runtime::ports::ProviderFailureCause::EmptyCompletion,
                &tried,
            )
            .await
            .ok()
            .flatten()?;
        tracing::warn!(
            target: "nexus_agent_graph::executor",
            from_provider = %provider,
            to_provider = %pick.provider,
            to_model = %pick.model,
            text_only_streak,
            "3.4: provider fermo (solo-testo oltre soglia) -> switch cross-provider invece di chiudere"
        );
        self.emit_phase(
            ctx,
            "escalation",
            format!(
                "{provider} non avanza (solo testo): passo a {}/{}",
                pick.provider, pick.model
            ),
            switch_payload(
                &provider,
                &model,
                &pick.provider,
                &pick.model,
                SwitchReason::ProviderNoProgress,
                Some(false),
                Some("no_progress"),
            ),
        )
        .await;
        let esc_nudge = human_msg(
            "Il provider precedente ha descritto senza agire per piu' turni consecutivi. \
Riprendi tu, sul nuovo provider: esegui SUBITO il prossimo step CONCRETO del compito con \
una tool call, non descrivere.",
        );
        tried.push(pick.provider.clone());
        let mut extra_out = state.extra.clone();
        extra_out.insert("auto_escalations".to_string(), json!(cd_escal + 1));
        extra_out.insert("failover_tried".to_string(), json!(tried));
        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
        Some(
            StateDelta {
                messages: Some(vec![esc_nudge]),
                sticky_provider: Some(Some(pick.provider)),
                sticky_model: Some(Some(pick.model)),
                current_tier: Some(pick.tier),
                recent_tool_signatures: Some(Some(vec![])),
                g1_reroute_count: Some(Some(0)),
                action_nudge_count: Some(Some(0)),
                consecutive_text_only_turns: Some(Some(0)),
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::G1Escalated)),
                iterations: Some(Some(iters_in + 1)),
                extra: Some(extra_out),
                ..Default::default()
            }
            .into_opaque(),
        )
    }

    /// del limite (token cumulativi / streak). NON chiama l'LLM (lo fa il nodo).
    async fn maybe_runaway_stall_delta(
        &self,
        state: &AgentState,
        axis: &str,
        count: i64,
        iters_in: i64,
        messages: &[Message],
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        // Gate CONDIVISO col gemello `maybe_stall_reason_delta` (regola L): flag +
        // budget per-SESSIONE + work_epoch + anti-meta-loop. `None` -> backstop
        // (`close_runaway`, bit-identico a 822e083). SENZA il check `needs_meta`: gli
        // assi runaway non sono in `ProgressSignals` e il runaway E' di per se' costoso.
        let StallEmitGate {
            epoch,
            escalations,
            moves_used_session,
        } = self.stall_emit_gate(state, axis, iters_in, ctx).await?;
        // Segnali cross-cutting per lo StallContext (regola M: tutti strutturati),
        // identici al gemello: esito ultimo tool, firme recenti, redazione, file
        // modificati, intent. `count` e' il limite runaway; escalations dal budget.
        let recent_tool_signatures = state.recent_tool_signatures.clone().unwrap_or_default();
        let last_outcome = Self::last_tool_outcome(messages);
        let modified = crate::routing::signals::modified_files_from_messages(messages, 40);
        let redaction_rejected = crate::routing::signals::recent_redaction_rejected(messages, 16);
        let action_oriented = turn_action_oriented(state.action_oriented);
        let stall = crate::decisions::meta_reason::build_runaway_context(
            axis,
            count,
            action_oriented,
            escalations,
            self.cfg.max_escalations,
            &recent_tool_signatures,
            last_outcome,
            redaction_rejected,
            state.user_intent.as_deref(),
            &modified,
            epoch,
        );
        let value = serde_json::to_value(&stall).ok()?;
        let extra = put_extra(state, STALL_CONTEXT_KEY, value);
        tracing::info!(
            target: "nexus_agent_graph::executor",
            axis,
            count,
            work_epoch = epoch,
            moves_used_run = Self::stall_moves_used(state),
            moves_used_session,
            "meta-reasoner: runaway pre-LLM -> emetto StallReason (giudice agentico)"
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

    /// NON-CONVERGENZA -> ESCALATION AGENTICA (regola H, "niente di fisso"): quando
    /// un limite di non-convergenza (budget token esaurito, cap iterazioni, o il
    /// final_gate che non converge) sta per chiudere secco il run, prova PRIMA a
    /// promuovere a un modello piu' capace via il punto unico [`pick_escalation_model`]
    /// (selezione tier+telemetria). Se ne esiste uno sano, lo rende sticky, AZZERA il
    /// budget token cumulativo (il promosso riparte con la sua quota, altrimenti
    /// ri-colpirebbe subito il tetto), propaga il `current_tier` (aggiorna lo
    /// scale-controller) e RI-DA il turno (`G1Escalated`). Bound da `auto_escalations
    /// < max_escalations` (anti-loop). Se non c'e' un candidato (catena esaurita / gia'
    /// max escalation / tutti in cooldown) -> `None`, il chiamante chiude col backstop.
    /// STESSO paradigma del cap G1 (regola L), applicato ai trigger di non-convergenza
    /// (PUNTO UNICO di escalation-da-non-convergenza: 3 call site delegano qui).
    ///
    /// `reset_iterations`: quando il limite che scatta E' il conteggio iterazioni
    /// (ramo `iteration_cap`), azzera anche `iterations` cosi' il modello promosso
    /// riparte con un ciclo pieno (simmetrico all'azzeramento del budget token);
    /// altrimenti (`budget_token`/`final_gate_nonconvergence`, dove iterations NON e'
    /// il limite) il conteggio prosegue (`iters_in + 1`). Il runaway resta bounded da
    /// `auto_escalations` + hard-cap token/costo (mai resettati).
    async fn maybe_escalate_nonconvergence(
        &self,
        state: &AgentState,
        iters_in: i64,
        reason: SwitchReason,
        ctx: &AgentNodeCtx,
        reset_iterations: bool,
    ) -> Option<OpaqueDelta> {
        let escal = state
            .extra
            .get("auto_escalations")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if escal >= self.cfg.max_escalations {
            return None;
        }
        let (cur_provider, cur_model) = self.escalation_current_pair(state);
        let inputs = self
            .escalation
            .escalation_inputs(
                state.user_intent.as_deref(),
                cur_provider.as_deref(),
                cur_model.as_deref(),
            )
            .await
            .unwrap_or_default();
        // Salita di UN gradino, non al tier MASSIMO disponibile (audit selezione
        // costi): la non-convergenza fa salire gradualmente (es. medium -> high, non
        // -> frontier). Il giudice meta-reasoner progress-aware ha gia' avuto la sua
        // chance a monte (maybe_runaway_stall_delta); esaurito quello, qui si promuove
        // con moderazione invece di saltare al piu' caro per abitudine.
        let stepped = cap_candidates_one_step(&inputs.candidates, state.current_tier.as_deref());
        let pick = pick_escalation_model(
            &stepped,
            cur_provider.as_deref(),
            cur_model.as_deref(),
            &inputs.policy,
        )?;
        tracing::warn!(
            target: "nexus_agent_graph::executor",
            from_provider = cur_provider.as_deref().unwrap_or(""),
            to_provider = %pick.provider,
            to_model = %pick.model,
            reason = reason.code(),
            "non-convergenza: ESCALATION agentica di un gradino -> budget del turno azzerato per il promosso"
        );
        self.emit_phase(
            ctx,
            "escalation",
            format!(
                "Passo a {}/{} (non-convergenza sul budget del turno)",
                pick.provider, pick.model
            ),
            // Punto unico del payload switch (regola L): senza from_provider/from_model
            // la card "CAMBIO PROVIDER" mostrava "Da: <provider> / ?". La coppia
            // corrente e' gia' in scope da escalation_current_pair (:5464).
            stall_switch_payload(&cur_provider, &cur_model, &pick.provider, &pick.model, reason),
        )
        .await;
        let esc_nudge = human_msg(
            "Il modello precedente non ha completato il compito entro il budget del turno. \
Prosegui tu da dove e' arrivato: NON ricominciare da capo ne' descrivere, procedi con \
azioni concrete e mirate verso il completamento.",
        );
        let mut extra_out = state.extra.clone();
        extra_out.insert("auto_escalations".to_string(), json!(escal + 1));
        // Grazia post-escalation: finestra pulita sugli assi anti-loop per il promosso.
        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
        // Consumo del trigger di non-convergenza del final_gate (se presente): il
        // modello promosso NON deve ri-scattare quel ramo al prossimo turno (sarebbe
        // un'escalation a raffica senza che abbia lavorato). No-op per gli altri
        // trigger (budget_token/iteration_cap non posano il flag).
        extra_out.remove(FINAL_GATE_ESCALATION_KEY);
        Some(
            StateDelta {
                messages: Some(vec![esc_nudge]),
                sticky_provider: Some(Some(pick.provider)),
                sticky_model: Some(Some(pick.model)),
                current_tier: Some(pick.tier),
                recent_tool_signatures: Some(Some(vec![])),
                g1_reroute_count: Some(Some(0)),
                action_nudge_count: Some(Some(0)),
                // Reset dei contatori di non-convergenza: il promosso riparte con la
                // sua quota piena (budget) e streak azzerato (altrimenti erediterebbe
                // il cumulativo e ri-colpirebbe subito il limite).
                tokens_used_total: Some(Some(0)),
                consecutive_text_only_turns: Some(Some(0)),
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::G1Escalated)),
                // `reset_iterations` (trigger iteration_cap): azzera il conteggio cosi'
                // il promosso riparte con un ciclo di iterazioni pieno — altrimenti
                // rientrerebbe subito nel ramo cap senza mai lavorare (escalation a
                // raffica). Simmetrico all'azzeramento del budget token sopra. Negli
                // altri trigger iterations NON e' il limite -> prosegue (+1).
                iterations: Some(Some(if reset_iterations { 0 } else { iters_in + 1 })),
                extra: Some(extra_out),
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
    /// (`record_consultation`, best-effort).
    async fn consume_recovery_move(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
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
        //    SESSIONE (append), cosi' il cap e' effettivo
        //    per-sessione anche sul loop email cross-run. Best-effort (fail-open):
        //    un guasto di persistenza non deve rompere il turno.
        let moves_used = Self::stall_moves_used(state) + 1;
        let mut extra_out = state.extra.clone();
        extra_out.insert("stall_moves_used".to_string(), json!(moves_used));
        if let Some(budget) = &self.stall_budget {
            if let Err(err) = budget.record_consultation(ctx.session_id).await {
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
                    // Turno di grazia figura: se il run e' una figura del consiglio
                    // senza parere dichiarato, il nudge di recovery (tipicamente
                    // "diagnostica il fallimento") viene dirottato a ELICERE
                    // advisory_verdict -> parere reale invece di failed_diagnosed
                    // (n/d). Per i run non-figura e' bit-identico.
                    messages_out.push(recovery_nudge_msg(state, t));
                } else if let Some(directive) = pending_role_channel_grace(state) {
                    // Reasoner senza nudge ma ruolo senza verdetto: inietta comunque
                    // la direttiva di grazia, altrimenti il turno riparte muto e il
                    // ruolo continua a esplorare fino a timeout/cap -> n/d.
                    messages_out.push(human_msg(directive.trim()));
                }
                // Il nudge del reasoner riparte con finestra pulita sui detector di
                // ripetizione (stessa grazia dei rami Escalate: il modello promosso/
                // ri-orientato non deve ereditare le firme che hanno causato lo stallo).
                extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
                self.emit_phase(
                    ctx,
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
                let escal = state
                    .extra
                    .get("auto_escalations")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let (cur_provider, cur_model) = self.escalation_current_pair(state);
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
                    pick_escalation_model(
                        &inputs.candidates,
                        cur_provider.as_deref(),
                        cur_model.as_deref(),
                        &inputs.policy,
                    )
                } else {
                    None
                };
                let pick = picked?;
                self.emit_phase(
                    ctx,
                    "escalation",
                    format!("Passo a {}/{} (meta-reasoner)", pick.provider, pick.model),
                    // Punto unico del payload switch (regola L): come sopra, from_* dalla
                    // coppia corrente (:5700) per non mostrare "Da: <provider> / ?".
                    stall_switch_payload(&cur_provider, &cur_model, &pick.provider, &pick.model, SwitchReason::StallRecovery),
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
                .unwrap_or_else(|_| json!({"outcome": "needs_input", "summary": question}));
                self.emit_phase(
                    ctx,
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
                            thinking_signature: None,
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
                .unwrap_or_else(|_| json!({"outcome": "blocked", "summary": summary}));
                self.emit_phase(
                    ctx,
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
                            thinking_signature: None,
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

    // ──────────────────────────────────────────────────────────────────────
    //  SCALE-CONTROLLER (PR-B3): detector-EMISSIONE (fine turno) + rientro-
    //  APPLICAZIONE (`ScaleResolved`). Gemello del blocco stall_recovery, su
    //  tipi DISGIUNTI. La DECISIONE (build_scale_context / scale_trigger /
    //  scale_cache_key) vive nel modulo puro `decisions::scale_reason` (regola
    //  L); qui c'e' solo la RACCOLTA dei segnali gia' risolti + il trasporto
    //  in extra. La selezione tier->modello e' dietro la porta
    //  `ModelUpscalePort::select_model_for_tier` (regola L), mai chiamata dal
    //  nodo direttamente.
    // ──────────────────────────────────────────────────────────────────────

    /// Iterazione dell'ultimo cambio-tier del run (default `-1` = mai cambiato).
    /// Vive in `extra["scale_last_change_iter"]`, checkpointato con lo stato ->
    /// REPLAY-SAFE. Il detector deriva `turns_since_change = iters - last_change_iter`
    /// (default grande al primo giro: il cooldown non blocca l'avvio). Preferito a un
    /// contatore incrementale per-turno (che richiederebbe una scrittura ogni turno
    /// anche senza emissione, rompendo l'inerzia bit-identica a flag OFF).
    fn scale_last_change_iter(state: &AgentState) -> i64 {
        state
            .extra
            .get("scale_last_change_iter")
            .and_then(Value::as_i64)
            .unwrap_or(-1)
    }

    /// Iterazione dell'ultimo cambio di POSTURA di sizing del run
    /// (`extra["scale_sizing_last_iter"]`, default `-1` = mai). Alimenta il cooldown
    /// anti-thrash del sizing (`sizing_turns_since_change = iters - last`), DISTINTO
    /// dal cooldown tier. Replay-safe (checkpointato).
    fn scale_sizing_last_iter(state: &AgentState) -> i64 {
        state
            .extra
            .get("scale_sizing_last_iter")
            .and_then(Value::as_i64)
            .unwrap_or(-1)
    }

    /// Legge gli [`SizingOverrides`] STICKY persistiti dal rientro `consume_scale_move`
    /// (ramo `AdjustSizing`) in `extra[SCALE_SIZING_OVERRIDES_KEY]`. `None` = sizing
    /// OFF / nessuna postura applicata -> gli helper `effective_*` lasciano invariate
    /// le soglie fisse (BIT-IDENTICO). Read-only, replay-safe (stato checkpointato).
    fn read_sizing_overrides(state: &AgentState) -> Option<SizingOverrides> {
        state
            .extra
            .get(SCALE_SIZING_OVERRIDES_KEY)
            .and_then(|v| serde_json::from_value::<SizingOverrides>(v.clone()).ok())
    }

    /// Consultazioni LLM dello scale-controller gia' fatte nel run
    /// (`extra["scale_evals_used"]`, checkpointato). Cap `max_evals_per_run` (regola
    /// G): oltre, il detector NON emette piu' `ScaleReason` (budget/costo).
    fn scale_evals_used(state: &AgentState) -> i64 {
        state
            .extra
            .get("scale_evals_used")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    /// Numero di INVERSIONI di direzione sulla stessa coppia di tier
    /// (`extra["scale_reversal_count"]`, checkpointato): alimenta il reversal-pin
    /// (gate 5 di `apply_hysteresis`). Aggiornato dal rientro `consume_scale_move`.
    fn scale_reversal_count(state: &AgentState) -> i64 {
        state
            .extra
            .get("scale_reversal_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    /// Numero di CAMBI-TIER effettivamente applicati nel run
    /// (`extra["scale_tier_changes_used"]`, checkpointato): alimenta il tetto
    /// `max_tier_changes_per_run` (FIX-C). Incrementato dal rientro
    /// `consume_scale_move` a OGNI cambio-tier effettivo. Distinto da
    /// `scale_reversal_count` (inversioni A->B->A) e da `scale_evals_used`
    /// (valutazioni LLM).
    fn scale_tier_changes_used(state: &AgentState) -> i64 {
        state
            .extra
            .get("scale_tier_changes_used")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    }

    /// Il run ha raggiunto il tetto `max_tier_changes_per_run` ed e' stato pinnato a
    /// Heavy (`extra["scale_pinned_heavy"]`, checkpointato, FIX-C): il detector non
    /// emette piu' `ScaleReason` (disattiva ulteriori cambi) e il rientro non applica
    /// piu' mosse. Set dal rientro `consume_scale_move` all'ultimo cambio consentito.
    fn scale_pinned_heavy(state: &AgentState) -> bool {
        state
            .extra
            .get("scale_pinned_heavy")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// `behavior_mode` come stringa opaca per lo `ScaleContext` (segnale, non prosa).
    /// `None` -> `"none"` (default conservativo, coerente con `AutomationMode::None`).
    fn behavior_mode_str(state: &AgentState) -> &'static str {
        match state.automation_mode {
            Some(crate::state::AutomationMode::None) | None => "none",
            Some(crate::state::AutomationMode::Confirm) => "confirm",
            Some(crate::state::AutomationMode::Automatic) => "automatic",
            Some(crate::state::AutomationMode::Continuous) => "continuous",
        }
    }

    /// Complessita' stimata del task come intero per lo `ScaleContext` (segnale
    /// strutturato del classifier, regola M). `None` -> `0` (default conservativo,
    /// nessuna complessita' affermata).
    fn task_complexity_est(state: &AgentState) -> i64 {
        match state.task_complexity {
            Some(crate::state::TaskComplexity::Low) => 1,
            Some(crate::state::TaskComplexity::Medium) => 2,
            Some(crate::state::TaskComplexity::High) => 3,
            None => 0,
        }
    }

    /// Il task e' CRITICO (FIX-D): PROTEGGE dal downscale. Derivato con cura (NON
    /// default-permissivo): `true` se la complessita' e' alta OPPURE il behavior_mode
    /// e' `continuous` (catena autonoma prolungata, alto costo di sbagliare) OPPURE
    /// il run ha gia' escalato (segnale che il task ha richiesto piu' potenza). Un
    /// intent classificato non e' disponibile come set chiuso qui (e' testo libero),
    /// quindi NON si deriva la criticita' dal parsing del testo intent (regola M): si
    /// usano solo segnali strutturati. In dubbio, non-critico (il floor e gli altri
    /// gate del downscale restano comunque attivi).
    fn task_critical(state: &AgentState, escalations_done: i64) -> bool {
        matches!(
            state.task_complexity,
            Some(crate::state::TaskComplexity::High)
        ) || matches!(
            state.automation_mode,
            Some(crate::state::AutomationMode::Continuous)
        ) || escalations_done > 0
    }

    /// Pavimento di tier per l'intent (FIX-D): il downscale non scende MAI sotto.
    /// DEFAULT CONSERVATIVO documentato: senza una tabella intent->floor risolta a
    /// monte nel path del turno (sarebbe I/O DB, vietato qui per il determinismo di
    /// replay), il floor deriva dai soli segnali strutturati gia' presenti:
    ///   - task CRITICO -> floor `medium` (mai downscale a `light` su task critici);
    ///   - altrimenti -> floor `light` (nessun pavimento extra: gli altri 5 gate del
    ///     downscale restano la protezione principale).
    /// Il pavimento vero DB-driven per-intent e' un miglioramento futuro (TODO):
    /// finche' assente, questa derivazione e' safety-biased (non permissiva).
    fn intent_tier_floor(task_critical: bool) -> ScaleTier {
        if task_critical {
            ScaleTier::Medium
        } else {
            ScaleTier::Light
        }
    }

    /// Gate di EMISSIONE dello `ScaleReason` (detector-emissione, PR-B3). GEMELLO di
    /// [`Self::maybe_stall_reason_delta`]. Chiamato PRE-LLM (FIX-A), subito prima
    /// della `complete` del turno, dopo che `hist` e' finalizzato (est_tokens
    /// accurato). Emetterlo prima della chiamata LLM lo rende idempotente al resume
    /// da checkpoint e non scarta mai una `complete`
    /// produttiva: se la scala cambia il tier, il turno si prosegue col modello
    /// nuovo; su KeepTier / cambio annullato si prosegue con UNA sola `complete`.
    /// Ritorna `Some(delta)` — che instrada al nodo `ScaleControl` — SOLO se il
    /// trigger scatta; `None` prosegue il turno normale.
    ///
    /// GUARD PRIMARIO (bit-identico): se `!self.cfg.scale.enabled` ritorna SUBITO
    /// `None`, PRIMA di qualunque lavoro (zero overhead a flag OFF, vincolo primario).
    ///
    /// Costruisce lo `ScaleContext` dai segnali gia' risolti (regola M) col punto
    /// unico `build_scale_context`, poi consulta `scale_trigger` (break-even + cadenza
    /// + precedenza stallo FIX-E). Se scatta E il budget `max_evals_per_run` non e'
    /// esaurito E non c'e' gia' una mossa in cache per la chiave-cache corrente:
    /// serializza lo `ScaleContext` in `extra[SCALE_CONTEXT_KEY]` + la chiave-cache in
    /// `extra[SCALE_MOVE_CACHE_KEY_KEY]` + la `ScaleHysteresisConfig` DB-driven in
    /// `extra[SCALE_HYSTERESIS_CFG_KEY]` (FIX-B: le soglie del gate raggiungono il
    /// nodo) + incrementa `scale_evals_used`, e ritorna `StopReason::ScaleReason`
    /// (clone-whole-map, `put_extra`).
    #[allow(clippy::too_many_arguments)]
    fn maybe_scale_reason_delta(
        &self,
        state: &AgentState,
        iters_in: i64,
        messages: &[Message],
        escalations_done: i64,
        est_tokens: i64,
        effective_window: i64,
        stall_active: bool,
    ) -> Option<OpaqueDelta> {
        // GUARD PRIMARIO: flag OFF -> zero overhead (nessun ScaleContext, nessun
        // trigger). Bit-identico a oggi (vincolo primario PR-B3).
        if !self.cfg.scale.enabled {
            return None;
        }
        // FIX-C (F5): tetto cambi-tier raggiunto e run pinnato a Heavy -> non emettere
        // piu' (il controller e' "disattivato" per il resto del run, mig 0516).
        if Self::scale_pinned_heavy(state) {
            return None;
        }
        // Budget consultazioni esaurito -> non emettere (rete anti-costo, regola G).
        let evals_used = Self::scale_evals_used(state);
        if evals_used >= self.cfg.scale.max_evals_per_run {
            return None;
        }

        // ── Raccolta segnali STRUTTURATI (regola M). READY vs DEFAULT documentati ──
        // context_window del modello del turno (config, regola G). 0 = ignoto ->
        // pressione Low + headroom pieno (conservativo).
        let context_window = self.cfg.context_window;
        let context_pressure = context_pressure_from_tokens(est_tokens, context_window);
        let token_headroom_ratio = if context_window > 0 {
            (est_tokens.max(0) as f64 / context_window as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // error_count / error_free_streak dal segnale strutturato (exit_code/is_error),
        // finestra 40 messaggi (coerente con modified_files lookback).
        let (error_count, error_free_streak) = tool_error_stats(messages, 40);
        // repeated_action_failed: l'ultimo tool_result e' errore (segnale strutturato).
        let repeated_action_failed = detect_recent_tool_error(messages, 4);
        // files_modified_delta: DEFAULT documentato = conteggio TOTALE dei file
        // modificati nella finestra (non il delta per-checkpoint, che richiederebbe un
        // segnale di checkpoint precedente non tracciato). Conservativo: il downscale
        // richiede `> 0`, quindi un run senza modifiche NON downscala per questo campo.
        let files_modified_delta = modified_files_from_messages(messages, 40).len() as i64;
        let task_complexity_est = Self::task_complexity_est(state);
        let task_critical = Self::task_critical(state, escalations_done);
        let intent_tier_floor = Self::intent_tier_floor(task_critical);
        // escalation_lock_active: DEFAULT safety-biased. Deriva da "c'e' stata
        // un'escalation nel run" (escalations_done > 0): finche' il run ha dovuto
        // salire di potenza, il downscale resta VIETATO (FIX-E). Non c'e' un
        // escalation-lock esplicito tracciato nel path; questo proxy e' conservativo
        // (protegge dal downscale piu' del necessario, mai meno).
        let escalation_lock_active = escalations_done > 0;
        let cost_spent_usd = state.subagent_cost_cumulative_usd.unwrap_or(0.0);
        // cost_cap_usd: DEFAULT 0 = nessun cap propagato nel path del turno (il cap
        // costo del run non e' un segnale disponibile qui). 0 disattiva le guard di
        // costo dello scale-controller (conservativo: nessuna decisione forzata dal
        // costo).
        let cost_cap_usd = 0.0;
        let turns_since_change = {
            let last = Self::scale_last_change_iter(state);
            if last < 0 {
                // Mai cambiato: coda ampia -> il cooldown non blocca l'avvio.
                iters_in.max(self.cfg.scale.change_cooldown_turns)
            } else {
                (iters_in - last).max(0)
            }
        };
        let reversal_count = Self::scale_reversal_count(state);
        // required_capability: DEFAULT `None`. Il selettore a valle
        // (select_model_for_tier) forza gia' l'eleggibilita' agentica (tool-use); una
        // capability specifica del turno (es. vision) non e' un segnale strutturato
        // disponibile qui, quindi non la si afferma (regola M). `requires_tool_use`
        // resta true (run agentico).
        let required_capability: Option<&str> = None;

        let mut scale_ctx = build_scale_context(
            state.current_tier.as_deref(),
            intent_tier_floor,
            state.user_intent.as_deref(),
            Self::behavior_mode_str(state),
            iters_in,
            self.cfg.iteration_cap,
            task_complexity_est,
            task_critical,
            context_pressure,
            est_tokens,
            token_headroom_ratio,
            files_modified_delta,
            // todos_closed: DEFAULT 0 (nessun segnale TodoStore nel path del turno;
            // TODO: propagare i todo chiusi dal TodoStore). Conservativo: il downscale
            // richiede `> 0`, quindi con 0 NON downscala per questo campo (safety).
            0,
            error_count,
            error_free_streak,
            repeated_action_failed,
            escalations_done,
            escalation_lock_active,
            cost_spent_usd,
            cost_cap_usd,
            required_capability,
            true,
            turns_since_change,
            reversal_count,
        );

        // ── Segnali di SIZING (mig 0524): popolati SOLO se il sizing e' abilitato ──
        // A sizing OFF restano None -> OMESSI dal JSON serializzato all'LLM
        // (skip_serializing_if) -> il flusso TIER resta BIT-IDENTICO. Sono gli OCCHI
        // del sizing (regola M): crescita history, rumore tool_result, progresso.
        if self.cfg.scale.sizing_enabled {
            let history_size = messages.len() as i64;
            scale_ctx.history_size = Some(history_size);
            // Crescita = messaggi per iterazione (proxy strutturato dell'espansione
            // del contesto; nessuno stato extra richiesto -> replay-safe).
            scale_ctx.history_growth_rate =
                Some(history_size as f64 / (iters_in.max(0) + 1) as f64);
            // Rumore storia = caratteri del piu' grande messaggio recente (proxy della
            // tool_result_size_distribution: un singolo output enorme che inquina il
            // contesto). Riusa il punto unico di stima char (regola L).
            let noise_window = 10usize;
            let start = messages.len().saturating_sub(noise_window);
            scale_ctx.tool_result_noise = Some(
                messages[start..]
                    .iter()
                    .map(|m| estimate_history_chars(&[message_to_history(m)]))
                    .max()
                    .unwrap_or(0),
            );
            scale_ctx.effective_window = if effective_window > 0 {
                Some(effective_window)
            } else {
                None
            };
            scale_ctx.recent_productive = Some(has_recent_productive_action(messages, 16));
            // Cooldown anti-thrash del SIZING (distinto dal cooldown TIER).
            let sizing_last = Self::scale_sizing_last_iter(state);
            scale_ctx.sizing_turns_since_change = Some(if sizing_last < 0 {
                // Mai cambiata la postura: coda ampia -> non blocca il primo cambio.
                iters_in.max(self.cfg.scale.sizing_cooldown_turns)
            } else {
                (iters_in - sizing_last).max(0)
            });
        }

        // ── Trigger: gate break-even + cadenza + precedenza stallo (FIX-E) ─────
        // I sotto-segnali di trigger (pressure_changed / todo_closed_now /
        // error_ratchet_advanced / escalation_advanced) non sono tracciati per-turno
        // nel path (richiederebbero uno snapshot precedente checkpointato): passiamo
        // `false` e ci affidiamo alla CADENZA (`iterations % eval_every == 0`), che e'
        // il trigger primario. DEFAULT conservativo documentato: senza gli snapshot
        // il controller valuta a cadenza fissa, mai piu' spesso (mai un costo extra).
        let trig_cfg = ScaleTriggerConfig {
            enabled: self.cfg.scale.enabled,
            eval_every_iters: self.cfg.scale.eval_every_iters,
            min_tail_iters: self.cfg.scale.min_tail_iters,
        };
        let triggered = scale_trigger(
            &scale_ctx,
            &trig_cfg,
            stall_active,
            false,
            false,
            false,
            false,
        );
        if !triggered {
            return None;
        }

        // Chiave-cache (punto unico, con l'eval_every_iters reale). Idempotenza
        // (anti meta-loop, FIX-A/F4/F6): NON ri-emettere per una chiave gia' valutata.
        // Due sotto-casi:
        //   - una MOSSA e' gia' in cache a questa chiave (`contains_key(&key)`): il
        //     nodo farebbe cache-hit, la mossa e' gia' decisa;
        //   - la chiave e' gia' stata TRASPORTATA a questo giro
        //     (`SCALE_MOVE_CACHE_KEY_KEY == key`): copre il KeepTier, che il nodo NON
        //     persiste. Senza questo, al rientro KeepTier (pre-LLM) il detector
        //     ri-emetterebbe la stessa chiave -> meta-loop fino a max_evals_per_run.
        let key = scale_cache_key(&scale_ctx, self.cfg.scale.eval_every_iters);
        if state.extra.contains_key(&key) {
            return None;
        }
        let already_evaluated = state
            .extra
            .get(SCALE_MOVE_CACHE_KEY_KEY)
            .and_then(|v| v.as_str())
            == Some(key.as_str());
        if already_evaluated {
            return None;
        }

        // Serializza ScaleContext + chiave-cache in extra (clone-whole-map: NON azzera
        // gli altri canali) e incrementa il budget consultazioni.
        let ctx_value = serde_json::to_value(&scale_ctx).ok()?;
        let mut extra_out = put_extra(state, SCALE_CONTEXT_KEY, ctx_value);
        extra_out.insert(SCALE_MOVE_CACHE_KEY_KEY.to_string(), json!(key));
        // FIX-B (F2/F3): trasporta le 5 soglie DB-driven del gate anti-oscillazione
        // al nodo (che non legge i settings, zero I/O). Costruite da
        // `self.cfg.scale` (letto dal DB in mcp-core), cosi'
        // `agent.scale.downscale_enabled` & co. raggiungono `apply_hysteresis` al
        // posto dei default hardcoded (chiude la config muta, regola G/L).
        let hyst_cfg = ScaleHysteresisConfig {
            downscale_enabled: self.cfg.scale.downscale_enabled,
            min_confidence: self.cfg.scale.min_confidence,
            change_cooldown_turns: self.cfg.scale.change_cooldown_turns,
            downscale_clean_window: self.cfg.scale.downscale_clean_window,
            max_reversals: self.cfg.scale.max_reversals,
            window_overhead_ratio: self.cfg.scale.window_overhead_ratio,
        };
        if let Ok(cfg_value) = serde_json::to_value(hyst_cfg) {
            extra_out.insert(SCALE_HYSTERESIS_CFG_KEY.to_string(), cfg_value);
        }
        // Trasporta la ScaleSizingConfig DB-driven al nodo SOLO se il sizing e'
        // abilitato (mig 0524): a sizing OFF nessuna chiave aggiunta -> extra map
        // identica a pre-0524 (bit-identico); il nodo, senza la config trasportata,
        // ricade sul fallback conservativo (sizing OFF) e degrada ogni AdjustSizing a
        // KeepTier. La `min_confidence` e' RIUSATA da `agent.scale.min_confidence`
        // (regola L: una soglia, due gate).
        if self.cfg.scale.sizing_enabled {
            let sizing_cfg = ScaleSizingConfig {
                enabled: true,
                min_confidence: self.cfg.scale.min_confidence,
                cooldown_turns: self.cfg.scale.sizing_cooldown_turns,
                aggressiveness: self.cfg.scale.sizing_aggressiveness,
            };
            if let Ok(sizing_value) = serde_json::to_value(sizing_cfg) {
                extra_out.insert(SCALE_SIZING_CFG_KEY.to_string(), sizing_value);
            }
        }
        extra_out.insert("scale_evals_used".to_string(), json!(evals_used + 1));
        tracing::info!(
            target: "nexus_agent_graph::executor",
            current_tier = scale_ctx.current_tier.as_str(),
            iterations = iters_in,
            tail_headroom = scale_ctx.tail_headroom,
            evals_used = evals_used + 1,
            "scale-controller: emetto ScaleReason -> nodo ScaleControl"
        );
        Some(
            StateDelta {
                extra: Some(extra_out),
                stop_reason: Some(Some(StopReason::ScaleReason)),
                iterations: Some(Some(iters_in + 1)),
                ..Default::default()
            }
            .into_opaque(),
        )
    }

    /// CONSUMO della `ScaleMove` al rientro dal nodo `ScaleControl`
    /// (`StopReason::ScaleResolved`, PR-B3). GEMELLO di [`Self::consume_recovery_move`].
    /// Legge la `ScaleMove` persistita dal nodo alla chiave-cache (trasportata in
    /// `extra[SCALE_MOVE_CACHE_KEY_KEY]`):
    ///   - `KeepTier` / mossa assente -> `None` (nessun cambio, prosegue il turno);
    ///   - `UpscaleTo{tier}` / `DownscaleTo{tier}` -> risolve il modello del tier via
    ///     la porta `select_model_for_tier` con `min_context_window = est_tokens *
    ///     window_overhead_ratio` (FIX-B). Se `Some((provider,model))`: `StateDelta`
    ///     con `sticky_provider`/`sticky_model` + `current_tier` aggiornato + meta_step
    ///     `scale_applied` + self-loop (`G1Escalated`, ri-fa il turno col nuovo
    ///     modello). Se `None` (nessun modello del tier con finestra/capability):
    ///     ANNULLA il cambio (fail-safe, resta sul modello corrente), log strutturato.
    /// Aggiorna `scale_last_change_iter`/`scale_reversal_count`/`scale_last_direction`
    /// al cambio effettivo (clone-whole-map preservato).
    ///
    /// FIX-C (F5): applica il tetto `max_tier_changes_per_run`. Conta i cambi-tier
    /// APPLICATI (`scale_tier_changes_used`); al raggiungimento del tetto forza il
    /// target a Heavy (pin-UP safety-biased) e marca `scale_pinned_heavy`, cosi' il
    /// detector non emette piu' e il rientro non applica piu' mosse (mig 0516). Se il
    /// run e' gia' pinnato -> `None` (mantiene Heavy).
    async fn consume_scale_move(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        // Contesto + chiave-cache trasportati dal detector (produttore). Assenti ->
        // guasto a monte: prosegui il turno normale (nessuna marcatura).
        let scale_ctx: crate::runtime::ports::ScaleContext = state
            .extra
            .get(SCALE_CONTEXT_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())?;
        let key = state
            .extra
            .get(SCALE_MOVE_CACHE_KEY_KEY)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| scale_cache_key(&scale_ctx, self.cfg.scale.eval_every_iters));
        // Mossa persistita dal nodo (KeepTier NON viene persistito: assenza = keep).
        let mv: ScaleMove = state
            .extra
            .get(&key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())?;

        // Ramo SIZING (mig 0524): AdjustSizing NON tocca il tier. Risolve la postura in
        // override concreti, li persiste STICKY e ri-fa il turno con le soglie adattive
        // (nessuna risoluzione modello/tier). Deviato QUI prima della macchina tier.
        if let ScaleMove::AdjustSizing { posture, .. } = &mv {
            return self
                .consume_sizing_move(state, iters_in, *posture, &scale_ctx, ctx)
                .await;
        }

        let (raw_target_tier, raw_is_down) = match &mv {
            // KeepTier: nessun cambio, prosegue il turno normale (rete di sicurezza).
            ScaleMove::KeepTier => return None,
            ScaleMove::UpscaleTo { tier, .. } => (*tier, false),
            ScaleMove::DownscaleTo { tier, .. } => (*tier, true),
            // Gia' deviato sopra (early return): arm difensivo per esaustivita' (regola
            // L, niente `_`). Irraggiungibile in pratica.
            ScaleMove::AdjustSizing { .. } => return None,
        };

        // ── FIX-C (F5): tetto cambi-tier per run + pin-heavy (mig 0516) ────────
        // `agent.scale.max_tier_changes_per_run`: cambi-tier EFFETTIVI massimi per
        // run. E' semanticamente distinto da `max_reversals` (inversioni A->B->A,
        // gia' in apply_hysteresis) e da `max_evals_per_run` (valutazioni LLM): qui
        // si contano i cambi APPLICATI. Al raggiungimento il run "pinna heavy e
        // disattiva" (descrizione mig 0516): l'ultimo cambio consentito e' forzato a
        // Heavy (pin-UP safety-biased, non mute secco) e da li' in poi il detector
        // non emette piu' (guard `scale_pinned_heavy`). Se il run e' GIA' pinnato
        // (guasto/ri-emissione residua), nessun cambio -> mantiene Heavy.
        if Self::scale_pinned_heavy(state) {
            tracing::debug!(
                target: "nexus_agent_graph::executor",
                "scale-controller: run gia' pinnato heavy (tetto cambi-tier), nessun cambio"
            );
            return None;
        }
        let changes_used = Self::scale_tier_changes_used(state);
        let cap = self.cfg.scale.max_tier_changes_per_run;
        // Questo cambio RAGGIUNGE il tetto se, applicandolo, il contatore arriva al
        // cap (`cap > 0`, altrimenti il tetto e' disattivato). In tal caso si pinna a
        // Heavy invece del target proposto dall'LLM.
        let reaches_cap = cap > 0 && changes_used + 1 >= cap;
        let (target_tier, is_down) = if reaches_cap {
            // Pin-UP a Heavy: la direzione diventa "up" se Heavy e' sopra il corrente.
            (
                ScaleTier::Heavy,
                ScaleTier::Heavy.rank() < scale_ctx.current_tier.rank(),
            )
        } else {
            (raw_target_tier, raw_is_down)
        };

        // Caso limite: il tetto e' raggiunto ma il pin (Heavy) coincide col tier
        // corrente (run gia' su heavy). Non c'e' cambio-modello da applicare, ma il
        // run va comunque "disattivato": marca `scale_pinned_heavy` (e sincronizza il
        // contatore al cap) senza ri-fare il turno con un G1Escalated inutile.
        if reaches_cap && target_tier == scale_ctx.current_tier {
            let mut extra_out = state.extra.clone();
            extra_out.insert("scale_pinned_heavy".to_string(), json!(true));
            extra_out.insert(
                "scale_tier_changes_used".to_string(),
                json!(cap.max(changes_used)),
            );
            tracing::info!(
                target: "nexus_agent_graph::executor",
                current_tier = scale_ctx.current_tier.as_str(),
                changes_used,
                cap,
                "scale-controller: tetto cambi-tier raggiunto, run gia' su Heavy -> pin+disattiva (nessun cambio)"
            );
            return Some(
                StateDelta {
                    stop_reason: Some(Some(StopReason::G1Escalated)),
                    iterations: Some(Some(iters_in + 1)),
                    extra: Some(extra_out),
                    ..Default::default()
                }
                .into_opaque(),
            );
        }

        // FIX-B: fabbisogno finestra = est_tokens * overhead. Nel downscale garantisce
        // che il tier piu' basso abbia finestra sufficiente (mai troncamento); nell'
        // upscale e' comunque una soglia minima sana.
        let overhead = if self.cfg.scale.window_overhead_ratio.is_finite()
            && self.cfg.scale.window_overhead_ratio >= 1.0
        {
            self.cfg.scale.window_overhead_ratio
        } else {
            1.0
        };
        let required = (scale_ctx.est_tokens.max(0) as f64 * overhead).ceil() as i64;

        // Risolve il modello del tier target dietro la porta (regola L: MAI
        // select_agentic_model direttamente dal nodo).
        let resolved = self
            .upscale
            .select_model_for_tier(target_tier.as_str(), required, None, &[])
            .await
            .unwrap_or(None);

        let Some((provider, model)) = resolved else {
            // Fail-safe: nessun modello del tier target con finestra/capability
            // sufficiente -> ANNULLA il cambio, resta sul modello corrente. Log
            // strutturato (segnale, non prosa): l'esito e' None dalla porta.
            tracing::warn!(
                target: "nexus_agent_graph::executor",
                from_tier = scale_ctx.current_tier.as_str(),
                to_tier = target_tier.as_str(),
                downscale = is_down,
                required_window = required,
                "scale-controller: nessun modello del tier target (finestra/capability), cambio ANNULLATO"
            );
            // FIX-C (F5): se questo cambio RAGGIUNGE il tetto ma il pin-Heavy non e'
            // risolvibile (tier heavy non popolato / tutti in cooldown / finestra
            // insufficiente), il tetto deve comunque DISATTIVARE il controller (mig
            // 0516), esattamente come il caso "gia' su Heavy" sopra: altrimenti il
            // detector ri-emette a ogni cadenza ri-tentando il pin fallito fino a
            // esaurire `max_evals_per_run` (consultazioni LLM sprecate). Marca
            // `scale_pinned_heavy` e sincronizza il contatore al cap; il G1Escalated e'
            // al rientro (pre-LLM) -> salva l'extra e prosegue, nessuna complete persa.
            if reaches_cap {
                let mut extra_out = state.extra.clone();
                extra_out.insert("scale_pinned_heavy".to_string(), json!(true));
                extra_out.insert(
                    "scale_tier_changes_used".to_string(),
                    json!(cap.max(changes_used)),
                );
                tracing::info!(
                    target: "nexus_agent_graph::executor",
                    changes_used,
                    cap,
                    "scale-controller: tetto cambi-tier raggiunto ma pin-Heavy non risolvibile -> pin+disattiva (nessun cambio)"
                );
                return Some(
                    StateDelta {
                        stop_reason: Some(Some(StopReason::G1Escalated)),
                        iterations: Some(Some(iters_in + 1)),
                        extra: Some(extra_out),
                        ..Default::default()
                    }
                    .into_opaque(),
                );
            }
            return None;
        };

        // Cambio applicato: aggiorna il tracking anti-oscillazione in extra
        // (clone-whole-map). Direzione corrente per il reversal-count.
        let mut extra_out = state.extra.clone();
        extra_out.insert("scale_last_change_iter".to_string(), json!(iters_in));
        let cur_dir = if is_down { "down" } else { "up" };
        let prev_dir = state
            .extra
            .get("scale_last_direction")
            .and_then(Value::as_str);
        if let Some(prev) = prev_dir {
            if prev != cur_dir {
                let rc = Self::scale_reversal_count(state) + 1;
                extra_out.insert("scale_reversal_count".to_string(), json!(rc));
            }
        }
        extra_out.insert("scale_last_direction".to_string(), json!(cur_dir));
        // Grazia sui detector di ripetizione (come l'escalation): il modello del nuovo
        // tier riparte con finestra pulita.
        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
        // FIX-C (F5): conta il cambio-tier APPLICATO. Al raggiungimento del tetto
        // `max_tier_changes_per_run` il target e' gia' stato forzato a Heavy sopra
        // (`reaches_cap`): marca `scale_pinned_heavy` cosi' il detector non emette piu'
        // (pin heavy + disattiva, mig 0516).
        let new_changes_used = changes_used + 1;
        extra_out.insert(
            "scale_tier_changes_used".to_string(),
            json!(new_changes_used),
        );
        if reaches_cap {
            extra_out.insert("scale_pinned_heavy".to_string(), json!(true));
            tracing::info!(
                target: "nexus_agent_graph::executor",
                changes_used = new_changes_used,
                cap,
                "scale-controller: tetto cambi-tier raggiunto -> pin a Heavy + disattiva ulteriori cambi"
            );
        }

        let direction_label = if is_down { "down" } else { "up" };
        self.emit_phase(
            ctx,
            "scale_applied",
            format!(
                "Scala {direction_label} a {provider}/{model} (tier {})",
                target_tier.as_str()
            ),
            json!({
                "from_tier": scale_ctx.current_tier.as_str(),
                "to_tier": target_tier.as_str(),
                "to_provider": provider,
                "to_model": model,
                "downscale": is_down,
            }),
        )
        .await;
        tracing::info!(
            target: "nexus_agent_graph::executor",
            from_tier = scale_ctx.current_tier.as_str(),
            to_tier = target_tier.as_str(),
            to_provider = %provider,
            to_model = %model,
            downscale = is_down,
            "scale-controller: ScaleMove applicata -> sticky + current_tier + ri-do il turno"
        );

        Some(
            StateDelta {
                sticky_provider: Some(Some(provider)),
                sticky_model: Some(Some(model)),
                current_tier: Some(Some(target_tier.as_str().to_string())),
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

    /// CONSUMO di una [`ScaleMove::AdjustSizing`] (mig 0524, gemello di
    /// [`Self::consume_scale_move`] ma su direzione DISGIUNTA: DIMENSIONAMENTO, non
    /// tier). Risolve la postura in [`SizingOverrides`] concreti col PUNTO UNICO
    /// `resolve_sizing_overrides` (proporzionali ai segnali dello ScaleContext), li
    /// persiste STICKY in `extra[SCALE_SIZING_OVERRIDES_KEY]`, aggiorna il cooldown del
    /// sizing (`scale_sizing_last_iter`) e ri-fa il turno (`G1Escalated`) cosi' il
    /// blocco di riduzione contesto riparte con le soglie ADATTIVE. Nessuna
    /// risoluzione modello/tier (il sizing e' ortogonale al tier).
    ///
    /// `Hold` / override effettivo vuoto -> `None` (prosegue il turno, nessun redo).
    async fn consume_sizing_move(
        &self,
        state: &AgentState,
        iters_in: i64,
        posture: crate::runtime::ports::SizingPosture,
        scale_ctx: &crate::runtime::ports::ScaleContext,
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        // Baseline = soglie FISSE correnti (regola L: il calcolo puro non tocca il DB;
        // il produttore risolve i valori da ExecutorConfig e li passa al risolutore).
        let baseline = SizingBaseline {
            compress_start_iter: self.cfg.ctx_mgmt.compress_start_iter,
            compress_phase_keep_recent: self.cfg.ctx_mgmt.compress_phase_keep_recent.clone(),
            compress_phase_max_chars: self.cfg.ctx_mgmt.compress_phase_max_chars.clone(),
            token_brake_max_context_ratio: self.cfg.token_brake.max_context_ratio,
            rolling_keep_recent: self.cfg.rolling_keep_recent,
        };
        let sizing_cfg = ScaleSizingConfig {
            enabled: self.cfg.scale.sizing_enabled,
            min_confidence: self.cfg.scale.min_confidence,
            cooldown_turns: self.cfg.scale.sizing_cooldown_turns,
            aggressiveness: self.cfg.scale.sizing_aggressiveness,
        };
        let overrides = resolve_sizing_overrides(posture, scale_ctx, &baseline, &sizing_cfg);
        // Hold / nessun override effettivo -> non ri-fare il turno (prosegue normale).
        if overrides == SizingOverrides::default() {
            return None;
        }
        let value = serde_json::to_value(&overrides).ok()?;

        // Clone-whole-map: persiste gli override STICKY + il cooldown del sizing senza
        // azzerare gli altri canali extra (auto_escalations, scale_*, ...).
        let mut extra_out = state.extra.clone();
        extra_out.insert(SCALE_SIZING_OVERRIDES_KEY.to_string(), value);
        extra_out.insert("scale_sizing_last_iter".to_string(), json!(iters_in));

        let posture_label = match posture {
            crate::runtime::ports::SizingPosture::Compact => "compact",
            crate::runtime::ports::SizingPosture::Relax => "relax",
            crate::runtime::ports::SizingPosture::Hold => "hold",
        };
        // Narrazione live: la chat spiega che il motore ha adattato il dimensionamento.
        self.emit_phase(
            ctx,
            "sizing_applied",
            format!("Dimensionamento adattato ({posture_label})"),
            json!({
                "posture": posture_label,
                "compress_start_iter": overrides.compress_start_iter,
                "rolling_summary_enabled": overrides.rolling_summary_enabled,
                "token_brake_max_context_ratio": overrides.token_brake_max_context_ratio,
                "g1_loop_threshold_mult": overrides.g1_loop_threshold_mult,
            }),
        )
        .await;
        tracing::info!(
            target: "nexus_agent_graph::executor",
            posture = posture_label,
            iterations = iters_in,
            "scale-controller: SIZING applicato -> override sticky + ri-do il turno"
        );

        Some(
            StateDelta {
                stop_reason: Some(Some(StopReason::G1Escalated)),
                iterations: Some(Some(iters_in + 1)),
                extra: Some(extra_out),
                ..Default::default()
            }
            .into_opaque(),
        )
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
                thinking_signature: None,
            }]),
            result: Some(Some(close_text)),
            pending_tool_uses: Some(Some(vec![])),
            stop_reason: Some(Some(StopReason::EndTurn)),
            iterations: Some(Some(iters_in + 1)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Chiusura deterministica anti-runaway per un limite basato sui TOKEN
    /// (budget cumulativo o turni solo-testo consecutivi). PUNTO UNICO (regola L)
    /// della costruzione del delta di force-close per questi due limiti: replica
    /// ESATTAMENTE il ramo `iteration_cap` (message assistant + `result` + pending
    /// vuoto + `EndTurn` + `iterations+1` + `forced_close_unverified=true`) cosi'
    /// il run NON finisce mai "completed" e il finalizzatore lo mappa a
    /// FailedDiagnosed. Emette un `MetaStep` con `reason` strutturato + valore
    /// (regola M: segnale strutturato, non testo) per la telemetria/chat.
    fn close_runaway(
        &self,
        iters_in: i64,
        close_text: String,
        reason: &str,
        meta_payload: Value,
    ) -> OpaqueDelta {
        StateDelta {
            messages: Some(vec![Message::Ai {
                content: MessageContent::text(close_text.clone()),
                tool_calls: vec![],
                reasoning: None,
                thinking_signature: None,
            }]),
            meta_steps: Some(vec![MetaStep {
                kind: "anti_runaway".to_string(),
                title: "Run interrotto per limite di sicurezza".to_string(),
                payload: json!({ "reason": reason, "detail": meta_payload }),
                correlation_id: None,
                created_at: None,
            }]),
            result: Some(Some(close_text)),
            pending_tool_uses: Some(Some(vec![])),
            stop_reason: Some(Some(StopReason::EndTurn)),
            iterations: Some(Some(iters_in + 1)),
            forced_close_unverified: Some(Some(true)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Turno di grazia figura al BACKSTOP runaway. Stesso concetto di
    /// [`recovery_nudge_msg`] (regola L: canale di grazia figura), ma applicato dove
    /// il meta-reasoner NON interviene e si andrebbe dritti a [`Self::close_runaway`]
    /// con esito n/d: e' il percorso reale in cui una figura del consiglio (es.
    /// `functional_analyst` deepseek, run fe4dc12c) muore al cap solo-testo dopo aver
    /// esaurito il budget stall. Se il run e' una figura senza parere
    /// ([`pending_role_channel_grace`]) e la grazia non e' ancora stata concessa,
    /// invece di chiudere concede UN turno mirato per dichiarare col canale del
    /// ruolo (parere/posizione reale al posto di n/d). Una-tantum
    /// ([`ADVISORY_GRACE_USED_KEY`]) per non ciclare. `None` -> il chiamante procede
    /// col `close_runaway` (bit-identico per i run senza canale di ruolo / grazia
    /// gia' concessa).
    /// SOLLECITO DI CHIUSURA a tempo: quando il budget del run e' oltre la soglia
    /// [`ExecutorConfig::time_grace_pct`], invece di lasciar morire muto un canale
    /// di ruolo ancora aperto gli concede il turno di grazia per dichiarare.
    ///
    /// `None` (il chiamante prosegue, comportamento invariato) se: il sollecito e'
    /// disabilitato (`time_grace_pct == 0`), la soglia non e' raggiunta, oppure non
    /// c'e' un canale di ruolo da sollecitare / la grazia e' gia' stata concessa
    /// (decide il punto unico [`Self::maybe_advisory_grace_delta`], che qui viene
    /// solo INNESCATO su un secondo criterio — il tempo invece dei turni a vuoto).
    async fn maybe_time_grace_delta(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
        elapsed_s: u64,
    ) -> Option<OpaqueDelta> {
        if self.cfg.time_grace_pct == 0 {
            return None;
        }
        let soglia_s = self.cfg.run_time_budget_s * self.cfg.time_grace_pct / 100;
        if elapsed_s < soglia_s {
            return None;
        }
        self.maybe_advisory_grace_delta(state, iters_in, ctx)
            .await
    }

    async fn maybe_advisory_grace_delta(
        &self,
        state: &AgentState,
        iters_in: i64,
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        self.maybe_advisory_grace_delta_preserving(state, None, iters_in, ctx)
            .await
    }

    /// Come [`Self::maybe_advisory_grace_delta`], ma puo' PRESERVARE il messaggio
    /// assistant del turno corrente (`preserve`): sul call site della chiusura
    /// volontaria la prosa diagnostica del modello e' il suo resoconto e non va
    /// buttata — la grazia le si accoda. Nei call site pre-LLM `preserve` e'
    /// `None` e il comportamento e' bit-identico allo storico.
    async fn maybe_advisory_grace_delta_preserving(
        &self,
        state: &AgentState,
        preserve: Option<Message>,
        iters_in: i64,
        ctx: &AgentNodeCtx,
    ) -> Option<OpaqueDelta> {
        let directive = pending_role_channel_grace(state)?;
        if state
            .extra
            .get(ADVISORY_GRACE_USED_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }
        let mut extra_out = state.extra.clone();
        extra_out.insert(ADVISORY_GRACE_USED_KEY.to_string(), json!(true));
        // La grazia non e' piu' solo prosa: il turno successivo diventa un TURNO
        // DICHIARATIVO DI RUOLO (catalogo ridotto al solo tool del canale +
        // tool_choice=required, stesso meccanismo di ADR 0034 per task_complete).
        // Misurato il perche': la sola direttiva testuale aveva efficacia 1/5 —
        // e' lo stesso tipo di segnale che il modello muto sta gia' ignorando.
        // L'obbligo del quorum diventa un vincolo di macchina, non una preghiera.
        extra_out.insert("force_role_declaration".to_string(), json!(true));
        // Finestra pulita sui detector di ripetizione (come i rami nudge fissi).
        extra_out.insert("repeat_scan_floor".to_string(), json!(state.messages.len()));
        self.emit_phase(
            ctx,
            "advisory_grace",
            "Turno di grazia: il ruolo chiude col proprio verdetto".to_string(),
            json!({ "iters": iters_in }),
        )
        .await;
        tracing::warn!(
            target: "nexus_agent_graph::executor",
            iters = iters_in,
            "ruolo senza verdetto al backstop runaway -> turno di grazia sul canale proprio"
        );
        // La prosa del turno corrente (chiusura volontaria) precede la direttiva:
        // e' il resoconto del modello e resta in conversazione.
        let mut msgs = Vec::with_capacity(2);
        if let Some(m) = preserve {
            msgs.push(m);
        }
        msgs.push(human_msg(directive.trim()));
        Some(
            StateDelta {
                messages: Some(msgs),
                // Azzera lo streak solo-testo: al re-entry il blocco text-only NON
                // ri-scatta prima di chiamare l'LLM, cosi' il turno di grazia raggiunge
                // davvero il modello (che puo' emettere advisory_verdict). Senza
                // l'azzeramento il re-entry richiuderebbe subito senza dare il turno.
                consecutive_text_only_turns: Some(Some(0)),
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
/// Chiave in `state.extra` dello streak di fallimenti gateway deterministici.
const GW_DET_STREAK_KEY: &str = "gw_det_streak";

/// Nome-causa se il fallimento del gateway e' DETERMINISTICO — cioe' rifarebbe
/// la stessa richiesta e otterrebbe la stessa risposta — altrimenti `None`.
///
/// Deterministici: `EmptyCompletion` (risposta degenere ripetibile) e
/// `ClientError` NON recuperabile (fuori dalla whitelist DB: il 4xx si ripete
/// identico). Transitori (fuori): cooldown, billing, transient — possono
/// risolversi da soli e un tetto li chiuderebbe ingiustamente.
fn deterministic_gateway_cause(
    err: &crate::runtime::ports::PortError,
    recoverable_codes: &[String],
) -> Option<&'static str> {
    use crate::runtime::ports::{PortError, ProviderFailureCause as Cause};
    let PortError::ProviderUnavailable(pu) = err else {
        return None;
    };
    match pu.cause {
        Cause::EmptyCompletion => Some("empty_completion"),
        Cause::ClientError if !pu.allows_cross_provider_failover(recoverable_codes) => {
            Some("client_error_non_recuperabile")
        }
        _ => None,
    }
}

/// Aggiorna lo streak dei fallimenti deterministici: stessa (provider, model,
/// causa) su iterazioni CONTIGUE -> count+1, altrimenti riparte da 1. La
/// contiguita' (`last_iter + 1 == iters_in`) rende il reset implicito: un turno
/// riuscito consuma un'iterazione senza toccare lo streak, e il fallimento
/// successivo non risulta piu' contiguo. Ritorna `(count, valore da persistere)`.
fn next_deterministic_streak(
    prev: Option<&Value>,
    provider: &str,
    model: &str,
    cause: &str,
    iters_in: i64,
) -> (u64, Value) {
    let contiguo = prev.is_some_and(|p| {
        p.get("provider").and_then(Value::as_str) == Some(provider)
            && p.get("model").and_then(Value::as_str) == Some(model)
            && p.get("cause").and_then(Value::as_str) == Some(cause)
            && p.get("last_iter").and_then(Value::as_i64) == Some(iters_in - 1)
    });
    let count = if contiguo {
        prev.and_then(|p| p.get("count").and_then(Value::as_u64))
            .unwrap_or(0)
            + 1
    } else {
        1
    };
    let val = json!({
        "provider": provider,
        "model": model,
        "cause": cause,
        "count": count,
        "last_iter": iters_in,
    });
    (count, val)
}

/// Esito del gate sui fallimenti gateway deterministici.
enum DetGate {
    /// Causa non deterministica (o errore non tipizzato): il tetto non si applica.
    NonDeterministico,
    /// Sotto soglia: lo streak aggiornato va persistito nel delta del turno.
    Under(Value),
    /// Soglia raggiunta: chiusura onesta, il delta e' pronto.
    Close(OpaqueDelta),
}

/// Gate del tetto sui fallimenti deterministici (stessa coppia provider/model,
/// stessa causa, iterazioni CONTIGUE): rifare la stessa chiamata produrrebbe la
/// stessa risposta, quindi oltre `gateway_deterministic_streak_max` si chiude
/// con esito onesto invece di ritentare fino al budget (mig 0619).
fn deterministic_streak_gate(
    state: &AgentState,
    cfg: &ExecutorConfig,
    err: &crate::runtime::ports::PortError,
    provider: &str,
    model: &str,
    iters_in: i64,
) -> DetGate {
    let Some(cause) = deterministic_gateway_cause(err, &cfg.recoverable_client_error_codes) else {
        return DetGate::NonDeterministico;
    };
    let (streak, streak_val) = next_deterministic_streak(
        state.extra.get(GW_DET_STREAK_KEY),
        provider,
        model,
        cause,
        iters_in,
    );
    let soglia = cfg.gateway_deterministic_streak_max;
    if soglia > 0 && streak >= soglia {
        return DetGate::Close(deterministic_close_delta(
            state, provider, model, cause, streak, streak_val, iters_in,
        ));
    }
    DetGate::Under(streak_val)
}

/// Delta di CHIUSURA per fallimento gateway deterministico oltre soglia:
/// messaggio onesto, `stop_reason=Error` ed `error_class` strutturato (stessa
/// forma del gemello context_overflow). Estratto dal ramo err dell'executor.
fn deterministic_close_delta(
    state: &AgentState,
    provider: &str,
    model: &str,
    cause: &str,
    streak: u64,
    streak_val: Value,
    iters_in: i64,
) -> OpaqueDelta {
    let text = format!(
        "[Errore provider {provider}/{model}: {streak} turni consecutivi falliti con causa \
         deterministica '{cause}'. Ritentare produrrebbe lo stesso esito: chiudo il run.]"
    );
    tracing::warn!(
        target: "nexus_agent_graph::executor",
        provider = %provider,
        model = %model,
        cause,
        streak,
        "fallimento gateway deterministico oltre soglia: chiusura onesta"
    );
    let mut extra = state.extra.clone();
    extra.insert("error_class".to_string(), json!("gateway_deterministic"));
    extra.insert(GW_DET_STREAK_KEY.to_string(), streak_val);
    StateDelta {
        messages: Some(vec![Message::Ai {
            content: MessageContent::text(text.clone()),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }]),
        result: Some(Some(text)),
        pending_tool_uses: Some(Some(vec![])),
        stop_reason: Some(Some(StopReason::Error)),
        iterations: Some(Some(iters_in + 1)),
        extra: Some(extra),
        ..Default::default()
    }
    .into_opaque()
}

/// Il messaggio UMANO di un [`PortError`], per il testo sintetico in chat.
///
/// Qui viveva `compact_provider_error`, che tagliava la stringa alla prima `{`
/// e a 200 caratteri. Era una toppa (regola H) travestita da traduzione: decideva
/// guardando il TESTO (regola M), funzionava solo sui body JSON, e lasciava
/// passare integro qualunque `Debug` senza graffa — il `MetadataMap { headers:
/// ... }` di tonic e la catena `io(ConnectionRefused, os_error=10061)` del
/// trasporto arrivavano in chat tali e quali.
///
/// Ora la frase arriva gia' fatta dal punto unico di presentazione: i tipi
/// d'errore la portano nel loro [`RenderedError`], costruito dove i segnali
/// strutturati (status, codice, natura del trasporto) erano ancora vivi. Il
/// MARKER `[Errore provider` del chiamante non cambia: la detection
/// dell'esito-certo in mcp-core (`is_provider_error_answer`) resta valida.
pub(crate) fn port_error_message(err: &PortError) -> String {
    let msg = match err {
        PortError::Llm(r) | PortError::Tool(r) => r.message.clone(),
        // Il Display di queste varianti e' gia' una frase costruita a mano, non
        // il travaso di un errore esterno.
        altro => altro.to_string(),
    };
    if msg.trim().is_empty() {
        "richiesta rifiutata dal provider (dettaglio tecnico nei log del run)".to_string()
    } else {
        msg
    }
}

fn is_forcing_failure(resp: &crate::runtime::ports::LlmResponse) -> bool {
    resp.stop_reason.as_deref() == Some("error")
}

/// Crea un `Message::Human` con testo (per i nudge iniettati).
fn human_msg(text: &str) -> Message {
    Message::Human {
        content: MessageContent::text(text),
    }
}

/// Direttiva deterministica di GRAZIA per una figura del consiglio di analisi
/// impantanata SENZA parere dichiarato: la spinge a CHIUDERE col proprio canale
/// (`advisory_verdict`) invece di continuare a esplorare o di "diagnosticare un
/// fallimento" (che sfocia in `failed_diagnosed` -> n/d al posto del parere).
/// Viene appesa al nudge di recovery (vedi [`recovery_nudge_msg`]).
const ADVISORY_GRACE_DIRECTIVE: &str = "\n\nHai gia' raccolto contesto sufficiente. \
CHIUDI ORA la tua analisi chiamando il tool advisory_verdict con la tua migliore \
valutazione corrente (verdict + summary + eventuali requirements). NON continuare a \
esplorare e NON diagnosticare un fallimento tecnico: emetti il tuo parere consultivo \
adesso, anche se parziale, basandoti su cio' che hai gia' osservato.";

/// Direttiva di GRAZIA per un AVVOCATO del dibattito impantanato senza posizione
/// dichiarata. Gemella di [`ADVISORY_GRACE_DIRECTIVE`] sul canale del dibattito:
/// un avvocato che tace lascia la sua tesi senza voce, e una tesi indifesa non
/// perde — falsa il confronto (il panel se ne accorge e dichiara `inconclusive`,
/// ma la spesa e' gia' fatta). Meglio una posizione parziale ma dichiarata.
const DEBATE_GRACE_DIRECTIVE: &str = "\n\nHai gia' raccolto prove sufficienti. \
CHIUDI ORA la tua arringa chiamando il tool debate_position con la tua conclusione \
corrente (assigned_position ripetuta alla lettera, stance, key_arguments). NON \
continuare a esplorare e NON diagnosticare un fallimento tecnico: se la tua posizione \
regge dichiara support, se studiando hai visto che non regge dichiara oppose coi \
rischi trovati. Tacere lascerebbe la tua tesi senza difensore e falserebbe il \
dibattito.";

/// Chiave `extra` che marca la grazia figura come GIA' concessa (una-tantum): il
/// backstop runaway ([`ExecutorNode::maybe_advisory_grace_delta`]) la concede una
/// sola volta per run, per non ciclare (grazia -> di nuovo cap -> grazia).
const ADVISORY_GRACE_USED_KEY: &str = "advisory_grace_used";

/// `true` se il `tools_json` del run espone il tool dato (segnale strutturale).
fn has_tool(state: &AgentState, tool: &str) -> bool {
    state.tools_json.as_ref().is_some_and(|tools| {
        tools
            .iter()
            .any(|t| t.get("name").and_then(Value::as_str) == Some(tool))
    })
}

/// Direttiva di grazia per un run che ha un canale di chiusura di RUOLO ancora
/// INUTILIZZATO; `None` per tutti gli altri (run principale, revisori, o ruoli
/// che hanno gia' dichiarato).
///
/// PUNTO UNICO (regola L) del riconoscimento: il concern e' "questo ruolo deve
/// chiudere col PROPRIO canale e non l'ha ancora fatto", identico per la figura
/// del consiglio e per l'avvocato del dibattito — cambia solo la direttiva.
/// Senza questa forma, ogni nuovo ruolo con canale proprio avrebbe richiesto di
/// duplicare lo stesso `if` in tre call site.
///
/// Segnale STRUTTURALE (regola M): presenza del tool nel `tools_json` + assenza
/// della dichiarazione in stato — nessun parsing di prosa. Per un ruolo in questo
/// stato una mossa di recovery generica ("diagnostica il fallimento") produrrebbe
/// `failed_diagnosed` invece del contributo atteso: il turno di grazia lo dirotta
/// a DICHIARARE con la sua miglior stima corrente.
///
/// Ordine dei rami: un kind ha UN solo canale di ruolo (le whitelist di 0546 e
/// 0605 sono disgiunte), quindi non c'e' ambiguita' reale; l'advisory resta
/// primo per continuita' col comportamento storico.
/// Canale di ruolo ancora muto: il TOOL da esigere e la direttiva per il modello.
struct RoleChannel {
    tool: &'static str,
    directive: &'static str,
}

/// PUNTO UNICO del riconoscimento "questo ruolo deve chiudere col proprio canale
/// e non l'ha ancora fatto". Ritorna il canale completo (tool + direttiva): il
/// turno di grazia usa la direttiva, il turno dichiarativo di ruolo usa il nome
/// del tool per ridurre il catalogo e FORZARE la dichiarazione.
fn pending_role_channel(state: &AgentState) -> Option<RoleChannel> {
    if state.advisory_verdict.is_none() && has_tool(state, "advisory_verdict") {
        return Some(RoleChannel {
            tool: "advisory_verdict",
            directive: ADVISORY_GRACE_DIRECTIVE,
        });
    }
    if state.debate_position.is_none() && has_tool(state, "debate_position") {
        return Some(RoleChannel {
            tool: "debate_position",
            directive: DEBATE_GRACE_DIRECTIVE,
        });
    }
    None
}

fn pending_role_channel_grace(state: &AgentState) -> Option<&'static str> {
    pending_role_channel(state).map(|c| c.directive)
}

/// Costruisce il messaggio-nudge di recovery applicando il turno di grazia di
/// ruolo: se il run ha un canale di chiusura proprio non ancora usato (vedi
/// [`pending_role_channel_grace`]), appende la direttiva corrispondente al testo
/// del reasoner cosi' il ruolo chiude col proprio verdetto invece di
/// diagnosticare un fallimento. Altrimenti bit-identico a `human_msg(nudge)`.
fn recovery_nudge_msg(state: &AgentState, nudge: &str) -> Message {
    match pending_role_channel_grace(state) {
        Some(directive) => human_msg(&format!("{nudge}{directive}")),
        None => human_msg(nudge),
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
fn build_assistant_message(
    resp: &crate::runtime::ports::LlmResponse,
    result_text: &str,
) -> Message {
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
            // Firma thinking Anthropic (per-messaggio): preservata per il
            // round-trip nei turni con tool. Vuota -> None.
            thinking_signature: resp
                .thinking_signature
                .as_ref()
                .filter(|s| !s.is_empty())
                .cloned(),
        };
    }
    // Forma minimale: testo + tool_calls (OpenAI-compat).
    Message::Ai {
        content: MessageContent::text(result_text),
        tool_calls: resp.tool_calls.clone(),
        reasoning: resp.reasoning.as_ref().filter(|r| !r.is_empty()).cloned(),
        thinking_signature: resp
            .thinking_signature
            .as_ref()
            .filter(|s| !s.is_empty())
            .cloned(),
    }
}

/// Mappa un [`Message`] del canale interno in [`HistoryMessage`] (forma su cui
/// operano le primitive di context_reduction): `is_human` dal ruolo, `content`
/// testo o blocchi, `anthropic_content` i blocchi se presenti.
fn message_to_history(m: &Message) -> HistoryMessage {
    match m {
        Message::Human { content } => history_from_content(content, true),
        Message::Ai {
            content,
            tool_calls,
            reasoning,
            thinking_signature,
        } => {
            // Se l'AI porta tool_calls (forma OpenAI-compat) ma content testuale,
            // espandiamo i tool_use in anthropic_content per la dedup/compress.
            let mut hm = history_from_content(content, false);
            if hm.anthropic_content.is_null() && !tool_calls.is_empty() {
                hm.anthropic_content = Value::Array(
                    tool_calls
                        .iter()
                        .map(|t| {
                            let mut b = json!({"type": "tool_use", "id": t.id, "name": t.name, "input": t.input});
                            // Firma PER-CALL (Gemini 3): preservata nell'espansione
                            // tool_calls -> anthropic_content per il round-trip.
                            if let Some(sig) = &t.thought_signature {
                                b["thought_signature"] = json!(sig);
                            }
                            b
                        })
                        .collect(),
                );
            }
            // Reasoning DeepSeek del turno: preservato per il round-trip al gateway.
            hm.reasoning = reasoning.clone();
            // Firma thinking Anthropic (per-messaggio): preservata attraverso la
            // riduzione di contesto per il round-trip nei turni con tool.
            hm.thinking_signature = thinking_signature.clone();
            hm
        }
        // Il `ToolMessage` (risultato) preserva ruolo e id: `history_to_llm_messages`
        // ne ricostruisce il `role="tool"` + `tool_call_id` per il wire (continuita'
        // tool_use/tool_result, bug 2026-06-26). Senza questi campi il messaggio
        // verrebbe degradato ad assistant testuale e Anthropic risponderebbe HTTP
        // 400 (`tool_use ids without tool_result`). La compressione che lo riscrive
        // azzera questi flag (vedi `HistoryMessage::rebuilt_human`).
        Message::Tool {
            content,
            tool_call_id,
        } => {
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
                        // Round-trip firma thinking Anthropic (per-messaggio).
                        thinking_signature: m.thinking_signature.clone(),
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
                reasoning: if m.is_human {
                    None
                } else {
                    m.reasoning.clone()
                },
                // Round-trip firma thinking Anthropic: solo sugli assistant.
                thinking_signature: if m.is_human {
                    None
                } else {
                    m.thinking_signature.clone()
                },
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
        reasoning: if m.is_human {
            None
        } else {
            m.reasoning.clone()
        },
        // Round-trip firma thinking Anthropic: solo sugli assistant.
        thinking_signature: if m.is_human {
            None
        } else {
            m.thinking_signature.clone()
        },
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
            let name = b
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
            // Firma PER-CALL (Gemini 3): estratta dal blocco tool_use per essere
            // ri-passata sulla stessa functionCall nel round-trip verso il gateway.
            let thought_signature = b
                .get("thought_signature")
                .and_then(Value::as_str)
                .map(String::from);
            Some(ToolUse {
                id,
                name,
                input,
                thought_signature,
            })
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
