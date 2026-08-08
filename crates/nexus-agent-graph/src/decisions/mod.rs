//! `decisions`: funzioni PURE decisionali del grafo agentico, portate 1:1 dal
//! brain Python (FASE 2a del porting LangGraph -> Rust).
//!
//! Punto unico (regola L) di ciascun concern decisionale: l'eventuale chiamante
//! Rust delega qui invece di re-implementare la logica. Tutte le funzioni sono
//! pure (nessun IO, nessuna lettura DB): la config DB-driven arriva come
//! parametro esplicito (regola G), cosi' resta deterministica e testabile.
//!
//! Moduli:
//!   - [`context_reduction`]: parte PURA della riduzione del contesto dell'executor
//!     (decisione di fase, dedup tool_result, drop base64, compress, freno token, 5
//!     iniezioni system). Le parti I/O (summary LLM, offload/continuity embeddings)
//!     restano TODO -> trait futuri; due confini I/O parametrizzati con callback pure.
//!   - [`progress_controller`]: controllo di avanzamento (anti-loop coordinato).
//!   - [`g1_accounting`]: CONTEGGIO puro del gate G1 (re-entry/cap) dell'executor.
//!     Solo il conteggio del contatore `g1_reroute_count`: la DECISIONE conseguente
//!     (escalation/abort/nudge) DELEGA a [`progress_controller::decide`].
//!   - [`tool_dispatch`]: 6 helper puri del tool_dispatch_node (run_notes, outcome,
//!     stima dimensione tool_result, byte ritornati, stima contesto, reminder block).
//!   - [`predictive_cap`]: guardia pura del predictive context cap + SENTINEL condivisa
//!     (PUNTO UNICO testuale del guard "blocked-da-cap").
//!   - [`m16`]: gate validazione tool-in-list discovery-first + parser dei tool scoperti
//!     (raw INTEGRO, robusto al JSON troncato) + accumulo dedup per il run.
//!   - [`dag_scheduler`]: PUNTO UNICO della logica DAG dei todo (ready layer,
//!     decisione di parallelizzazione, selezione sequenziale `pick_next_todo`,
//!     discendenti per il cascade-skip). Prerequisito di todo_runner e verifier.
//!   - [`escalation`]: SELEZIONE pura del modello di auto-escalation (Tier 1 catena
//!     intra-provider `nexus_model_escalation_chain` + Tier 2 cross-provider
//!     `loop_fallback_default`, cooldown-aware). PUNTO UNICO di `_pick_escalation_model`;
//!     l'I/O (catena DB + cooldown) e' la porta [`crate::runtime::ports::EscalationPort`].
//!   - [`helpers`]: tool_choice forcing, segnale strutturale, action-oriented,
//!     stima complessita' e budget iterazioni.
//!   - [`severity`]: PUNTO UNICO del vocabolario di gravita' (alta/media/bassa)
//!     degli elementi di evidenza dei panel e del test "evidenza grave" che
//!     abilita il veto della minoranza. Advisory, review e debate delegano qui.
//!   - [`debate_panel`]: PUNTO UNICO delle TESI CONTRAPPOSTE (assegnazione delle
//!     posizioni agli avvocati + selezione dell'opzione). Disgiunto da
//!     `advisory_panel` (lenti diverse sullo stesso oggetto -> approva/veta): qui
//!     posizioni AVVERSE su opzioni alternative -> selezione.
//!   - [`orchestration_sizing`]: PUNTO UNICO del dimensionamento dei panel
//!     multi-agente (consiglio/review/multi-provider/debate) dalla classe di
//!     complessita' + profili admin + budget residuo costo/tempo; i cap storici
//!     restano backstop. DISGIUNTO da `orchestration_reason` (mosse a run
//!     avviato) e `scale_reason` (tier/contesto del singolo agente).
//!   - [`tiers`]: PUNTO UNICO del vocabolario performance-tier (scala a 5 livelli
//!     light<medium<high<heavy<frontier): ordinamento (`tier_rank`) e validazione
//!     (`is_performance_tier`). I due `tier_rank` storici (escalation qui,
//!     routing_matrix_auto_promoter in mcp-core) e i validatori admin/pavimento
//!     agentico DELEGANO qui invece di re-elencare i tier.
//!   - [`loop_signatures`]: RILEVAZIONE pura del loop di tool call per signature
//!     ripetuta + aggiornamento del contatore di esplorazione (PUNTO UNICO della
//!     signature anti-loop dell'executor; l'auto-escalation I/O resta nel nodo).
//!   - [`clarify_signature`]: firma PURA (sha1 della domanda normalizzata) di una
//!     domanda-chiarimento, per la loop-detection CROSS-RUN delle domande ripetute
//!     (asse `RepeatedUserQuestion`). PUNTO UNICO condiviso dall'impl DB della
//!     porta `ClarifyHistoryPort` (firma le domande storiche) e dal call site
//!     (firma la domanda corrente); l'I/O (lettura meta_step `kind='clarify'`) e'
//!     dietro la porta.
//!   - [`reward`]: reward euristico + fusione del reward finale (punto unico
//!     condiviso reflection/learner, regola L).
//!   - [`turn_focus`]: direttiva "focus del turno corrente" (anti-contaminazione
//!     history). PUNTO UNICO condiviso da planner ed executor (regola L).
//!   - [`hitl`]: gate HITL strutturale per modalita' Conferma (sospensione prima
//!     dei tool mutativi, pending_actions, compatibile con interrupt-resume).
//!   - [`supervisor`]: scheduling e parsing della risposta del supervisore worker.
//!   - [`end_turn`]: decisioni DETERMINISTICHE post-end_turn dell'executor
//!     (unfulfilled-report, rimozione blocco `<suggested_actions>`, messaggio
//!     billing fail-fast, gate smart-upscale). PUNTO UNICO dei rami che
//!     riscrivono/gatano il `result` a turno concluso; l'I/O (derivazione LLM
//!     delle scelte, lista provider esauriti, lookup window catalog) e' dietro le
//!     porte [`crate::runtime::ports::NextActionsDeriver`] /
//!     [`crate::runtime::ports::BillingCooldownPort`] /
//!     [`crate::runtime::ports::ModelUpscalePort`].
//!   - [`correction_progress`]: PUNTO UNICO di "questo rimando in correzione ha
//!     prodotto progresso?". Il criterio e' il confronto degli hash del
//!     contenuto (`before_sha256` != `after_sha256`), mai il conteggio delle
//!     chiamate ai tool di scrittura ne' la prosa dell'agente (regola M); l'I/O
//!     e' la porta [`crate::runtime::ports::MutationProgressPort`], che porta i
//!     fatti e non li giudica.
//!   - [`timeout_cause`]: PUNTO UNICO di "un run e' morto per tempo scaduto: su
//!     che cosa lo stava spendendo?". Il criterio guarda la CODA della storia e
//!     legge il fallimento dal CAMPO dell'esito (il ponte
//!     `nexus_types::tool_outcome::RispostaTool`), mai dal testo (regola M);
//!     l'I/O (leggere `agent_steps`) sta in `mcp-core`, che porta i fatti e non
//!     li giudica. E' una MISURA: non allunga budget e non riavvia nulla.
//!   - [`requirement_conformance`]: PUNTO UNICO di "i requisiti emessi dal
//!     Consiglio sono stati applicati?". Il criterio e' il CONTENUTO del file
//!     (regola M), mai la dichiarazione dell'agente; l'incertezza degrada sempre
//!     a "non verificabile", mai a "soddisfatto". Solo `requirements` entra nella
//!     misura: `recommendations` e' l'altra lista e non genera rilievi. L'I/O
//!     (leggere il file dal workspace) e' il parametro `leggi` di
//!     `compose_conformance`.
//!   - [`static_render`]: PUNTO UNICO di "l'app SENZA server mostra davvero il
//!     suo contenuto?". Terzo di una famiglia: `endpoint_probes` chiede al
//!     server, `browser_dialogue` chiede alla pagina se ottiene i propri dati
//!     (e pretende un'origine HTTP), qui non c'e' nessun servizio a cui
//!     chiedere — il contenuto lo genera il JS della pagina, e l'unico modo di
//!     saperlo e' guardare il DOM dopo che ha girato. Anche il DISCRIMINANTE
//!     sta qui (`classifica_natura`): quale delle tre misure si applica e' un
//!     fatto del progetto, mai un'euristica sul testo del task.
//!
//! Le `route_after_*` NON sono qui: stanno nel PR 2b.

pub mod adversarial_review;
pub mod advisory_panel;
pub mod browser_dialogue;
pub mod clarify_signature;
pub mod context_reduction;
pub mod correction_progress;
pub mod dag_scheduler;
pub mod debate_panel;
pub mod end_turn;
pub mod endpoint_probes;
pub mod escalation;
pub mod g1_accounting;
pub mod governance;
pub mod helpers;
pub mod hitl;
pub mod step_gate;
pub mod loop_signatures;
pub mod m16;
pub mod meta_reason;
pub mod orchestration_reason;
pub mod orchestration_sizing;
pub mod panel_quorum;
pub mod severity;
pub mod shell_command;
pub mod predictive_cap;
pub mod progress_controller;
pub mod requirement_conformance;
pub mod reward;
pub mod run_summary;
pub mod scale_reason;
pub mod static_render;
pub mod supervisor;
pub mod suspension_watch;
pub mod switch_reason;
pub mod text_repetition;
pub mod tiers;
pub mod timeout_cause;
pub mod tool_dispatch;
pub mod turn_focus;
pub mod turn_task;
pub mod ui_surface;

pub use adversarial_review::{
    compose_panel_verdict, PanelOutcome, PanelVerdict, QuorumPolicy, ReviewVerdict, ReviewerRef,
};
pub use advisory_panel::{
    compose_advisory_synthesis, AdvisoryPanelVerdict, AdvisoryPolicy, AdvisoryRoster,
    AdvisorySynthesis, AdvisoryVerdict, Requirement,
};
pub use clarify_signature::{clarify_signature, normalize_question};
pub use context_reduction::{
    apply_token_brake, compress_old_tool_results, dedup_tool_results, dedup_tool_results_history,
    degraded_marker, drop_unused_base64_payloads, first_human_index, inject_forced_rag_reminder,
    inject_language_reminder, inject_turn_focus, inject_verification_directive, looks_like_base64,
    should_compress_now, CompressParams, CtxMgmtConfig, HistoryMessage, TokenBrakeConfig,
    AGGRESSIVE_TRUNC_MARKER, LANG_REMINDER_MARKER, RAG_REMINDER_MARKER, VERIFY_DIRECTIVE_MARKER,
};
pub use correction_progress::{
    classify_correction_progress, CorrectionProgress, WriteFact,
};
pub use timeout_cause::{classifica_causa_timeout, CausaTimeout, TentativoOsservato};
pub use requirement_conformance::{
    compose_conformance, conformance_senza_progetto, requirements_from_synthesis,
    ConformanceReport, Direction, FileEvidence, RequirementCriterion, RequirementOutcome,
    RequirementVerdict,
};
pub use dag_scheduler::{
    compute_ready_layer, descendants, pick_next_todo, should_parallelize, DagConfig, Todo,
    TodoStatus,
};
pub use end_turn::{
    billing_fail_fast_message, build_unfulfilled_report, should_substitute_unfulfilled_report,
    should_upscale, strip_suggested_actions, upscale_required_tokens,
};
pub use escalation::{
    pick_escalation_model, ChainEntry, CrossProviderCandidate, EscalationCandidate, EscalationPick,
};
pub use g1_accounting::{g1_accounting, G1Accounting, G1Signals};
pub use governance::{
    is_recently_failed, likelihood_score, rank_candidates, rolling_summary_worthwhile,
    GovernancePolicy, ModelTelemetry,
};
pub use helpers::{
    action_oriented_for_intent, compute_iteration_budget, estimate_prompt_complexity,
    provider_style_supports_forcing, should_force_tool_choice, structural_unfulfilled_signal,
    turn_action_oriented, AdaptiveBudgetConfig,
};
pub use loop_signatures::{
    build_signature, detect_signature_loop, detect_signature_loop_progress_aware,
    detect_signature_loop_progress_aware_with, detect_signature_loop_with,
    exploration_counter_update, ExplorationCounterUpdate, LoopDetection, LoopThresholds,
    LOOP_THRESHOLD, RECENT_SIGNATURES_CAP,
};
pub use m16::{
    build_m16_allowed, is_tool_allowed, merge_discovered_run, parse_discovered_tools,
    py_json_len_ascii, DiscoveredTool, M16_META_TOOLS,
};
pub use meta_reason::{build_stall_context, translate, validate_move, work_epoch, VALID_BLOCKERS};
pub use orchestration_reason::{
    build_orchestration_context, classify_write, context_pressure_from_tokens, delegation_forbidden,
    modo_scrittura_batch, orch_epoch, path_in_scope, subtasks_are_disjoint, validate_orch_move,
    ModoScritturaBatch, OndataDiScrittura, ScopeVerdict,
};
pub use panel_quorum::{classify_panel, PanelClass, QuorumTally};
pub use predictive_cap::{is_cap_exempt, predictive_cap_check, PREDICTIVE_CAP_SENTINEL};
pub use progress_controller::{
    decide, Action, Axis, ProgressDecision, ProgressSignals, ABORT_STOP_REASON,
};
pub use reward::{
    aggregate_score, final_reward, heuristic_reward, prelim_reward, round_half_even,
    MAX_AGENT_ITERATIONS,
};
pub use run_summary::{riassunto_del_run, Finalizzatore, FontiRiassunto, RiassuntoRun};
pub use scale_reason::{
    apply_hysteresis, build_scale_context, context_window_ok, scale_cache_key, scale_trigger,
    validate_scale_move, ScaleHysteresisConfig, ScaleTriggerConfig,
};
pub use supervisor::{
    build_anomaly_block, build_steps_summary, detect_anomalies, should_invoke,
    supervisor_cache_key, validate_supervisor_response, SupervisorAnomalies, SupervisorConfig,
    SupervisorDecision,
};
pub use suspension_watch::{
    classify_suspension, nota_scadenza, SuspensionOrigin, SuspensionWatch,
    SUSPENSION_ORIGIN_EXTRA_KEY,
};
pub use text_repetition::{detect_repetition_collapse, RepetitionHit, RepetitionThresholds};
pub use tool_dispatch::{
    append_reminder_block, apply_run_notes, current_context_token_estimate, estimate_context_chars,
    estimate_tool_result_size_bytes, extract_returned_bytes, normalize_declared_outcome,
    ContextMessage, MAX_CONTEXT_CHARS, MAX_TOOL_RESULT_CHARS, RUN_NOTES_MAX_CHARS,
    TOKEN_CHARS_DIVISOR, VALID_OUTCOMES,
};
pub use turn_focus::{
    build_turn_focus_directive, sanitize_for_system_block, user_text_only, TURN_FOCUS_MARKER,
};
pub use turn_task::{current_turn_task, extract_original_task, ORIGINAL_TASK_KEY};

#[cfg(test)]
mod golden_tests;
