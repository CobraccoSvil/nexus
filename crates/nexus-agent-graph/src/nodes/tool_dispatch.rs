//! `ToolDispatchNode` — porta il `tool_dispatch_node` Python
//! (`brain/agents/nodes/__init__.py:3525-4221`).
//!
//! E' la META' del loop agentico che ESEGUE i tool_use pendenti prodotti
//! dall'executor (l'executor li produce, questo nodo li esegue e produce i
//! `tool_result`). Piu' semplice dell'executor: nessuna scelta modello/prompt,
//! nessuna chiamata LLM. L'edge post-nodo e' FISSO verso l'executor (il loop e'
//! guidato dal runtime): questo nodo NON instrada, NON ha una `route_after_*`
//! propria (l'edge `tool_dispatch -> executor` e' modellato dal grafo, vedi
//! `routing::mod` dove NON esiste — ne' va aggiunta — una decisione post-dispatch).
//!
//! Nome del modulo `nodes::tool_dispatch` distinto da `decisions::tool_dispatch`
//! (che contiene gli HELPER PURI): questo e' il NODO, quello sono i calcoli puri
//! che il nodo RIUSA (regola L, niente re-implementazione).
//!
//! ## Riuso (regola L, nessuna logica ri-implementata qui)
//!
//! - `decisions::predictive_cap`: [`predictive_cap_check`] + [`is_cap_exempt`] +
//!   [`PREDICTIVE_CAP_SENTINEL`] (guard "blocked-da-cap" testuale).
//! - `decisions::m16`: [`build_m16_allowed`]/[`is_tool_allowed`]/
//!   [`parse_discovered_tools`]/[`merge_discovered_run`] (il parser usa gia' il
//!   fix ensure_ascii, PR-G).
//! - `decisions::tool_dispatch`: [`apply_run_notes`]/[`normalize_declared_outcome`]/
//!   [`estimate_tool_result_size_bytes`]/[`extract_returned_bytes`]/
//!   [`estimate_context_chars`]/[`current_context_token_estimate`]/
//!   [`append_reminder_block`] + le costanti di cap.
//! - Trait `runtime::ports`: [`ToolExecutor`] (esecuzione, `ExecMode` Real|Replay),
//!   [`AgentStepStore`] (persist step), [`RunControlStore`] (superseded +
//!   heartbeat), [`TodoStore`] (reminder), [`ContextOffload`] (offload RAG). Sono
//!   CAMPI del nodo (coerente con `FinalGateNode`/`TodoRunnerNode`).
//!
//! ## Ordine dei gate (1:1 col Python, load-bearing)
//!
//! 1. `ctx.cancel` / `RunControlStore::is_superseded` -> early return
//!    `stop_reason=superseded` (uscita cooperativa, mig 0370).
//! 2. `pending_tool_uses` vuoto -> `{pending_tool_uses:[], stop_reason:end_turn}`.
//! 3. per ogni pending, NELL'ORDINE: predictive_cap_check (priorita') ->
//!    SYNTHETIC-blocked col SENTINEL (NON eseguito); M16 `is_tool_allowed` ->
//!    SYNTHETIC error (forza nexus_mcp_tool_search); budget allegati ->
//!    SYNTHETIC error; altrimenti KEPT.
//! 4. esecuzione: `join_all` dei KEPT via `ToolExecutor::execute(mode)`. Il nodo
//!    PRESERVA l'ordine ORIGINALE dei pending nella ricomposizione (allineamento
//!    per POSIZIONE, non per id): load-bearing.
//! 5. exit_code: fluisce da `ToolOutcome::exit_code` al `ContentBlock::ToolResult`
//!    INVARIATO (segnale anti-stallo).
//! 6. aggiorna `attachment_read_bytes` dai tool_result attachment.
//! 7. guard "blocked-da-cap": se task_complete outcome=blocked + un tool_result
//!    col SENTINEL -> rifiuta la dichiarazione UNA volta.
//! 8. persist step via `AgentStepStore::persist_step` (gata Real).
//! 9. context-budget cap: se `ctx_chars + new_chars > max_context_chars`, tronca
//!    ogni tool_result a `budget_per_tool` con offload best-effort
//!    (`ContextOffload`), degrado a troncamento testa+coda.
//! 10. reminder TODO via `TodoStore::build_reminder_text` + `append_reminder_block`
//!     (se `plan_phase_active` e soglia raggiunta).
//! 11. `_dispatch_updates`: `discovered_tools_next_turn` SEMPRE scritto, ANCHE `[]`
//!     (distinzione `None`=no-op vs `Some(vec![])`=azzera, load-bearing) +
//!     `merge_discovered_run` per `discovered_tools_run`. Heartbeat best-effort.
//!
//! ## SHADOW (`ExecMode::Replay`)
//!
//! In shadow tutto l'I/O e' no-op/replay: `ToolExecutor` rilegge il primario,
//! `AgentStepStore`/`heartbeat` no-op, `ContextOffload::offload_to_rag` e' gata
//! `Real` (in Replay ritorna `PortError`, no-op) -> il troncamento degrada
//! inline testa+coda senza scrivere su Qdrant. Zero side-effect.
//!
//! ## TURN_FOCUS
//!
//! Il `tool_dispatch_node` NON tocca `turn_focus` (lo inietta l'executor nel
//! prompt): nessuna replica qui.
//!
//! ## Cosa NON porta (parita' Python documentata)
//!
//! - `_tool_runner is None` / `session_id` assente (error_blocks): in Rust la
//!   porta `ToolExecutor` e il `ctx.session_id` sono SEMPRE presenti (costruttore
//!   del nodo / ctx); quei due rami non sono raggiungibili.
//! - `tr_max_chars` da capability del modello: e' DB-driven (regola G), risolto
//!   a MONTE nella [`ToolDispatchConfig`] (come `final_gate`/`todo_runner`).
//! - meta_steps live (`meta_steps.make`/`persist_async`): il nodo POPOLA i
//!   `MetaStep` `kind="tool_executed"` (uno per pending, vedi
//!   [`tool_executed_meta_step`]) nel `delta.meta_steps` (canale `add`), 1:1 col
//!   Python `__init__.py:4065-4114`/`_dispatch_updates["meta_steps"]`. L'emissione
//!   SSE live + la persistenza meta-step sono I/O dell'integrazione (gia' coperti
//!   da `EventSink`/`MetaStepStore`): il nodo resta deterministico (`created_at`
//!   `None`, il gating per-kind via flag DB e' concern dell'integrazione).

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::join_all;
use serde_json::{json, Map, Value};

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::m16::DiscoveredTool;
use crate::decisions::predictive_cap::{is_cap_exempt, predictive_cap_check, PREDICTIVE_CAP_SENTINEL};
use crate::decisions::tool_dispatch::{
    append_reminder_block, apply_run_notes, current_context_token_estimate, estimate_context_chars,
    estimate_tool_result_size_bytes, extract_returned_bytes, normalize_declared_outcome,
    ContextMessage,
};
use crate::decisions::{build_m16_allowed, is_tool_allowed, merge_discovered_run, M16_META_TOOLS};
use crate::py_json::{py_json_dumps, SortKeys};
use crate::runtime::ports::{
    AgentStepStore, ContextOffload, ExecMode, MetaStepStore, OffloadKind, RunControlStore, SseEvent,
    ToolCall,
    TodoStore,
    ToolExecutor,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, MessageContent, MetaStep, StateDelta, StopReason};

/// Tool brain-only `task_complete` (`TASK_COMPLETE_TOOL_NAME`, helpers.py:413):
/// non eseguito via ToolExecutor, registra l'esito dichiarato.
const TASK_COMPLETE_TOOL_NAME: &str = "task_complete";

/// Tool brain-only `nexus_run_notes` (`RUN_NOTES_TOOL_NAME`, helpers.py:450):
/// aggiorna il taccuino del run nello stato, non eseguito via ToolExecutor.
const RUN_NOTES_TOOL_NAME: &str = "nexus_run_notes";

/// Tool di lettura allegato soggetti al budget cumulativo della sessione
/// (`_ATTACHMENT_READ_TOOLS`, helpers.py:3647).
const ATTACHMENT_READ_TOOLS: &[&str] = &["nexus_read_attachment", "nexus_read_archive_entry"];

/// Tool brain-only sempre ammessi da M16 oltre la whitelist DB (Python:
/// `{TASK_COMPLETE_TOOL_NAME, RUN_NOTES_TOOL_NAME}`).
const M16_BRAIN_TOOLS: &[&str] = &[TASK_COMPLETE_TOOL_NAME, RUN_NOTES_TOOL_NAME];

/// Config DB-driven del nodo tool_dispatch, PASSATA (regola G: nessuna lettura
/// DB nel nodo, nessun fallback hardcoded nella logica decisionale).
///
/// Risolve a MONTE (come `FinalGateConfig`/`TodoRunnerConfig`) tutto cio' che il
/// Python legge dal DB/capability inline nel nodo:
///   - `predictive_cap_ratio`  -> `agent.predictive_cap_ratio` (FIX D, ADR 0014);
///   - `context_window`        -> `context_window` del modello del turno (catalogo);
///   - `tool_result_max_chars` -> capability del modello del turno (vista 0318),
///     fallback `MAX_TOOL_RESULT_CHARS` lato chiamante;
///   - `attachment_budget_bytes` -> `agent.attachment.session_read_budget_bytes`;
///   - `discovery_first_enabled` -> `agent.tools.discovery_first_enabled`;
///   - `discovery_first_whitelist` -> `agent.tools.discovery_first_whitelist`;
///   - `always_on_tools` -> always-on del profilo (FONTE UNICA, regola L);
///   - `discovery_schema_max_bytes` -> `agent.tools.discovery_schema_max_bytes`;
///   - `todo_reminder_every_n_steps` -> `todo_reminder_every_n_steps`;
///   - `max_context_chars` -> `MAX_CONTEXT_CHARS` (cap aggressivo del contesto).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDispatchConfig {
    /// Frazione del context window oltre cui il predictive cap blocca una
    /// chiamata (`predictive_cap_ratio`). Il cap NON si applica se `window <= 0`.
    pub predictive_cap_ratio: f64,
    /// Context window (token) del modello del turno, risolto dal catalogo
    /// (regola G). `0` = nessun cap predittivo applicabile (window ignoto).
    pub context_window: i64,
    /// Cap del singolo tool_result in char (`tool_result_max_chars` capability,
    /// fallback `MAX_TOOL_RESULT_CHARS`). Risolto a monte.
    pub tool_result_max_chars: usize,
    /// Budget cumulativo letture allegati per sessione in byte
    /// (`agent.attachment.session_read_budget_bytes`, default 500_000).
    pub attachment_budget_bytes: i64,
    /// Gate M16 discovery-first attivo (`agent.tools.discovery_first_enabled`).
    pub discovery_first_enabled: bool,
    /// Whitelist dei tool sempre ammessi da M16 al primo turno
    /// (`agent.tools.discovery_first_whitelist`). Insieme risolto a monte.
    pub discovery_first_whitelist: Vec<String>,
    /// Always-on del profilo (FONTE UNICA, regola L): tool core non rifiutabili.
    pub always_on_tools: Vec<String>,
    /// Cap dimensione schema di un tool scoperto (`discovery_schema_max_bytes`).
    pub discovery_schema_max_bytes: usize,
    /// Soglia di tool-use per il reminder TODO (`todo_reminder_every_n_steps`).
    pub todo_reminder_every_n_steps: i64,
    /// Budget totale del contesto in char (`MAX_CONTEXT_CHARS`): oltre, i
    /// tool_result vengono compressi (offload + troncamento testa+coda).
    pub max_context_chars: usize,
}

impl Default for ToolDispatchConfig {
    fn default() -> Self {
        // Default IDENTICI ai safe-default del brain (valgono SOLO se il DB e'
        // irraggiungibile, mai come magic fallback nella logica).
        Self {
            predictive_cap_ratio: 0.8,
            context_window: 0,
            tool_result_max_chars: crate::decisions::tool_dispatch::MAX_TOOL_RESULT_CHARS,
            attachment_budget_bytes: 500_000,
            discovery_first_enabled: false,
            discovery_first_whitelist: vec![
                "nexus_mcp_tool_search".to_string(),
                "nexus_mcp_tool_call".to_string(),
            ],
            always_on_tools: Vec::new(),
            discovery_schema_max_bytes: 8192,
            todo_reminder_every_n_steps: 5,
            max_context_chars: crate::decisions::tool_dispatch::MAX_CONTEXT_CHARS,
        }
    }
}

/// Esito di UNA tool_result, prima della ricomposizione nell'ordine dei pending.
/// Mappa il dict `{type, tool_use_id, content, is_error, exit_code?, raw_content?}`
/// del Python. `content` e' un `Value` (la stringa JSON del tool, gia' eventualmente
/// troncata): nel `ContentBlock::ToolResult` `content` e' opaco.
#[derive(Debug, Clone)]
struct ToolResultBlock {
    /// Id della ToolUse a cui risponde (round-trip).
    tool_use_id: String,
    /// Contenuto del risultato (Value: stringa JSON o struttura).
    content: Value,
    /// `true` se il tool ha fallito (errore applicativo o synthetic-block).
    is_error: bool,
    /// Exit code STRUTTURATO del tool-comando (fluisce invariato nel ToolResult).
    exit_code: Option<i64>,
    /// Per `nexus_mcp_tool_search`: il raw_content INTEGRO (pre-truncation) per
    /// il parser M16. Rimosso prima di costruire il blocco finale (non arriva al
    /// modello). `None` per ogni altro tool.
    raw_content: Option<String>,
}

/// Nodo tool_dispatch. Le porte I/O (`ToolExecutor`, `AgentStepStore`,
/// `RunControlStore`, `TodoStore`, `ContextOffload`) sono CAMPI del nodo (come
/// `CriteriaRunner` in `FinalGateNode`), non nel ctx. La config DB-driven e'
/// risolta A MONTE (regola G); la macchina dei gate e' interamente qui.
pub struct ToolDispatchNode {
    /// Config DB-driven (regola G: passata, mai letta dal nodo).
    cfg: ToolDispatchConfig,
    /// Esecutore dei tool (Real -> ToolRunner gRPC, Replay -> shadow read-only).
    tools: Arc<dyn ToolExecutor>,
    /// Persistenza incrementale degli step (gata Real, no-op Replay).
    steps: Arc<dyn AgentStepStore>,
    /// Controllo run condiviso (superseded + heartbeat). PUNTO UNICO (regola L)
    /// con l'executor.
    run_control: Arc<dyn RunControlStore>,
    /// Store dei todo per il reminder TODO (sola lettura `build_reminder_text`).
    todos: Arc<dyn TodoStore>,
    /// Offload del contesto verso RAG (best-effort, degrado a troncamento).
    offload: Arc<dyn ContextOffload>,
    /// Persistenza dei meta-step semantici `tool_executed` (narrazione live,
    /// gata Real). Stesso pattern emit+persist dell'`executor_call` (regola L).
    meta_steps: Arc<dyn MetaStepStore>,
}

impl ToolDispatchNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta e le porte I/O
    /// concrete (o stub nei test).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: ToolDispatchConfig,
        tools: Arc<dyn ToolExecutor>,
        steps: Arc<dyn AgentStepStore>,
        run_control: Arc<dyn RunControlStore>,
        todos: Arc<dyn TodoStore>,
        offload: Arc<dyn ContextOffload>,
        meta_steps: Arc<dyn MetaStepStore>,
    ) -> Self {
        Self {
            cfg,
            tools,
            steps,
            run_control,
            todos,
            offload,
            meta_steps,
        }
    }

    /// Costruisce l'insieme dei tool ammessi da M16 (PUNTO UNICO
    /// [`build_m16_allowed`], regola L): meta-tool + whitelist DB + always-on del
    /// profilo + brain-only (`task_complete`/`nexus_run_notes`).
    fn m16_allowed(&self) -> std::collections::HashSet<String> {
        build_m16_allowed(
            M16_META_TOOLS,
            &self.cfg.discovery_first_whitelist,
            &self.cfg.always_on_tools,
            M16_BRAIN_TOOLS,
        )
    }

    /// Insieme dei nomi dei tool scoperti nel turno precedente
    /// (`discovered_tools_next_turn`), per il gate M16 (`_disc_now` Python).
    fn discovered_now(state: &AgentState) -> std::collections::HashSet<String> {
        state
            .discovered_tools_next_turn
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str).map(String::from))
            .collect()
    }

    /// Stima del contesto corrente (token) per il predictive cap. PURO: delega a
    /// [`current_context_token_estimate`] sui messaggi dello stato + il system.
    /// I messaggi sono mappati nella forma [`ContextMessage`] (content +
    /// anthropic_content) leggendo i `Message` dello stato.
    fn predictive_tokens(&self, state: &AgentState) -> i64 {
        let msgs: Vec<ContextMessage> = state.messages.iter().map(message_to_ctx).collect();
        let system = state.system_text.as_deref().unwrap_or("");
        current_context_token_estimate(&msgs, system)
    }

    /// Costruisce un tool_result SYNTHETIC d'errore (non eseguito): il `content`
    /// e' una stringa JSON (per gli errori M16/budget) o gia' il messaggio col
    /// SENTINEL (predictive cap). Replica i dict `{type:tool_result,...}` Python.
    fn synthetic_error(tool_use_id: &str, content: Value) -> ToolResultBlock {
        ToolResultBlock {
            tool_use_id: tool_use_id.to_string(),
            content,
            is_error: true,
            exit_code: None,
            raw_content: None,
        }
    }

    /// Esegue UN tool (parita' con la closure `_run` Python, righe 3786-3896).
    /// `task_complete`/`nexus_run_notes` sono brain-only (non via ToolExecutor):
    /// ritornano un ack e raccolgono outcome/notes nel `RunCollector`. Gli altri
    /// vanno via `ToolExecutor::execute(mode)`. Il `try/except` Python e'
    /// ONNICOMPRENSIVO (qualunque errore -> tool_result d'errore, niente
    /// propagazione): qui un `Err(PortError)` (anche infra) diventa un
    /// ToolResult `is_error=true` (NON un `NodeError`), 1:1 col Python.
    async fn run_one(
        &self,
        block: &Value,
        mode: ExecMode,
        collector: &RunCollector,
    ) -> ToolResultBlock {
        let tool_use_id = block
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
        let input = block.get("input").cloned().unwrap_or(json!({}));

        // ── nexus_run_notes (brain-only) ──────────────────────────────────────
        if name == RUN_NOTES_TOOL_NAME {
            // apply_run_notes sul valore corrente del taccuino (holder mutabile).
            let new_notes = {
                let cur = collector.run_notes.lock().expect("lock run_notes");
                apply_run_notes(cur.as_deref(), &input)
            };
            let acknowledged = new_notes.is_some();
            let notes_chars = new_notes.as_deref().map(str::chars).map(Iterator::count).unwrap_or(0);
            if let Some(n) = new_notes {
                *collector.run_notes.lock().expect("lock run_notes") = Some(n);
            }
            return ToolResultBlock {
                tool_use_id,
                content: Value::String(py_dumps(
                    &json!({"acknowledged": acknowledged, "notes_chars": notes_chars}),
                )),
                is_error: !acknowledged,
                exit_code: None,
                raw_content: None,
            };
        }

        // ── task_complete (brain-only) ────────────────────────────────────────
        if name == TASK_COMPLETE_TOOL_NAME {
            collector
                .task_complete_ids
                .lock()
                .expect("lock tc ids")
                .push(tool_use_id.clone());
            let decl = normalize_declared_outcome(&input);
            let outcome = decl
                .as_ref()
                .and_then(|d| d.get("outcome").cloned())
                .unwrap_or(Value::Null);
            let acknowledged = decl.is_some();
            if let Some(d) = decl {
                collector.declared_outcomes.lock().expect("lock outcomes").push(d);
            }
            return ToolResultBlock {
                tool_use_id,
                content: Value::String(py_dumps(
                    &json!({"acknowledged": acknowledged, "outcome": outcome}),
                )),
                is_error: !acknowledged,
                exit_code: None,
                raw_content: None,
            };
        }

        // ── tool generico via ToolExecutor ────────────────────────────────────
        let call = ToolCall {
            id: tool_use_id.clone(),
            name: name.to_string(),
            input,
            thought_signature: None,
        };
        match self.tools.execute(call, mode).await {
            Ok(outcome) => {
                // WAVE 2.2: errore infrastrutturale (ToolRunner gRPC down)
                // segnalato strutturato (mcp-core NON scala i provider).
                if outcome.is_infrastructure {
                    *collector.infra_error.lock().expect("lock infra") = true;
                }
                // Il content del tool e' gia' JSON (stringa o struttura). Il
                // troncamento `tool_result_max_chars` + offload e' applicato A
                // VALLE su content stringa (parita' con _smart_truncate_lossless
                // chiamato in _run): qui conserviamo il content grezzo, lo
                // tronca/offloada il chiamante (run) cosi' l'offload e' un solo
                // punto e il join_all resta puramente CPU.
                let raw_content = if name == "nexus_mcp_tool_search" {
                    Some(value_as_json_string(&outcome.content))
                } else {
                    None
                };
                ToolResultBlock {
                    tool_use_id,
                    content: outcome.content,
                    is_error: outcome.is_error,
                    exit_code: outcome.exit_code,
                    raw_content,
                }
            }
            // try/except onnicomprensivo: QUALSIASI errore (infra incluso) ->
            // tool_result d'errore, niente NodeError (il run non fallisce).
            // Python: json.dumps({"error": str(exc)}) (separatori con spazio).
            Err(exc) => Self::synthetic_error(
                &tool_use_id,
                Value::String(py_dumps(&json!({"error": exc.to_string()}))),
            ),
        }
    }
}

/// Raccoglitore degli effetti dei tool brain-only durante il `join_all` (replica
/// gli holder-list della closure `_run` Python, che raccoglie senza scrivere lo
/// stato). `Mutex` perche' le `run_one` girano concorrenti.
#[derive(Default)]
struct RunCollector {
    /// Esiti dichiarati via task_complete (l'ultimo prevale).
    declared_outcomes: std::sync::Mutex<Vec<Value>>,
    /// tool_use_id dei task_complete del turno (guard blocked-da-cap).
    task_complete_ids: std::sync::Mutex<Vec<String>>,
    /// Taccuino del run (holder mutabile, P4). Inizializzato al valore di stato.
    run_notes: std::sync::Mutex<Option<String>>,
    /// `true` se almeno un tool e' fallito per infrastruttura (WAVE 2.2).
    infra_error: std::sync::Mutex<bool>,
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ToolDispatchNode {
    fn id(&self) -> NodeId {
        NodeId::ToolDispatch
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        let pending: Vec<Value> = state.pending_tool_uses.clone().unwrap_or_default();

        // ── (2) pending vuoto -> end_turn (py:3532-3533) ──────────────────────
        // NB: in Python questo gate viene PRIMA di _check_superseded; replicato 1:1.
        if pending.is_empty() {
            return Ok(StateDelta {
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::EndTurn)),
                ..Default::default()
            }
            .into_opaque());
        }

        let run_id = state.thread_id.clone().unwrap_or_default();
        let mode = ctx.exec_mode();

        // ── (1) Uscita cooperativa: cancel del ctx O run superato (py:3545) ───
        // ctx.cancel e' la fonte primaria in Rust (l'orchestratore lo cancella
        // sul supersede); RunControlStore::is_superseded e' il segnale esplicito
        // POLLABILE complementare (fail-open: errore DB -> false, il run prosegue).
        let superseded = ctx.cancel.is_cancelled()
            || self
                .run_control
                .is_superseded(&run_id)
                .await
                .unwrap_or(false);
        if superseded {
            tracing::warn!(
                target: "nexus_agent_graph::tool_dispatch",
                pending = pending.len(),
                "run superato/cancellato, salto i tool pending (uscita cooperativa)"
            );
            return Ok(StateDelta {
                pending_tool_uses: Some(Some(vec![])),
                stop_reason: Some(Some(StopReason::Superseded)),
                ..Default::default()
            }
            .into_opaque());
        }

        // Heartbeat best-effort (anti-recovery prematuro), gata Real.
        let _ = self.run_control.heartbeat(&run_id, mode).await;

        // ── (3) Loop sui pending: cap predittivo / M16 / budget allegati ──────
        let ctx_chars = estimate_context_chars(
            &state.messages.iter().map(message_to_ctx).collect::<Vec<_>>(),
        );
        let current_bytes = state.attachment_read_bytes.unwrap_or(0);
        let budget_total = self.cfg.attachment_budget_bytes;
        let allowed = self.m16_allowed();
        let disc_now = Self::discovered_now(state);
        let predictive_tokens = self.predictive_tokens(state);
        // Finestra per il predictive cap: quella EFFETTIVA dell'ultimo turno LLM
        // (scritta dall'executor: config o modello PROMOSSO dallo smart-upscale),
        // fallback alla finestra statica di config quando assente (primo turno /
        // checkpoint storici). Regola H (incidente 2026-07-06): col gate fermo
        // alla finestra del modello di partenza, dopo l'upscale per
        // context_overflow OGNI tool veniva bloccato per sempre dal cap mentre
        // le chiamate LLM giravano gia' su un modello con finestra ampia.
        let cap_window = state
            .effective_context_window
            .filter(|w| *w > 0)
            .unwrap_or(self.cfg.context_window);

        // Esito per posizione: None = ancora da eseguire (kept), Some = synthetic.
        let mut slots: Vec<Option<ToolResultBlock>> = Vec::with_capacity(pending.len());
        // Indici dei pending KEPT (da eseguire), nell'ordine originale.
        let mut kept_indices: Vec<usize> = Vec::new();

        for (i, b) in pending.iter().enumerate() {
            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
            let tool_use_id = b.get("id").and_then(Value::as_str).unwrap_or("");
            let input = b.get("input").cloned().unwrap_or(json!({}));

            // (a) predictive cap (priorita'): esente -> salta; altrimenti la
            //     proiezione e' valutata SOLO se window e' nota (>0).
            let cap_msg = if cap_window > 0 && !is_cap_exempt(name) {
                let expected = estimate_tool_result_size_bytes(name, &input);
                predictive_cap_check(
                    self.cfg.predictive_cap_ratio,
                    cap_window,
                    expected,
                    predictive_tokens,
                )
            } else {
                None
            };
            if let Some(msg) = cap_msg {
                // SYNTHETIC-blocked: il content e' il messaggio col SENTINEL in
                // testa (stringa nuda, NON un JSON {error:...}), 1:1 col Python.
                slots.push(Some(Self::synthetic_error(tool_use_id, Value::String(msg))));
                continue;
            }

            // (b) M16: tool non ammesso e non scoperto -> SYNTHETIC error.
            if self.cfg.discovery_first_enabled && !is_tool_allowed(name, &allowed, &disc_now) {
                tracing::info!(
                    target: "nexus_agent_graph::tool_dispatch",
                    tool = name,
                    "M16: tool non scoperto/non in whitelist -> rifiutato"
                );
                let err = json!({
                    "error": format!(
                        "Il tool '{name}' non e' disponibile direttamente in questo turno. \
                         Usa prima nexus_mcp_tool_search (query: \"{name}\") per scoprirlo, \
                         poi richiamalo al turno successivo."
                    )
                });
                slots.push(Some(Self::synthetic_error(tool_use_id, Value::String(py_dumps(&err)))));
                continue;
            }

            // (c) budget allegati: tool di lettura allegato oltre il budget.
            if ATTACHMENT_READ_TOOLS.contains(&name) && current_bytes >= budget_total {
                tracing::warn!(
                    target: "nexus_agent_graph::tool_dispatch",
                    already_read = current_bytes,
                    budget = budget_total,
                    tool = name,
                    "budget letture allegati esaurito, tool bloccato"
                );
                let err = json!({
                    "error": format!(
                        "budget letture allegati esaurito ({current_bytes} byte gia' letti su \
                         {budget_total} budget). Usa un tool di estrazione strutturata \
                         (nexus_extract_pdf_text, nexus_extract_figma_structure, \
                         nexus_extract_docx_text, nexus_extract_xlsx_data) oppure chiedi \
                         all'utente una versione testuale del file."
                    ),
                    "budget_bytes": budget_total,
                    "already_read": current_bytes,
                });
                slots.push(Some(Self::synthetic_error(tool_use_id, Value::String(py_dumps(&err)))));
                continue;
            }

            // KEPT: da eseguire.
            slots.push(None);
            kept_indices.push(i);
        }

        // ── (4) Esecuzione dei KEPT (join_all), ordine preservato per POSIZIONE ─
        let collector = RunCollector {
            run_notes: std::sync::Mutex::new(state.run_notes.clone()),
            ..Default::default()
        };
        let kept_futs = kept_indices.iter().map(|&i| self.run_one(&pending[i], mode, &collector));
        let kept_results: Vec<ToolResultBlock> = join_all(kept_futs).await;
        // Ricompone nell'ordine originale: ogni slot vuoto prende il prossimo kept.
        let mut kept_iter = kept_results.into_iter();
        let mut results: Vec<ToolResultBlock> = Vec::with_capacity(pending.len());
        for slot in slots {
            match slot {
                Some(synth) => results.push(synth),
                None => results.push(
                    kept_iter
                        .next()
                        .expect("ogni slot KEPT ha un risultato corrispondente"),
                ),
            }
        }

        // ── (5) Tronca i singoli tool_result a tool_result_max_chars (offload) ─
        // Solo i KEPT (non synthetic: i synthetic sono brevi). Stringa-content.
        // `mode` propagato: in Replay l'offload e' saltato (degrado a troncamento).
        for (idx, r) in results.iter_mut().enumerate() {
            if kept_indices.contains(&idx) {
                self.truncate_content(&mut r.content, self.cfg.tool_result_max_chars, mode)
                    .await;
            }
        }

        // ── (6) Aggiorna attachment_read_bytes (py:3909-3914) ─────────────────
        let mut added_bytes = 0i64;
        for (b, r) in pending.iter().zip(results.iter()) {
            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
            if ATTACHMENT_READ_TOOLS.contains(&name) && !r.is_error {
                added_bytes += extract_returned_bytes(&value_as_json_string(&r.content));
            }
        }
        let new_attachment_read_bytes = current_bytes + added_bytes;

        // ── (7) Guard blocked-da-cap (py:3924-3953) ───────────────────────────
        let declared_outcomes = collector.declared_outcomes.into_inner().expect("outcomes");
        let task_complete_ids = collector.task_complete_ids.into_inner().expect("tc ids");
        let infra_error = collector.infra_error.into_inner().expect("infra");
        let final_run_notes = collector.run_notes.into_inner().expect("run_notes");

        let mut declared_outcomes = declared_outcomes;
        let mut blocked_cap_rejected_now = false;
        let last_blocked = declared_outcomes
            .last()
            .and_then(|d| d.get("outcome").and_then(Value::as_str))
            == Some("blocked");
        let already_rejected = state.blocked_cap_rejected.unwrap_or(false);
        let any_cap_sentinel = results
            .iter()
            .any(|r| value_as_json_string(&r.content).contains(PREDICTIVE_CAP_SENTINEL));
        if !declared_outcomes.is_empty() && last_blocked && !already_rejected && any_cap_sentinel {
            // La `reason` e' una stringa SINGOLA (spazi singoli, niente
            // indentazione spuria): 1:1 col Python (concatenazione implicita di
            // literal adiacenti). py_dumps -> separatori con spazio come json.dumps.
            let reason = "Dichiarazione 'blocked' RIFIUTATA: l'unico blocco di questo turno e' \
il predictive context cap su una singola chiamata, NON un blocco del task. \
Prosegui col task usando i dati gia' raccolti e rispondi alla richiesta corrente \
dell'utente.";
            for r in results.iter_mut() {
                if task_complete_ids.contains(&r.tool_use_id) {
                    r.content = Value::String(py_dumps(&json!({
                        "acknowledged": false,
                        "reason": reason,
                    })));
                    r.is_error = true;
                }
            }
            declared_outcomes.clear();
            blocked_cap_rejected_now = true;
            tracing::warn!(
                target: "nexus_agent_graph::tool_dispatch",
                "task_complete blocked RIFIUTATO (blocco era del predictive cap, non del task)"
            );
        }

        // ── (8) Persist step incrementale (gata Real, py:3956-4007) ───────────
        let iteration = state.iterations.unwrap_or(0);
        if !run_id.is_empty() {
            for (idx, (b, r)) in pending.iter().zip(results.iter()).enumerate() {
                let t_name = b.get("name").and_then(Value::as_str).unwrap_or("");
                let t_input = b.get("input").cloned().unwrap_or(json!({}));
                let block = json!({"tool_name": t_name, "tool_input": t_input});
                let result = Some(json!({
                    "content": value_as_json_string(&r.content),
                    "status": if r.is_error { "failed" } else { "completed" },
                }));
                // Best-effort (errore DB loggato dall'impl, Ok(()) ritornato):
                // un guasto della persistenza NON deve far fallire il run.
                let _ = self
                    .steps
                    .persist_step(&run_id, iteration, idx as i64, block, result, mode)
                    .await;
            }
        }

        // ── (9) Context-budget cap (py:4009-4032) ─────────────────────────────
        let new_chars: i64 = results
            .iter()
            .map(|r| value_as_json_string(&r.content).chars().count() as i64)
            .sum();
        if ctx_chars + new_chars > self.cfg.max_context_chars as i64 {
            let span = self.cfg.max_context_chars as i64 - ctx_chars;
            let budget_per_tool =
                std::cmp::max(1500i64, span / std::cmp::max(results.len() as i64, 1)) as usize;
            for r in results.iter_mut() {
                self.truncate_content(&mut r.content, budget_per_tool, mode).await;
            }
            tracing::warn!(
                target: "nexus_agent_graph::tool_dispatch",
                ctx_chars,
                new_chars,
                budget_per_tool,
                "contesto vicino al limite, troncamento aggressivo"
            );
        }

        // ── (10) Reminder TODO (anti-amnesia, py:4034-4059) ───────────────────
        let mut new_reminder_counter =
            state.since_last_todo_reminder.unwrap_or(0) + pending.len() as i64;
        let mut reminder_text: Option<String> = None;
        if state.plan_phase_active.unwrap_or(false) {
            let every_n = std::cmp::max(1, self.cfg.todo_reminder_every_n_steps);
            if new_reminder_counter >= every_n && !run_id.is_empty() {
                reminder_text = self.todos.build_reminder_text(&run_id).await.unwrap_or(None);
                if reminder_text.is_some() {
                    // Best-effort: traccia che i todos sono stati "visti".
                    let _ = self.todos.increment_iteration_seen(&run_id, mode).await;
                    new_reminder_counter = 0;
                }
            }
        }

        // ── Costruzione del HumanMessage coi blocchi tool_result ──────────────
        // final_blocks = i tool_result (senza raw_content) + eventuale reminder.
        let mut final_blocks: Vec<Value> = results.iter().map(tool_result_to_block).collect();
        if let Some(txt) = &reminder_text {
            append_reminder_block(&mut final_blocks, txt);
        }
        // Il Python costruisce un HumanMessage con content="" e i blocchi in
        // additional_kwargs["anthropic_content"]. In Rust la forma autoritativa
        // del contenuto a blocchi e' MessageContent::Blocks: deserializziamo i
        // blocchi JSON in ContentBlock (un blocco non riconosciuto, es. il
        // reminder text, cade su ContentBlock::Text).
        let tool_msg = human_message_from_blocks(final_blocks);

        // ── (11) M16: parse dei tool scoperti dal search (py:4116-4196) ───────
        // discovered_tools_next_turn e' SEMPRE scritto, ANCHE [] (azzera i
        // discovered del turno prima, durata esatta 1 turno: load-bearing).
        let mut discovered_next: Vec<DiscoveredTool> = Vec::new();
        for (b, r) in pending.iter().zip(results.iter()) {
            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
            if name != "nexus_mcp_tool_search" || r.is_error {
                continue;
            }
            // JSON INTEGRO (pre-truncation): il raw_content, altrimenti il content.
            let raw = r
                .raw_content
                .clone()
                .unwrap_or_else(|| value_as_json_string(&r.content));
            // parse_discovered_tools usa gia' il fix ensure_ascii (PR-G).
            let parsed = crate::decisions::parse_discovered_tools(
                &raw,
                self.cfg.discovery_schema_max_bytes,
            );
            // Dedup cross-search nel turno: la prima occorrenza vince (come Python
            // `if not any(d.name == ...)`).
            for t in parsed {
                if !discovered_next.iter().any(|d| d.name == t.name) {
                    discovered_next.push(t);
                }
            }
        }

        // ── meta_steps "tool_executed" (live UX, py:4065-4114) ────────────────
        // Un MetaStep per OGNI tool del turno (KEPT, synthetic-blocked,
        // brain-only): l'allineamento per posizione `zip(pending, results)` e'
        // 1:1 col Python. provider/model emittenti del turno (UI badge): catena
        // di fallback identica al Python (provider_used -> sticky -> override).
        let exec_provider = state
            .provider_used
            .as_deref()
            .or(state.sticky_provider.as_deref())
            .or(state.provider_override.as_deref());
        let exec_model = state
            .model_used
            .as_deref()
            .or(state.sticky_model.as_deref())
            .or(state.model_override.as_deref());
        let tool_steps: Vec<MetaStep> = pending
            .iter()
            .zip(results.iter())
            .map(|(b, r)| tool_executed_meta_step(b, r.is_error, exec_provider, exec_model))
            .collect();
        // NARRAZIONE LIVE: ogni tool eseguito diventa una riga della cronaca in
        // chat ("tool edit_file — src/x.ts", "errore run_command — pnpm build").
        // Prima restavano solo nel canale stato (delta) e NON arrivavano mai al
        // frontend ne' al DB: la timeline mostrava solo gli executor_call
        // ("quale modello") — incidente narrazione 2026-07-02. Pattern
        // emit (live, sink no-op in shadow) + persist (storico, gata Real),
        // identico all'executor_call (regola L).
        for ms in &tool_steps {
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                ctx.exec_mode(),
                &ms.kind,
                ms.title.clone(),
                ms.payload.clone(),
            )
            .await;
        }

        // ── SSE tool_result verso il frontend (parita' 1:1 con run_via_brain) ──
        // Un evento per ogni risultato, correlato alla ToolUse via `tool_call_id`,
        // 1:1 col `tool_result` del brain. Emesso DOPO i cap/troncamenti (5/9)
        // cosi' il contenuto inviato all'utente coincide con quello consegnato al
        // modello. Best-effort/infallibile: in shadow il sink iniettato nel ctx e'
        // `NullEventSink` (no-op) -> nessun evento esce in Replay (gate gia'
        // assicurato a monte da `build_native_engine`, qui niente `if shadow`).
        for r in &results {
            ctx.emit.emit(SseEvent::ToolResult {
                tool_call_id: r.tool_use_id.clone(),
                content: r.content.clone(),
                is_error: r.is_error,
            });
        }

        // ── _dispatch_updates ─────────────────────────────────────────────────
        let mut delta = StateDelta {
            messages: Some(vec![tool_msg]),
            meta_steps: Some(tool_steps),
            pending_tool_uses: Some(Some(vec![])),
            stop_reason: Some(Some(StopReason::ToolUse)),
            since_last_todo_reminder: Some(Some(new_reminder_counter)),
            attachment_read_bytes: Some(Some(new_attachment_read_bytes)),
            // SEMPRE scritto (anche []): durata esatta 1 turno (overwrite reducer).
            discovered_tools_next_turn: Some(Some(
                discovered_next.iter().map(discovered_to_value).collect(),
            )),
            ..Default::default()
        };

        // P3: accumulo persistente per il run (merge dedup, ultimo schema vince).
        if !discovered_next.is_empty() {
            let previous: Vec<DiscoveredTool> = state
                .discovered_tools_run
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter_map(|v| serde_json::from_value::<DiscoveredTool>(v.clone()).ok())
                .collect();
            let merged = merge_discovered_run(&previous, &discovered_next);
            delta.discovered_tools_run =
                Some(Some(merged.iter().map(discovered_to_value).collect()));
        }

        // WAVE 3: esito dichiarato (l'ultimo prevale) + conteggio cumulativo done.
        if let Some(last) = declared_outcomes.last() {
            delta.declared_outcome = Some(Some(last.clone()));
            let done_now = declared_outcomes
                .iter()
                .filter(|d| d.get("outcome").and_then(Value::as_str) == Some("done"))
                .count() as i64;
            if done_now > 0 {
                let prev = state.declared_done_count.unwrap_or(0);
                delta.declared_done_count = Some(Some(prev + done_now));
            }
        } else if state.declared_outcome.is_some() {
            // INVALIDAZIONE dichiarazione STANTIA (ADR 0034): il run PROSEGUE
            // con altri tool DOPO una dichiarazione precedente -> quella
            // dichiarazione era intermedia, non l'esito finale. Senza questo
            // azzeramento, un "partial"/"blocked" dichiarato a meta' run
            // falsava lo status canonico FINALE anche a lavoro poi completato
            // (il finalizzatore legge l'ULTIMA dichiarazione dallo stato).
            // `declared_done_count` resta cumulativo (gate done>=3 in testa).
            delta.declared_outcome = Some(None);
        }

        // WAVE 2.2: errore infrastruttura tool (mcp-core NON scala i provider).
        if infra_error {
            delta.tool_infra_error = Some(Some(true));
        }
        // Guard blocked-da-cap: marca il flag (la 2a dichiarazione sara' onorata).
        if blocked_cap_rejected_now {
            delta.blocked_cap_rejected = Some(Some(true));
        }
        // P4: persiste il taccuino aggiornato SOLO se cambiato (py:4219).
        if final_run_notes != state.run_notes {
            delta.run_notes = Some(final_run_notes);
        }

        Ok(delta.into_opaque())
    }
}

impl ToolDispatchNode {
    /// Tronca `content` (se stringa) a `max_chars` con offload best-effort in RAG
    /// e degrado a troncamento testa+coda (`_smart_truncate_lossless`,
    /// `__init__.py:153-181`). In [`ExecMode::Replay`] l'offload e' un no-op (la
    /// porta gata `Real` ritorna `PortError`): si degrada DIRETTAMENTE al
    /// troncamento testa+coda non-RAG (zero scritture Qdrant nel run shadow).
    ///
    /// PURO se il content non e' stringa o e' sotto soglia (no-op). Sopra soglia:
    /// head = `max_chars/5`, tail = `max(200, max_chars - head - 200)`, pointer
    /// in mezzo (col pointer RAG se l'offload riesce, un marker di troncamento
    /// altrimenti).
    async fn truncate_content(&self, content: &mut Value, max_chars: usize, mode: ExecMode) {
        let Value::String(text) = content else {
            return; // non-stringa: il tool_result reale e' sempre JSON-stringa.
        };
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= max_chars {
            return;
        }
        let head_size = max_chars / 5;
        let tail_size = std::cmp::max(200, max_chars.saturating_sub(head_size).saturating_sub(200));
        let total = chars.len();
        // Offload best-effort: pointer RAG se la porta riesce (solo in Real),
        // altrimenti marker di troncamento (Replay o guasto Qdrant).
        let pointer = match self.try_offload(text, mode).await {
            Some(ptr) => format!(
                "\n\n[...troncato: {total} char totali offloadati in RAG, recupera con \
                 nexus_search_semantic (pointer={ptr})...]\n\n"
            ),
            None => format!(
                "\n\n[...troncato: {total} char totali, coda preservata sotto...]\n\n"
            ),
        };
        let head: String = chars.iter().take(head_size).collect();
        let tail: String = chars.iter().skip(total.saturating_sub(tail_size)).collect();
        *content = Value::String(format!("{head}{pointer}{tail}"));
    }

    /// Offload best-effort verso RAG. `None` su errore della porta (degrado a
    /// troncamento). Il gate shadow vive nella porta (regola L): in
    /// [`ExecMode::Replay`] [`ContextOffload::offload_to_rag`] e' un no-op che
    /// ritorna `PortError` -> qui `None` -> il chiamante tronca senza scrivere
    /// Qdrant. Passiamo `mode` end-to-end senza re-implementare il gate qui.
    async fn try_offload(&self, text: &str, mode: ExecMode) -> Option<String> {
        // Tool_result grande al dispatch: collection ToolResult, senza filtro
        // session/project (comportamento storico, cache del contesto offloadato).
        self.offload
            .offload_to_rag(json!({"text": text}), OffloadKind::ToolResult, None, None, mode)
            .await
            .ok()
    }
}

/// Converte un [`ToolResultBlock`] nel `ContentBlock::tool_result` JSON (forma
/// anthropic_content), SENZA `raw_content` (rimosso: non arriva al modello).
/// `exit_code` e' incluso solo se presente (tool-comando), 1:1 col Python che
/// aggiunge la chiave solo `if result.exit_code is not None`.
fn tool_result_to_block(r: &ToolResultBlock) -> Value {
    let mut m = Map::new();
    m.insert("type".to_string(), json!("tool_result"));
    m.insert("tool_use_id".to_string(), json!(r.tool_use_id));
    m.insert("content".to_string(), r.content.clone());
    m.insert("is_error".to_string(), json!(r.is_error));
    if let Some(code) = r.exit_code {
        m.insert("exit_code".to_string(), json!(code));
    }
    Value::Object(m)
}

/// Serializza un `DiscoveredTool` come Value `{name, description, input_schema}`
/// per lo stato (round-trip).
fn discovered_to_value(d: &DiscoveredTool) -> Value {
    serde_json::to_value(d).unwrap_or(Value::Null)
}

/// Campi candidati per il `target` del meta_step `tool_executed`: il PRIMO
/// presente (stringa non vuota) nell'input del tool vince (1:1 col Python
/// `__init__.py:4092`). Ordine load-bearing.
const META_TARGET_KEYS: &[&str] = &[
    "path",
    "file_path",
    "abs_path",
    "command",
    "query",
    "pattern",
    "name",
    "tool_name",
];

/// PUNTO UNICO (regola L) del "target leggibile" di un tool per la narrazione:
/// primo campo presente (stringa non vuota) tra [`META_TARGET_KEYS`] negli
/// argomenti del tool, troncato a 80 char (77 + "..."). `None` se nessun campo
/// candidato e' presente. Usato da `tool_executed_meta_step` (narrazione tool
/// del run corrente) e dal ponte narrazione sub-agente in mcp-core.
pub fn tool_target_from_input(input: &Value) -> Option<String> {
    let obj = input.as_object()?;
    META_TARGET_KEYS.iter().find_map(|k| {
        obj.get(*k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(|v| {
                if v.chars().count() <= 80 {
                    v.to_string()
                } else {
                    let head: String = v.chars().take(77).collect();
                    format!("{head}...")
                }
            })
    })
}

/// Costruisce il `MetaStep` `kind="tool_executed"` per UN tool del turno (live UX
/// progressiva, PORTA il `meta_steps.make(kind="tool_executed", ...)` Python,
/// `__init__.py:4087-4114`). PURO (nessun side-effect): il nodo lo accumula nel
/// `delta.meta_steps` (canale `add`); l'emissione SSE live + la persistenza DB
/// sono I/O dell'integrazione (`EventSink`/`MetaStepStore`).
///
/// `created_at` resta `None` (come `planner_node`): il timestamp e' assegnato a
/// valle dall'integrazione, restando deterministico per i golden-test.
fn tool_executed_meta_step(
    block: &Value,
    is_error: bool,
    provider: Option<&str>,
    model: Option<&str>,
) -> MetaStep {
    let tool = block.get("name").and_then(Value::as_str).unwrap_or("?");
    let tool_use_id = block.get("id").and_then(Value::as_str);
    // target: punto unico `tool_target_from_input` (primo campo presente tra
    // META_TARGET_KEYS, troncato a 80 char), 1:1 col Python.
    let target = block
        .get("input")
        .and_then(tool_target_from_input)
        .unwrap_or_default();
    let title = {
        let kind_word = if is_error { "errore" } else { "tool" };
        if target.is_empty() {
            format!("{kind_word} {tool}")
        } else {
            format!("{kind_word} {tool} — {target}")
        }
    };
    MetaStep {
        kind: "tool_executed".to_string(),
        title,
        payload: json!({
            "tool": tool,
            "target": target,
            "is_error": is_error,
            "tool_use_id": tool_use_id,
            // Provider/model emittenti del tool_use nel turno (UI badge). null se ignoti.
            "provider": provider,
            "model": model,
        }),
        correlation_id: None,
        created_at: None,
    }
}

/// Mappa un [`Message`] nella forma [`ContextMessage`] usata dalle stime di
/// contesto: `content` (testo o blocchi) + `anthropic_content` (i blocchi del
/// messaggio assistente/tool). Le stime Python leggono `m.content` e
/// `additional_kwargs["anthropic_content"]`: qui derivati dal Message tipizzato.
fn message_to_ctx(m: &Message) -> ContextMessage {
    match m {
        Message::Human { content } | Message::Tool { content, .. } => content_to_ctx(content),
        Message::Ai { content, .. } => content_to_ctx(content),
    }
}

/// Mappa un [`MessageContent`] in [`ContextMessage`]: `Text` -> `content` stringa;
/// `Blocks` -> `anthropic_content` lista di blocchi (i blocchi sono serializzati
/// come Value, cosi' le stime contano le stringhe nei loro campi, 1:1 col Python).
fn content_to_ctx(c: &MessageContent) -> ContextMessage {
    match c {
        MessageContent::Text(s) => ContextMessage {
            content: Value::String(s.clone()),
            anthropic_content: Value::Null,
        },
        MessageContent::Blocks(blocks) => ContextMessage {
            content: Value::Null,
            anthropic_content: Value::Array(
                blocks
                    .iter()
                    .map(|b| serde_json::to_value(b).unwrap_or(Value::Null))
                    .collect(),
            ),
        },
    }
}

/// Costruisce un `Message::Human` che trasporta i blocchi `anthropic_content`
/// come `ContentBlock` (forma autoritativa Rust del contenuto a blocchi,
/// equivalente all'HumanMessage content="" + additional_kwargs Python). I
/// blocchi tool_result (JSON) sono deserializzati in `ContentBlock`; se uno non
/// corrisponde a una variante nota (es. il reminder `{"type":"text","text":...}`)
/// cade su `ContentBlock::Text` (fallback).
fn human_message_from_blocks(blocks: Vec<Value>) -> Message {
    use crate::state::ContentBlock;
    let parsed: Vec<ContentBlock> = blocks
        .into_iter()
        .map(|b| {
            serde_json::from_value::<ContentBlock>(b.clone()).unwrap_or_else(|_| {
                // Fallback: testo piatto (es. blocco con content non-stringa).
                ContentBlock::Text {
                    text: value_as_json_string(&b),
                }
            })
        })
        .collect();
    Message::Human {
        content: MessageContent::Blocks(parsed),
    }
}

/// Rende un `Value` come stringa JSON: se e' gia' una stringa la ritorna nuda
/// (il tool_result reale e' JSON-stringa), altrimenti serializza compatto. Usato
/// per il conteggio char e per il parser M16 (che si aspetta una stringa JSON).
fn value_as_json_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Serializza un `Value` come `json.dumps(v, ensure_ascii=False)` di Python
/// (PUNTO UNICO [`py_json_dumps`], regola L): separatori `", "`/`": "` con
/// SPAZIO, ordine d'inserimento. I content dei tool_result brain-only
/// (run_notes/task_complete/guard) e gli errori synthetic (M16/budget) sono
/// prodotti dal Python con `json.dumps` -> stessa forma bit-identica. Usare
/// `serde_json::to_string` (separatori compatti) divergerebbe dal Python.
fn py_dumps(v: &Value) -> String {
    py_json_dumps(v, SortKeys::No)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::runtime::ports::{PortError, SseEvent, ToolOutcome};
    use crate::runtime::test_doubles::{
        NullEventSink, RecordingEventSink, StubAgentStepStore, StubContextOffload, StubLlmGateway,
        StubMetaStepStore, StubRunControlStore, StubTodoStore,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::routing::config::RoutingConfig;
    use crate::state::{ContentBlock, Message, MessageContent};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    /// Esecutore di tool a coda per il dispatch: mappa per nome del tool a un
    /// `ToolOutcome` (content) e registra le chiamate con la mode. Cosi' un test
    /// puo' restituire payload diversi per tool diversi e verificare l'ordine.
    struct MapToolExecutor {
        by_name: std::collections::HashMap<String, ToolOutcome>,
        default: ToolOutcome,
        pub seen: std::sync::Mutex<Vec<(ToolCall, ExecMode)>>,
    }

    impl MapToolExecutor {
        fn new() -> Self {
            Self {
                by_name: std::collections::HashMap::new(),
                default: ToolOutcome {
                    tool_call_id: "x".into(),
                    content: json!("{\"ok\":true}"),
                    is_error: false,
                    ..Default::default()
                },
                seen: std::sync::Mutex::new(vec![]),
            }
        }
        fn with(mut self, name: &str, outcome: ToolOutcome) -> Self {
            self.by_name.insert(name.to_string(), outcome);
            self
        }
    }

    #[async_trait]
    impl ToolExecutor for MapToolExecutor {
        async fn execute(&self, call: ToolCall, mode: ExecMode) -> Result<ToolOutcome, PortError> {
            self.seen.lock().unwrap().push((call.clone(), mode));
            Ok(self
                .by_name
                .get(&call.name)
                .cloned()
                .unwrap_or_else(|| self.default.clone()))
        }
    }

    /// Esecutore che ritorna sempre un errore di porta (guasto infra): il nodo
    /// NON deve propagare NodeError, ma produrre un tool_result d'errore.
    struct FailingToolExecutor;
    #[async_trait]
    impl ToolExecutor for FailingToolExecutor {
        async fn execute(&self, _call: ToolCall, _mode: ExecMode) -> Result<ToolOutcome, PortError> {
            Err(PortError::Tool("grpc down".into()))
        }
    }

    fn ctx_with(shadow: bool, cancel: CancellationToken) -> AgentNodeCtx {
        ctx_with_emit(shadow, cancel, Arc::new(NullEventSink))
    }

    /// Come [`ctx_with`] ma con un [`EventSink`] iniettabile (per asserire gli
    /// emit `ToolResult` del nodo): i test passano un `RecordingEventSink`.
    fn ctx_with_emit(
        shadow: bool,
        cancel: CancellationToken,
        emit: Arc<dyn crate::runtime::ports::EventSink>,
    ) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(MapToolExecutor::new()),
            emit,
            cfg: RoutingConfig::default(),
            cancel,
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
        }
    }

    /// Nodo con porte stub configurabili.
    fn node(
        cfg: ToolDispatchConfig,
        tools: Arc<dyn ToolExecutor>,
    ) -> (
        ToolDispatchNode,
        Arc<StubAgentStepStore>,
        Arc<StubRunControlStore>,
    ) {
        let steps = Arc::new(StubAgentStepStore::default());
        let rc = Arc::new(StubRunControlStore::default());
        let n = ToolDispatchNode::new(
            cfg,
            tools,
            steps.clone(),
            rc.clone(),
            Arc::new(StubTodoStore::with_todos(vec![])),
            Arc::new(StubContextOffload::default()),
            Arc::new(StubMetaStepStore::default()),
        );
        (n, steps, rc)
    }

    fn node_full(
        cfg: ToolDispatchConfig,
        tools: Arc<dyn ToolExecutor>,
        rc: Arc<StubRunControlStore>,
        todos: Arc<StubTodoStore>,
        offload: Arc<StubContextOffload>,
    ) -> (ToolDispatchNode, Arc<StubAgentStepStore>) {
        let steps = Arc::new(StubAgentStepStore::default());
        let n = ToolDispatchNode::new(
            cfg,
            tools,
            steps.clone(),
            rc,
            todos,
            offload,
            Arc::new(StubMetaStepStore::default()),
        );
        (n, steps)
    }

    /// Stato con pending dato e thread_id valido.
    fn state_with_pending(pending: Vec<Value>) -> AgentState {
        AgentState {
            pending_tool_uses: Some(pending),
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            ..Default::default()
        }
    }

    fn pending_tool(id: &str, name: &str, input: Value) -> Value {
        json!({"id": id, "name": name, "input": input})
    }

    /// Estrae i blocchi tool_result (anthropic_content) dal Message Human del delta.
    fn blocks_of(msg: &Message) -> Vec<Value> {
        match msg {
            Message::Human {
                content: MessageContent::Blocks(blocks),
            } => blocks.iter().map(|b| serde_json::to_value(b).unwrap()).collect(),
            _ => vec![],
        }
    }

    // ── (2) pending vuoto -> end_turn ────────────────────────────────────────────

    #[tokio::test]
    async fn pending_vuoto_end_turn() {
        let (n, steps, _rc) = node(ToolDispatchConfig::default(), Arc::new(MapToolExecutor::new()));
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(out.pending_tool_uses, Some(vec![]));
        // Nessuno step persistito (uscita prima del dispatch).
        assert!(steps.steps.lock().unwrap().is_empty());
    }

    // ── DEBITO 3 (SSE primario): emit ToolResult per ogni risultato ──────────────

    /// Dopo l'esecuzione, il nodo emette un `SseEvent::ToolResult` per ogni tool,
    /// correlato via `tool_call_id`, con `is_error` coerente (ok vs errore).
    #[tokio::test]
    async fn tool_dispatch_emette_tool_result_per_ogni_tool() {
        let tools = Arc::new(
            MapToolExecutor::new()
                .with(
                    "read_file",
                    ToolOutcome {
                        tool_call_id: "a".into(),
                        content: json!("contenuto ok"),
                        is_error: false,
                        ..Default::default()
                    },
                )
                .with(
                    "run_command",
                    ToolOutcome {
                        tool_call_id: "b".into(),
                        content: json!("boom"),
                        is_error: true,
                        ..Default::default()
                    },
                ),
        );
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), tools);
        let sink = Arc::new(RecordingEventSink::default());
        let ctx = ctx_with_emit(false, CancellationToken::new(), sink.clone());
        let st = state_with_pending(vec![
            pending_tool("a", "read_file", json!({"path": "x"})),
            pending_tool("b", "run_command", json!({"cmd": "ls"})),
        ]);
        let _ = n.run(&st, &ctx).await.expect("run ok");

        let events = sink.events.lock().expect("lock");
        let results: Vec<&SseEvent> = events
            .iter()
            .filter(|e| matches!(e, SseEvent::ToolResult { .. }))
            .collect();
        assert_eq!(results.len(), 2, "un ToolResult per tool, eventi: {events:?}");
        assert!(
            results.iter().any(|e| matches!(
                e,
                SseEvent::ToolResult { tool_call_id, is_error, .. }
                    if tool_call_id == "a" && !*is_error
            )),
            "read_file -> ToolResult ok"
        );
        assert!(
            results.iter().any(|e| matches!(
                e,
                SseEvent::ToolResult { tool_call_id, is_error, .. }
                    if tool_call_id == "b" && *is_error
            )),
            "run_command -> ToolResult errore"
        );
    }

    /// Shadow intatto: in shadow il sink iniettato e' NullEventSink (no-op). Qui
    /// verifichiamo la proprieta' a livello di nodo: con NullEventSink (sink dello
    /// shadow) il run non panica e nessun emit e' osservabile (by-construction).
    #[tokio::test]
    async fn tool_dispatch_shadow_sink_noop() {
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), tools);
        let ctx = ctx_with(true, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("a", "read_file", json!({}))]);
        // shadow=true + NullEventSink (la combinazione di build_native_engine):
        // gli emit cadono nel no-op, nessun panic.
        let _ = n.run(&st, &ctx).await.expect("run shadow ok");
    }

    // ── (1) superseded via RunControlStore -> stop_reason superseded ─────────────

    #[tokio::test]
    async fn superseded_via_store_esce_cooperativo() {
        let rc = Arc::new(StubRunControlStore {
            superseded: true,
            ..Default::default()
        });
        let (n, _steps) = node_full(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
            rc,
            Arc::new(StubTodoStore::with_todos(vec![])),
            Arc::new(StubContextOffload::default()),
        );
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("a", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::Superseded));
        assert_eq!(out.pending_tool_uses, Some(vec![]));
    }

    // ── (1b) cancel del ctx -> superseded (senza interrogare lo store) ───────────

    #[tokio::test]
    async fn cancel_token_esce_cooperativo() {
        let (n, _steps, _rc) =
            node(ToolDispatchConfig::default(), Arc::new(MapToolExecutor::new()));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = ctx_with(false, cancel);
        let st = state_with_pending(vec![pending_tool("a", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::Superseded));
    }

    // ── is_superseded errore -> fail-open (il run prosegue) ──────────────────────

    #[tokio::test]
    async fn superseded_errore_fail_open() {
        let rc = Arc::new(StubRunControlStore {
            fail_is_superseded: true,
            ..Default::default()
        });
        let (n, _steps) = node_full(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
            rc,
            Arc::new(StubTodoStore::with_todos(vec![])),
            Arc::new(StubContextOffload::default()),
        );
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("a", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Errore di lettura -> trattato come false -> il run esegue (tool_use).
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
    }

    // ── (4) kept eseguiti, ordine preservato + exit_code invariato ───────────────

    #[tokio::test]
    async fn kept_eseguiti_ordine_preservato_e_exit_code() {
        let tools = Arc::new(
            MapToolExecutor::new()
                .with(
                    "run_command",
                    ToolOutcome {
                        tool_call_id: "c2".into(),
                        content: json!("{\"stdout\":\"build ok\"}"),
                        is_error: false,
                        exit_code: Some(0),
                        ..Default::default()
                    },
                )
                .with(
                    "read_file",
                    ToolOutcome {
                        tool_call_id: "c1".into(),
                        content: json!("{\"text\":\"file\"}"),
                        is_error: false,
                        ..Default::default()
                    },
                ),
        );
        let (n, steps, _rc) = node(ToolDispatchConfig::default(), tools);
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![
            pending_tool("c1", "read_file", json!({"path": "a"})),
            pending_tool("c2", "run_command", json!({"command": "build"})),
        ]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::ToolUse));
        let last = out.messages.last().expect("msg");
        let blocks = blocks_of(last);
        assert_eq!(blocks.len(), 2);
        // Ordine ORIGINALE preservato (c1 prima di c2), per posizione.
        assert_eq!(blocks[0]["tool_use_id"], json!("c1"));
        assert_eq!(blocks[1]["tool_use_id"], json!("c2"));
        // exit_code fluisce invariato (solo per il tool-comando).
        assert!(blocks[0].get("exit_code").is_none());
        assert_eq!(blocks[1]["exit_code"], json!(0));
        // Step persistiti (Real): 2 step con step_index iteration*1000+idx.
        let persisted = steps.steps.lock().unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[0].1, 0); // idx 0
        assert_eq!(persisted[1].1, 1); // idx 1
    }

    // ── (3a) predictive cap -> synthetic-blocked col SENTINEL (non eseguito) ─────

    #[tokio::test]
    async fn predictive_cap_blocca_col_sentinel() {
        let cfg = ToolDispatchConfig {
            context_window: 1000,
            predictive_cap_ratio: 0.1, // cap molto basso -> blocca subito.
            ..Default::default()
        };
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg, tools.clone());
        let ctx = ctx_with(false, CancellationToken::new());
        // un tool NON esente, con length grande -> sfora il cap.
        let st = state_with_pending(vec![pending_tool(
            "c1",
            "nexus_read_attachment",
            json!({"length": 100000}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        let content = blocks[0]["content"].as_str().unwrap();
        assert!(content.starts_with(PREDICTIVE_CAP_SENTINEL));
        // Tool NON eseguito (synthetic-block).
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    // ── (3a) la finestra EFFETTIVA dello stato (post smart-upscale) prevale ──────

    /// Fixture dei test del cap con finestra effettiva: config con finestra
    /// piccola + cap bassissimo (con 1000 blocca sempre) e UN pending
    /// `nexus_read_attachment` grande. `effective` valorizza lo stato.
    fn cap_fixture(effective: Option<i64>) -> (ToolDispatchConfig, AgentState) {
        let cfg = ToolDispatchConfig {
            context_window: 1000,
            predictive_cap_ratio: 0.1,
            ..Default::default()
        };
        let mut st = state_with_pending(vec![pending_tool(
            "c1",
            "nexus_read_attachment",
            json!({"length": 100000}),
        )]);
        st.effective_context_window = effective;
        (cfg, st)
    }

    /// Regressione incidente 2026-07-06: run partito su un modello con finestra
    /// piccola (o placeholder) e PROMOSSO dallo smart-upscale a un modello con
    /// finestra ampia. Il gate del predictive cap deve usare la finestra
    /// EFFETTIVA scritta dall'executor nello stato, non quella statica di
    /// config: prima restava alla finestra di partenza e bloccava OGNI tool
    /// per sempre (il figlio "completava" senza aver mai scritto un file).
    #[tokio::test]
    async fn predictive_cap_usa_finestra_effettiva_dello_stato() {
        // L'executor ha promosso il modello: finestra effettiva AMPIA nello stato.
        let (cfg, st) = cap_fixture(Some(10_000_000));
        let tools = Arc::new(MapToolExecutor::new().with(
            "nexus_read_attachment",
            ToolOutcome {
                tool_call_id: "c1".into(),
                content: json!("{\"text\":\"ok\"}"),
                is_error: false,
                ..Default::default()
            },
        ));
        let (n, _steps, _rc) = node(cfg, tools.clone());
        let ctx = ctx_with(false, CancellationToken::new());
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert_eq!(blocks.len(), 1);
        // Nessun blocco sintetico: il tool e' stato ESEGUITO davvero.
        assert!(!blocks[0]["is_error"].as_bool().expect("bool"));
        assert_eq!(tools.seen.lock().expect("lock").len(), 1, "tool eseguito");
    }

    /// Contro-prova: finestra effettiva assente o non valida (<=0) -> fallback
    /// alla finestra di config (comportamento storico invariato).
    #[tokio::test]
    async fn predictive_cap_fallback_su_config_senza_finestra_effettiva() {
        let (cfg, st) = cap_fixture(Some(0)); // non valida -> fallback config
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg, tools.clone());
        let ctx = ctx_with(false, CancellationToken::new());
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().expect("bool"));
        assert!(blocks[0]["content"]
            .as_str()
            .expect("str")
            .starts_with(PREDICTIVE_CAP_SENTINEL));
        assert!(
            tools.seen.lock().expect("lock").is_empty(),
            "tool bloccato dal cap"
        );
    }

    // ── (3a) guard blocked-da-cap matcha il SENTINEL e NON riesegue ──────────────

    #[tokio::test]
    async fn guard_blocked_da_cap_rifiuta_task_complete() {
        let cfg = ToolDispatchConfig {
            context_window: 1000,
            predictive_cap_ratio: 0.1,
            ..Default::default()
        };
        let (n, _steps, _rc) = node(cfg, Arc::new(MapToolExecutor::new()));
        let ctx = ctx_with(false, CancellationToken::new());
        // Una chiamata bloccata dal cap + un task_complete outcome=blocked.
        let st = state_with_pending(vec![
            pending_tool("c1", "nexus_read_attachment", json!({"length": 100000})),
            pending_tool("c2", "task_complete", json!({"outcome": "blocked", "summary": "stop"})),
        ]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // La dichiarazione blocked viene rifiutata (declared_outcome NON settato).
        assert_eq!(out.declared_outcome, None);
        assert_eq!(out.blocked_cap_rejected, Some(true));
        // Il tool_result del task_complete e' marcato is_error con la reason.
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let tc = blocks.iter().find(|b| b["tool_use_id"] == json!("c2")).unwrap();
        assert!(tc["is_error"].as_bool().unwrap());
        assert!(tc["content"].as_str().unwrap().contains("RIFIUTATA"));
    }

    // ── (3b) M16: tool non scoperto -> synthetic error, forza search ─────────────

    #[tokio::test]
    async fn m16_tool_non_scoperto_rifiutato() {
        let cfg = ToolDispatchConfig {
            discovery_first_enabled: true,
            // whitelist contiene solo i meta; read_file NON e' ammesso.
            discovery_first_whitelist: vec!["nexus_mcp_tool_search".into()],
            ..Default::default()
        };
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg, tools.clone());
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({"path": "a"}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        assert!(blocks[0]["content"].as_str().unwrap().contains("nexus_mcp_tool_search"));
        // Tool NON eseguito.
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    // ── (3c) budget allegati esaurito -> synthetic error ─────────────────────────

    #[tokio::test]
    async fn budget_allegati_esaurito_blocca() {
        let cfg = ToolDispatchConfig {
            attachment_budget_bytes: 1000,
            ..Default::default()
        };
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg, tools.clone());
        let ctx = ctx_with(false, CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "nexus_read_attachment", json!({}))]);
        st.attachment_read_bytes = Some(2000); // gia' oltre il budget.
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        assert!(blocks[0]["content"].as_str().unwrap().contains("budget letture allegati esaurito"));
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    // ── run_notes / task_complete brain-only (non via ToolExecutor) ──────────────

    #[tokio::test]
    async fn brain_only_run_notes_e_task_complete() {
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), tools.clone());
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![
            pending_tool("c1", "nexus_run_notes", json!({"action": "set", "content": "appunto"})),
            pending_tool("c2", "task_complete", json!({"outcome": "done", "summary": "ok"})),
        ]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // run_notes persistito; declared_outcome=done; declared_done_count=1.
        assert_eq!(out.run_notes.as_deref(), Some("appunto"));
        assert_eq!(
            out.declared_outcome.as_ref().unwrap()["outcome"],
            json!("done")
        );
        assert_eq!(out.declared_done_count, Some(1));
        // Nessun tool eseguito via ToolExecutor (entrambi brain-only).
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dichiarazione_stantia_invalidata_da_lavoro_successivo() {
        // ADR 0034: se il run PROSEGUE con altri tool dopo una dichiarazione
        // precedente, quella dichiarazione era intermedia -> azzerata. Senza,
        // un "partial"/"blocked" dichiarato a meta' run falsava lo status
        // canonico finale anche a lavoro poi completato.
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), Arc::new(MapToolExecutor::new()));
        let ctx = ctx_with(false, CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.declared_outcome = Some(json!({"outcome": "partial", "summary": "meta'"}));
        st.declared_done_count = Some(0);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Dichiarazione stantia azzerata; il contatore done resta cumulativo.
        assert!(out.declared_outcome.is_none());
    }

    // ── (5) errore infrastruttura -> tool_result d'errore, niente NodeError ──────

    #[tokio::test]
    async fn errore_porta_non_propaga_node_error() {
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), Arc::new(FailingToolExecutor));
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run NON deve fallire"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        assert!(blocks[0]["content"].as_str().unwrap().contains("grpc down"));
    }

    // ── (11) discovered_tools_next_turn SEMPRE scritto (anche []) ────────────────

    #[tokio::test]
    async fn discovered_sempre_scritto_anche_vuoto() {
        // Un read_file qualunque: nessun search -> discovered_next = [] ma SCRITTO.
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), Arc::new(MapToolExecutor::new()));
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Scritto a lista vuota (azzera i discovered del turno prima).
        assert_eq!(out.discovered_tools_next_turn, Some(vec![]));
    }

    #[tokio::test]
    async fn discovered_parse_da_search() {
        let search_payload = json!({
            "results": [
                {"tool_name": "nexus_foo", "description": "fa foo", "input_schema": {"type": "object"}},
                {"name": "nexus_bar"}
            ]
        })
        .to_string();
        let tools = Arc::new(MapToolExecutor::new().with(
            "nexus_mcp_tool_search",
            ToolOutcome {
                tool_call_id: "c1".into(),
                content: Value::String(search_payload),
                is_error: false,
                ..Default::default()
            },
        ));
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), tools);
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool(
            "c1",
            "nexus_mcp_tool_search",
            json!({"query": "foo"}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let disc = out.discovered_tools_next_turn.expect("discovered");
        assert_eq!(disc.len(), 2);
        assert_eq!(disc[0]["name"], json!("nexus_foo"));
        assert_eq!(disc[1]["name"], json!("nexus_bar"));
        // P3: discovered_tools_run accumulato (merge dedup).
        let run_acc = out.discovered_tools_run.expect("run acc");
        assert_eq!(run_acc.len(), 2);
    }

    // ── (8) shadow: ExecMode::Replay, zero side-effect ───────────────────────────

    #[tokio::test]
    async fn shadow_usa_replay_zero_side_effect() {
        let tools = Arc::new(MapToolExecutor::new());
        let steps = Arc::new(StubAgentStepStore::default());
        let rc = Arc::new(StubRunControlStore::default());
        let n = ToolDispatchNode::new(
            ToolDispatchConfig::default(),
            tools.clone(),
            steps.clone(),
            rc.clone(),
            Arc::new(StubTodoStore::with_todos(vec![])),
            Arc::new(StubContextOffload::default()),
            Arc::new(StubMetaStepStore::default()),
        );
        let ctx = ctx_with(true, CancellationToken::new()); // shadow
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        let _ = n.run(&st, &ctx).await.expect("run ok");
        // ToolExecutor chiamato in Replay.
        let seen = tools.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, ExecMode::Replay);
        // AgentStepStore no-op in shadow (zero step persistiti).
        assert!(steps.steps.lock().unwrap().is_empty());
        // Heartbeat no-op in shadow.
        assert!(rc.heartbeats.lock().unwrap().is_empty());
    }

    // ── (10) reminder TODO iniettato alla soglia ─────────────────────────────────

    #[tokio::test]
    async fn reminder_todo_iniettato_alla_soglia() {
        // StubTodoStore default ritorna None da build_reminder_text; serve uno
        // store che ritorna un testo. Usiamo un piccolo stub locale.
        struct ReminderStore;
        #[async_trait]
        impl TodoStore for ReminderStore {
            async fn list_todos(&self, _r: &str) -> Result<Vec<crate::decisions::dag_scheduler::Todo>, PortError> {
                Ok(vec![])
            }
            async fn mark_status(
                &self,
                _id: &str,
                _s: crate::decisions::dag_scheduler::TodoStatus,
                _m: ExecMode,
            ) -> Result<(), PortError> {
                Ok(())
            }
            async fn build_reminder_text(&self, _r: &str) -> Result<Option<String>, PortError> {
                Ok(Some("CHECKLIST: 1) fai X".to_string()))
            }
        }
        let cfg = ToolDispatchConfig {
            todo_reminder_every_n_steps: 1,
            ..Default::default()
        };
        let steps = Arc::new(StubAgentStepStore::default());
        let n = ToolDispatchNode::new(
            cfg,
            Arc::new(MapToolExecutor::new()),
            steps,
            Arc::new(StubRunControlStore::default()),
            Arc::new(ReminderStore),
            Arc::new(StubContextOffload::default()),
            Arc::new(StubMetaStepStore::default()),
        );
        let ctx = ctx_with(false, CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.plan_phase_active = Some(true);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Counter resettato a 0 dopo l'injection.
        assert_eq!(out.since_last_todo_reminder, Some(0));
        // Il blocco reminder e' appeso ai blocchi (ContentBlock::Text col tag).
        let last = out.messages.last().expect("msg");
        if let Message::Human { content: MessageContent::Blocks(blocks) } = last {
            let has_reminder = blocks.iter().any(|b| {
                matches!(b, ContentBlock::Text { text } if text.contains("system-reminder"))
            });
            assert!(has_reminder, "il reminder deve essere appeso ai blocchi");
        } else {
            panic!("atteso HumanMessage a blocchi");
        }
    }

    // ── (9) context-budget cap: troncamento aggressivo con offload ───────────────

    #[tokio::test]
    async fn context_budget_cap_tronca_e_offloada() {
        let cfg = ToolDispatchConfig {
            max_context_chars: 100, // soglia minuscola -> forza il cap.
            tool_result_max_chars: 1_000_000, // non taglia al passo (5).
            ..Default::default()
        };
        let big = "x".repeat(5000);
        let tools = Arc::new(MapToolExecutor::new().with(
            "read_file",
            ToolOutcome {
                tool_call_id: "c1".into(),
                content: Value::String(big.clone()),
                is_error: false,
                ..Default::default()
            },
        ));
        let offload = Arc::new(StubContextOffload::default());
        let (n, _steps) = node_full(
            cfg,
            tools,
            Arc::new(StubRunControlStore::default()),
            Arc::new(StubTodoStore::with_todos(vec![])),
            offload.clone(),
        );
        let ctx = ctx_with(false, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().unwrap();
        // Troncato (molto piu' corto dell'originale) + pointer di offload.
        assert!(content.chars().count() < big.chars().count());
        assert!(content.contains("offloadati in RAG"));
        // L'offload e' stato chiamato.
        assert_eq!(offload.offloaded.lock().unwrap().len(), 1);
    }

    /// FIX 1 (gate shadow su ContextOffload): in `ExecMode::Replay` il troncamento
    /// NON deve produrre scritture su Qdrant. Con un tool_result oltre soglia, il
    /// content e' troncato INLINE (testa+coda, marker non-RAG) e `offloaded` resta
    /// VUOTO (zero side-effect, gate `Real` nella porta).
    #[tokio::test]
    async fn context_budget_cap_in_shadow_non_offloada() {
        let cfg = ToolDispatchConfig {
            max_context_chars: 100, // soglia minuscola -> forza il cap (passo 9).
            tool_result_max_chars: 50, // taglia anche al passo (5).
            ..Default::default()
        };
        let big = "x".repeat(5000);
        let tools = Arc::new(MapToolExecutor::new().with(
            "read_file",
            ToolOutcome {
                tool_call_id: "c1".into(),
                content: Value::String(big.clone()),
                is_error: false,
                ..Default::default()
            },
        ));
        let offload = Arc::new(StubContextOffload::default());
        let (n, _steps) = node_full(
            cfg,
            tools,
            Arc::new(StubRunControlStore::default()),
            Arc::new(StubTodoStore::with_todos(vec![])),
            offload.clone(),
        );
        // ctx SHADOW: ExecMode::Replay.
        let ctx = ctx_with(true, CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().unwrap();
        // Content TRONCATO inline (testa+coda) ma SENZA pointer RAG.
        assert!(
            content.chars().count() < big.chars().count(),
            "il content deve essere troncato anche in shadow"
        );
        assert!(
            content.contains("coda preservata sotto"),
            "in shadow il marker e' il troncamento testa+coda non-RAG"
        );
        assert!(
            !content.contains("offloadati in RAG"),
            "in shadow NON ci deve essere un pointer di offload RAG"
        );
        // ZERO scritture Qdrant: il gate Real della porta rende l'offload no-op.
        assert!(
            offload.offloaded.lock().unwrap().is_empty(),
            "in shadow l'offload non deve scrivere nulla (zero side-effect)"
        );
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del nodo
    //! tool_dispatch. Lo script `scripts/gen_golden_tool_dispatch.py` importa le
    //! funzioni PURE reali dal brain + replica la decision-machine del nodo data
    //! l'esito STUBATO dei tool (`tool_results` mappato per id) e salva
    //! `{case_id, input, output}` in `/tmp/golden_tool_dispatch.json`. Qui
    //! ricostruiamo stato/config/esiti, eseguiamo il NODO con un ToolExecutor
    //! che ritorna gli esiti stubati, e confrontiamo i campi del delta + i blocchi
    //! tool_result col golden Python.
    //!
    //! `#[ignore]`: dipende dal file generato. Comando:
    //!   python3 crates/nexus-agent-graph/scripts/gen_golden_tool_dispatch.py
    //!   cargo test -p nexus-agent-graph --lib golden_tool_dispatch_parita -- --ignored

    use std::sync::Arc;

    use async_trait::async_trait;
    use nexus_graph::node::GraphNode;
    use nexus_graph::GraphState as _;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::runtime::ports::{PortError, ToolOutcome};
    use crate::runtime::test_doubles::{
        NullEventSink, StubAgentStepStore, StubContextOffload, StubLlmGateway, StubRunControlStore,
        StubTodoStore,
    };
    use crate::routing::config::RoutingConfig;
    use crate::state::{AgentState, Message, MessageContent};

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        input: Value,
        output: Value,
    }

    /// ToolExecutor che ritorna l'esito stubato per id (dai `tool_results` golden).
    struct GoldenToolExecutor {
        by_id: std::collections::HashMap<String, ToolOutcome>,
    }

    #[async_trait]
    impl ToolExecutor for GoldenToolExecutor {
        async fn execute(&self, call: ToolCall, _mode: ExecMode) -> Result<ToolOutcome, PortError> {
            Ok(self.by_id.get(&call.id).cloned().unwrap_or(ToolOutcome {
                tool_call_id: call.id,
                content: Value::String("{}".to_string()),
                is_error: false,
                ..Default::default()
            }))
        }
    }

    /// Costruisce lo stato dall'input golden (`state`).
    fn state_from(v: &Value) -> AgentState {
        let mut st = AgentState {
            pending_tool_uses: v
                .get("pending_tool_uses")
                .and_then(Value::as_array)
                .cloned(),
            thread_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            ..Default::default()
        };
        if let Some(n) = v.get("attachment_read_bytes").and_then(Value::as_i64) {
            st.attachment_read_bytes = Some(n);
        }
        if let Some(d) = v.get("discovered_tools_next_turn").and_then(Value::as_array) {
            st.discovered_tools_next_turn = Some(d.clone());
        }
        if let Some(b) = v.get("blocked_cap_rejected").and_then(Value::as_bool) {
            st.blocked_cap_rejected = Some(b);
        }
        if let Some(n) = v.get("declared_done_count").and_then(Value::as_i64) {
            st.declared_done_count = Some(n);
        }
        if let Some(rn) = v.get("run_notes").and_then(Value::as_str) {
            st.run_notes = Some(rn.to_string());
        }
        if let Some(s) = v.get("system_text").and_then(Value::as_str) {
            st.system_text = Some(s.to_string());
        }
        st
    }

    /// Costruisce la config dall'input golden (`cfg`).
    fn cfg_from(v: &Value) -> ToolDispatchConfig {
        let d = ToolDispatchConfig::default();
        ToolDispatchConfig {
            predictive_cap_ratio: v
                .get("predictive_cap_ratio")
                .and_then(Value::as_f64)
                .unwrap_or(d.predictive_cap_ratio),
            context_window: v
                .get("context_window")
                .and_then(Value::as_i64)
                .unwrap_or(d.context_window),
            attachment_budget_bytes: v
                .get("attachment_budget_bytes")
                .and_then(Value::as_i64)
                .unwrap_or(d.attachment_budget_bytes),
            discovery_first_enabled: v
                .get("discovery_first_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(d.discovery_first_enabled),
            discovery_first_whitelist: v
                .get("discovery_first_whitelist")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or(d.discovery_first_whitelist),
            always_on_tools: v
                .get("always_on_tools")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or(d.always_on_tools),
            discovery_schema_max_bytes: v
                .get("discovery_schema_max_bytes")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(d.discovery_schema_max_bytes),
            // Il golden NON esercita il troncamento (tr_max_chars/max_context_chars
            // alti): la decisione deterministica e' gate/ordine/discovered/delta.
            tool_result_max_chars: 100_000_000,
            max_context_chars: usize::MAX,
            todo_reminder_every_n_steps: d.todo_reminder_every_n_steps,
        }
    }

    /// Esiti stubati dei tool (`tool_results`: id -> {content, is_error, exit_code}).
    fn tool_results_from(v: &Value) -> std::collections::HashMap<String, ToolOutcome> {
        let mut map = std::collections::HashMap::new();
        if let Some(obj) = v.as_object() {
            for (id, stub) in obj {
                let content = stub.get("content").cloned().unwrap_or(json!("{}"));
                let is_error = stub.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                let exit_code = stub.get("exit_code").and_then(Value::as_i64);
                map.insert(
                    id.clone(),
                    ToolOutcome {
                        tool_call_id: id.clone(),
                        content,
                        is_error,
                        exit_code,
                        ..Default::default()
                    },
                );
            }
        }
        map
    }

    /// Estrae i blocchi tool_result (anthropic_content) dal Message Human del delta,
    /// nella stessa forma del Python (`{type, tool_use_id, content, is_error,
    /// exit_code?}`). I blocchi reminder (Text) sono esclusi (il golden non li
    /// genera: nessun plan_phase_active negli input).
    fn blocks_of(msg: &Message) -> Vec<Value> {
        match msg {
            Message::Human {
                content: MessageContent::Blocks(blocks),
            } => blocks
                .iter()
                .filter_map(|b| {
                    let v = serde_json::to_value(b).ok()?;
                    if v.get("type").and_then(Value::as_str) == Some("tool_result") {
                        Some(v)
                    } else {
                        None
                    }
                })
                .collect(),
            _ => vec![],
        }
    }

    fn ctx() -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("x")),
            tools: Arc::new(GoldenToolExecutor {
                by_id: std::collections::HashMap::new(),
            }),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow: false,
        }
    }

    /// Costruisce dall'`out` Rust (delta applicato) la forma confrontabile col
    /// golden Python: solo i campi che il delta del nodo scrive in modo
    /// deterministico (i synthetic/kept content sono nei blocchi).
    fn rust_output(out: &AgentState, msg: Option<&Message>) -> Value {
        let mut m = serde_json::Map::new();
        m.insert(
            "pending_tool_uses".to_string(),
            json!(out.pending_tool_uses.clone().unwrap_or_default()),
        );
        if let Some(sr) = out.stop_reason {
            m.insert("stop_reason".to_string(), serde_json::to_value(sr).unwrap());
        }
        // I rami end_turn/superseded NON scrivono gli altri campi.
        if out.stop_reason == Some(StopReason::ToolUse) {
            m.insert(
                "attachment_read_bytes".to_string(),
                json!(out.attachment_read_bytes.unwrap_or(0)),
            );
            m.insert(
                "discovered_tools_next_turn".to_string(),
                json!(out.discovered_tools_next_turn.clone().unwrap_or_default()),
            );
            if let Some(msg) = msg {
                m.insert("blocks".to_string(), json!(blocks_of(msg)));
            }
            // meta_steps "tool_executed": serializzati come {kind, title, payload}
            // (created_at/correlation_id None -> omessi via skip_serializing_if),
            // 1:1 col golden Python che non include created_at.
            m.insert(
                "meta_steps".to_string(),
                json!(out
                    .meta_steps
                    .iter()
                    .map(|ms| serde_json::to_value(ms).unwrap())
                    .collect::<Vec<_>>()),
            );
            if let Some(d) = &out.declared_outcome {
                m.insert("declared_outcome".to_string(), d.clone());
            }
            if let Some(c) = out.declared_done_count {
                m.insert("declared_done_count".to_string(), json!(c));
            }
            if out.tool_infra_error == Some(true) {
                m.insert("tool_infra_error".to_string(), json!(true));
            }
            if out.blocked_cap_rejected == Some(true) {
                m.insert("blocked_cap_rejected".to_string(), json!(true));
            }
            // run_notes scritto solo se cambiato: lo stato finale lo riflette.
            // Il Python lo include solo quando diverge dallo stato di partenza.
        }
        Value::Object(m)
    }

    #[tokio::test]
    #[ignore = "richiede /tmp/golden_tool_dispatch.json generato da gen_golden_tool_dispatch.py"]
    async fn golden_tool_dispatch_parita() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_tool_dispatch.json",
            "gen_golden_tool_dispatch.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 8, "attesi >= 8 casi, trovati {}", cases.len());

        let mut checked = 0usize;
        for c in &cases {
            let st = state_from(c.input.get("state").unwrap_or(&Value::Null));
            let cfg = cfg_from(c.input.get("cfg").unwrap_or(&Value::Null));
            let tr = tool_results_from(c.input.get("tool_results").unwrap_or(&Value::Null));

            // Il caso "superseded" e' segnalato via _superseded nello stato golden:
            // lo mappiamo su uno StubRunControlStore superseded.
            let supersed = c
                .input
                .get("state")
                .and_then(|s| s.get("_superseded"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let rc = Arc::new(StubRunControlStore {
                superseded: supersed,
                ..Default::default()
            });
            let steps = Arc::new(StubAgentStepStore::default());
            let node = ToolDispatchNode::new(
                cfg,
                Arc::new(GoldenToolExecutor { by_id: tr }),
                steps,
                rc,
                Arc::new(StubTodoStore::with_todos(vec![])),
                Arc::new(StubContextOffload::default()),
                Arc::new(crate::runtime::test_doubles::StubMetaStepStore::default()),
            );
            let context = ctx();
            let delta = node.run(&st, &context).await.expect("run ok");
            let mut applied = st.clone();
            applied.merge(delta);
            let msg = applied.messages.last().cloned();
            let got = rust_output(&applied, msg.as_ref());

            // run_notes: il Python lo mette nel delta solo se cambiato; per i casi
            // dove cambia (brain_only) lo confrontiamo separatamente sullo stato.
            if let Some(expected_rn) = c.output.get("run_notes").and_then(Value::as_str) {
                assert_eq!(
                    applied.run_notes.as_deref(),
                    Some(expected_rn),
                    "run_notes diverge nel caso {}",
                    c.case_id
                );
            }

            // Confronto dei campi deterministici (esclude run_notes, gia' sopra).
            let mut expected = c.output.clone();
            if let Some(obj) = expected.as_object_mut() {
                obj.remove("run_notes");
            }
            assert_eq!(
                got, expected,
                "PARITA' FALLITA caso {}:\n  rust   = {}\n  python = {}",
                c.case_id, got, expected
            );
            checked += 1;
        }
        println!("golden tool_dispatch: {checked} casi verificati, tutti verdi");
    }
}
