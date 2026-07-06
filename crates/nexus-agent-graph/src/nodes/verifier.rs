//! `VerifierNode` — porta la parte DETERMINISTICA del `verifier_node`
//! (`brain/agents/verifier_node.py:47-287` + helper 508-669).
//!
//! Il verifier e' il gate PLAN-PHASE: si attiva quando l'executor emette
//! `end_turn` mentre c'e' un piano attivo, carica gli `acceptance_criteria` del
//! todo attivo, li esegue (deterministico, NO LLM) e decide se avanzare al
//! prossimo todo, ritentare lo stesso, o bloccarlo al cap. E' un nodo
//! DETERMINISTICO come [`crate::nodes::final_gate::FinalGateNode`]: NON chiama
//! l'LLM nel ramo portato (la verifica esplorativa/panel — gli UNICI rami che
//! usano l'LLM — sono OFF di default e NON portati, vedi TODO sotto). L'unico
//! I/O e' l'esecuzione dei criteri (delegata al trait [`CriteriaRunner`]), la
//! lettura/scrittura dei todo ([`TodoStore`]) e la persistenza dell'esito
//! ([`VerifierRunStore`]); la config DB-driven e' risolta a MONTE (regola G).
//!
//! Stato di default: il verifier e' **OFF** (`verifier_enabled=false`,
//! `orchestrator_config.py:175`) cosi' il sistema continua a comportarsi come
//! prima del PR-2. Il path Rust inoltre NON e' instradato (porting incrementale:
//! golden 1:1 + shadow PRIMA di imboccare il path).
//!
//! ## Cosa porta QUESTO modulo (deterministico, golden 1:1)
//!
//! - **La decision machine** (`verifier_node.py:47-287`, [`VerifierNode::run`]):
//!   guard `verifier_enabled`/`plan_phase_active`/`thread_id`; risoluzione
//!   `active_todo_id` (via [`TodoStore::active_todo`] se assente); parse degli
//!   `acceptance_criteria` (json.loads se stringa); CONTEGGIO criteri evaluable
//!   vs inconclusive (un criterio con `evidence.inconclusive` non conta ne' pass
//!   ne' fail, `verifier_node.py:163-181`); `all_passed` sugli evaluable;
//!   `verify_cycle++`; branch pass/cap/retry; ramo fail-closed (criteri assenti
//!   o tutti-inconclusive su task software); prefisso `<autonomy_hint>` per le
//!   modalita' autonome.
//! - **`_pick_next_todo`** (`verifier_node.py:508-534`): DELEGATO al PUNTO UNICO
//!   [`crate::decisions::dag_scheduler::pick_next_todo`] (regola L: identico a
//!   quel che usa il `todo_runner`, NON re-implementato).
//! - **`_advance_or_end`** (`verifier_node.py:537-558`, [`VerifierNode::advance_or_end`]):
//!   rilegge i todo, delega la selezione a `pick_next_todo`; nessun next ->
//!   `{active_todo_id:None, stop_reason:end_turn}`; altrimenti marca il prossimo
//!   `in_progress` + `{active_todo_id, stop_reason:tool_use, current_todos}`.
//! - **`_render_failed_block`** (`verifier_node.py:604-639`,
//!   [`VerifierNode::render_failed_block`]): testo `<verification_failed>` con
//!   evidence (json trunc 300), output diagnostico (trunc 800), remediation.
//! - **`_suggest_remediation`** (`verifier_node.py:642-669`,
//!   [`suggest_remediation`]): euristica per tipo criterion (http/run_command/
//!   file_exists/db_query/regex_in_output), su `evidence`.
//! - **`_is_software_task`** (per il ramo fail-closed): DELEGATO al PUNTO UNICO
//!   [`crate::routing::signals::is_software_task`] (regola L).
//! - **`run_general_gates`** (per il ramo fail-closed, `final_gate.py:381-486`):
//!   i criteri generali sono ESATTAMENTE quelli costruiti da
//!   [`FinalGateNode::build_criteria`] (RIUSO, regola L: il verifier delega a quel
//!   punto + [`FinalGateNode::all_passed`], non duplica la costruzione). Sono i 2
//!   criteri sempre presenti (`no_orphan_imported` + `outputs_exist`) PIU' gli
//!   opzionali risolti a monte nella `FinalGateConfig` (`service_logs_clean`,
//!   `run_command`-build, `http`-endpoint): delegando a `build_criteria` il
//!   verifier li eredita TUTTI automaticamente, senza re-elencarli. L'esecuzione
//!   passa per lo stesso trait [`CriteriaRunner`].
//!
//! ## Cosa NON porta (rami OFF default + TODO espliciti)
//!
//! - **Verifica esplorativa / panel / RAG** (`_run_exploratory_check`,
//!   `_run_verify_panel`, `_rag_past_failures`, `_panel_lens_prompt`,
//!   `verifier_node.py:197-505`): rami OFF di default
//!   (`exploratory_verify_enabled=false`, `verify_panel_enabled=false`,
//!   `orchestrator_config.py:201,230`). Usano un LlmGateway + RAG semantico:
//!   sotto-sistemi da portare in un PR dedicato. Coi default OFF il blocco
//!   `if all_passed and (panel or exploratory)` (`verifier_node.py:197`) e' SEMPRE
//!   falso, quindi questo ramo NON viene mai eseguito -> ZERO divergenza dal
//!   Python. TODO: portare i rami esplorativo/panel quando l'LlmGateway + un
//!   trait RAG saranno disponibili nel runtime. NIENTE LlmGateway nel ramo
//!   deterministico portato.
//! - **`verify_failures` (incremento)**: il `_mark_todo_status` Python
//!   (`verifier_node.py:561-581`) incrementa `verify_failures` SOLO quando il
//!   nuovo status e' `blocked` (`CASE WHEN ... 'blocked' THEN verify_failures+1`).
//!   Il trait [`TodoStore::mark_status`] NON espone `verify_failures` come
//!   parametro: l'incremento e' responsabilita' dell'IMPL CONCRETA dell'UPDATE
//!   (l'unico punto che scrive `nexus_agent_todos`, regola L: non si inventa una
//!   seconda via DB). TODO impl concreta mcp-core: la `mark_status(.., Blocked)`
//!   deve replicare il `CASE WHEN ... verify_failures+1`. Qui il nodo si limita a
//!   chiamare `mark_status(.., Blocked)` come fa il Python.
//! - **Risoluzione config DB** (`orchestrator_config.get()`): tutta lettura DB
//!   (regola G) -> risolta A MONTE dal chiamante in [`VerifierConfig`].
//! - **`project_id`/`NEXUS_PROJECT_ID`/`session_id` nel ctx criteri**: il ctx
//!   concreto del [`CriteriaRunner`] li ricava dal proprio ambiente (mcp-core),
//!   non sono input della decision machine.
//!
//! GATING SHADOW: in `ctx.shadow == true` l'esecuzione criteri usa
//! `ExecMode::Replay` (rilegge i tool_result del primario = zero side-effect);
//! `mark_status` e `VerifierRunStore::record` sono no-op in Replay. Il nodo NON
//! emette eventi e NON scrive. Verificato nei test.
//!
//! Il nodo NON instrada: l'edge post-verifier vive in
//! `routing::route_after_verifier` (gia' portato 1:1).

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::dag_scheduler::{self, TodoStatus};
use crate::nodes::final_gate::FinalGateNode;
use crate::py_json::{py_json_dumps, SortKeys};
use crate::routing::config::RoutingConfig;
use crate::routing::signals;
use crate::runtime::ports::{
    CriteriaRunner, CriterionResult, ExecMode, PortError, TodoStore, VerifierRunRecord,
    VerifierRunStore,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, MessageContent, StateDelta, StopReason};

/// Config DB-driven del nodo verifier, PASSATA (regola G: nessuna lettura DB nel
/// nodo, nessun fallback hardcoded nella logica decisionale).
///
/// Mappa i settings risolti dal brain (`orchestrator_config.get()`):
///   - `verifier_enabled`         -> `agent.verifier.enabled` (default false)
///   - `max_verify_cycles`        -> `agent.verifier.max_cycles` (default 3)
///   - `verifier_fail_closed`     -> `agent.verifier.fail_closed` (default true)
///   - `dag_topological_enabled`  -> `agent.dag.topological_enabled` (default
///     false): serve a [`dag_scheduler::pick_next_todo`] (punto unico).
///   - `exploratory_verify_max_total` -> il cap GLOBALE per run della verifica
///     esplorativa. ATTENZIONE: lato Python questo NON ha un safe-default in
///     `orchestrator_config` (non e' nei `_SAFE_DEFAULTS`); il default `3` vive
///     SOLO nel `.get("exploratory_verify_max_total", 3)` (`verifier_node.py:208`).
///     Qui lo rendiamo ESPLICITO (default 3) per non perdere la semantica.
///     Trasportato per completezza: i rami esplorativo/panel sono OFF e NON
///     portati, quindi non e' letto dal codice di questo PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifierConfig {
    /// Verifier abilitato (`verifier_enabled`, default FALSE). OFF -> pass-through
    /// `{}` (`verifier_node.py:52`).
    pub enabled: bool,
    /// Cap di cicli di verifica per todo (`max_verify_cycles`, default 3). Al cap
    /// il todo viene bloccato (`verifier_node.py:246-247`).
    pub max_verify_cycles: i64,
    /// Fail-closed sui task software (`verifier_fail_closed`, default TRUE): con
    /// criteri assenti / tutti-inconclusive, esegui comunque i gate generali
    /// invece di promuovere a vuoto (`verifier_node.py:89,177`).
    pub fail_closed: bool,
    /// DAG topologico abilitato (`dag_topological_enabled`, default false):
    /// passato a [`dag_scheduler::pick_next_todo`] (punto unico della selezione).
    pub dag_topological_enabled: bool,
    /// Cap GLOBALE per run della verifica esplorativa
    /// (`exploratory_verify_max_total`, default ESPLICITO 3). Non letto in questo
    /// PR (rami esplorativi OFF + non portati): trasportato per fedelta'.
    pub exploratory_verify_max_total: i64,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        // Default IDENTICI ai safe-default del brain (orchestrator_config.py)
        // + il default-locale di exploratory_verify_max_total (verifier_node.py:208).
        // Valgono SOLO se il DB e' irraggiungibile, mai come magic fallback nella
        // logica.
        Self {
            enabled: false,
            max_verify_cycles: 3,
            fail_closed: true,
            dag_topological_enabled: false,
            exploratory_verify_max_total: 3,
        }
    }
}

/// `true` se un criterio NON e' valutabile (inconcludente): la sua evidence porta
/// `inconclusive` truthy (`verifier_node.py:163`,
/// `(r.get("evidence") or {}).get("inconclusive")`). Semantica truthy Python:
/// `true` se il campo e' presente e "verita'" (bool true, numero non-zero,
/// stringa non vuota, lista/oggetto non vuoti); assente / null / falsy -> NON
/// inconcludente (conta come evaluable).
fn is_inconclusive(r: &CriterionResult) -> bool {
    match r.evidence.get("inconclusive") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `_suggest_remediation` (`verifier_node.py:642-669`): hint operativo dedotto
/// dal TIPO del PRIMO criterio fallito + la sua `evidence`. PURA: niente LLM,
/// solo stringhe. Riproduce 1:1 i rami http/run_command/file_exists/db_query/
/// regex_in_output e i due default.
pub fn suggest_remediation(failed: &[CriterionResult]) -> String {
    let Some(first) = failed.first() else {
        return "verifica i criteri e riprova".to_string();
    };
    let ev = &first.evidence;
    match first.criterion_type.as_str() {
        "http" => {
            // `status = ev.get("status")`; None -> servizio giu'.
            match status_as_i64(ev.get("status")) {
                None => {
                    "il servizio HTTP non risponde: verifica che sia avviato sulla porta corretta"
                        .to_string()
                }
                Some(status) if status >= 500 => format!(
                    "HTTP {status}: errore lato server, leggi i log del servizio per la causa"
                ),
                Some(404) => {
                    "HTTP 404: la route non esiste, registra l'endpoint nel router".to_string()
                }
                Some(status) => {
                    format!("HTTP {status} != atteso, verifica la risposta del servizio")
                }
            }
        }
        "run_command" => {
            // `f"comando ritorna exit_code={ev.get('exit_code')}: ..."`. None -> "None".
            let exit_c = python_repr_opt(ev.get("exit_code"));
            format!("comando ritorna exit_code={exit_c}: leggi STDERR e correggi")
        }
        "file_exists" => "il file non esiste sul filesystem: scrivilo con write_file".to_string(),
        "db_query" => {
            // `notes = ev.get("notes") or []`; "; ".join(notes) se non vuota.
            let notes: Vec<String> = ev
                .get("notes")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|n| match n {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if notes.is_empty() {
                "verifica lo schema e lo stato del DB".to_string()
            } else {
                notes.join("; ")
            }
        }
        "regex_in_output" => {
            "il pattern atteso non e' presente nell'output: rivedi il comando o l'output"
                .to_string()
        }
        _ => "rivedi il criterion e applica una correzione mirata".to_string(),
    }
}

/// `int(status or 0)` con la semantica Python: `status` None -> ritorna `None`
/// (caso "servizio giu'", `verifier_node.py:652`); presente -> intero (truthy
/// `or 0` lo lascerebbe a 0 se falsy, ma il ramo None e' gestito a parte). Qui
/// distinguiamo None (assente/null) da un intero presente.
fn status_as_i64(v: Option<&Value>) -> Option<i64> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        // `int(status or 0)`: una stringa non-numerica solleverebbe in Python; nel
        // flusso reale `status` e' sempre un intero. Un valore non numerico ->
        // trattato come 0 (il ramo `>= 500`/`== 404` non scatta).
        Some(_) => Some(0),
    }
}

/// `repr` Python di un campo opzionale interpolato in f-string: `None` se assente,
/// altrimenti il valore. Replica `f"...{ev.get('exit_code')}..."`.
fn python_repr_opt(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => {
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// Nodo verifier. Le dipendenze I/O (`TodoStore`, `CriteriaRunner`,
/// `VerifierRunStore`) sono CAMPI del nodo (come in `FinalGateNode`/
/// `TodoRunnerNode`), non nel ctx: minimizza l'impatto sugli altri nodi. La
/// config DB-driven e' risolta A MONTE (regola G); la decision machine e' qui.
pub struct VerifierNode {
    /// Config DB-driven del verifier (regola G: passata, mai letta dal nodo).
    cfg: VerifierConfig,
    /// Config di routing: serve a `signals::is_software_task` (lista mutator +
    /// whitelist intent) e a [`FinalGateNode`] per i gate generali fail-closed
    /// (punto unico, regola L).
    routing_cfg: RoutingConfig,
    /// Store dei todo (`nexus_agent_todos`). Impl concreta in mcp-core; stub nei test.
    store: Arc<dyn TodoStore>,
    /// Motore criteri (sotto-sistema delegato, condiviso col final_gate). mcp-core
    /// lo implementera' col ToolRunner gRPC; nei test e' stubato.
    criteria: Arc<dyn CriteriaRunner>,
    /// Persistenza degli esiti del verifier (`nexus_agent_verifier_runs`,
    /// best-effort, no-op in shadow). Impl concreta in mcp-core; stub nei test.
    runs: Arc<dyn VerifierRunStore>,
    /// Nodo final_gate per i gate generali fail-closed (RIUSO di
    /// `build_criteria`/`all_passed`, regola L: nessuna duplicazione di
    /// `run_general_gates`). Condivide lo stesso `CriteriaRunner`.
    final_gate: FinalGateNode,
}

impl VerifierNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante e le
    /// porte I/O concrete (o stub nei test). Il [`FinalGateNode`] interno e'
    /// costruito con la `FinalGateConfig` data (per i gate generali fail-closed) e
    /// condivide lo stesso `CriteriaRunner` (un solo motore criteri).
    pub fn new(
        cfg: VerifierConfig,
        final_gate_cfg: crate::nodes::final_gate::FinalGateConfig,
        routing_cfg: RoutingConfig,
        store: Arc<dyn TodoStore>,
        criteria: Arc<dyn CriteriaRunner>,
        runs: Arc<dyn VerifierRunStore>,
        meta_steps: Arc<dyn crate::runtime::ports::MetaStepStore>,
    ) -> Self {
        let final_gate =
            FinalGateNode::new(final_gate_cfg, routing_cfg.clone(), criteria.clone(), meta_steps);
        Self {
            cfg,
            routing_cfg,
            store,
            criteria,
            runs,
            final_gate,
        }
    }

    /// `_advance_or_end` (`verifier_node.py:537-558`): rilegge i todo, delega la
    /// selezione al PUNTO UNICO [`dag_scheduler::pick_next_todo`] (regola L). Se
    /// non c'e' prossimo -> `{active_todo_id:None, stop_reason:end_turn}`;
    /// altrimenti marca il prossimo `in_progress` e ritorna `{active_todo_id,
    /// stop_reason:tool_use, current_todos}`. `mode` propaga il gate shadow.
    async fn advance_or_end(
        &self,
        run_id: &str,
        mode: ExecMode,
    ) -> Result<StateDelta, NodeError> {
        let todos = self.store.list_todos(run_id).await.map_err(port_err)?;
        match dag_scheduler::pick_next_todo(&todos, self.cfg.dag_topological_enabled) {
            None => {
                tracing::info!(
                    target: "nexus_agent_graph::verifier",
                    total = todos.len(),
                    "tutti i todo terminali -> end_turn"
                );
                Ok(StateDelta {
                    active_todo_id: Some(None),
                    stop_reason: Some(Some(StopReason::EndTurn)),
                    ..Default::default()
                })
            }
            Some(next) => {
                let next_id = next.id.clone();
                self.store
                    .mark_status(&next_id, TodoStatus::InProgress, mode)
                    .await
                    .map_err(port_err)?;
                Ok(StateDelta {
                    active_todo_id: Some(Some(next_id)),
                    stop_reason: Some(Some(StopReason::ToolUse)),
                    current_todos: Some(Some(todos_to_values(&todos))),
                    ..Default::default()
                })
            }
        }
    }

    /// `_render_failed_block` (`verifier_node.py:604-639`). PURA: i risultati
    /// arrivano gia' calcolati. Riproduce 1:1 il corpo `<verification_failed>`:
    /// criteri falliti renderizzati (`[{type}] {json(evidence)[:300]}`), output
    /// diagnostico (`output_excerpt` or `error` del primo fallito, trunc 800),
    /// suggerimento ([`suggest_remediation`]). NOTA: il template DB
    /// `verification.failed_block` (registry) NON e' portato (lettura DB, regola
    /// G); qui si usa SEMPRE il fallback inline, che e' anche il ramo del Python
    /// quando il template e' assente/vuoto (`verifier_node.py:624-639`).
    pub fn render_failed_block(
        todo_content: &str,
        cycle: i64,
        max_cycles: i64,
        results: &[CriterionResult],
    ) -> String {
        let failed: Vec<&CriterionResult> = results.iter().filter(|r| !r.passed).collect();
        // failed_rendered: "- [{type}] {json.dumps(evidence)[:300]}" per ciascuno.
        let failed_rendered = failed
            .iter()
            .map(|r| {
                // json.dumps(..., ensure_ascii=False)[:300] -> formato Python
                // (separatori ", " / ": ", ensure_ascii=False, ORDINE
                // d'inserimento: il verifier non usa sort_keys) + taglio su CHAR.
                // Delega al PUNTO UNICO py_json (regola L).
                let ev_json = py_json_dumps(&r.evidence, SortKeys::No);
                let ev_trunc: String = ev_json.chars().take(300).collect();
                format!("- [{}] {ev_trunc}", criterion_type_repr(&r.criterion_type))
            })
            .collect::<Vec<_>>()
            .join("\n");

        // diagnostic = failed[0].evidence["output_excerpt"] or ["error"] or "".
        // Solo se c'e' un primo fallito con evidence (Python: `if failed and
        // failed[0].get("evidence")`). Semantica `or` FALSY (stringa vuota cade).
        let diagnostic = failed
            .first()
            .map(|r| {
                str_truthy(r.evidence.get("output_excerpt"))
                    .or_else(|| str_truthy(r.evidence.get("error")))
                    .unwrap_or("")
            })
            .unwrap_or("");
        // diagnostic[:800] su CHAR.
        let diagnostic_trunc: String = diagnostic.chars().take(800).collect();

        let remediation = suggest_remediation(
            &failed.iter().map(|r| (*r).clone()).collect::<Vec<_>>(),
        );

        format!(
            "<verification_failed cycle=\"{cycle}/{max_cycles}\" todo=\"{todo_content}\">\n\
             Acceptance criteria falliti:\n{failed_rendered}\n\n\
             Output diagnostico:\n{diagnostic_trunc}\n\n\
             Suggerimento operativo: {remediation}\n\
             </verification_failed>"
        )
    }

    /// Prefisso `<autonomy_hint>` per le modalita' autonome
    /// (`verifier_node.py:266-279`): `behavior_mode` trimmed+lower in {automatic,
    /// automatico, continuous, continuo}. PURA. Ritorna `None` se non autonomo.
    fn autonomy_prefix(state: &AgentState, max_cycles: i64) -> Option<String> {
        let behavior_mode = state
            .behavior_mode
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !matches!(
            behavior_mode.as_str(),
            "automatic" | "automatico" | "continuous" | "continuo"
        ) {
            return None;
        }
        Some(format!(
            "<autonomy_hint mode=\"{behavior_mode}\">\n\
             L'utente ha selezionato la modalita' '{behavior_mode}': procedi\n\
             AUTONOMAMENTE col retry. NON chiedere conferma all'utente, NON\n\
             scrivere domande tipo 'Vuoi che lo faccia?' o 'Confermi?'. Esegui\n\
             direttamente le azioni necessarie usando i tool disponibili per\n\
             risolvere i criteri di accettazione falliti. Se non riesci dopo\n\
             questo ciclo, l'agente verra' automaticamente bloccato dal verifier\n\
             al raggiungimento del cap di {max_cycles} cicli.\n\
             </autonomy_hint>\n\n"
        ))
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for VerifierNode {
    fn id(&self) -> NodeId {
        NodeId::Verifier
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Guard enabled / plan_phase (verifier_node.py:52) ──────────────────
        if !self.cfg.enabled || !state.plan_phase_active.unwrap_or(false) {
            return Ok(pass_through());
        }

        // ── thread_id assente -> {} (verifier_node.py:55-59) ──────────────────
        let run_id = match state.thread_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => {
                tracing::debug!(target: "nexus_agent_graph::verifier", "thread_id assente, skip");
                return Ok(pass_through());
            }
        };

        // Punto unico del gate shadow (regola L): Replay in shadow -> mark_status
        // / persist no-op (zero scritture).
        let mode = ctx.exec_mode();

        // ── Risoluzione active_todo_id (verifier_node.py:56-67) ───────────────
        // Se assente, prova a calcolarlo via TodoStore::active_todo.
        let active_todo_id = match state
            .active_todo_id
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            Some(id) => id.to_string(),
            None => match self.store.active_todo(&run_id).await.map_err(port_err)? {
                Some(t) => t.id,
                None => {
                    tracing::debug!(
                        target: "nexus_agent_graph::verifier",
                        "nessun todo attivo, skip"
                    );
                    return Ok(pass_through());
                }
            },
        };

        // ── Carica todo + acceptance_criteria (verifier_node.py:70-81) ────────
        let todos = self.store.list_todos(&run_id).await.map_err(port_err)?;
        // Il todo completo (con content/acceptance_criteria) arriva come Value
        // opaco. Il modello DAG `Todo` non li trasporta: nel runtime concreto la
        // TodoStore mappera' il todo completo (TODO impl concreta, gia' annotato
        // per il todo_runner). Per ora i campi non-DAG sono assenti -> criteri
        // vuoti = ramo fail-closed, content vuoto nel render.
        let todo_value = todo_value_of(&todos, &active_todo_id);
        if todo_value.is_null() {
            tracing::warn!(
                target: "nexus_agent_graph::verifier",
                todo_id = %active_todo_id,
                "todo non trovato nel DB"
            );
            return Ok(pass_through());
        }
        let todo_content = str_truthy(todo_value.get("content")).unwrap_or("");
        let criteria_raw = normalize_criteria(todo_value.get("acceptance_criteria"));

        // ── Ramo "nessun criterion" (verifier_node.py:82-133) ────────────────
        if criteria_raw.is_empty() {
            return self
                .fail_closed_or_complete(state, &run_id, &active_todo_id, todo_content, mode)
                .await;
        }

        // ── Esegui tutti i criteria (verifier_node.py:135-159) ────────────────
        let started = Instant::now();
        let mut results = self
            .criteria
            .run(criteria_raw, mode)
            .await
            .map_err(|e| NodeError::Failed {
                node: "verifier",
                message: format!("esecuzione criteri fallita: {e}"),
            })?;
        let duration_ms = started.elapsed().as_millis() as i64;

        // ── Conteggio evaluable vs inconclusive (verifier_node.py:163-181) ────
        // Un criterio con evidence.inconclusive truthy NON conta ne' pass ne'
        // fail: e' escluso dal conteggio. all_passed = all(passed) sugli evaluable.
        let evaluable: Vec<&CriterionResult> =
            results.iter().filter(|r| !is_inconclusive(r)).collect();
        let inconclusive_n = results.len() - evaluable.len();
        if inconclusive_n > 0 {
            tracing::warn!(
                target: "nexus_agent_graph::verifier",
                inconclusive = inconclusive_n,
                total = results.len(),
                "criteri inconcludenti esclusi dal conteggio"
            );
        }
        let all_passed = if !evaluable.is_empty() {
            evaluable.iter().all(|r| r.passed)
        } else {
            // Tutti inconcludenti: fail-closed su task software (gate generali),
            // altrimenti passato (nulla di valutabile). I gate generali vengono
            // ACCODATI ai results (verifier_node.py:178-181) cosi' la persistenza
            // / il render li vede.
            if self.cfg.fail_closed && signals::is_software_task(state, &self.routing_cfg) {
                let (gate_passed, gate_results) = self.run_general_gates(state, mode).await?;
                results.extend(gate_results);
                gate_passed
            } else {
                true
            }
        };

        let cycle = state.verify_cycle.unwrap_or(0) + 1;

        // ── Persistenza best-effort (verifier_node.py:185) ────────────────────
        // Scrittura DB gated su shadow (no-op in Replay). I results includono gli
        // eventuali gate generali accodati sopra.
        self.runs
            .record(
                VerifierRunRecord {
                    run_id: run_id.clone(),
                    todo_id: active_todo_id.clone(),
                    cycle,
                    criteria_results: results_to_value(&results),
                    passed: all_passed,
                    duration_ms,
                },
                mode,
            )
            .await
            .map_err(port_err)?;

        tracing::info!(
            target: "nexus_agent_graph::verifier",
            todo_id = %active_todo_id,
            cycle,
            all_passed,
            criteria = results.len(),
            duration_ms,
            "verifier: esito criteri"
        );

        // ── Rami esplorativo/panel (verifier_node.py:197-236): OFF + NON portati.
        //     Coi default OFF il blocco `if all_passed and (panel or expl)` e'
        //     SEMPRE falso -> non eseguito (vedi TODO di modulo). NIENTE LLM qui.

        // ── Branch su esito (verifier_node.py:238-287) ────────────────────────
        if all_passed {
            // PASSED: completa + advance, azzera i due cicli.
            self.store
                .mark_status(&active_todo_id, TodoStatus::Completed, mode)
                .await
                .map_err(port_err)?;
            let mut advance = self.advance_or_end(&run_id, mode).await?;
            advance.verify_cycle = Some(Some(0));
            advance.exploratory_verify_cycle = Some(Some(0));
            tracing::info!(
                target: "nexus_agent_graph::verifier",
                todo_id = %active_todo_id,
                "verifier: tutti i criteri passati -> advance"
            );
            return Ok(advance.into_opaque());
        }

        // CAP: cycle >= max -> blocked + advance + verifier_last_result.
        let max_cycles = self.cfg.max_verify_cycles;
        if cycle >= max_cycles {
            self.store
                .mark_status(&active_todo_id, TodoStatus::Blocked, mode)
                .await
                .map_err(port_err)?;
            tracing::warn!(
                target: "nexus_agent_graph::verifier",
                todo_id = %active_todo_id,
                cycle,
                "verifier: cap raggiunto -> blocked"
            );
            let mut advance = self.advance_or_end(&run_id, mode).await?;
            advance.verify_cycle = Some(Some(0));
            advance.verifier_last_result = Some(Some(json!({
                "passed": false,
                "cycle": cycle,
                "results": results_to_value(&results),
            })));
            return Ok(advance.into_opaque());
        }

        // RETRY: cycle < max -> HumanMessage <verification_failed> + tool_use.
        let block = self.build_failed_message(state, todo_content, cycle, max_cycles, &results);
        Ok(StateDelta {
            messages: Some(vec![Message::Human {
                content: MessageContent::text(block),
            }]),
            verify_cycle: Some(Some(cycle)),
            verifier_last_result: Some(Some(json!({
                "passed": false,
                "cycle": cycle,
                "results": results_to_value(&results),
            }))),
            stop_reason: Some(Some(StopReason::ToolUse)),
            // pending_tool_uses azzerato a lista vuota (durata 1 turno):
            // Some(Some(vec![])) e' AZZERA, distinto da None (no-op).
            pending_tool_uses: Some(Some(vec![])),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl VerifierNode {
    /// Ramo "nessun criterion sul todo" (`verifier_node.py:82-133`): fail-closed
    /// sui task software (gate generali), altrimenti completa + advance. Identico
    /// al ramo "tutti inconcludenti" ma SENZA risultati criteri da accodare (qui
    /// la lista dei criteri specifici e' vuota). Riusa i gate generali del
    /// `final_gate` (RIUSO, regola L).
    async fn fail_closed_or_complete(
        &self,
        state: &AgentState,
        run_id: &str,
        active_todo_id: &str,
        todo_content: &str,
        mode: ExecMode,
    ) -> Result<OpaqueDelta, NodeError> {
        // Task non software / fail-closed OFF: comportamento storico (completed +
        // advance), senza gate generali (verifier_node.py:131-133).
        if !(self.cfg.fail_closed && signals::is_software_task(state, &self.routing_cfg)) {
            self.store
                .mark_status(active_todo_id, TodoStatus::Completed, mode)
                .await
                .map_err(port_err)?;
            return Ok(self.advance_or_end(run_id, mode).await?.into_opaque());
        }

        // Fail-closed: esegui i gate generali (no_orphan_imported + outputs_exist).
        let (gate_passed, gate_results) = self.run_general_gates(state, mode).await?;
        if gate_passed {
            self.store
                .mark_status(active_todo_id, TodoStatus::Completed, mode)
                .await
                .map_err(port_err)?;
            return Ok(self.advance_or_end(run_id, mode).await?.into_opaque());
        }

        // Gate generale fallito: cap / retry (verifier_node.py:94-130).
        let fc_cycle = state.verify_cycle.unwrap_or(0) + 1;
        let fc_max = self.cfg.max_verify_cycles;
        if fc_cycle >= fc_max {
            self.store
                .mark_status(active_todo_id, TodoStatus::Blocked, mode)
                .await
                .map_err(port_err)?;
            tracing::warn!(
                target: "nexus_agent_graph::verifier",
                todo_id = %active_todo_id,
                cycle = fc_cycle,
                "verifier: gate generale fallito -> blocked"
            );
            let mut advance = self.advance_or_end(run_id, mode).await?;
            advance.verify_cycle = Some(Some(0));
            advance.verifier_last_result = Some(Some(json!({
                "passed": false,
                "cycle": fc_cycle,
                "results": results_to_value(&gate_results),
            })));
            return Ok(advance.into_opaque());
        }

        // Retry: inietta il verdetto e rimanda all'executor.
        tracing::info!(
            target: "nexus_agent_graph::verifier",
            cycle = fc_cycle,
            max = fc_max,
            "verifier: gate generale fallito -> retry executor"
        );
        let block = self.build_failed_message(state, todo_content, fc_cycle, fc_max, &gate_results);
        Ok(StateDelta {
            messages: Some(vec![Message::Human {
                content: MessageContent::text(block),
            }]),
            verify_cycle: Some(Some(fc_cycle)),
            verifier_last_result: Some(Some(json!({
                "passed": false,
                "cycle": fc_cycle,
                "results": results_to_value(&gate_results),
            }))),
            stop_reason: Some(Some(StopReason::ToolUse)),
            pending_tool_uses: Some(Some(vec![])),
            ..Default::default()
        }
        .into_opaque())
    }

    /// Esegue i gate generali (`run_general_gates`, `final_gate.py:381-486`)
    /// DELEGANDO al [`FinalGateNode`] (RIUSO di `build_criteria` + `all_passed`,
    /// regola L: nessuna duplicazione della costruzione). `build_criteria` produce
    /// i 2 criteri sempre presenti + gli opzionali (service_logs_clean, build,
    /// http-endpoint) risolti a monte nella `FinalGateConfig`: il fail-closed del
    /// verifier eredita anche il criterio endpoint senza re-implementarlo. Ritorna
    /// `(all_passed, results)`. `mode` propaga il gate shadow al `CriteriaRunner`.
    async fn run_general_gates(
        &self,
        state: &AgentState,
        mode: ExecMode,
    ) -> Result<(bool, Vec<CriterionResult>), NodeError> {
        let criteria = self.final_gate.build_criteria(state);
        let results = self
            .criteria
            .run(criteria, mode)
            .await
            .map_err(|e| NodeError::Failed {
                node: "verifier",
                message: format!("gate generali falliti: {e}"),
            })?;
        let passed = FinalGateNode::all_passed(&results);
        Ok((passed, results))
    }

    /// Costruisce il testo del HumanMessage da iniettare al retry: il blocco
    /// `<verification_failed>` con eventuale prefisso `<autonomy_hint>`. Centralizza
    /// la composizione condivisa dai due rami retry (criteri specifici + gate
    /// generali), regola L.
    fn build_failed_message(
        &self,
        state: &AgentState,
        todo_content: &str,
        cycle: i64,
        max_cycles: i64,
        results: &[CriterionResult],
    ) -> String {
        let block = Self::render_failed_block(todo_content, cycle, max_cycles, results);
        match Self::autonomy_prefix(state, max_cycles) {
            Some(prefix) => format!("{prefix}{block}"),
            None => block,
        }
    }
}

/// Delta pass-through `{}` (`verifier_node.py:53,59,66,74`): nessun campo
/// modificato, il flusso prosegue (route_after_verifier instrada al learner o
/// re-executor in base allo stop_reason invariato).
fn pass_through() -> OpaqueDelta {
    StateDelta::default().into_opaque()
}

/// Mappa un `PortError` su `NodeError::Failed` del nodo verifier.
fn port_err(e: PortError) -> NodeError {
    NodeError::Failed {
        node: "verifier",
        message: e.to_string(),
    }
}

/// Estrae una `&str` con la semantica `or` FALSY Python: `Some(s)` solo se il
/// campo e' una stringa NON vuota; "" / tipo diverso / assente -> `None` (cade
/// sul campo successivo). Identico a `final_gate::str_truthy` (privato la');
/// duplicato MINIMALE locale per non esportare un helper privato cross-modulo.
fn str_truthy(v: Option<&Value>) -> Option<&str> {
    match v.and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// `r.get("type")` interpolato in f-string: `None` se assente (`verifier_node.py`
/// usa `c.get("type")` che diventa "None" in interpolazione). Il
/// `CriterionResult.criterion_type` Rust e' sempre una stringa (anche vuota):
/// se vuota la rendiamo come stringa vuota (il Python avrebbe il valore reale).
fn criterion_type_repr(t: &str) -> String {
    if t.is_empty() {
        "None".to_string()
    } else {
        t.to_string()
    }
}

/// Normalizza `acceptance_criteria` del todo (`verifier_node.py:76-81`): lista ->
/// [`crate::runtime::ports::CriterionSpec`]; stringa -> json.loads (se array);
/// altro/assente -> vuoto. Ogni elemento e' una spec `{type, spec, expected}`
/// (l'`id` viene assegnato dal runner se assente, qui non serve per il golden
/// deterministico). I campi mancanti diventano default coerenti col Python.
fn normalize_criteria(v: Option<&Value>) -> Vec<crate::runtime::ports::CriterionSpec> {
    let arr = match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    arr.into_iter()
        .filter_map(|c| {
            let map = c.as_object()?;
            Some(crate::runtime::ports::CriterionSpec {
                criterion_type: map
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                spec: map.get("spec").cloned().unwrap_or(json!({})),
                expected: map.get("expected").cloned().unwrap_or(json!({})),
                timeout_s: map.get("timeout_s").and_then(Value::as_f64),
            })
        })
        .collect()
}

/// Serializza i `Todo` (forma DAG) in `Vec<Value>` per `current_todos` dello
/// stato. Round-trip via serde (id/status/depends_on/seq). Identico al
/// `todos_to_values` del todo_runner (helper locale minimale, regola L: la logica
/// e' la stessa serializzazione serde, non una decisione duplicata).
fn todos_to_values(todos: &[dag_scheduler::Todo]) -> Vec<Value> {
    todos
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
        .collect()
}

/// Restituisce il `Todo` (forma DAG) come `Value` opaco, o `Value::Null` se non
/// trovato. Vedi nota in `todo_runner::todo_value_of`: il modello DAG non porta i
/// campi non-DAG (content/acceptance_criteria); nel runtime concreto la
/// `TodoStore` mappera' il todo completo (TODO impl concreta).
fn todo_value_of(todos: &[dag_scheduler::Todo], todo_id: &str) -> Value {
    todos
        .iter()
        .find(|t| t.id == todo_id)
        .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

/// Serializza i [`CriterionResult`] nella forma `{id?, type, passed, evidence}`
/// per `verifier_last_result.results` / la persistenza. Il Python porta anche
/// `id`; qui il `CriterionResult` non lo modella (assegnato dal runner), quindi
/// emettiamo `{type, passed, evidence}` (l'`id` non e' load-bearing per le
/// decisioni). I campi sono quelli usati da render/persist.
fn results_to_value(results: &[CriterionResult]) -> Value {
    Value::Array(
        results
            .iter()
            .map(|r| {
                json!({
                    "type": r.criterion_type,
                    "passed": r.passed,
                    "evidence": r.evidence,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::decisions::dag_scheduler::{Todo, TodoStatus};
    use crate::nodes::final_gate::FinalGateConfig;
    use crate::runtime::test_doubles::{
        NullEventSink, StubCriteriaRunner, StubLlmGateway, StubToolExecutor, StubTodoStore,
        StubVerifierRunStore,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, ContentBlock, Message, MessageContent};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    fn ok_result(t: &str) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            passed: true,
            evidence: json!({}),
        }
    }

    fn fail_result(t: &str, evidence: Value) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            passed: false,
            evidence,
        }
    }

    fn inconclusive_result(t: &str) -> CriterionResult {
        CriterionResult {
            criterion_type: t.to_string(),
            passed: false,
            evidence: json!({"inconclusive": true}),
        }
    }

    /// Ctx con flag shadow. Le porte del verifier vivono nel nodo (non nel ctx);
    /// qui basta lo shadow per derivare la ExecMode. PgPool lazy (il verifier non
    /// scrive DB direttamente: tutto via i trait), LLM/tool stub mai usati.
    fn ctx_with(shadow: bool) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
        }
    }

    fn node_with(
        cfg: VerifierConfig,
        store: Arc<dyn TodoStore>,
        criteria: Arc<dyn CriteriaRunner>,
        runs: Arc<dyn VerifierRunStore>,
    ) -> VerifierNode {
        VerifierNode::new(
            cfg,
            FinalGateConfig::default(),
            RoutingConfig::default(),
            store,
            criteria,
            runs,
            Arc::new(crate::runtime::test_doubles::StubMetaStepStore::default()),
        )
    }

    fn enabled_cfg() -> VerifierConfig {
        VerifierConfig {
            enabled: true,
            ..VerifierConfig::default()
        }
    }

    fn todo(id: &str, status: TodoStatus, deps: &[&str], seq: i64) -> Todo {
        Todo {
            id: id.to_string(),
            status,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            seq: Some(seq),
            write_scope: Vec::new(),
        }
    }

    /// Messaggio AI con un tool_use mutativo: rende il task "software"
    /// strutturalmente (write_file e' in fs_mutator_tools), cosi' il ramo
    /// fail-closed scatta.
    fn ai_mutation() -> Message {
        Message::Ai {
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "write_file".into(),
                input: json!({"path": "src/main.tsx"}),
                thought_signature: None,
            }]),
            tool_calls: vec![],
            reasoning: None,
            thinking_signature: None,
        }
    }

    /// Stato plan-phase con active_todo_id e thread_id.
    fn plan_state(active_todo: &str, software: bool) -> AgentState {
        AgentState {
            plan_phase_active: Some(true),
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            active_todo_id: Some(active_todo.to_string()),
            messages: if software { vec![ai_mutation()] } else { vec![] },
            ..Default::default()
        }
    }

    // ── Guard ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn off_passthrough() {
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![]));
        let runs = Arc::new(StubVerifierRunStore::default());
        // Default: enabled=false -> pass-through {}.
        let node = node_with(VerifierConfig::default(), store.clone(), runner.clone(), runs);
        let ctx = ctx_with(false);
        let st = plan_state("a", false);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, None);
        assert_eq!(out.verify_cycle, None);
        assert!(runner.seen.lock().unwrap().is_empty());
        assert!(store.marks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_plan_phase_passthrough() {
        let store = Arc::new(StubTodoStore::with_todos(vec![]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![]));
        let runs = Arc::new(StubVerifierRunStore::default());
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs);
        let ctx = ctx_with(false);
        let mut st = plan_state("a", false);
        st.plan_phase_active = Some(false); // non plan-phase -> {}.
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, None);
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn todo_non_trovato_passthrough() {
        // active_todo_id punta a un todo che non e' nella lista -> {}.
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::InProgress,
            &[],
            1,
        )]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![]));
        let runs = Arc::new(StubVerifierRunStore::default());
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs);
        let ctx = ctx_with(false);
        let st = plan_state("inesistente", false);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, None);
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    // ── PASSED -> advance ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn passed_advance_reentry() {
        // Todo "a" con criterio file_exists che passa; "b" pending -> advance a b.
        // Iniettiamo i criteri via lo store (il todo_value DAG non porta criteri,
        // quindi serializziamo un Todo arricchito non e' possibile: usiamo il ramo
        // gate generale? No: qui vogliamo i criteri specifici). Usiamo il fatto che
        // normalize_criteria legge da todo_value.acceptance_criteria; il modello
        // Todo DAG non li porta. Quindi questo test esercita il ramo "nessun
        // criterion" -> fail-closed. Per il ramo criteri-specifici vedi golden.
        // Qui: todo NON software (no mutation), nessun criterion -> completed+advance.
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::InProgress, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![]));
        let runs = Arc::new(StubVerifierRunStore::default());
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs.clone());
        let ctx = ctx_with(false);
        let st = plan_state("a", false); // non software -> niente fail-closed.
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // a completato (nessun criterion + non software -> completed), advance a b.
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.active_todo_id.as_deref(), Some("b"));
        let marks = store.marks.lock().unwrap();
        assert!(marks.contains(&("a".to_string(), TodoStatus::Completed)));
        assert!(marks.contains(&("b".to_string(), TodoStatus::InProgress)));
    }

    #[tokio::test]
    async fn fail_closed_gate_passa_completa() {
        // Software + nessun criterion + gate generale PASSA -> completed + advance.
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::InProgress, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        // I gate generali (no_orphan + outputs_exist) passano.
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("no_orphan_imported"),
            ok_result("outputs_exist"),
        ]));
        let runs = Arc::new(StubVerifierRunStore::default());
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs);
        let ctx = ctx_with(false);
        let st = plan_state("a", true); // software.
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.active_todo_id.as_deref(), Some("b"));
        // Gate generali eseguiti.
        assert_eq!(runner.seen.lock().unwrap().len(), 1);
        let marks = store.marks.lock().unwrap();
        assert!(marks.contains(&("a".to_string(), TodoStatus::Completed)));
    }

    #[tokio::test]
    async fn fail_closed_gate_fallisce_retry() {
        // Software + nessun criterion + gate generale FALLISCE + cycle<max -> retry.
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::InProgress,
            &[],
            1,
        )]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "placeholder rilevato"}),
        )]));
        let runs = Arc::new(StubVerifierRunStore::default());
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs);
        let ctx = ctx_with(false);
        let st = plan_state("a", true);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(out.verify_cycle, Some(1));
        assert_eq!(out.pending_tool_uses, Some(vec![]));
        // HumanMessage <verification_failed> iniettato.
        let last = out.messages.last().expect("messaggio");
        match last {
            Message::Human { content } => {
                let text = content.flatten_text();
                assert!(text.contains("<verification_failed cycle=\"1/3\""));
            }
            other => panic!("atteso HumanMessage, trovato {other:?}"),
        }
    }

    // ── CAP -> blocked ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn cap_raggiunto_blocked() {
        // Software + gate fallisce + verify_cycle gia' a max-1 -> cap -> blocked.
        let store = Arc::new(StubTodoStore::with_todos(vec![todo(
            "a",
            TodoStatus::InProgress,
            &[],
            1,
        )]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![fail_result(
            "no_orphan_imported",
            json!({"verdict": "fail"}),
        )]));
        let runs = Arc::new(StubVerifierRunStore::default());
        // max_verify_cycles=3, verify_cycle gia' a 2 -> cycle=3 >= 3 -> cap.
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs);
        let ctx = ctx_with(false);
        let mut st = plan_state("a", true);
        st.verify_cycle = Some(2);
        let out = apply(st.clone(), node.run(&st, &ctx).await.expect("run ok"));
        // Blocca + advance (nessun altro pending -> end_turn) + verifier_last_result.
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.verify_cycle, Some(0));
        assert!(out.verifier_last_result.is_some());
        let marks = store.marks.lock().unwrap();
        assert!(marks.contains(&("a".to_string(), TodoStatus::Blocked)));
    }

    // ── Shadow: Replay, zero scritture ────────────────────────────────────────────

    #[tokio::test]
    async fn shadow_replay_zero_scritture() {
        let store = Arc::new(StubTodoStore::with_todos(vec![
            todo("a", TodoStatus::InProgress, &[], 1),
            todo("b", TodoStatus::Pending, &[], 2),
        ]));
        let runner = Arc::new(StubCriteriaRunner::with_results(vec![
            ok_result("no_orphan_imported"),
            ok_result("outputs_exist"),
        ]));
        let runs = Arc::new(StubVerifierRunStore::default());
        let node = node_with(enabled_cfg(), store.clone(), runner.clone(), runs.clone());
        let ctx = ctx_with(true); // shadow.
        let st = plan_state("a", true);
        let _ = node.run(&st, &ctx).await.expect("run ok");
        // Criteri eseguiti in Replay.
        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen[0].1, ExecMode::Replay);
        // mark_status no-op in shadow (marks vuoto).
        assert!(store.marks.lock().unwrap().is_empty(), "zero scritture todo");
        // persist no-op in shadow (records vuoto).
        assert!(runs.records.lock().unwrap().is_empty(), "zero persist");
    }

    // ── Conteggio evaluable / inconclusive ────────────────────────────────────────

    #[test]
    fn is_inconclusive_semantica_truthy() {
        assert!(is_inconclusive(&inconclusive_result("http")));
        assert!(!is_inconclusive(&ok_result("http")));
        // false / null / assente -> NON inconcludente.
        assert!(!is_inconclusive(&CriterionResult {
            criterion_type: "x".into(),
            passed: true,
            evidence: json!({"inconclusive": false}),
        }));
        assert!(!is_inconclusive(&CriterionResult {
            criterion_type: "x".into(),
            passed: true,
            evidence: json!({"inconclusive": null}),
        }));
    }

    // ── render_failed_block / suggest_remediation ─────────────────────────────────

    #[test]
    fn render_failed_block_struttura() {
        let results = vec![fail_result(
            "http",
            json!({"status": 500, "output_excerpt": "Internal Server Error"}),
        )];
        let block = VerifierNode::render_failed_block("Crea endpoint", 1, 3, &results);
        assert!(block.contains("<verification_failed cycle=\"1/3\" todo=\"Crea endpoint\">"));
        assert!(block.contains("[http]"));
        assert!(block.contains("Internal Server Error"));
        assert!(block.contains("HTTP 500"));
    }

    #[test]
    fn suggest_remediation_per_tipo() {
        assert_eq!(
            suggest_remediation(&[]),
            "verifica i criteri e riprova"
        );
        assert!(suggest_remediation(&[fail_result("http", json!({}))])
            .contains("non risponde"));
        assert!(suggest_remediation(&[fail_result("http", json!({"status": 503}))])
            .contains("HTTP 503"));
        assert!(suggest_remediation(&[fail_result("http", json!({"status": 404}))])
            .contains("404"));
        assert!(
            suggest_remediation(&[fail_result("run_command", json!({"exit_code": 2}))])
                .contains("exit_code=2")
        );
        assert!(suggest_remediation(&[fail_result("file_exists", json!({}))])
            .contains("write_file"));
        assert!(suggest_remediation(&[fail_result(
            "db_query",
            json!({"notes": ["tabella mancante", "indice assente"]})
        )])
        .contains("tabella mancante; indice assente"));
        assert!(suggest_remediation(&[fail_result("regex_in_output", json!({}))])
            .contains("pattern atteso"));
        assert!(suggest_remediation(&[fail_result("sconosciuto", json!({}))])
            .contains("correzione mirata"));
    }

    #[test]
    fn autonomy_prefix_solo_autonomo() {
        let mut st = plan_state("a", false);
        assert!(VerifierNode::autonomy_prefix(&st, 3).is_none());
        st.behavior_mode = Some("automatico".to_string());
        let p = VerifierNode::autonomy_prefix(&st, 3).expect("prefisso");
        assert!(p.contains("<autonomy_hint mode=\"automatico\">"));
        assert!(p.contains("cap di 3 cicli"));
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! verifier. Lo script `scripts/gen_golden_verifier.py` importa le funzioni
    //! reali da `brain/agents/verifier_node.py` (`_suggest_remediation`,
    //! `_render_failed_block`) + replica byte-fedele la parte deterministica della
    //! decision machine (conteggio evaluable/inconclusive, all_passed, branch
    //! pass/cap/retry, autonomy_hint) e salva `{case_id, function, input, output}`
    //! in `/tmp/golden_verifier.json`. Qui ricostruiamo l'input, chiamiamo la
    //! funzione Rust corrispondente e verifichiamo `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_verifier.py
    //!   cargo test -p nexus-agent-graph --lib golden_verifier_parita -- --ignored

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::{is_inconclusive, suggest_remediation, VerifierNode};
    use crate::runtime::ports::CriterionResult;
    use crate::state::AgentState;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Risultati criteri dall'input golden (lista {type, passed, evidence}).
    fn results_from(arr: Option<&Value>) -> Vec<CriterionResult> {
        arr.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|r| CriterionResult {
                        criterion_type: r
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        passed: r.get("passed").and_then(Value::as_bool).unwrap_or(false),
                        evidence: r.get("evidence").cloned().unwrap_or(json!({})),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replica in Rust la DECISION MACHINE deterministica (vedi `_decision_machine`
    /// dello script golden): conteggio evaluable/inconclusive, all_passed, branch.
    /// Ritorna il delta nella forma confrontabile col Python (con la chiave
    /// `branch` discriminante; per il branch passed restituiamo SOLO i campi
    /// deterministici azzerati, l'avanzamento e' coperto dai test del nodo).
    fn decision_delta(input: &Value, max_cycles: i64) -> Value {
        let results = results_from(input.get("results"));
        let verify_cycle = input.get("verify_cycle").and_then(Value::as_i64).unwrap_or(0);
        let behavior = input
            .get("behavior_mode")
            .and_then(Value::as_str)
            .unwrap_or("");
        let todo_content = input
            .get("todo_content")
            .and_then(Value::as_str)
            .unwrap_or("");

        let evaluable: Vec<&CriterionResult> =
            results.iter().filter(|r| !is_inconclusive(r)).collect();
        let all_passed = if !evaluable.is_empty() {
            evaluable.iter().all(|r| r.passed)
        } else {
            input
                .get("_all_passed_when_inconclusive")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        };
        let cycle = verify_cycle + 1;
        let results_value = Value::Array(
            results
                .iter()
                .map(|r| {
                    json!({"type": r.criterion_type, "passed": r.passed, "evidence": r.evidence})
                })
                .collect(),
        );

        if all_passed {
            return json!({"branch": "passed", "verify_cycle": 0, "exploratory_verify_cycle": 0});
        }
        if cycle >= max_cycles {
            return json!({
                "branch": "cap",
                "verify_cycle": 0,
                "verifier_last_result": {"passed": false, "cycle": cycle, "results": results_value},
            });
        }
        // RETRY: usa lo stesso builder del nodo (render + autonomy_hint).
        let st = AgentState {
            behavior_mode: Some(behavior.to_string()),
            ..Default::default()
        };
        let block = match VerifierNode::autonomy_prefix(&st, max_cycles) {
            Some(prefix) => {
                format!(
                    "{prefix}{}",
                    VerifierNode::render_failed_block(todo_content, cycle, max_cycles, &results)
                )
            }
            None => VerifierNode::render_failed_block(todo_content, cycle, max_cycles, &results),
        };
        json!({
            "branch": "retry",
            "messages": [block],
            "verify_cycle": cycle,
            "verifier_last_result": {"passed": false, "cycle": cycle, "results": results_value},
            "stop_reason": "tool_use",
            "pending_tool_uses": [],
        })
    }

    #[test]
    #[ignore = "richiede /tmp/golden_verifier.json generato da gen_golden_verifier.py"]
    fn golden_verifier_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_verifier.json", "gen_golden_verifier.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 25, "attesi >=25 casi, trovati {}", cases.len());

        const MAX_VERIFY_CYCLES: i64 = 3;
        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "suggest_remediation" => {
                    let failed = results_from(c.input.get("failed"));
                    json!(suggest_remediation(&failed))
                }
                "render_failed_block" => {
                    let todo_content = c
                        .input
                        .get("todo_content")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let cycle = c.input.get("cycle").and_then(Value::as_i64).unwrap_or(1);
                    let max_cycles =
                        c.input.get("max_cycles").and_then(Value::as_i64).unwrap_or(3);
                    let results = results_from(c.input.get("results"));
                    json!(VerifierNode::render_failed_block(
                        todo_content,
                        cycle,
                        max_cycles,
                        &results
                    ))
                }
                "decision_machine" => decision_delta(&c.input, MAX_VERIFY_CYCLES),
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
        println!("golden verifier: {checked} casi verificati, tutti verdi");
    }
}
