//! `StateDelta` tipizzato — la forma "cio' che un nodo ha modificato".
//!
//! Replica campo-per-campo `AgentState`, ma ogni campo e' AVVOLTO in un livello
//! di `Option` aggiuntivo che codifica la presenza-nel-delta. Il reducer generato
//! dal `#[derive(GraphState)]` (vedi `nexus-graph-derive`) consuma questa struct:
//! per ogni campo del delta `Some(...)` => applica, `None` => no-op (non tocca lo
//! stato). La semantica di merge vive nel derive (PUNTO UNICO, regola L); questo
//! modulo definisce SOLO la forma dati che il derive sa interpretare.
//!
//! ## Regola di wrapping (load-bearing)
//!
//! Il derive, per i campi NON-append, genera:
//! ```text
//! if let Some(__value) = delta.campo { self.campo = __value; }
//! ```
//! quindi `__value` deve avere ESATTAMENTE il tipo del campo di `AgentState`. Ne
//! consegue:
//!
//! - Campo `append` (`messages`, `meta_steps`, gli UNICI due con `#[reduce(append)]`):
//!   il derive fa `self.campo.extend(__items)`, dove `__items: Vec<T>`. Quindi nel
//!   delta il campo e' `Option<Vec<T>>`: `Some(v)` => append di `v`, `None` => no-op.
//!
//! - Campo che in `AgentState` e' `Option<U>` (la stragrande maggioranza): il
//!   derive assegna `self.campo = __value` con `self.campo: Option<U>`, quindi
//!   `__value: Option<U>`. Nel delta il campo diventa `Option<Option<U>>`:
//!     * `None`            => chiave ASSENTE nel delta => NO-OP (non toccare).
//!     * `Some(None)`      => chiave PRESENTE col valore JSON `null` => imposta a `None`.
//!     * `Some(Some(x))`   => chiave PRESENTE col valore `x` => imposta a `Some(x)`.
//!   La distinzione assente(None) vs presente-vuoto e' LOAD-BEARING: per i campi
//!   lista come `discovered_tools_next_turn`, `Some(Some(vec![]))` AZZERA la lista
//!   (durata esatta 1 turno), mentre `None` la lascia intatta.
//!
//! - `extra`: in `AgentState` e' `serde_json::Map<String, Value>` con
//!   `#[serde(flatten)]`. Nel delta diventa `Option<serde_json::Map<String, Value>>`
//!   (campo NORMALE, niente flatten): `Some(map)` => overwrite della mappa extra,
//!   `None` => no-op. Niente flatten qui perche' nel delta opaco le chiavi extra
//!   arriverebbero al top-level e non vogliamo che catturino chiavi non note: il
//!   merge dello stato runtime instrada le chiavi note ai campi tipizzati e le
//!   ignote restano nello stato concreto via il proprio flatten.
//!
//! ## Tolleranza in lettura
//!
//! `#[serde(default)]` sullo struct: un delta JSON con chiavi mancanti deserializza
//! a `None` su quei campi (= no-op). Cosi' il giro
//! `from_value(delta_opaco) -> StateDelta -> merge_typed` preserva esattamente la
//! distinzione chiave-assente vs chiave-presente del delta opaco di runtime.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::{AutomationMode, Message, MetaStep, StopReason, SupervisorMode, TaskComplexity};

/// Deserializzatore per i campi `Option<Option<T>>` che distingue chiave-assente
/// da chiave-presente-con-`null` (regola load-bearing, vedi doc del modulo).
///
/// Senza questo helper, serde mappa il JSON `null` sull'`Option` ESTERNO
/// (`None`), rendendolo indistinguibile da una chiave assente: si perderebbe la
/// semantica "chiave presente = overwrite" della regola autoritativa
/// (`nexus-graph/src/state.rs`). Combinato con `#[serde(default)]` sul campo
/// (chiave assente => `None`, no-op), qui una chiave PRESENTE col valore `null`
/// produce `Some(None)` (overwrite a `None`), un valore `x` produce `Some(Some(x))`.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    // Il campo e' raggiunto SOLO se la chiave e' presente (altrimenti scatta il
    // `default` => None). Quindi qui deserializziamo sempre l'Option interno
    // (`null` => Some(None), valore => Some(Some(v))).
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

/// Delta tipizzato dello stato del grafo agentico.
///
/// Ogni campo replica l'omonimo di `AgentState` con un livello di `Option` in
/// piu' (vedi regola di wrapping nel doc del modulo). `#[serde(default)]` rende
/// ogni campo omesso un no-op.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StateDelta {
    // ── Canali con reducer `add` (gli UNICI due) ─────────────────────────────
    /// Append alla cronologia messaggi. `Some(v)` => `messages.extend(v)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
    /// Append ai meta-step semantici. `Some(v)` => `meta_steps.extend(v)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_steps: Option<Vec<MetaStep>>,

    // ── Classificazione / intent ─────────────────────────────────────────────
    /// Vedi `AgentState::user_intent`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub user_intent: Option<Option<String>>,
    /// Vedi `AgentState::intent_confidence`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub intent_confidence: Option<Option<f64>>,
    /// Vedi `AgentState::task_complexity`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_complexity: Option<Option<TaskComplexity>>,
    /// Vedi `AgentState::agentic_score`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub agentic_score: Option<Option<f64>>,
    /// Vedi `AgentState::is_ambiguous`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub is_ambiguous: Option<Option<bool>>,
    /// Vedi `AgentState::expanded_query`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expanded_query: Option<Option<String>>,
    /// Vedi `AgentState::pending_clarify`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_clarify: Option<Option<bool>>,
    /// Vedi `AgentState::clarify_attempts`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub clarify_attempts: Option<Option<i64>>,
    /// Vedi `AgentState::repeated_clarify_count`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub repeated_clarify_count: Option<Option<i64>>,
    /// Vedi `AgentState::intent_hint`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub intent_hint: Option<Option<String>>,
    /// Vedi `AgentState::action_oriented`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub action_oriented: Option<Option<bool>>,
    /// Vedi `AgentState::report_only`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub report_only: Option<Option<bool>>,

    // ── Esito dichiarato / governance chiusura ───────────────────────────────
    /// Vedi `AgentState::declared_outcome`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub declared_outcome: Option<Option<Value>>,
    /// Vedi `AgentState::closure_verdict`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub closure_verdict: Option<Option<Value>>,
    /// Vedi `AgentState::review_gate_verdict`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub review_verdict: Option<Option<Value>>,
    /// Vedi `AgentState::advisory_verdict`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub advisory_verdict: Option<Option<Value>>,
    /// Vedi `AgentState::debate_position`. Reducer overwrite last-wins come i due
    /// gemelli: l'ultima dichiarazione valida del turno vince.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub debate_position: Option<Option<Value>>,
    /// Vedi `AgentState::tool_infra_error`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_infra_error: Option<Option<bool>>,
    /// Vedi `AgentState::playbook_steps`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub playbook_steps: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::playbook_key`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub playbook_key: Option<Option<String>>,
    /// Vedi `AgentState::declared_done_count`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub declared_done_count: Option<Option<i64>>,
    /// Vedi `AgentState::blocked_cap_rejected`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub blocked_cap_rejected: Option<Option<bool>>,

    // ── Tool discovery / compressione prefix ─────────────────────────────────
    /// Vedi `AgentState::discovered_tools_run`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub discovered_tools_run: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::compress_cutoff_index`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub compress_cutoff_index: Option<Option<i64>>,
    /// Vedi `AgentState::compress_cutoff_phase`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub compress_cutoff_phase: Option<Option<i64>>,
    /// Vedi `AgentState::run_notes`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub run_notes: Option<Option<String>>,

    // ── Routing / esecuzione base ────────────────────────────────────────────
    /// Vedi `AgentState::task_type`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_type: Option<Option<String>>,
    /// Vedi `AgentState::behavior_mode`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub behavior_mode: Option<Option<String>>,
    /// Vedi `AgentState::token_budget`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_budget: Option<Option<i64>>,
    /// Vedi `AgentState::result`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<Option<String>>,
    /// Vedi `AgentState::reasoning_acc` (FIX D4: reasoning persistito).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_acc: Option<Option<String>>,
    /// Vedi `AgentState::provider_used`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_used: Option<Option<String>>,
    /// Vedi `AgentState::model_used`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_used: Option<Option<String>>,
    /// Vedi `AgentState::feedback_score`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub feedback_score: Option<Option<f64>>,
    /// Vedi `AgentState::latency_ms`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub latency_ms: Option<Option<f64>>,
    /// Vedi `AgentState::token_usage`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_usage: Option<Option<i64>>,
    /// Vedi `AgentState::iterations`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub iterations: Option<Option<i64>>,
    /// Vedi `AgentState::thread_id`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub thread_id: Option<Option<String>>,

    // ── Agent tool loop ──────────────────────────────────────────────────────
    /// Vedi `AgentState::pending_tool_uses`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_tool_uses: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::stop_reason`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_reason: Option<Option<StopReason>>,
    /// Vedi `AgentState::recent_tool_signatures`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub recent_tool_signatures: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::tools_json`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tools_json: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::discovered_tools_next_turn`. Distinzione load-bearing:
    /// `None` no-op, `Some(Some(vec![]))` azzera (durata 1 turno), `Some(Some(v))` set.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub discovered_tools_next_turn: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::system_text`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_text: Option<Option<String>>,
    /// Vedi `AgentState::session_id`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_id: Option<Option<String>>,
    /// Vedi `AgentState::approved`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub approved: Option<Option<bool>>,
    /// Vedi `AgentState::provider_override`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_override: Option<Option<String>>,
    /// Vedi `AgentState::model_override`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_override: Option<Option<String>>,
    /// Vedi `AgentState::profile_name`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub profile_name: Option<Option<String>>,

    // ── Metriche AI estese ────────────────────────────────────────────────────
    /// Vedi `AgentState::prompt_tokens`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub prompt_tokens: Option<Option<i64>>,
    /// Vedi `AgentState::completion_tokens`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub completion_tokens: Option<Option<i64>>,
    /// Vedi `AgentState::cache_creation_tokens`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_creation_tokens: Option<Option<i64>>,
    /// Vedi `AgentState::cache_read_tokens`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_read_tokens: Option<Option<i64>>,
    /// Vedi `AgentState::total_tokens`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub total_tokens: Option<Option<i64>>,
    /// Vedi `AgentState::total_cost_usd`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub total_cost_usd: Option<Option<f64>>,
    /// Vedi `AgentState::cache_hit_rate`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_hit_rate: Option<Option<f64>>,
    /// Vedi `AgentState::temperature`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub temperature: Option<Option<f64>>,
    /// Vedi `AgentState::top_p`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub top_p: Option<Option<f64>>,
    /// Vedi `AgentState::created_at`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<Option<String>>,
    /// Vedi `AgentState::completed_at`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub completed_at: Option<Option<String>>,

    // ── Self-reflection ────────────────────────────────────────────────────────
    /// Vedi `AgentState::reflection_score`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub reflection_score: Option<Option<f64>>,
    /// Vedi `AgentState::reflection_dimensions`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub reflection_dimensions: Option<Option<Value>>,
    /// Vedi `AgentState::reflection_weaknesses`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub reflection_weaknesses: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::reflection_suggestions`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub reflection_suggestions: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::final_reward`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_reward: Option<Option<f64>>,

    // ── Plan / Act / Verify ────────────────────────────────────────────────────
    /// Vedi `AgentState::plan_phase_active`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_phase_active: Option<Option<bool>>,
    /// Vedi `AgentState::plan_phase_skip_reason`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_phase_skip_reason: Option<Option<String>>,
    /// Vedi `AgentState::current_plan_id`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_plan_id: Option<Option<String>>,
    /// Vedi `AgentState::current_todos`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_todos: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::acceptance_criteria`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub acceptance_criteria: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::active_todo_id`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_todo_id: Option<Option<String>>,
    /// Vedi `AgentState::plan_rationale`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_rationale: Option<Option<String>>,
    /// Vedi `AgentState::plan_constraints`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_constraints: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::plan_alternatives`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_alternatives: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::plan_rationale_context`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_rationale_context: Option<Option<String>>,
    /// Vedi `AgentState::context_brief`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_brief: Option<Option<String>>,
    /// Vedi `AgentState::understanding_active`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub understanding_active: Option<Option<bool>>,
    /// Vedi `AgentState::understanding_skip_reason`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub understanding_skip_reason: Option<Option<String>>,
    /// Vedi `AgentState::since_last_todo_reminder`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub since_last_todo_reminder: Option<Option<i64>>,
    /// Vedi `AgentState::verify_cycle`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub verify_cycle: Option<Option<i64>>,
    /// Vedi `AgentState::exploratory_verify_cycle`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub exploratory_verify_cycle: Option<Option<i64>>,
    /// Vedi `AgentState::exploratory_verify_total`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub exploratory_verify_total: Option<Option<i64>>,
    /// Vedi `AgentState::final_gate_cycle`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_gate_cycle: Option<Option<i64>>,
    /// Vedi `AgentState::final_gate_verdict`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_gate_verdict: Option<Option<crate::state::FinalGateVerdict>>,
    /// Vedi `AgentState::review_gate_cycle`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub review_gate_cycle: Option<Option<i64>>,
    /// Vedi `AgentState::review_gate_verdict`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub review_gate_verdict: Option<Option<crate::state::ReviewGateVerdict>>,
    /// Vedi `AgentState::review_correction_watermark`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub review_correction_watermark: Option<Option<i64>>,
    /// Vedi `AgentState::review_correction_no_progress`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub review_correction_no_progress: Option<Option<i64>>,
    /// Vedi `AgentState::gate_routing`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub gate_routing: Option<Option<crate::state::GateRouting>>,
    /// Vedi `AgentState::final_gate_passed`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_gate_passed: Option<Option<bool>>,
    /// Vedi `AgentState::final_gate_unverified`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_gate_unverified: Option<Option<bool>>,
    /// Vedi `AgentState::verifier_last_result`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub verifier_last_result: Option<Option<Value>>,
    /// Vedi `AgentState::plan_revisions`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_revisions: Option<Option<i64>>,
    /// Vedi `AgentState::pending_clarifications`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_clarifications: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::applied_default_assumptions`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_default_assumptions: Option<Option<Vec<Value>>>,

    // ── Sub-agents ──────────────────────────────────────────────────────────────
    /// Vedi `AgentState::parent_run_id`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_run_id: Option<Option<String>>,
    /// Vedi `AgentState::subagent_depth`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_depth: Option<Option<i64>>,
    /// Vedi `AgentState::subagent_results`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_results: Option<Option<Vec<Value>>>,
    /// Vedi `AgentState::active_subagent_runs`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_subagent_runs: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::subagent_cost_cumulative_usd`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_cost_cumulative_usd: Option<Option<f64>>,
    /// Vedi `AgentState::todo_isolation_retries`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub todo_isolation_retries: Option<Option<i64>>,

    // ── Allegati / budget ─────────────────────────────────────────────────────
    /// Vedi `AgentState::attachment_read_bytes`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub attachment_read_bytes: Option<Option<i64>>,

    // ── G1 / loop-detection ────────────────────────────────────────────────────
    /// Vedi `AgentState::action_nudge_count`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub action_nudge_count: Option<Option<i64>>,
    /// Vedi `AgentState::g1_reroute_count`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub g1_reroute_count: Option<Option<i64>>,
    /// Vedi `AgentState::consecutive_exploration_calls`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub consecutive_exploration_calls: Option<Option<i64>>,
    /// Vedi `AgentState::exploration_nudge_sent`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub exploration_nudge_sent: Option<Option<bool>>,
    /// Vedi `AgentState::repeated_cmd_nudge_sent`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub repeated_cmd_nudge_sent: Option<Option<bool>>,
    /// Vedi `AgentState::tokens_used_total`. Reducer overwrite (last-write):
    /// l'executor scrive `Some(Some(prev + turn_total))` dopo ogni risposta LLM.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tokens_used_total: Option<Option<i64>>,
    /// Vedi `AgentState::run_cost_cumulative_usd`. Reducer overwrite (last-write):
    /// l'executor scrive `Some(Some(prev + turn_cost))` dopo ogni risposta LLM (il
    /// costo del turno arriva dall'usage, gia' col prezzo del modello del turno).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub run_cost_cumulative_usd: Option<Option<f64>>,
    /// Vedi `AgentState::run_started_at_epoch_s`. Scritto solo alla costruzione
    /// dello stato iniziale (nessun nodo lo muta a run avviato); presente qui
    /// perche' il derive GraphState genera il merge per NOME su ogni campo.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub run_started_at_epoch_s: Option<Option<i64>>,
    /// Vedi `AgentState::consecutive_text_only_turns`. Reducer overwrite: `0` al
    /// primo tool_use, `prev + 1` su turno solo-testo.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub consecutive_text_only_turns: Option<Option<i64>>,

    // ── progress_controller ────────────────────────────────────────────────────
    /// Vedi `AgentState::progress_guided_axes`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub progress_guided_axes: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::progress_diagnosed_axes`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub progress_diagnosed_axes: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::progress_strategy_axes`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub progress_strategy_axes: Option<Option<Vec<String>>>,
    /// Vedi `AgentState::forced_close_unverified`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub forced_close_unverified: Option<Option<bool>>,
    /// Vedi `AgentState::provider_error_close`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_error_close: Option<Option<bool>>,

    // ── Sticky cascade ──────────────────────────────────────────────────────────
    /// Vedi `AgentState::sticky_provider`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sticky_provider: Option<Option<String>>,
    /// Vedi `AgentState::sticky_model`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sticky_model: Option<Option<String>>,
    /// Vedi `AgentState::planner_sticky_provider`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub planner_sticky_provider: Option<Option<String>>,
    /// Vedi `AgentState::planner_sticky_model`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub planner_sticky_model: Option<Option<String>>,

    // ── Scale-controller (FIX-A) ─────────────────────────────────────────────────
    /// Vedi `AgentState::current_tier`. Semantica coerente con `sticky_provider`:
    /// `None` no-op, `Some(Some(t))` set, `Some(None)` azzera. SCRITTO in PR-B1 dai
    /// call-site che cambiano modello (routing iniziale in `native_engine`, escalation
    /// e failover in `executor` col `tier` del pick, upscale via `UpscalePick::tier`)
    /// col `performance_tier` gia' noto al pick (regola M: campo strutturato). NESSUN
    /// decisore lo legge ancora (detector/nodo scale = PR-B2/B3) -> bit-identico.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub current_tier: Option<Option<String>>,

    /// Vedi `AgentState::effective_context_window`. Scritto dall'executor a ogni
    /// turno (finestra del modello effettivo del turno, post smart-upscale).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub effective_context_window: Option<Option<i64>>,

    // ── Automazione ─────────────────────────────────────────────────────────────
    /// Vedi `AgentState::automation_mode`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub automation_mode: Option<Option<AutomationMode>>,
    /// Vedi `AgentState::supervisor_mode`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub supervisor_mode: Option<Option<SupervisorMode>>,

    // ── Interrupt-resume (HITL + fan-in) ─────────────────────────────────────────
    /// Vedi `AgentState::awaiting_confirmation`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub awaiting_confirmation: Option<Option<bool>>,
    /// Vedi `AgentState::awaiting_subagents` (fan-in deterministico, Fase D).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub awaiting_subagents: Option<Option<bool>>,

    // ── Schema aperto ───────────────────────────────────────────────────────────
    /// Overwrite della mappa `extra` (campo NORMALE, niente flatten): `Some(map)`
    /// sostituisce `AgentState::extra`, `None` e' no-op. Vedi doc del modulo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Costruisce la mappa `extra` per un delta che vuole SCRIVERE UNA chiave in
/// `AgentState::extra` senza azzerare le altre (PUNTO UNICO, regola L).
///
/// `StateDelta::extra` ha semantica OVERWRITE TOTALE (vedi doc del modulo:
/// `Some(map)` sostituisce l'intera `AgentState::extra`). Scrivere un extra
/// PARZIALE — es. `Some(map!{"stall_move::..." => v})` — cancellerebbe TUTTE le
/// altre chiavi dello schema aperto gia' presenti nello stato
/// (`auto_escalations`, `repeat_scan_floor`, `iteration_budget`, `error_class`,
/// ...): un bug subdolo. Questo helper CLONA l'intera `state.extra` corrente,
/// vi inserisce/sostituisce `key -> value`, e ritorna la mappa completa da
/// mettere nel delta. Cosi' un nodo che aggiunge una sola chiave preserva tutte
/// le altre (clone-whole-map).
///
/// Uso tipico:
/// ```ignore
/// let extra = put_extra(state, "stall_move::signature::7", json!({"move": "escalate_model"}));
/// let delta = StateDelta { extra: Some(extra), ..Default::default() };
/// ```
pub fn put_extra(
    state: &super::AgentState,
    key: impl Into<String>,
    value: Value,
) -> Map<String, Value> {
    let mut map = state.extra.clone();
    map.insert(key.into(), value);
    map
}

impl StateDelta {
    /// Converte il delta TIPIZZATO nel delta OPACO del runtime
    /// (`nexus_graph::StateDelta`), punto unico tipizzato->opaco (regola L: i
    /// nodi non costruiscono il delta opaco a mano).
    ///
    /// La conversione passa da `serde_json`: grazie a
    /// `skip_serializing_if = "Option::is_none"` su ogni campo, un campo `None`
    /// (no-op) viene OMESSO dalla mappa (chiave assente = non toccare), mentre
    /// `Some(None)` produce `null` (overwrite a None) e `Some(Some(x))` produce
    /// `x`. Cosi' la distinzione load-bearing chiave-assente vs
    /// chiave-presente-`null` e' preservata bit-per-bit fino al reducer.
    pub fn into_opaque(self) -> nexus_graph::StateDelta {
        match serde_json::to_value(&self) {
            Ok(Value::Object(map)) => nexus_graph::StateDelta::from_map(map),
            // Uno `StateDelta` serializza SEMPRE a un oggetto JSON (struct con
            // campi nominali); il ramo non-oggetto e' irraggiungibile in pratica.
            // Niente panic (regola: errori espliciti): ritorniamo un delta vuoto
            // (no-op) cosi' un'eventuale anomalia non corrompe lo stato.
            _ => nexus_graph::StateDelta::empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentState, MessageContent};
    use nexus_graph::{GraphState as _, StateDelta as OpaqueDelta};
    use serde_json::json;

    /// Costruttore di un messaggio utente testuale (helper di test).
    fn human(text: &str) -> Message {
        Message::Human {
            content: MessageContent::text(text),
        }
    }

    /// Reducer `append` su `messages`: `Some(v)` accoda, `None` e' no-op.
    #[test]
    fn reducer_append_messages_estende() {
        let mut state = AgentState {
            messages: vec![human("primo")],
            ..Default::default()
        };

        // Some(v) => extend.
        state.merge_typed(StateDelta {
            messages: Some(vec![human("secondo"), human("terzo")]),
            ..Default::default()
        });
        assert_eq!(state.messages.len(), 3);
        assert_eq!(state.messages[0], human("primo"));
        assert_eq!(state.messages[2], human("terzo"));

        // None => no-op: la lista resta com'era (non azzerata, non duplicata).
        state.merge_typed(StateDelta {
            messages: None,
            ..Default::default()
        });
        assert_eq!(state.messages.len(), 3);
    }

    /// Distinzione load-bearing su `discovered_tools_next_turn` (campo lista
    /// overwrite): None=no-op, Some(Some(vec![]))=azzera, Some(Some(v))=set.
    #[test]
    fn reducer_overwrite_distinzione_load_bearing() {
        let mut state = AgentState {
            discovered_tools_next_turn: Some(vec![json!({"name": "preesistente"})]),
            ..Default::default()
        };

        // 1) None => NO-OP: lo stato resta invariato.
        state.merge_typed(StateDelta {
            discovered_tools_next_turn: None,
            ..Default::default()
        });
        assert_eq!(
            state.discovered_tools_next_turn,
            Some(vec![json!({"name": "preesistente"})]),
            "None deve essere no-op, non toccare il campo"
        );

        // 2) Some(Some(vec![])) => AZZERA: diventa Some(vec![]) (lista vuota,
        //    durata esatta 1 turno). NON diventa None.
        state.merge_typed(StateDelta {
            discovered_tools_next_turn: Some(Some(vec![])),
            ..Default::default()
        });
        assert_eq!(
            state.discovered_tools_next_turn,
            Some(vec![]),
            "Some(Some(vec![])) deve azzerare a lista vuota (non None)"
        );

        // 3) Some(Some(v)) => SET al nuovo valore.
        state.merge_typed(StateDelta {
            discovered_tools_next_turn: Some(Some(vec![json!({"name": "nuovo"})])),
            ..Default::default()
        });
        assert_eq!(
            state.discovered_tools_next_turn,
            Some(vec![json!({"name": "nuovo"})])
        );

        // 4) Some(None) => imposta a None (chiave presente col valore JSON null).
        state.merge_typed(StateDelta {
            discovered_tools_next_turn: Some(None),
            ..Default::default()
        });
        assert_eq!(state.discovered_tools_next_turn, None);
    }

    /// Overwrite di un campo scalare `Option<T>`: la stessa distinzione vale
    /// anche fuori dalle liste (es. `user_intent`).
    #[test]
    fn reducer_overwrite_scalare() {
        let mut state = AgentState {
            user_intent: Some("code_write".to_string()),
            ..Default::default()
        };

        // None => no-op.
        state.merge_typed(StateDelta {
            user_intent: None,
            ..Default::default()
        });
        assert_eq!(state.user_intent.as_deref(), Some("code_write"));

        // Some(Some(x)) => set.
        state.merge_typed(StateDelta {
            user_intent: Some(Some("code_read".to_string())),
            ..Default::default()
        });
        assert_eq!(state.user_intent.as_deref(), Some("code_read"));

        // Some(None) => azzera a None.
        state.merge_typed(StateDelta {
            user_intent: Some(None),
            ..Default::default()
        });
        assert_eq!(state.user_intent, None);
    }

    /// Giro completo via trait `nexus_graph::GraphState::merge` con delta JSON
    /// OPACO: una chiave ASSENTE e' no-op, una PRESENTE (anche []) applica.
    /// Testa la catena `from_value(map) -> StateDelta -> merge_typed`.
    #[test]
    fn merge_trait_opaco_chiave_assente_vs_presente() {
        let mut state = AgentState {
            user_intent: Some("originale".to_string()),
            discovered_tools_next_turn: Some(vec![json!({"name": "vecchio"})]),
            ..Default::default()
        };

        // Delta opaco con SOLO discovered_tools_next_turn presente (= []).
        // user_intent e' ASSENTE dalla mappa => deve restare invariato.
        let mut map = serde_json::Map::new();
        map.insert("discovered_tools_next_turn".to_string(), json!([]));
        let delta = OpaqueDelta::from_map(map);

        state.merge(delta);

        // Chiave presente (anche []): azzera la lista a vuota.
        assert_eq!(
            state.discovered_tools_next_turn,
            Some(vec![]),
            "chiave presente [] deve azzerare la lista"
        );
        // Chiave assente: no-op, valore originale preservato.
        assert_eq!(
            state.user_intent.as_deref(),
            Some("originale"),
            "chiave assente deve essere no-op"
        );
    }

    /// Via trait: una chiave presente col valore `null` imposta il campo a None.
    #[test]
    fn merge_trait_opaco_null_azzera_a_none() {
        let mut state = AgentState {
            result: Some("vecchio risultato".to_string()),
            ..Default::default()
        };

        let mut map = serde_json::Map::new();
        map.insert("result".to_string(), Value::Null);
        state.merge(OpaqueDelta::from_map(map));

        assert_eq!(state.result, None);
    }

    /// Via trait: append `messages` attraverso il delta opaco.
    #[test]
    fn merge_trait_opaco_append_messages() {
        let mut state = AgentState {
            messages: vec![human("uno")],
            ..Default::default()
        };

        let mut map = serde_json::Map::new();
        map.insert(
            "messages".to_string(),
            json!([{"role": "user", "content": "due"}]),
        );
        state.merge(OpaqueDelta::from_map(map));

        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[1], human("due"));
    }
}
