//! `TodoRunnerNode` — porta la parte DETERMINISTICA del `todo_runner_node`
//! (`brain/agents/todo_runner_node.py:211-365`).
//!
//! Strategia (Claude Code, opzione 3): i todo del planner NON girano nel loop
//! executor principale (che accumula history e fa degradare il modello), ma
//! UNO ALLA VOLTA come SUB-RUN ISOLATE con context fresco. Il nodo e'
//! RE-ENTRANT: gira una volta per todo, e il routing replica l'edge
//! `todo_runner -> todo_runner` su `stop_reason == tool_use`
//! (`routing::route_after_todo_runner`, gia' presente).
//!
//! ## Cosa porta QUESTO modulo (deterministico, golden 1:1)
//!
//! - **`_compact`** (`todo_runner_node.py:56-62`, [`compact`]): troncamento su
//!   CHAR a `max_chars` (default 600) con suffisso `...[troncato]`.
//! - **`_build_context_blob`** (`:65-142`, [`TodoRunnerNode::build_context_blob`]):
//!   blocchi `<todo_gia_eseguiti>` (ultimi 8 di `subagent_results`), `<piano>`
//!   (`rationale[:1200]` + `constraints[:10]` da `plan_rationale`/
//!   `plan_constraints`), `<definition_of_done>` (`acceptance_criteria[:10]` con
//!   `json.loads` se stringa). Tutti i troncamenti su CHAR (codepoint). PURO.
//! - **`_result_failed`** (`:199-208`, [`result_failed`]): `error` non-null OR
//!   `status` non in `{completed, completed_verified}`.
//! - **`_todo_kind`** (`:145-152`, via [`TodoRunnerConfig::todo_kind`]):
//!   `todo_isolation_kind` (default `implement`).
//! - **`_advance_patch`** (`:368-395`, [`TodoRunnerNode::advance_patch`]): rilegge
//!   i todo via store, delega la selezione al PUNTO UNICO
//!   [`dag_scheduler::pick_next_todo`] (regola L), decide `stop_reason`
//!   `tool_use` (re-entry) vs `end_turn`, popola `subagent_results`/cost/
//!   `active_todo_id`/`current_todos`/`todo_isolation_retries`.
//! - **La macchina a stati on_failure** (`:291-365`,
//!   [`TodoRunnerNode::run`]): completed -> advance; failed+stop -> blocked +
//!   cascade-skip(descendants); continue -> blocked + cascade ma advance
//!   (prosegue); retry (retries < max) -> secondo dispatch con err_ctx, se
//!   fallisce ancora degrada a stop.
//!
//! ## Riuso dei punti unici (regola L, CRITICO)
//!
//! - **Selezione / cascade**: [`dag_scheduler::pick_next_todo`] e
//!   [`dag_scheduler::descendants`] esistono gia' come PUNTO UNICO; questo nodo
//!   li RIUSA, non re-implementa la cascata ne' lo skip. La struct [`Todo`] /
//!   [`TodoStatus`] vivono in `dag_scheduler`.
//! - **I/O todo**: il trait [`crate::runtime::TodoStore`] (`list_todos` /
//!   `mark_status`) isola l'accesso a `nexus_agent_todos` (cast `::text[]` sul
//!   lato concreto).
//! - **Dispatch sub-run**: il trait [`crate::runtime::ToolExecutor`] esegue il
//!   tool `dispatch_subagents` (`{tasks, max_parallel: 1}`). In shadow
//!   `ctx.exec_mode() -> Replay` (rilegge il result del primario, ZERO
//!   side-effect: nessun sub-run, nessuna scrittura FS/DB). E' l'UNICO I/O attivo.
//!
//! ## Cosa NON porta (TODO espliciti)
//!
//! - **Worklog fetch** (`session_worklog.fetch_worklog_block`,
//!   `todo_runner_node.py:84`): best-effort fail-open lato Python; non esiste un
//!   trait worklog in `runtime::ports` -> TODO esplicito. Il `context_blob` si
//!   costruisce SENZA il blocco worklog, ESATTAMENTE come quando il worklog
//!   Python e' vuoto/non disponibile (il blocco `parts` semplicemente non viene
//!   appeso). Quando mcp-core fornira' un trait/metodo worklog, il blocco va
//!   prepeso qui (prima di `<todo_gia_eseguiti>`).
//! - **Campi non-DAG del todo (`content`, `acceptance_criteria`)**: il punto
//!   unico [`dag_scheduler::Todo`] modella SOLO i campi DAG (id/status/
//!   depends_on/seq) — e' tutto cio' che serve a selezione/cascade. Il
//!   `_dispatch_one`/`_build_context_blob` Python leggono pero' `content`
//!   (il task del sub-run) e `acceptance_criteria` (la Definition of Done) dal
//!   todo dict completo. Questo PR mantiene `build_context_blob`/`dispatch_one`
//!   parametrici su un `serde_json::Value` opaco (cosi' il golden testa la
//!   logica COMPLETA con tutti i campi); nel path runtime, [`todo_value_of`] li
//!   ricava dai soli campi DAG noti, quindi `content` risulta vuoto e
//!   `acceptance_criteria` assente finche' la [`TodoStore`] non espone il todo
//!   completo -> TODO impl concreta in mcp-core: estendere `TodoStore` (o
//!   aggiungere un `list_todos_full`) per restituire anche `content`/criteri.
//! - **`_append_worklog_fact`** (`:398-417`): SOLO logging (osservabilita'),
//!   nessun side-effect di stato. Non portato come scrittura; gli stessi fatti
//!   sono gia' loggati via `tracing` qui.
//! - **Impl concreta** di [`TodoStore`] / [`ToolExecutor`] / risoluzione DB della
//!   [`TodoRunnerConfig`]: vivono in mcp-core (PR d'integrazione).
//!
//! Il nodo NON instrada: l'edge post-todo_runner vive in
//! `routing::route_after_todo_runner` (gia' portato 1:1).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::dag_scheduler::{self, Todo, TodoStatus};
use crate::runtime::ports::{ExecMode, ToolCall, TodoStore, ToolExecutor};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, StateDelta, StopReason};

/// Limite di default del summary compatto (`_SUMMARY_MAX_CHARS`,
/// `todo_runner_node.py:44`). Il main accumula solo questo, non l'intera
/// sub-conversazione.
const SUMMARY_MAX_CHARS: usize = 600;

/// Config DB-driven del nodo todo_runner, PASSATA (regola G: nessuna lettura DB
/// nel nodo, nessun fallback hardcoded nella logica decisionale).
///
/// Mappa i settings risolti dal brain (`orchestrator_config.get()`):
///   - `todo_isolation_kind`        -> `agent.continuous.todo_isolation_kind`
///   - `todo_isolation_on_failure`  -> `agent.continuous.todo_isolation_on_failure`
///   - `todo_isolation_max_retries` -> `agent.continuous.todo_isolation_max_retries`
///   - `dag_topological_enabled`    -> `agent.dag.topological_enabled` (serve a
///     [`dag_scheduler::pick_next_todo`], punto unico).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoRunnerConfig {
    /// Kind del sub-agent per l'esecuzione di un todo
    /// (`todo_isolation_kind`, default `implement`). Deve essere in
    /// `orchestrator.subagent_kinds_whitelist`.
    pub todo_isolation_kind: String,
    /// Politica al fallimento di un sub-run (`todo_isolation_on_failure`,
    /// default `stop`). Valori: `stop` | `retry` | `continue`.
    pub on_failure: OnFailure,
    /// Numero massimo di retry per un todo fallito (`todo_isolation_max_retries`,
    /// default 1). Rilevante solo con `on_failure == retry`.
    pub max_retries: i64,
    /// DAG topologico abilitato (`dag_topological_enabled`, default false):
    /// passato a [`dag_scheduler::pick_next_todo`] (punto unico della selezione).
    pub dag_topological_enabled: bool,
    /// Ampiezza minima del fronte ready per attivare il MULTICASTING (ondata
    /// parallela). `orchestrator.dag_parallel_min_ready`, default 2 (con < 2 resta
    /// il comportamento storico). Passato a [`dag_scheduler::should_parallelize`].
    pub dag_parallel_min_ready: i64,
    /// Limite caratteri del summary compatto (default 600). Esposto per
    /// completezza; il Python usa la costante `_SUMMARY_MAX_CHARS`.
    pub summary_max_chars: usize,
}

impl Default for TodoRunnerConfig {
    fn default() -> Self {
        // Default IDENTICI ai `_SAFE_DEFAULTS` del brain
        // (orchestrator_config.py:219,268-270). Valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica.
        Self {
            todo_isolation_kind: "implement".to_string(),
            on_failure: OnFailure::Stop,
            max_retries: 1,
            dag_topological_enabled: false,
            dag_parallel_min_ready: 2,
            summary_max_chars: SUMMARY_MAX_CHARS,
        }
    }
}

impl TodoRunnerConfig {
    /// `_todo_kind` (`todo_runner_node.py:145-152`): il kind, trimmato; se vuoto,
    /// `implement`. Replica `cfg.get("todo_isolation_kind") or "implement"` con
    /// `.strip()`.
    pub fn todo_kind(&self) -> String {
        let kind = self.todo_isolation_kind.trim();
        if kind.is_empty() {
            "implement".to_string()
        } else {
            kind.to_string()
        }
    }
}

/// Politica al fallimento di un sub-run (`todo_isolation_on_failure`). Enum
/// esaustivo invece di stringa: il `match` nel nodo non puo' dimenticare un caso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Blocca il todo + cascade-skip dei discendenti + chiude la catena (DEFAULT).
    Stop,
    /// Ritenta il todo (fino a `max_retries`); se fallisce ancora, degrada a `Stop`.
    Retry,
    /// Blocca il todo + cascade-skip, ma PROSEGUE col prossimo pending.
    Continue,
}

/// Tronca un summary su CHAR (codepoint) a `max_chars`, con suffisso
/// `...[troncato]`. Replica 1:1 `_compact` (`todo_runner_node.py:56-62`):
/// `str(text or "").strip()`, se `len <= max_chars` ritorna intero, altrimenti
/// `text[: max_chars - len(suffix)] + suffix`. Le lunghezze Python sono in
/// CARATTERI (str), non byte.
pub fn compact(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= max_chars {
        return trimmed.to_string();
    }
    const SUFFIX: &str = "...[troncato]";
    let suffix_len = SUFFIX.chars().count();
    // `max_chars - len(suffix)`: in Python e' una sottrazione fra interi; con
    // max_chars >= len(suffix) (il caso reale, default 600/240/200) e' positiva.
    // saturating_sub replica il troncamento a 0 nel caso degenere max_chars piccolo
    // (Python con max_chars < len(suffix) produrrebbe uno slice negativo = stringa
    // vuota + suffix; saturating_sub a 0 da' lo stesso prefisso vuoto).
    let keep = max_chars.saturating_sub(suffix_len);
    let prefix: String = chars.iter().take(keep).collect();
    format!("{prefix}{SUFFIX}")
}

/// Estrae una stringa "truthy" (semantica `or` Python) da `v`: `Some(s)` solo se
/// `v` e' una stringa NON vuota. Replica `str(v or "")`: una stringa vuota o un
/// tipo diverso/assente cadono come falsy.
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        // `str(v or "")` su un valore non-stringa truthy lo stringerebbe; ma i
        // campi qui usati (content/summary/rationale) sono testo: un non-stringa
        // e' trattato come assente (falsy) coerentemente col Python che fa
        // `str(... or "")` su valori gia' stringa.
        _ => String::new(),
    }
}

/// Primi `n` CHAR (codepoint) di `s`, come lo slice Python `s[:n]`.
fn head_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// True se il sub-run NON e' andato a buon fine (`_result_failed`,
/// `todo_runner_node.py:199-208`): `error` presente e non-null, OPPURE `status`
/// (default `completed`, trimmed+lower) non in `{completed, completed_verified}`.
pub fn result_failed(result: &Value) -> bool {
    // `result.get("error") is not None`: chiave presente con valore non-null.
    let error_present = result
        .get("error")
        .map(|e| !e.is_null())
        .unwrap_or(false);
    if error_present {
        return true;
    }
    // `str(result.get("status") or "completed").strip().lower()`.
    let status = match result.get("status") {
        Some(Value::String(s)) if !s.is_empty() => s.trim().to_lowercase(),
        // `or "completed"`: assente / null / stringa vuota -> "completed".
        _ => "completed".to_string(),
    };
    !matches!(status.as_str(), "completed" | "completed_verified")
}

/// Nodo todo_runner. Le dipendenze I/O (`TodoStore`, `ToolExecutor`) sono CAMPI
/// del nodo (come `CriteriaRunner` in `FinalGateNode`), non nel ctx: sono
/// specifiche di questo nodo (e del verifier, che le iniettera' a sua volta) e
/// tenerle fuori dal ctx minimizza l'impatto sugli altri nodi. La config
/// DB-driven e' risolta A MONTE (regola G); la macchina decisionale e' qui.
pub struct TodoRunnerNode {
    /// Config DB-driven del nodo (regola G: passata, mai letta dal nodo).
    cfg: TodoRunnerConfig,
    /// Store dei todo (`nexus_agent_todos`). Impl concreta in mcp-core; stub nei test.
    store: Arc<dyn TodoStore>,
    /// Esecutore del tool `dispatch_subagents` (Real -> ToolRunner gRPC, Replay
    /// -> rilegge il result del primario in shadow). Impl concreta in mcp-core.
    tools: Arc<dyn ToolExecutor>,
}

impl TodoRunnerNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante e le
    /// porte I/O concrete (o stub nei test).
    pub fn new(
        cfg: TodoRunnerConfig,
        store: Arc<dyn TodoStore>,
        tools: Arc<dyn ToolExecutor>,
    ) -> Self {
        Self { cfg, store, tools }
    }

    /// Costruisce il context del sub-run (testo provider-neutro,
    /// `_build_context_blob`, `todo_runner_node.py:65-142`). PURO sui dati dello
    /// stato + il todo + i risultati precedenti.
    ///
    /// Componenti nell'ordine (load-bearing):
    ///   (a) worklog di sessione -> NON portato (vedi TODO di modulo): il blocco
    ///       semplicemente non viene appeso, come quando il worklog Python e' vuoto.
    ///   (b) `<todo_gia_eseguiti>` dagli ultimi 8 di `prior_results`.
    ///   (c) `<piano>` con `rationale[:1200]` + `vincoli` (`constraints[:10]`,
    ///       ciascuno `[:200]`).
    ///   (d) `<definition_of_done>` da `acceptance_criteria[:10]` del todo
    ///       (con `json.loads` se e' una stringa).
    ///
    /// `prior_results` sono i `subagent_results` (Vec di oggetti JSON). `todo` e'
    /// il todo corrente con i campi opachi (`content`, `acceptance_criteria`, ...).
    pub fn build_context_blob(
        state: &AgentState,
        todo: &Value,
        prior_results: &[Value],
    ) -> String {
        let mut parts: Vec<String> = Vec::new();

        // (a) worklog: TODO (vedi doc di modulo). Niente blocco: equivalente al
        //     ramo Python `if wl:` con `wl` vuoto.

        // (b) <todo_gia_eseguiti>: ultimi 8 di prior_results.
        if !prior_results.is_empty() {
            let start = prior_results.len().saturating_sub(8);
            let mut done_lines: Vec<String> = Vec::new();
            for r in &prior_results[start..] {
                // seq puo' essere un intero JSON o null; Python lo interpola come
                // r.get("seq") (None -> "None"). Riproduciamo la repr Python.
                let seq = python_repr_opt(r.get("seq"));
                let content = head_chars(&str_or_empty(r.get("content")), 120);
                // `r.get("status") or "?"`: assente / null / "" -> "?".
                let status = match r.get("status") {
                    Some(Value::String(s)) if !s.is_empty() => s.clone(),
                    _ => "?".to_string(),
                };
                let summary = compact(&str_or_empty(r.get("summary")), 240);
                done_lines.push(format!("  - todo {seq} ({status}): {content} -> {summary}"));
            }
            parts.push(format!(
                "<todo_gia_eseguiti>\n\
                 I seguenti passi del piano sono gia' stati eseguiti in sub-run \
                 isolate (non rifarli, costruisci sopra il loro esito):\n{}\n\
                 </todo_gia_eseguiti>",
                done_lines.join("\n")
            ));
        }

        // (c) <piano>: rationale + constraints.
        let rationale = state.plan_rationale.as_deref().unwrap_or("").trim().to_string();
        let constraints: Vec<String> = state
            .plan_constraints
            .as_deref()
            .unwrap_or(&[])
            .to_vec();
        if !rationale.is_empty() || !constraints.is_empty() {
            let mut block: Vec<String> = vec!["<piano>".to_string()];
            if !rationale.is_empty() {
                block.push(format!("  <rationale>{}</rationale>", head_chars(&rationale, 1200)));
            }
            if !constraints.is_empty() {
                let items = constraints
                    .iter()
                    .take(10)
                    .map(|c| format!("    - {}", head_chars(c, 200)))
                    .collect::<Vec<_>>()
                    .join("\n");
                block.push(format!("  <vincoli>\n{items}\n  </vincoli>"));
            }
            block.push("</piano>".to_string());
            parts.push(block.join("\n"));
        }

        // (d) <definition_of_done>: acceptance_criteria del todo (json.loads se str).
        let criteria = normalize_acceptance_criteria(todo.get("acceptance_criteria"));
        if !criteria.is_empty() {
            let mut crit_lines: Vec<String> = Vec::new();
            for c in criteria.iter().take(10) {
                // Solo gli oggetti producono una riga (Python: `if isinstance(c, dict)`).
                if let Value::Object(map) = c {
                    // `c.get("type") or "criterio"`.
                    let ctype = match map.get("type") {
                        Some(Value::String(s)) if !s.is_empty() => s.clone(),
                        _ => "criterio".to_string(),
                    };
                    // `c.get("expected") or c.get("description") or ""`.
                    let expected = first_truthy_str(&[map.get("expected"), map.get("description")]);
                    crit_lines.push(format!("    - [{ctype}] {}", head_chars(&expected, 200)));
                }
            }
            if !crit_lines.is_empty() {
                parts.push(format!(
                    "<definition_of_done>\n\
                     Il passo e' completo SOLO se questi criteri sono soddisfatti:\n{}\n\
                     </definition_of_done>",
                    crit_lines.join("\n")
                ));
            }
        }

        parts.join("\n\n")
    }

    /// Costruisce il patch di avanzamento (`_advance_patch`,
    /// `todo_runner_node.py:368-395`): rilegge i todo, delega la selezione al
    /// PUNTO UNICO [`dag_scheduler::pick_next_todo`] (regola L), e decide
    /// `stop_reason`. Se non c'e' un prossimo todo -> `end_turn` + `active_todo_id`
    /// None; altrimenti -> `tool_use` (re-entry) + `active_todo_id` del prossimo +
    /// `current_todos`. `extra_retries` (>0 solo dopo un retry riuscito) valorizza
    /// `todo_isolation_retries`.
    async fn advance_patch(
        &self,
        run_id: &str,
        accumulated: Vec<Value>,
        cost: f64,
        extra_retries: i64,
    ) -> Result<StateDelta, NodeError> {
        let todos = self.store.list_todos(run_id).await.map_err(port_err)?;
        let nxt = dag_scheduler::pick_next_todo(&todos, self.cfg.dag_topological_enabled);

        let mut delta = StateDelta {
            subagent_results: Some(Some(accumulated)),
            subagent_cost_cumulative_usd: Some(Some(cost)),
            ..Default::default()
        };
        if extra_retries != 0 {
            delta.todo_isolation_retries = Some(Some(extra_retries));
        }
        match nxt {
            None => {
                delta.active_todo_id = Some(None);
                delta.stop_reason = Some(Some(StopReason::EndTurn));
            }
            Some(t) => {
                delta.active_todo_id = Some(Some(t.id.clone()));
                delta.stop_reason = Some(Some(StopReason::ToolUse));
                // current_todos = i todos riletti, serializzati come JSON opaco
                // (lo stato li trasporta come Vec<Value>).
                delta.current_todos = Some(Some(todos_to_values(&todos)));
            }
        }
        Ok(delta)
    }

    /// Esegue UN todo come sub-run isolata via il tool `dispatch_subagents`
    /// (max_parallel=1, `_dispatch_one`, `todo_runner_node.py:155-196`). Ritorna
    /// `Ok(Some(result))` col primo elemento di `results`, `Ok(None)` se il
    /// dispatch e' fallito (errore tool, payload non valido o `results` vuoto):
    /// in quel caso il chiamante fa fallback all'executor classico. Un guasto
    /// infrastrutturale del ToolExecutor (porta) propaga `NodeError`.
    ///
    /// `extra_context` (vuoto al primo dispatch, `<tentativo_precedente_fallito>`
    /// al retry) e' PREPESO al context_blob.
    async fn dispatch_one(
        &self,
        state: &AgentState,
        todo: &Value,
        mode: ExecMode,
        extra_context: &str,
    ) -> Result<Option<Value>, NodeError> {
        let prior = state.subagent_results.clone().unwrap_or_default();
        let mut context_blob = Self::build_context_blob(state, todo, &prior);
        if !extra_context.is_empty() {
            // `(extra_context + "\n\n" + context_blob).strip()`.
            context_blob = format!("{extra_context}\n\n{context_blob}")
                .trim()
                .to_string();
        }

        let task = str_or_empty(todo.get("content")).trim().to_string();
        let tasks = json!([{
            "kind": self.cfg.todo_kind(),
            "task": task,
            "context": context_blob,
            "expected_output_format":
                "riepilogo conciso delle modifiche applicate e dell'esito",
        }]);
        let call = ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: "dispatch_subagents".to_string(),
            input: json!({ "tasks": tasks, "max_parallel": 1 }),
        };

        // Parita' 1:1 col `try/except Exception` di `todo_runner_node.py:179-190`,
        // che e' ONNICOMPRENSIVO: QUALSIASI errore di `execute_tool` (guasto
        // infrastrutturale gRPC/ToolRunner down INCLUSO, non solo un fallimento
        // applicativo) viene catturato e ritorna None -> il chiamante ripristina
        // il todo a `pending` e fa pass-through {} (fallback all'executor
        // classico). Quindi un `Err(PortError)` (Tool/Llm/ReplayMissing) NON deve
        // propagare come NodeError::Failed: il run NON fallisce, si degrada al
        // fallback come fa Python. La cancellation cooperativa NON passa di qui
        // (e' modellata via `ctx.cancel`, non come variante di `PortError`),
        // quindi questo catch-all non la inghiotte.
        let outcome = match self.tools.execute(call, mode).await {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        // Fallimento applicativo del tool (is_error) -> anch'esso dispatch fallito.
        if outcome.is_error {
            return Ok(None);
        }

        // `data = json.loads(res.result_json or "{}")`. Il contenuto del tool e'
        // gia' JSON (stringa o oggetto): se e' una stringa, la riparsiamo.
        let data = match &outcome.content {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({})),
            other => other.clone(),
        };
        // `results = data.get("results") or []`; `results[0] if dict else None`.
        let results = data.get("results").and_then(Value::as_array);
        match results {
            Some(arr) if !arr.is_empty() => {
                if arr[0].is_object() {
                    Ok(Some(arr[0].clone()))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// MULTICASTING: esegue l'ONDATA di todo ready come sub-run isolate IN
    /// PARALLELO via un solo `dispatch_subagents` (max_parallel OMESSO -> tetto
    /// `orchestrator.max_parallel_subagents`, punto unico del tool). Il fronte
    /// parallelo arriva da [`dag_scheduler::compute_ready_layer`] (regola L).
    /// Politica esito: successo -> completed; fallimento -> blocked + cascade-skip
    /// dei discendenti; con `on_failure==Stop` un qualunque fallimento CHIUDE la
    /// catena (parita' col ramo stop sequenziale), altrimenti (`Continue`) si
    /// prosegue coi pending rimasti via [`Self::advance_patch`]. Il retry inline NON
    /// e' nel wave (gate a monte: `on_failure != Retry`). Dispatch infrastrutturale
    /// fallito / `results` non allineati -> ripristina i todo a Pending +
    /// pass-through (fallback executor), come [`Self::dispatch_one`].
    async fn dispatch_wave(
        &self,
        state: &AgentState,
        run_id: &str,
        todos: &[Todo],
        ready: Vec<Todo>,
        mode: ExecMode,
    ) -> Result<OpaqueDelta, NodeError> {
        // Cap all'ampiezza del batch del tool (dispatch_subagents: max 8 task).
        let wave: Vec<Todo> = ready.into_iter().take(8).collect();
        let prior = state.subagent_results.clone().unwrap_or_default();

        // Marca in_progress + costruisci i task (ordine preservato = ordine results:
        // join_all/chunks del tool conservano l'ordine d'ingresso).
        let mut tasks: Vec<Value> = Vec::with_capacity(wave.len());
        for t in &wave {
            self.store
                .mark_status(&t.id, TodoStatus::InProgress, mode)
                .await
                .map_err(port_err)?;
            let tv = todo_value_of(todos, &t.id);
            let blob = Self::build_context_blob(state, &tv, &prior);
            let task_text = str_or_empty(tv.get("content")).trim().to_string();
            tasks.push(json!({
                "kind": self.cfg.todo_kind(),
                "task": task_text,
                "context": blob,
                "expected_output_format":
                    "riepilogo conciso delle modifiche applicate e dell'esito",
            }));
        }

        tracing::info!(
            target: "nexus_agent_graph::todo_runner",
            wave = wave.len(),
            "MULTICASTING: dispatch ondata parallela di sub-run"
        );

        let call = ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: "dispatch_subagents".to_string(),
            input: json!({ "tasks": tasks }),
        };
        let outcome = match self.tools.execute(call, mode).await {
            Ok(o) => o,
            Err(_) => return self.wave_dispatch_failed(&wave, mode).await,
        };
        if outcome.is_error {
            return self.wave_dispatch_failed(&wave, mode).await;
        }
        let data = match &outcome.content {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({})),
            other => other.clone(),
        };
        let results = data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if results.len() != wave.len() {
            // results non allineati ai task -> trattato come dispatch fallito.
            return self.wave_dispatch_failed(&wave, mode).await;
        }

        let mut accumulated = prior.clone();
        let mut total_cost = 0.0_f64;
        let mut any_failed = false;
        for (t, result) in wave.iter().zip(results.iter()) {
            let tv = todo_value_of(todos, &t.id);
            let seq = tv.get("seq").cloned();
            let content = str_or_empty(tv.get("content"));
            let summary = compact(&str_or_empty(result.get("summary")), self.cfg.summary_max_chars);
            let cost = cost_of(result, "cost_usd");
            total_cost += cost;
            let mut record = build_record(seq.as_ref(), &t.id, &content, &summary, cost);
            if !result_failed(result) {
                self.store
                    .mark_status(&t.id, TodoStatus::Completed, mode)
                    .await
                    .map_err(port_err)?;
                record.insert("status".to_string(), json!("completed"));
            } else {
                any_failed = true;
                self.store
                    .mark_status(&t.id, TodoStatus::Blocked, mode)
                    .await
                    .map_err(port_err)?;
                self.cascade_skip(&t.id, todos, mode).await?;
                record.insert("status".to_string(), json!("failed"));
            }
            accumulated.push(Value::Object(record));
        }

        // on_failure==Stop + almeno un fallito -> chiusura catena (parita' ramo stop).
        if any_failed && self.cfg.on_failure == OnFailure::Stop {
            let prev_cost = state.subagent_cost_cumulative_usd.unwrap_or(0.0);
            tracing::warn!(
                target: "nexus_agent_graph::todo_runner",
                "wave: fallimento con on_failure=stop -> chiusura catena"
            );
            return Ok(StateDelta {
                active_todo_id: Some(None),
                stop_reason: Some(Some(StopReason::EndTurn)),
                subagent_results: Some(Some(accumulated)),
                subagent_cost_cumulative_usd: Some(Some(prev_cost + total_cost)),
                ..Default::default()
            }
            .into_opaque());
        }

        // Nessun fallito o on_failure==Continue -> advance_patch decide re-entry
        // (pending rimasti) vs end_turn (tutti terminali).
        Ok(self
            .advance_patch(run_id, accumulated, total_cost, 0)
            .await?
            .into_opaque())
    }

    /// Dispatch dell'ondata fallito a livello infrastrutturale (o `results` non
    /// allineati): ripristina tutti i todo della wave a Pending e fa pass-through
    /// (fallback executor classico), come [`Self::dispatch_one`] sul singolo.
    async fn wave_dispatch_failed(
        &self,
        wave: &[Todo],
        mode: ExecMode,
    ) -> Result<OpaqueDelta, NodeError> {
        for t in wave {
            self.store
                .mark_status(&t.id, TodoStatus::Pending, mode)
                .await
                .map_err(port_err)?;
        }
        tracing::warn!(
            target: "nexus_agent_graph::todo_runner",
            "wave: dispatch fallito, ripristino pending + fallback executor"
        );
        Ok(pass_through())
    }

    /// Cascade-skip: marca tutti i discendenti di `todo_id` come `skipped`,
    /// delegando l'insieme dei discendenti al PUNTO UNICO
    /// [`dag_scheduler::descendants`] (regola L: niente DFS re-implementata qui).
    /// `mode` propaga il gate shadow alla [`TodoStore::mark_status`] (no-op in
    /// Replay).
    async fn cascade_skip(&self, todo_id: &str, todos: &[Todo], mode: ExecMode) -> Result<(), NodeError> {
        for desc in dag_scheduler::descendants(todos, todo_id) {
            self.store
                .mark_status(desc, TodoStatus::Skipped, mode)
                .await
                .map_err(port_err)?;
        }
        Ok(())
    }
}

/// Costo `float(result.get("...") or 0.0)`: numero non-zero -> il valore, altrimenti 0.0.
fn cost_of(result: &Value, key: &str) -> f64 {
    match result.get(key).and_then(Value::as_f64) {
        Some(n) if n != 0.0 => n,
        _ => 0.0,
    }
}

/// `record["content"] = content[:200]` e i campi del record di esito.
fn build_record(seq: Option<&Value>, todo_id: &str, content: &str, summary: &str, cost: f64) -> serde_json::Map<String, Value> {
    let mut record = serde_json::Map::new();
    record.insert("seq".to_string(), seq.cloned().unwrap_or(Value::Null));
    record.insert("todo_id".to_string(), json!(todo_id));
    record.insert("content".to_string(), json!(head_chars(content, 200)));
    record.insert("summary".to_string(), json!(summary));
    record.insert("cost_usd".to_string(), json!(cost));
    record
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for TodoRunnerNode {
    fn id(&self) -> NodeId {
        NodeId::TodoRunner
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── (1) Guard isolamento attivo (todo_runner_node.py:225-227) ─────────
        // NOT todo_isolation_active -> {} (no-op): route_after_todo_runner
        // instrada all'executor classico (fallback al comportamento storico).
        if !crate::routing::signals::todo_isolation_active(state, &ctx.cfg) {
            tracing::info!(
                target: "nexus_agent_graph::todo_runner",
                "isolamento non attivo, no-op (fallback executor)"
            );
            return Ok(pass_through());
        }

        // ── (2) thread_id assente -> {} (todo_runner_node.py:233-236) ─────────
        // (`_tool_runner is None` Python: in Rust la porta e' sempre presente nel
        // costruttore; il fallback "executor classico" resta sul ramo thread_id
        // assente e sul ramo dispatch fallito.)
        let run_id = match state.thread_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => {
                tracing::debug!(
                    target: "nexus_agent_graph::todo_runner",
                    "thread_id assente, fallback"
                );
                return Ok(pass_through());
            }
        };

        // ── (3) todos vuoti -> end_turn + active_todo_id None (py:238-252) ────
        let todos = self.store.list_todos(&run_id).await.map_err(port_err)?;
        if todos.is_empty() {
            tracing::info!(
                target: "nexus_agent_graph::todo_runner",
                "nessun todo nel piano, chiudo"
            );
            return Ok(end_turn_no_active());
        }

        // (3a) MULTICASTING — ondata parallela del fronte ready (regola L:
        // compute_ready_layer + should_parallelize sono il punto unico del fronte
        // parallelo). Attivo solo con DAG topologico ON e on_failure != Retry (il
        // retry inline resta sul ramo sequenziale dispatch_one, dove e' definito).
        if self.cfg.dag_topological_enabled && self.cfg.on_failure != OnFailure::Retry {
            let ready = dag_scheduler::compute_ready_layer(&todos);
            let dag_cfg = dag_scheduler::DagConfig {
                dag_parallel_min_ready: self.cfg.dag_parallel_min_ready,
            };
            if ready.len() >= 2 && dag_scheduler::should_parallelize(&ready, &todos, &dag_cfg) {
                let mode = ctx.exec_mode();
                return self.dispatch_wave(state, &run_id, &todos, ready, mode).await;
            }
        }

        // (3b) pick_next None -> end_turn (PUNTO UNICO pick_next_todo, regola L).
        let next_todo = match dag_scheduler::pick_next_todo(&todos, self.cfg.dag_topological_enabled) {
            Some(t) => t.clone(),
            None => {
                tracing::info!(
                    target: "nexus_agent_graph::todo_runner",
                    total = todos.len(),
                    "tutti i todo terminali -> end_turn"
                );
                return Ok(end_turn_no_active());
            }
        };
        // `next_todo` come Value opaco per i campi non-DAG (content, criteria, seq).
        let next_value = todo_value_of(&todos, &next_todo.id);
        let todo_id = next_todo.id.clone();
        let seq = next_value.get("seq").cloned();
        let content = str_or_empty(next_value.get("content"));

        // Punto unico del gate shadow (regola L): un run shadow usa Replay ->
        // mark_status diventa no-op (zero scritture su nexus_agent_todos).
        let mode = ctx.exec_mode();

        // ── (4) Marca in_progress prima di delegare (py:259) ──────────────────
        self.store
            .mark_status(&todo_id, TodoStatus::InProgress, mode)
            .await
            .map_err(port_err)?;

        tracing::info!(
            target: "nexus_agent_graph::todo_runner",
            %todo_id,
            "dispatch sub-run isolata"
        );

        // ── (6) Delega via dispatch_subagents (max_parallel=1) ────────────────
        let result = self.dispatch_one(state, &next_value, mode, "").await?;

        // Dispatch fallito (NON sub-run fallita) -> ripristina pending + {} (py:271-276).
        let result = match result {
            Some(r) => r,
            None => {
                self.store
                    .mark_status(&todo_id, TodoStatus::Pending, mode)
                    .await
                    .map_err(port_err)?;
                tracing::warn!(
                    target: "nexus_agent_graph::todo_runner",
                    %todo_id,
                    "dispatch fallito, fallback executor"
                );
                return Ok(pass_through());
            }
        };

        // ── (7-8) Esito del sub-run (py:278-365) ──────────────────────────────
        let mut accumulated = state.subagent_results.clone().unwrap_or_default();
        let summary = compact(&str_or_empty(result.get("summary")), self.cfg.summary_max_chars);
        let cost = cost_of(&result, "cost_usd");
        let mut record = build_record(seq.as_ref(), &todo_id, &content, &summary, cost);

        // ── Ramo SUCCESSO ─────────────────────────────────────────────────────
        if !result_failed(&result) {
            self.store
                .mark_status(&todo_id, TodoStatus::Completed, mode)
                .await
                .map_err(port_err)?;
            record.insert("status".to_string(), json!("completed"));
            accumulated.push(Value::Object(record));
            tracing::info!(
                target: "nexus_agent_graph::todo_runner",
                %todo_id,
                "todo completato -> prossimo"
            );
            return Ok(self
                .advance_patch(&run_id, accumulated, cost, 0)
                .await?
                .into_opaque());
        }

        // ── Ramo FALLIMENTO ───────────────────────────────────────────────────
        record.insert("status".to_string(), json!("failed"));
        // `accumulated.append(record)` PRIMA delle ramificazioni (py:303-304):
        // il record "failed" e' gia' dentro `accumulated` per stop/continue. Il
        // ramo retry-riuscito invece SOSTITUISCE i campi del record (lo stesso
        // oggetto in Python), quindi teniamo un riferimento all'indice.
        let failed_idx = accumulated.len();
        accumulated.push(Value::Object(record));

        // (retry) on_failure == retry e budget disponibile.
        if self.cfg.on_failure == OnFailure::Retry {
            let retries_done = state.todo_isolation_retries.unwrap_or(0);
            let max_retries = self.cfg.max_retries;
            if retries_done < max_retries {
                tracing::warn!(
                    target: "nexus_agent_graph::todo_runner",
                    %todo_id,
                    retry = retries_done + 1,
                    max = max_retries,
                    "todo fallito, retry con context arricchito"
                );
                self.store
                    .mark_status(&todo_id, TodoStatus::Pending, mode)
                    .await
                    .map_err(port_err)?;
                let err_ctx = format!(
                    "<tentativo_precedente_fallito>\n\
                     Il passo e' gia' stato tentato e NON e' riuscito. Esito del \
                     tentativo precedente:\n{summary}\n\
                     Affronta la causa del fallimento prima di riprovare.\n\
                     </tentativo_precedente_fallito>"
                );
                let retry_result = self.dispatch_one(state, &next_value, mode, &err_ctx).await?;
                if let Some(rr) = retry_result.as_ref() {
                    if !result_failed(rr) {
                        self.store
                            .mark_status(&todo_id, TodoStatus::Completed, mode)
                            .await
                            .map_err(port_err)?;
                        // Sostituisce i campi del record (lo stesso oggetto Python):
                        // status=completed_after_retry, summary del retry.
                        let retry_summary =
                            compact(&str_or_empty(rr.get("summary")), self.cfg.summary_max_chars);
                        if let Some(Value::Object(rec)) = accumulated.get_mut(failed_idx) {
                            rec.insert("status".to_string(), json!("completed_after_retry"));
                            rec.insert("summary".to_string(), json!(retry_summary));
                        }
                        let total_cost = cost + cost_of(rr, "cost_usd");
                        return Ok(self
                            .advance_patch(&run_id, accumulated, total_cost, 1)
                            .await?
                            .into_opaque());
                    }
                }
                // Retry fallito -> degrada a stop (cade nel ramo stop sotto).
                tracing::warn!(
                    target: "nexus_agent_graph::todo_runner",
                    %todo_id,
                    "retry fallito, degrado a stop"
                );
            }
        }

        // (continue) blocca questo + cascade-skip, ma PROSEGUE col prossimo (py:340-348).
        if self.cfg.on_failure == OnFailure::Continue {
            self.store
                .mark_status(&todo_id, TodoStatus::Blocked, mode)
                .await
                .map_err(port_err)?;
            self.cascade_skip(&todo_id, &todos, mode).await?;
            tracing::warn!(
                target: "nexus_agent_graph::todo_runner",
                %todo_id,
                "todo blocked (on_failure=continue), prosegui"
            );
            return Ok(self
                .advance_patch(&run_id, accumulated, cost, 0)
                .await?
                .into_opaque());
        }

        // (stop, DEFAULT o degrado dal retry) blocca + cascade-skip + chiusura onesta (py:350-365).
        self.store
            .mark_status(&todo_id, TodoStatus::Blocked, mode)
            .await
            .map_err(port_err)?;
        self.cascade_skip(&todo_id, &todos, mode).await?;
        tracing::warn!(
            target: "nexus_agent_graph::todo_runner",
            %todo_id,
            "todo blocked (on_failure=stop) -> chiusura catena"
        );
        let prev_cost = state.subagent_cost_cumulative_usd.unwrap_or(0.0);
        Ok(StateDelta {
            active_todo_id: Some(Some(todo_id)),
            stop_reason: Some(Some(StopReason::EndTurn)),
            subagent_results: Some(Some(accumulated)),
            subagent_cost_cumulative_usd: Some(Some(prev_cost + cost)),
            ..Default::default()
        }
        .into_opaque())
    }
}

/// Delta pass-through `{}` (`todo_runner_node.py:227,231,236,276`): nessun campo
/// modificato -> route_after_todo_runner cade sul fallback executor.
fn pass_through() -> OpaqueDelta {
    StateDelta::default().into_opaque()
}

/// Delta di chiusura catena: `{active_todo_id: None, stop_reason: end_turn}`
/// (`todo_runner_node.py:241,252`). `active_todo_id: Some(None)` = chiave
/// presente col valore null = azzera (distinto dal no-op `None`).
fn end_turn_no_active() -> OpaqueDelta {
    StateDelta {
        active_todo_id: Some(None),
        stop_reason: Some(Some(StopReason::EndTurn)),
        ..Default::default()
    }
    .into_opaque()
}

/// Mappa un `PortError` su `NodeError::Failed` del nodo todo_runner.
fn port_err(e: crate::runtime::ports::PortError) -> NodeError {
    NodeError::Failed {
        node: "todo_runner",
        message: e.to_string(),
    }
}

/// Serializza i `Todo` (forma DAG) in `Vec<Value>` per `current_todos` dello
/// stato. Round-trip via serde (id/status/depends_on/seq).
fn todos_to_values(todos: &[Todo]) -> Vec<Value> {
    todos
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
        .collect()
}

/// Restituisce il `Todo` (forma DAG) come `Value` opaco. Il `dag_scheduler::Todo`
/// non porta i campi non-DAG (content/acceptance_criteria); per il context_blob
/// servono. Qui ri-serializziamo il todo DAG: i campi extra (content/criteria)
/// NON sono nel modello DAG, quindi nei test/golden arrivano dal Value originale.
/// Nel runtime concreto la `TodoStore` mappera' il todo completo; per la parte
/// DETERMINISTICA testata via golden, `build_context_blob` riceve il Value
/// diretto. Vedi `todo_value_of`.
fn todo_value_of(todos: &[Todo], todo_id: &str) -> Value {
    // Il modello DAG (`Todo`) non trasporta `content`/`acceptance_criteria`: nel
    // path runtime mcp-core fornira' il todo completo (TODO impl concreta). Per
    // ora ricostruiamo un Value coi soli campi DAG noti (id/status/depends_on/seq):
    // `content` vuoto e `acceptance_criteria` assente sono il comportamento
    // corretto quando lo store non li espone (il context_blob omette i blocchi
    // relativi, come quando i dati mancano lato Python).
    todos
        .iter()
        .find(|t| t.id == todo_id)
        .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

/// `repr` Python di un campo opzionale interpolato in f-string: `None` se assente,
/// altrimenti il valore (intero senza decimali, stringa nuda). Replica
/// `f"... {r.get('seq')} ..."`.
fn python_repr_opt(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => if *b { "True".to_string() } else { "False".to_string() },
        Some(other) => other.to_string(),
    }
}

/// Primo campo "truthy" (semantica `or` Python) come stringa, fra una lista di
/// candidati: `a or b or ""`. Una stringa vuota / null / assente cade sul
/// successivo.
fn first_truthy_str(candidates: &[Option<&Value>]) -> String {
    for c in candidates {
        match c {
            Some(Value::String(s)) if !s.is_empty() => return s.clone(),
            // Un valore non-stringa truthy (es. numero) verrebbe stringato da
            // `str(...)`; ma `expected`/`description` sono testo: trattiamo i
            // non-stringa come falsy (cadono al candidato successivo, poi "").
            _ => continue,
        }
    }
    String::new()
}

/// Normalizza `acceptance_criteria` (`todo_runner_node.py:121-126`): se e' una
/// stringa prova `json.loads`; se fallisce o non e' una lista, lista vuota.
fn normalize_acceptance_criteria(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(arr)) => arr.clone(),
        Some(Value::String(s)) => {
            // `json.loads(criteria)`; se non e' un array, lista vuota (Python
            // assegnerebbe il valore parsato, ma poi `for c in criteria` su un
            // non-iterabile fallirebbe; il `criteria or []` a monte e il flusso
            // reale assumono una lista. Restiamo conservativi: solo array).
            match serde_json::from_str::<Value>(s) {
                Ok(Value::Array(arr)) => arr,
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::decisions::dag_scheduler::{Todo, TodoStatus};
    use crate::routing::config::RoutingConfig;
    use crate::runtime::ports::{PortError, ToolCall, ToolOutcome};
    use crate::runtime::test_doubles::{NullEventSink, StubLlmGateway, StubTodoStore};
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, AutomationMode};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    /// Esecutore di tool a CODA: ritorna le risposte in ordine (per i test del
    /// retry, che fanno due dispatch). Registra le chiamate ricevute con la mode.
    struct QueueToolExecutor {
        /// Risposte da ritornare in ordine (clonate; l'ultima si ripete).
        responses: Mutex<Vec<Result<ToolOutcome, PortError>>>,
        seen: Mutex<Vec<(ToolCall, ExecMode)>>,
    }

    impl QueueToolExecutor {
        /// Crea l'esecutore da una lista di payload-contenuto (ogni payload e' il
        /// JSON `data` ritornato dal tool, racchiuso in un `ToolOutcome` ok).
        fn with_payloads(payloads: Vec<Value>) -> Self {
            let responses = payloads
                .into_iter()
                .map(|p| {
                    Ok(ToolOutcome {
                        tool_call_id: "dispatch".to_string(),
                        content: p,
                        is_error: false,
                        ..Default::default()
                    })
                })
                .collect();
            Self {
                responses: Mutex::new(responses),
                seen: Mutex::new(vec![]),
            }
        }

        /// Esecutore che ritorna un errore applicativo del tool (is_error) ->
        /// dispatch fallito.
        fn tool_error() -> Self {
            Self {
                responses: Mutex::new(vec![Ok(ToolOutcome {
                    tool_call_id: "dispatch".to_string(),
                    content: json!({}),
                    is_error: true,
                    ..Default::default()
                })]),
                seen: Mutex::new(vec![]),
            }
        }

        /// Esecutore che ritorna un guasto INFRASTRUTTURALE della porta
        /// (`PortError::Tool`, es. ToolRunner/gRPC down): deve essere trattato
        /// come dispatch fallito (None), non propagato come NodeError::Failed.
        fn port_error(message: &str) -> Self {
            Self {
                responses: Mutex::new(vec![Err(PortError::Tool(message.to_string()))]),
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl ToolExecutor for QueueToolExecutor {
        async fn execute(
            &self,
            call: ToolCall,
            mode: ExecMode,
        ) -> Result<ToolOutcome, PortError> {
            self.seen.lock().unwrap().push((call, mode));
            let mut q = self.responses.lock().unwrap();
            if q.len() > 1 {
                // Consuma la prima risposta finche' ne resta piu' di una.
                let first = q.remove(0);
                match first {
                    Ok(o) => Ok(o),
                    Err(e) => Err(match e {
                        PortError::Llm(s) => PortError::Llm(s),
                        PortError::Tool(s) => PortError::Tool(s),
                        PortError::ReplayMissing(s) => PortError::ReplayMissing(s),
                    }),
                }
            } else {
                // L'ultima si ripete (clona).
                match q.first() {
                    Some(Ok(o)) => Ok(o.clone()),
                    Some(Err(PortError::Llm(s))) => Err(PortError::Llm(s.clone())),
                    Some(Err(PortError::Tool(s))) => Err(PortError::Tool(s.clone())),
                    Some(Err(PortError::ReplayMissing(s))) => {
                        Err(PortError::ReplayMissing(s.clone()))
                    }
                    None => Ok(ToolOutcome {
                        tool_call_id: "empty".to_string(),
                        content: json!({}),
                        is_error: false,
                        ..Default::default()
                    }),
                }
            }
        }
    }

    /// Payload `data` di un sub-run riuscito (status completed) col summary dato.
    fn ok_payload(summary: &str, cost: f64) -> Value {
        json!({"results": [{"status": "completed", "summary": summary, "cost_usd": cost}]})
    }

    /// Payload `data` di un sub-run fallito (status failed).
    fn failed_payload(summary: &str) -> Value {
        json!({"results": [{"status": "failed", "summary": summary}]})
    }

    /// Ctx con shadow flag e la RoutingConfig data (per il gate todo_isolation).
    fn ctx_with(shadow: bool, cfg: RoutingConfig) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            // Il ToolExecutor del ctx NON e' usato dal todo_runner (il nodo ha il
            // proprio campo `tools`); qui basta uno stub qualsiasi.
            tools: Arc::new(QueueToolExecutor::with_payloads(vec![json!({})])),
            emit: Arc::new(NullEventSink),
            cfg,
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
        }
    }

    /// RoutingConfig col gate isolamento ON (todo_isolation_enabled=true).
    fn routing_cfg_on() -> RoutingConfig {
        RoutingConfig {
            todo_isolation_enabled: true,
            ..RoutingConfig::default()
        }
    }

    /// Stato che SUPERA il gate todo_isolation_active (plan_phase + modalita'
    /// autonoma + thread_id), con subagent_results dati.
    fn isolated_state(thread_id: Option<&str>) -> AgentState {
        AgentState {
            plan_phase_active: Some(true),
            automation_mode: Some(AutomationMode::Automatic),
            thread_id: thread_id.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn todo(id: &str, status: TodoStatus, deps: &[&str], seq: i64) -> Todo {
        Todo {
            id: id.to_string(),
            status,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            seq: Some(seq),
        }
    }

    fn node(
        cfg: TodoRunnerConfig,
        store: Arc<dyn TodoStore>,
        tools: Arc<dyn ToolExecutor>,
    ) -> TodoRunnerNode {
        TodoRunnerNode::new(cfg, store, tools)
    }

    // ── Gate OFF (isolamento non attivo) -> no-op {} ─────────────────────────────

    #[tokio::test]
    async fn gate_off_passthrough() {
        // RoutingConfig default ha todo_isolation_enabled=false -> gate OFF.
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("x", 0.0)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools.clone());
        let ctx = ctx_with(false, RoutingConfig::default());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // No-op: nessun cambio stop_reason/active_todo_id.
        assert_eq!(out.stop_reason, None);
        assert_eq!(out.active_todo_id, None);
        // Nessun dispatch, nessuna mark_status (gate prima di tutto).
        assert!(tools.seen.lock().unwrap().is_empty());
        assert!(store.marks.lock().unwrap().is_empty());
    }

    // ── thread_id assente -> no-op {} ────────────────────────────────────────────

    #[tokio::test]
    async fn thread_id_assente_passthrough() {
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("x", 0.0)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools);
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(None); // thread_id None.
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, None);
        assert!(store.marks.lock().unwrap().is_empty());
    }

    // ── todos vuoti -> end_turn + active_todo_id None ────────────────────────────

    #[tokio::test]
    async fn todos_vuoti_end_turn() {
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("x", 0.0)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.active_todo_id, None);
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    // ── pick_next None (tutti terminali) -> end_turn ─────────────────────────────

    #[tokio::test]
    async fn tutti_terminali_end_turn() {
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Completed, &[], 1),
            todo("b", TodoStatus::Skipped, &[], 2),
        ]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("x", 0.0)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.active_todo_id, None);
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    // ── completed -> advance (re-entry tool_use sul prossimo pending) ────────────

    #[tokio::test]
    async fn completed_advance_reentry() {
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("fatto a", 0.5)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // a completato, b ancora pending -> re-entry (tool_use), active_todo_id = b.
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.active_todo_id.as_deref(), Some("b"));
        assert_eq!(out.subagent_cost_cumulative_usd, Some(0.5));
        // subagent_results contiene il record completed di a.
        let results = out.subagent_results.expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["status"], json!("completed"));
        assert_eq!(results[0]["todo_id"], json!("a"));
        // mark: a in_progress poi completed.
        let marks = store.marks.lock().unwrap();
        assert_eq!(marks[0], ("a".to_string(), TodoStatus::InProgress));
        assert_eq!(marks[1], ("a".to_string(), TodoStatus::Completed));
        // Dispatch eseguito in Real (non shadow).
        assert_eq!(tools.seen.lock().unwrap()[0].1, ExecMode::Real);
    }

    // ── MULTICASTING: ondata parallela (dispatch_wave) ───────────────────────────

    #[tokio::test]
    async fn wave_parallelo_due_todo_completed() {
        // DAG topologico ON + 2 todo pending senza dipendenze -> compute_ready_layer
        // ritorna entrambi, should_parallelize=true -> UN solo dispatch_subagents con
        // 2 task (ondata), non due dispatch sequenziali. Entrambi completed -> end_turn.
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        let payload = json!({"results": [
            {"status": "completed", "summary": "fatto a", "cost_usd": 0.3},
            {"status": "completed", "summary": "fatto b", "cost_usd": 0.2},
        ]});
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![payload]));
        let cfg = TodoRunnerConfig {
            dag_topological_enabled: true,
            dag_parallel_min_ready: 2,
            ..TodoRunnerConfig::default()
        };
        let n = node(cfg, store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        {
            let seen = tools.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "una sola chiamata dispatch per l'intera ondata");
            assert_eq!(
                seen[0].0.input["tasks"].as_array().unwrap().len(),
                2,
                "il dispatch porta 2 task in parallelo"
            );
        }
        let results = out.subagent_results.expect("results");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["status"], json!("completed"));
        assert_eq!(results[1]["status"], json!("completed"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.subagent_cost_cumulative_usd, Some(0.5));
        let marks = store.marks.lock().unwrap();
        assert!(marks.contains(&("a".to_string(), TodoStatus::InProgress)));
        assert!(marks.contains(&("b".to_string(), TodoStatus::Completed)));
    }

    #[tokio::test]
    async fn wave_fallito_stop_chiude_catena() {
        // Un fallito nell'ondata con on_failure=stop (default) -> blocked + chiusura
        // catena (end_turn), parita' col ramo stop sequenziale.
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        let payload = json!({"results": [
            {"status": "completed", "summary": "ok a", "cost_usd": 0.1},
            {"status": "failed", "summary": "ko b"},
        ]});
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![payload]));
        // on_failure default = Stop
        let cfg = TodoRunnerConfig {
            dag_topological_enabled: true,
            ..TodoRunnerConfig::default()
        };
        let n = node(cfg, store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.active_todo_id, None);
        let results = out.subagent_results.expect("results");
        assert_eq!(results.len(), 2);
        let statuses: Vec<_> = results.iter().map(|r| r["status"].clone()).collect();
        assert!(statuses.contains(&json!("completed")));
        assert!(statuses.contains(&json!("failed")));
        let marks = store.marks.lock().unwrap();
        assert!(marks.contains(&("b".to_string(), TodoStatus::Blocked)));
    }

    // ── completed ultimo todo -> end_turn (niente prossimo) ──────────────────────

    #[tokio::test]
    async fn completed_ultimo_end_turn() {
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("fatto", 0.2)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools);
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.active_todo_id, None);
    }

    // ── failed + on_failure stop -> blocked + cascade-skip + end_turn ────────────

    #[tokio::test]
    async fn failed_stop_blocked_cascade() {
        // a fallisce; b e c dipendono da a (discendenti) -> skipped.
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &["a"], 2),
            todo("c", TodoStatus::Pending, &["b"], 3),
        ]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![failed_payload("rotto")]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools);
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Chiusura catena: end_turn, active_todo_id = a (il bloccato).
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.active_todo_id.as_deref(), Some("a"));
        // Record failed accumulato.
        let results = out.subagent_results.expect("results");
        assert_eq!(results[0]["status"], json!("failed"));
        // marks: a in_progress, a blocked, b skipped, c skipped (ordine descendants
        // non garantito fra b/c: verifichiamo l'insieme).
        let marks = store.marks.lock().unwrap();
        assert_eq!(marks[0], ("a".to_string(), TodoStatus::InProgress));
        assert_eq!(marks[1], ("a".to_string(), TodoStatus::Blocked));
        let skipped: std::collections::HashSet<&str> = marks[2..]
            .iter()
            .filter(|(_, s)| *s == TodoStatus::Skipped)
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(skipped, ["b", "c"].into_iter().collect());
    }

    // ── failed + on_failure continue -> blocked + cascade ma advance ─────────────

    #[tokio::test]
    async fn failed_continue_advance() {
        // a fallisce ma c'e' un todo indipendente d pending -> prosegue su d.
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &["a"], 2), // discendente -> skipped
            todo("d", TodoStatus::Pending, &[], 3),    // indipendente -> prossimo
        ]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![failed_payload("rotto")]));
        let cfg = TodoRunnerConfig {
            on_failure: OnFailure::Continue,
            ..TodoRunnerConfig::default()
        };
        let n = node(cfg, store.clone(), tools);
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Prosegue (advance): tool_use, prossimo pending = d (b e' skipped).
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.active_todo_id.as_deref(), Some("d"));
        let marks = store.marks.lock().unwrap();
        assert_eq!(marks[0], ("a".to_string(), TodoStatus::InProgress));
        assert_eq!(marks[1], ("a".to_string(), TodoStatus::Blocked));
        assert!(marks.iter().any(|m| *m == ("b".to_string(), TodoStatus::Skipped)));
    }

    // ── failed + retry riuscito -> completed_after_retry + advance ───────────────

    #[tokio::test]
    async fn failed_retry_riuscito() {
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        // Primo dispatch fallisce, secondo (retry) riesce.
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![
            failed_payload("primo tentativo rotto"),
            ok_payload("ok al retry", 0.3),
        ]));
        let cfg = TodoRunnerConfig {
            on_failure: OnFailure::Retry,
            max_retries: 1,
            ..TodoRunnerConfig::default()
        };
        let n = node(cfg, store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Retry riuscito -> advance, prossimo = b.
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.active_todo_id.as_deref(), Some("b"));
        assert_eq!(out.todo_isolation_retries, Some(1));
        // Due dispatch (primo + retry).
        assert_eq!(tools.seen.lock().unwrap().len(), 2);
        // Record promosso a completed_after_retry.
        let results = out.subagent_results.expect("results");
        assert_eq!(results[0]["status"], json!("completed_after_retry"));
        assert_eq!(results[0]["summary"], json!("ok al retry"));
        // marks: a in_progress, a pending (retry), a completed.
        let marks = store.marks.lock().unwrap();
        assert_eq!(marks[0], ("a".to_string(), TodoStatus::InProgress));
        assert_eq!(marks[1], ("a".to_string(), TodoStatus::Pending));
        assert_eq!(marks[2], ("a".to_string(), TodoStatus::Completed));
    }

    // ── failed + retry fallito -> degrada a stop ─────────────────────────────────

    #[tokio::test]
    async fn failed_retry_degrada_stop() {
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::Pending, &[], 1),
            todo("b", TodoStatus::Pending, &["a"], 2),
        ]));
        // Entrambi i dispatch falliscono.
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![
            failed_payload("primo rotto"),
            failed_payload("retry rotto"),
        ]));
        let cfg = TodoRunnerConfig {
            on_failure: OnFailure::Retry,
            max_retries: 1,
            ..TodoRunnerConfig::default()
        };
        let n = node(cfg, store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Degrado a stop: end_turn, a bloccato, b skipped.
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.active_todo_id.as_deref(), Some("a"));
        assert_eq!(tools.seen.lock().unwrap().len(), 2);
        let marks = store.marks.lock().unwrap();
        assert!(marks.iter().any(|m| *m == ("a".to_string(), TodoStatus::Blocked)));
        assert!(marks.iter().any(|m| *m == ("b".to_string(), TodoStatus::Skipped)));
    }

    // ── retry budget esaurito (retries_done >= max) -> direttamente stop ─────────

    #[tokio::test]
    async fn retry_budget_esaurito_stop() {
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![failed_payload("rotto")]));
        let cfg = TodoRunnerConfig {
            on_failure: OnFailure::Retry,
            max_retries: 1,
            ..TodoRunnerConfig::default()
        };
        let n = node(cfg, store.clone(), tools.clone());
        let ctx = ctx_with(false, routing_cfg_on());
        let mut st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        st.todo_isolation_retries = Some(1); // gia' al massimo.
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Nessun secondo dispatch: budget esaurito -> stop diretto.
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(tools.seen.lock().unwrap().len(), 1);
    }

    // ── dispatch fallito (tool is_error) -> ripristina pending + no-op {} ────────

    #[tokio::test]
    async fn dispatch_fallito_ripristina_pending() {
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::tool_error());
        let n = node(TodoRunnerConfig::default(), store.clone(), tools);
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // No-op (fallback executor): nessun stop_reason.
        assert_eq!(out.stop_reason, None);
        // marks: a in_progress poi ripristinato a pending.
        let marks = store.marks.lock().unwrap();
        assert_eq!(marks[0], ("a".to_string(), TodoStatus::InProgress));
        assert_eq!(marks[1], ("a".to_string(), TodoStatus::Pending));
    }

    // ── dispatch fallito INFRASTRUTTURALE (Err PortError) -> stesso fallback ─────

    #[tokio::test]
    async fn dispatch_err_infrastrutturale_ripristina_pending() {
        // Parita' col `except Exception` Python (test_dispatch_fallito_fallback,
        // RuntimeError("gRPC down")): un guasto infrastrutturale NON fa fallire il
        // run, si degrada al fallback executor (todo ripristinato a pending + {}).
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "t1",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::port_error("gRPC down"));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools);
        let ctx = ctx_with(false, routing_cfg_on());
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        // Il run NON deve fallire: si aspetta Ok (pass-through), non Err.
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run NON deve fallire"));
        // Delta pass-through {}: nessun stop_reason (fallback executor).
        assert_eq!(out.stop_reason, None);
        // marks: t1 in_progress poi ripristinato a pending (come is_error).
        let marks = store.marks.lock().unwrap();
        assert_eq!(marks[0], ("t1".to_string(), TodoStatus::InProgress));
        assert_eq!(marks[1], ("t1".to_string(), TodoStatus::Pending));
        assert_eq!(marks.len(), 2);
    }

    // ── Shadow: ExecMode::Replay sul dispatch ────────────────────────────────────

    #[tokio::test]
    async fn shadow_usa_replay() {
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::Pending,
            &[],
            1,
        )]));
        let tools = Arc::new(QueueToolExecutor::with_payloads(vec![ok_payload("ok", 0.0)]));
        let n = node(TodoRunnerConfig::default(), store.clone(), tools.clone());
        let ctx = ctx_with(true, routing_cfg_on()); // shadow
        let st = isolated_state(Some("11111111-1111-1111-1111-111111111111"));
        let _ = n.run(&st, &ctx).await.expect("run ok");
        // Il ToolExecutor riceve Replay (nessun side-effect del dispatch).
        assert_eq!(tools.seen.lock().unwrap()[0].1, ExecMode::Replay);
        // ZERO scritture su nexus_agent_todos in shadow: tutte le mark_status
        // (in_progress/completed/...) sono no-op in Replay -> marks vuoto. Senza
        // il gate sul trait, il run shadow corromperebbe il DAG del primario.
        assert!(
            store.marks.lock().unwrap().is_empty(),
            "in shadow mark_status deve essere no-op (zero scritture)"
        );
    }

    // ── Funzioni pure (smoke; il golden copre la parita' 1:1) ────────────────────

    #[test]
    fn compact_tronca_su_char() {
        assert_eq!(compact("  ciao  ", 600), "ciao");
        // Stringa lunga: prefisso + suffisso, tutto su char.
        let long = "a".repeat(700);
        let out = compact(&long, 600);
        assert!(out.ends_with("...[troncato]"));
        assert_eq!(out.chars().count(), 600);
    }

    #[test]
    fn result_failed_branching() {
        assert!(!result_failed(&json!({"status": "completed"})));
        assert!(!result_failed(&json!({"status": "completed_verified"})));
        // status assente -> default completed -> non fallito.
        assert!(!result_failed(&json!({})));
        // error non-null -> fallito (anche se status completed).
        assert!(result_failed(&json!({"status": "completed", "error": "boom"})));
        // status timeout/failed -> fallito.
        assert!(result_failed(&json!({"status": "timeout"})));
        assert!(result_failed(&json!({"status": "FAILED"})));
        // error null -> non conta come errore.
        assert!(!result_failed(&json!({"status": "completed", "error": null})));
    }

    #[test]
    fn todo_kind_default() {
        assert_eq!(TodoRunnerConfig::default().todo_kind(), "implement");
        let cfg = TodoRunnerConfig {
            todo_isolation_kind: "  refactor  ".to_string(),
            ..TodoRunnerConfig::default()
        };
        assert_eq!(cfg.todo_kind(), "refactor");
        let empty = TodoRunnerConfig {
            todo_isolation_kind: "   ".to_string(),
            ..TodoRunnerConfig::default()
        };
        assert_eq!(empty.todo_kind(), "implement");
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! todo_runner. Lo script `scripts/gen_golden_todo_runner.py` importa/replica
    //! `_compact`, `_build_context_blob`, `_result_failed`, `_todo_kind` e la
    //! decision machine on_failure (deterministica dati i risultati dispatch
    //! stubati), e salva `{case_id, function, input, output}` in
    //! `/tmp/golden_todo_runner.json`. Qui ricostruiamo l'input, chiamiamo la
    //! funzione Rust corrispondente e verifichiamo `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_todo_runner.py
    //!   cargo test -p nexus-agent-graph --lib golden_todo_runner_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::*;
    use crate::decisions::dag_scheduler::Todo;
    use crate::state::AgentState;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Ricostruisce un `AgentState` minimale dai campi usati dal golden
    /// (plan_rationale, plan_constraints, subagent_results,
    /// subagent_cost_cumulative_usd, todo_isolation_retries).
    fn state_from(input: &Value) -> AgentState {
        let mut st = AgentState::default();
        if let Some(r) = input.get("plan_rationale").and_then(Value::as_str) {
            st.plan_rationale = Some(r.to_string());
        }
        if let Some(arr) = input.get("plan_constraints").and_then(Value::as_array) {
            st.plan_constraints = Some(
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
            );
        }
        if let Some(arr) = input.get("subagent_results").and_then(Value::as_array) {
            st.subagent_results = Some(arr.clone());
        }
        if let Some(c) = input.get("subagent_cost_cumulative_usd").and_then(Value::as_f64) {
            st.subagent_cost_cumulative_usd = Some(c);
        }
        if let Some(r) = input.get("todo_isolation_retries").and_then(Value::as_i64) {
            st.todo_isolation_retries = Some(r);
        }
        st
    }

    /// Replica in Rust la DECISION MACHINE on_failure data la pre-condizione che
    /// il gate sia entrato e i risultati dispatch stubati. Ritorna il delta nella
    /// forma confrontabile col Python (dict di chiavi modificate). NON esegue I/O:
    /// `pick_next` sui todos PASSATI nell'input (gia' aggiornati per riflettere il
    /// mark) replica `_advance_patch` senza store.
    fn decision_delta(input: &Value) -> Value {
        let st = state_from(input);
        let result: Value = input.get("result").cloned().unwrap_or(json!({}));
        let retry_result: Option<Value> = input.get("retry_result").cloned();
        let on_failure = input
            .get("on_failure")
            .and_then(Value::as_str)
            .unwrap_or("stop");
        let max_retries = input.get("max_retries").and_then(Value::as_i64).unwrap_or(1);
        let dag = input
            .get("dag_topological_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // todos DOPO il dispatch (l'oracolo Python applica i mark e passa lo stato
        // finale dei todos qui, cosi' pick_next e' deterministico senza store).
        let todos_after: Vec<Todo> = input
            .get("todos_after")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let todo_id = input.get("todo_id").and_then(Value::as_str).unwrap_or("a");
        let seq = input.get("seq").cloned().unwrap_or(Value::Null);
        let content = input.get("content").and_then(Value::as_str).unwrap_or("");
        let summary_max = SUMMARY_MAX_CHARS;

        let mut accumulated: Vec<Value> = st.subagent_results.clone().unwrap_or_default();
        let summary = compact(&str_or_empty(result.get("summary")), summary_max);
        let cost = cost_of(&result, "cost_usd");
        let mut record = build_record(Some(&seq), todo_id, content, &summary, cost);

        // advance helper puro (senza store): pick_next sui todos_after.
        let advance = |accumulated: Vec<Value>, cost: f64, extra_retries: i64| -> Value {
            let nxt = crate::decisions::dag_scheduler::pick_next_todo(&todos_after, dag);
            let mut delta = serde_json::Map::new();
            delta.insert("subagent_results".to_string(), json!(accumulated));
            delta.insert("subagent_cost_cumulative_usd".to_string(), json!(cost));
            if extra_retries != 0 {
                delta.insert("todo_isolation_retries".to_string(), json!(extra_retries));
            }
            match nxt {
                None => {
                    delta.insert("active_todo_id".to_string(), Value::Null);
                    delta.insert("stop_reason".to_string(), json!("end_turn"));
                }
                Some(t) => {
                    delta.insert("active_todo_id".to_string(), json!(t.id));
                    delta.insert("stop_reason".to_string(), json!("tool_use"));
                    delta.insert("current_todos".to_string(), json!(todos_to_values(&todos_after)));
                }
            }
            Value::Object(delta)
        };

        if !result_failed(&result) {
            record.insert("status".to_string(), json!("completed"));
            accumulated.push(Value::Object(record));
            return advance(accumulated, cost, 0);
        }
        record.insert("status".to_string(), json!("failed"));
        let failed_idx = accumulated.len();
        accumulated.push(Value::Object(record));

        if on_failure == "retry" {
            let retries_done = st.todo_isolation_retries.unwrap_or(0);
            if retries_done < max_retries {
                if let Some(rr) = retry_result.as_ref() {
                    if !result_failed(rr) {
                        let retry_summary = compact(&str_or_empty(rr.get("summary")), summary_max);
                        if let Some(Value::Object(rec)) = accumulated.get_mut(failed_idx) {
                            rec.insert("status".to_string(), json!("completed_after_retry"));
                            rec.insert("summary".to_string(), json!(retry_summary));
                        }
                        let total_cost = cost + cost_of(rr, "cost_usd");
                        return advance(accumulated, total_cost, 1);
                    }
                }
                // degrada a stop.
            }
        }
        if on_failure == "continue" {
            return advance(accumulated, cost, 0);
        }
        // stop.
        let prev_cost = st.subagent_cost_cumulative_usd.unwrap_or(0.0);
        json!({
            "active_todo_id": todo_id,
            "stop_reason": "end_turn",
            "subagent_results": accumulated,
            "subagent_cost_cumulative_usd": prev_cost + cost,
        })
    }

    #[test]
    #[ignore = "richiede /tmp/golden_todo_runner.json generato da gen_golden_todo_runner.py"]
    fn golden_todo_runner_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_todo_runner.json", "gen_golden_todo_runner.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 25, "attesi >=25 casi, trovati {}", cases.len());

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "compact" => {
                    let text = c.input.get("text").and_then(Value::as_str).unwrap_or("");
                    let max = c.input.get("max_chars").and_then(Value::as_u64).unwrap_or(600) as usize;
                    json!(compact(text, max))
                }
                "result_failed" => json!(result_failed(c.input.get("result").unwrap_or(&json!({})))),
                "todo_kind" => {
                    let kind = c.input.get("kind").and_then(Value::as_str).unwrap_or("");
                    let cfg = TodoRunnerConfig {
                        todo_isolation_kind: kind.to_string(),
                        ..TodoRunnerConfig::default()
                    };
                    json!(cfg.todo_kind())
                }
                "build_context_blob" => {
                    let st = state_from(&c.input);
                    let todo = c.input.get("todo").cloned().unwrap_or(json!({}));
                    let prior: Vec<Value> = c
                        .input
                        .get("prior_results")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    json!(TodoRunnerNode::build_context_blob(&st, &todo, &prior))
                }
                "decision_machine" => decision_delta(&c.input),
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
        println!("golden todo_runner: {checked} casi verificati, tutti verdi");
    }
}
