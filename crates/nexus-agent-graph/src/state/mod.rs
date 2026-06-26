//! Stato tipizzato del grafo agentico Nexus (replica `brain/agents/state.py`).
//!
//! I ~90 campi del `TypedDict` Python diventano campi Rust type-safe. Scelte di
//! tipizzazione (vedi `/tmp/langgraph_plan.md` sez. 3):
//!
//! - Campi `X | None` Python -> `Option<T>`.
//! - `messages` e `meta_steps` -> `Vec<...>` con `#[reduce(append)]`: sono gli
//!   UNICI due canali con reducer `add` (verificato su `state.py:17,24`). Tutti
//!   gli altri campi usano il reducer di default (overwrite-by-last-write).
//! - Campi enumerabili (`stop_reason`, `automation_mode`, `task_complexity`)
//!   -> enum Rust dedicati con `serde(rename...)`, NON `String`.
//! - Schema APERTO: `#[serde(default)]` sullo struct + `#[serde(flatten)] extra`
//!   per i campi runtime non presenti nel `TypedDict` (`iteration_budget`,
//!   `complexity_score`, `project_id`, `auto_escalations`, ...). Garantisce la
//!   non-perdita nel round-trip e la tolleranza in avanti nella coesistenza.
//!
//! Il reducer (`merge_typed`) e' generato dal `#[derive(GraphState)]` (punto
//! unico, regola L): l'impl del trait `nexus_graph::GraphState` (che lavora su
//! un delta JSON opaco lato runtime) vi delega.

pub mod delta;
pub mod lc_serde;
pub mod message;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use nexus_graph_derive::GraphState as DeriveGraphState;

pub use delta::StateDelta;
pub use message::{ContentBlock, Message, MessageContent, ToolUse};

/// Modalita' di automazione del turno chat propagata da mcp-core
/// (`state.py:274`). Enum dedicato invece di `String`: il `match` esaustivo
/// nei nodi non puo' dimenticare un caso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationMode {
    /// Nessuna automazione (default conservativo).
    None,
    /// Conferma richiesta prima di azioni potenzialmente distruttive.
    Confirm,
    /// L'agente esplora/agisce senza fermarsi a chiedere.
    Automatic,
    /// Modalita' continua (catena di turni autonomi).
    Continuous,
}

/// Complessita' del task stimata dal classifier agentico (`state.py:31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    /// Task semplice.
    Low,
    /// Task di media difficolta'.
    Medium,
    /// Task complesso.
    High,
}

/// Motivo di arresto del turno executor (`state.py:107-108` + piano sez. 5).
///
/// Chiave di tutto il routing post-executor: enum esaustivo, non `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Il modello ha risposto con tool_use: loop verso tool_dispatch.
    ToolUse,
    /// Fine turno naturale: verso learner.
    EndTurn,
    /// Stop generico del modello.
    Stop,
    /// L'executor ha rilevato chiamate ripetute: forza chiusura.
    LoopDetected,
    /// Run soppiantato da un run piu' recente sulla stessa sessione.
    Superseded,
    /// Escalation G1 (risposta descrittiva su richiesta d'azione).
    G1Escalated,
    /// Abort coordinato del progress_controller.
    LoopAbort,
    /// Cap G1 raggiunto.
    G1CapReached,
    /// Errore provider durante l'executor (`__init__.py:3104-3107`): l'executor
    /// scrive `result="[Errore provider ...]"` (NON vuoto) e `stop_reason="error"`.
    /// Serializza in `"error"` (snake_case): e' il SOLO valore che fa entrare il
    /// punto unico `heuristic_reward` nel ramo 0.0. Senza questa variante quel
    /// ramo era irraggiungibile dallo stato Rust (un run fallito sarebbe stato
    /// premiato 1.0 invece di 0.0). Sul routing cade sul default come in Python
    /// (route_after_executor -> learner; route_after_todo_runner -> executor).
    Error,
}

/// Meta-step semantico pubblicato al frontend chat (`state.py:24`).
///
/// Ogni nodo che vuole emettere uno step semantico (plan/routing/clarify/
/// fallback/reflection) aggiunge un `MetaStep` a `AgentState.meta_steps`; il
/// generator SSE li converte in eventi `{"type":"meta_step",...}`. E' uno dei
/// due canali con reducer `add` (accumulo cross-nodo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaStep {
    /// Tipo dello step (plan, routing, clarify, fallback, reflection, ...).
    pub kind: String,
    /// Titolo leggibile mostrato in chat.
    pub title: String,
    /// Payload arbitrario (dipende dal kind).
    #[serde(default)]
    pub payload: Value,
    /// Id di correlazione opzionale (per raggruppare step collegati).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Timestamp ISO8601 di creazione (opzionale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// Stato condiviso fra tutti i nodi del grafo agentico Nexus.
///
/// Replica `AgentState` (TypedDict, `total=False`) di `brain/agents/state.py`.
/// Tutti i campi sono `#[serde(default)]` (lo stato iniziale puo' omettere
/// qualsiasi campo). Solo `messages` e `meta_steps` hanno reducer `append`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, DeriveGraphState)]
#[serde(default)]
#[graph_state(delta = "StateDelta")]
pub struct AgentState {
    // ── Canali con reducer `add` (gli UNICI due, state.py:17,24) ─────────────
    /// Cronologia messaggi. Reducer `add` (append cross-nodo).
    #[reduce(append)]
    pub messages: Vec<Message>,
    /// Meta-step semantici pubblicati al frontend. Reducer `add`.
    #[reduce(append)]
    pub meta_steps: Vec<MetaStep>,

    // ── Classificazione / intent ─────────────────────────────────────────────
    /// Intent dell'utente classificato dal router.
    pub user_intent: Option<String>,
    /// Confidence della classificazione intent (0..1).
    pub intent_confidence: Option<f64>,
    /// Complessita' del task (segnale del classifier agentico).
    pub task_complexity: Option<TaskComplexity>,
    /// Score agentico (0..1) del classifier.
    pub agentic_score: Option<f64>,
    /// `true` se la richiesta e' ambigua.
    pub is_ambiguous: Option<bool>,
    /// Query arricchita prodotta dal clarify_or_expand (mode=expand).
    pub expanded_query: Option<String>,
    /// `true` se e' stata emessa una richiesta di chiarimento (turno fermo).
    pub pending_clarify: Option<bool>,
    /// Numero di chiarimenti gia' emessi nel run.
    pub clarify_attempts: Option<i64>,
    /// Intent gia' risolto a monte da mcp-core (salta la ri-classificazione).
    pub intent_hint: Option<String>,
    /// Punto unico: il turno corrente richiede azione con tool?
    pub action_oriented: Option<bool>,

    // ── Esito dichiarato / governance chiusura ───────────────────────────────
    /// Esito dichiarato dal modello via tool task_complete.
    pub declared_outcome: Option<Value>,
    /// Verdetto del closure_judge (LLM) quando l'esito non e' dichiarato.
    pub closure_verdict: Option<Value>,
    /// `true` se un tool e' fallito per ToolRunner gRPC down (infrastruttura).
    pub tool_infra_error: Option<bool>,
    /// Passi strutturati del playbook matchato.
    pub playbook_steps: Option<Vec<String>>,
    /// Chiave del playbook matchato.
    pub playbook_key: Option<String>,
    /// Conteggio dichiarazioni task_complete outcome=done.
    pub declared_done_count: Option<i64>,
    /// `true` se una dichiarazione blocked e' gia' stata rifiutata (guard).
    pub blocked_cap_rejected: Option<bool>,

    // ── Tool discovery / compressione prefix ─────────────────────────────────
    /// Tool M16 scoperti PERSISTENTI per il run (accumulo dedup per nome).
    pub discovered_tools_run: Option<Vec<Value>>,
    /// Indice di taglio della compressione "a generazioni".
    pub compress_cutoff_index: Option<i64>,
    /// Fase del taglio di compressione.
    pub compress_cutoff_phase: Option<i64>,
    /// Taccuino del run gestito dall'agente (tool nexus_run_notes).
    pub run_notes: Option<String>,

    // ── Routing / esecuzione base ────────────────────────────────────────────
    /// Tipo di task.
    pub task_type: Option<String>,
    /// Modalita' di comportamento (behavior_mode).
    pub behavior_mode: Option<String>,
    /// Budget token del turno.
    pub token_budget: Option<i64>,
    /// Risultato testuale finale del run.
    pub result: Option<String>,
    /// Provider effettivamente usato.
    pub provider_used: Option<String>,
    /// Modello effettivamente usato.
    pub model_used: Option<String>,
    /// Punteggio di feedback (telemetria).
    pub feedback_score: Option<f64>,
    /// Latenza in millisecondi.
    pub latency_ms: Option<f64>,
    /// Token usati (aggregato legacy).
    pub token_usage: Option<i64>,
    /// Numero di iterazioni eseguite.
    pub iterations: Option<i64>,
    /// Id del thread LangGraph (= run_id Nexus).
    pub thread_id: Option<String>,

    // ── Agent tool loop ──────────────────────────────────────────────────────
    /// Tool_use pendenti emessi dall'executor.
    pub pending_tool_uses: Option<Vec<Value>>,
    /// Motivo di arresto del turno executor.
    pub stop_reason: Option<StopReason>,
    /// Firme delle ultime tool calls (loop detection).
    pub recent_tool_signatures: Option<Vec<String>>,
    /// Tool dichiarati al modello (schema Anthropic-compatible).
    pub tools_json: Option<Vec<Value>>,
    /// Tool scoperti da iniettare come native nel SOLO turno successivo.
    /// Overwrite: `[]` azzera (durata esatta 1 turno) -> distinzione
    /// `None` (no-op) vs `Some(vec![])` (azzera) LOAD-BEARING.
    pub discovered_tools_next_turn: Option<Vec<Value>>,
    /// System prompt del profilo agente (vuoto = default).
    pub system_text: Option<String>,
    /// Id della sessione (iniettato dal chiamante).
    pub session_id: Option<String>,
    /// `true` dopo /agent/approve (HITL): salta interrupt in loop.
    pub approved: Option<bool>,
    /// Provider forzato dall'esterno (override routing).
    pub provider_override: Option<String>,
    /// Modello forzato dall'esterno (override routing).
    pub model_override: Option<String>,
    /// Profilo agente selezionato (core/github/specialized/general).
    pub profile_name: Option<String>,

    // ── Metriche AI estese ────────────────────────────────────────────────────
    /// Token di prompt.
    pub prompt_tokens: Option<i64>,
    /// Token di completion.
    pub completion_tokens: Option<i64>,
    /// Token di creazione cache.
    pub cache_creation_tokens: Option<i64>,
    /// Token letti da cache.
    pub cache_read_tokens: Option<i64>,
    /// Token totali.
    pub total_tokens: Option<i64>,
    /// Costo totale in USD.
    pub total_cost_usd: Option<f64>,
    /// Tasso di cache hit.
    pub cache_hit_rate: Option<f64>,
    /// Temperature usata.
    pub temperature: Option<f64>,
    /// Top-p usato.
    pub top_p: Option<f64>,
    /// Timestamp ISO8601 di creazione.
    pub created_at: Option<String>,
    /// Timestamp ISO8601 di completamento.
    pub completed_at: Option<String>,

    // ── Self-reflection ────────────────────────────────────────────────────────
    /// Punteggio della reflection (0..1).
    pub reflection_score: Option<f64>,
    /// Dettaglio per dimensione della reflection.
    pub reflection_dimensions: Option<Value>,
    /// Punti deboli rilevati.
    pub reflection_weaknesses: Option<Vec<Value>>,
    /// Suggerimenti di miglioramento.
    pub reflection_suggestions: Option<Vec<Value>>,
    /// Reward finale fuso.
    pub final_reward: Option<f64>,

    // ── Plan / Act / Verify ────────────────────────────────────────────────────
    /// `true` quando il planner ha prodotto un piano.
    pub plan_phase_active: Option<bool>,
    /// Motivo dello skip del planner.
    pub plan_phase_skip_reason: Option<String>,
    /// UUID del plan corrente.
    pub current_plan_id: Option<String>,
    /// Snapshot dei todos letti dal DB.
    pub current_todos: Option<Vec<Value>>,
    /// Acceptance criteria globali del plan.
    pub acceptance_criteria: Option<Vec<Value>>,
    /// Id del todo attivo.
    pub active_todo_id: Option<String>,
    /// Razionale decisionale del planner forte.
    pub plan_rationale: Option<String>,
    /// Vincoli del plan.
    pub plan_constraints: Option<Vec<String>>,
    /// Alternative considerate dal planner.
    pub plan_alternatives: Option<Vec<Value>>,
    /// Contesto RAG recuperato prima di pianificare.
    pub plan_rationale_context: Option<String>,
    /// Brief di comprensione pre-planning.
    pub context_brief: Option<String>,
    /// `true` se il nodo understanding e' attivo.
    pub understanding_active: Option<bool>,
    /// Motivo dello skip di understanding.
    pub understanding_skip_reason: Option<String>,
    /// Contatore per il reminder injection dei todo.
    pub since_last_todo_reminder: Option<i64>,
    /// Ciclo verifier corrente per active_todo.
    pub verify_cycle: Option<i64>,
    /// Ciclo della verifica esplorativa LLM.
    pub exploratory_verify_cycle: Option<i64>,
    /// Cap globale per run della verifica esplorativa.
    pub exploratory_verify_total: Option<i64>,
    /// Ciclo del final gate generale.
    pub final_gate_cycle: Option<i64>,
    /// `true` quando il final gate ha PASSATO la verifica E2E (esito canonico
    /// CompletedVerified lato mcp-core). Settato solo sul ramo PASSED del
    /// `final_gate_node`; il ramo forced_close/cap NON lo imposta (resta
    /// FailedDiagnosed). Vedi `final_gate.py:521`.
    pub final_gate_passed: Option<bool>,
    /// Ultimo risultato del verifier.
    pub verifier_last_result: Option<Value>,
    /// Contatore revisioni strutturali del plan.
    pub plan_revisions: Option<i64>,
    /// Domande di chiarimento pendenti emesse dal planner (HITL Confirm): se
    /// valorizzato il turno si ferma in attesa della risposta utente. `None`
    /// (assente) abilita la pre-flight clarifying del planner; una lista (anche
    /// vuota) la salta. Forma opaca (lista di `{id, question, suggested_default}`).
    pub pending_clarifications: Option<Vec<Value>>,
    /// Assunzioni di default applicate dal planner in modalita' autonoma
    /// (Automatico/Continuo) al posto di fermarsi per chiarimenti: trasparenza
    /// (le stesse domande con il loro `suggested_default`). Forma opaca.
    pub applied_default_assumptions: Option<Vec<Value>>,

    // ── Sub-agents ──────────────────────────────────────────────────────────────
    /// Id del run genitore (se sub-run).
    pub parent_run_id: Option<String>,
    /// Profondita' del sub-agente.
    pub subagent_depth: Option<i64>,
    /// Risultati dei sub-agenti.
    pub subagent_results: Option<Vec<Value>>,
    /// Sub-run attivi.
    pub active_subagent_runs: Option<Vec<String>>,
    /// Costo cumulativo dei sub-agenti in USD.
    pub subagent_cost_cumulative_usd: Option<f64>,
    /// Numero di retry gia' consumati dal todo_runner per il todo corrente
    /// (`todo_runner_node.py:308`, `int(state.get("todo_isolation_retries") or 0)`).
    /// Letto prima del retry, valorizzato a `extra_retries` da `_advance_patch`.
    pub todo_isolation_retries: Option<i64>,

    // ── Allegati / budget ─────────────────────────────────────────────────────
    /// Byte cumulativi letti via read_attachment/read_archive_entry.
    pub attachment_read_bytes: Option<i64>,

    // ── G1 / loop-detection ────────────────────────────────────────────────────
    /// Contatore nudge G1 iniettati.
    pub action_nudge_count: Option<i64>,
    /// Contatore re-routing G1 verso executor.
    pub g1_reroute_count: Option<i64>,
    /// Chiamate consecutive di sola esplorazione.
    pub consecutive_exploration_calls: Option<i64>,
    /// `true` dopo aver iniettato il nudge anti-esplorazione.
    pub exploration_nudge_sent: Option<bool>,
    /// `true` dopo aver iniettato il nudge anti-loop-comando.
    pub repeated_cmd_nudge_sent: Option<bool>,

    // ── progress_controller ────────────────────────────────────────────────────
    /// Assi di stallo gia' guidati (GUIDE applicata) in questo run.
    pub progress_guided_axes: Option<Vec<String>>,
    /// Assi gia' passati per la diagnosi forzata.
    pub progress_diagnosed_axes: Option<Vec<String>>,
    /// `true` quando un abort coordinato ha chiuso senza verifica.
    pub forced_close_unverified: Option<bool>,

    // ── Sticky cascade ──────────────────────────────────────────────────────────
    /// Provider sticky dopo un cascade riuscito.
    pub sticky_provider: Option<String>,
    /// Modello sticky dopo un cascade riuscito.
    pub sticky_model: Option<String>,
    /// Provider sticky specifico del planner.
    pub planner_sticky_provider: Option<String>,
    /// Modello sticky specifico del planner.
    pub planner_sticky_model: Option<String>,

    // ── Automazione ─────────────────────────────────────────────────────────────
    /// Modalita' automazione del turno chat propagata da mcp-core.
    pub automation_mode: Option<AutomationMode>,

    // ── HITL (predicati di interrupt) ───────────────────────────────────────────
    /// `true` quando lo stato attende una conferma umana (HITL). Non presente
    /// nel TypedDict come campo top-level: lo aggiungiamo qui esplicitamente
    /// perche' e' uno dei due predicati di interrupt del runtime.
    pub awaiting_confirmation: Option<bool>,

    // ── Schema aperto ───────────────────────────────────────────────────────────
    /// Campi runtime non promossi a campi nativi (`iteration_budget`,
    /// `complexity_score`, `project_id`, `auto_escalations`, ...). `flatten`
    /// preserva qualsiasi chiave sconosciuta nel round-trip e tollera l'avanti
    /// durante la coesistenza Python<->Rust (regola: non-perdita).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl AgentState {
    /// `true` se lo stato richiede una conferma umana prima di proseguire.
    pub fn is_awaiting_confirmation(&self) -> bool {
        self.awaiting_confirmation.unwrap_or(false)
    }

    /// `true` se lo stato attende un chiarimento dall'utente (disambiguazione).
    pub fn is_pending_clarify(&self) -> bool {
        self.pending_clarify.unwrap_or(false)
    }
}

/// Impl del trait di runtime `nexus_graph::GraphState`.
///
/// Il runtime lavora su un `nexus_graph::StateDelta` OPACO (mappa JSON): qui lo
/// deserializziamo nello `StateDelta` tipizzato e deleghiamo a `merge_typed`
/// (generato dal derive). La semantica reducer resta in UN solo posto (regola L);
/// l'impl del trait e' solo l'adattatore mappa-opaca -> delta-tipizzato.
impl nexus_graph::GraphState for AgentState {
    fn merge(&mut self, delta: nexus_graph::StateDelta) {
        // Il delta opaco e' una mappa JSON {campo -> valore}. La sua
        // deserializzazione nello StateDelta tipizzato porta ogni chiave PRESENTE
        // a `Some(...)` e ogni chiave ASSENTE a `None` (no-op), preservando la
        // distinzione load-bearing chiave-assente vs chiave-presente.
        let map = serde_json::Value::Object(delta.as_map().clone());
        match serde_json::from_value::<StateDelta>(map) {
            Ok(typed) => self.merge_typed(typed),
            Err(err) => {
                // Niente panic (regola: errori espliciti): un delta malformato e'
                // un bug del nodo chiamante; lo logghiamo e non tocchiamo lo stato.
                tracing::error!(
                    target: "nexus_agent_graph::state",
                    error = %err,
                    "StateDelta opaco non deserializzabile: merge ignorato"
                );
            }
        }
    }

    fn is_awaiting_confirmation(&self) -> bool {
        AgentState::is_awaiting_confirmation(self)
    }

    fn is_pending_clarify(&self) -> bool {
        AgentState::is_pending_clarify(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Round-trip serde di `AgentState`: serialize -> deserialize identico, con
    /// un campo extra SCONOSCIUTO preservato (flatten dello schema aperto).
    #[test]
    fn round_trip_serde_preserva_extra_sconosciuto() {
        // JSON con campi noti + un campo runtime non promosso a campo nativo.
        let raw = json!({
            "user_intent": "code_write",
            "iterations": 7,
            "discovered_tools_next_turn": [],
            "automation_mode": "automatic",
            "stop_reason": "tool_use",
            // Campi NON presenti nello struct: devono finire in `extra` (flatten).
            "iteration_budget": 42,
            "project_id": "proj-123",
            "auto_escalations": ["a", "b"],
        });

        // Deserializza: i campi noti popolano i campi nativi, gli ignoti `extra`.
        let state: AgentState = serde_json::from_value(raw.clone()).expect("deserialize");
        assert_eq!(state.user_intent.as_deref(), Some("code_write"));
        assert_eq!(state.iterations, Some(7));
        assert_eq!(state.discovered_tools_next_turn, Some(vec![]));
        assert_eq!(state.automation_mode, Some(AutomationMode::Automatic));
        assert_eq!(state.stop_reason, Some(StopReason::ToolUse));
        // Le chiavi sconosciute sono catturate da `extra` (flatten).
        assert_eq!(state.extra.get("iteration_budget"), Some(&json!(42)));
        assert_eq!(state.extra.get("project_id"), Some(&json!("proj-123")));
        assert_eq!(state.extra.get("auto_escalations"), Some(&json!(["a", "b"])));

        // Round-trip: re-serializza e ri-deserializza; deve essere identico.
        let serialized = serde_json::to_value(&state).expect("serialize");
        let back: AgentState = serde_json::from_value(serialized).expect("re-deserialize");
        // Confronto via JSON canonico (serde_json::Value ignora l'ordine chiavi).
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
        // L'extra sconosciuto sopravvive al giro completo.
        assert_eq!(back.extra.get("iteration_budget"), Some(&json!(42)));
        assert_eq!(back.extra.get("auto_escalations"), Some(&json!(["a", "b"])));
    }

    /// I tre enum dedicati serializzano/deserializzano col rename atteso
    /// (snake_case), coerente col contratto verso il brain Python.
    #[test]
    fn enum_rename_snake_case() {
        assert_eq!(
            serde_json::to_value(StopReason::G1Escalated).unwrap(),
            json!("g1_escalated")
        );
        assert_eq!(
            serde_json::to_value(AutomationMode::Continuous).unwrap(),
            json!("continuous")
        );
        assert_eq!(
            serde_json::to_value(TaskComplexity::High).unwrap(),
            json!("high")
        );
        let r: StopReason = serde_json::from_value(json!("loop_abort")).unwrap();
        assert_eq!(r, StopReason::LoopAbort);
    }
}
