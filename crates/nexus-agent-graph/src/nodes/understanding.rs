//! `UnderstandingNode` — porta `understanding_node`
//! (`brain/agents/understanding_node.py:69-206`), nodo di comprensione
//! pre-planning (Cluster 2).
//!
//! Il nodo si attiva SOLO per task complessi (gated da complessita' + token
//! budget); quando attivo fa grounding semantico (tool `nexus_search_semantic`),
//! fan-out `explore` opzionale (tool `dispatch_subagent`) e produce un
//! `context_brief` (concatenazione strutturata o sintesi LLM economica) che il
//! planner inietta nel proprio system prompt. Flag OFF o task non complesso =>
//! pass-through (delta vuoto), path identico a oggi. Best-effort: ogni errore
//! I/O degrada a no-op, non blocca mai il run.
//!
//! ## Cosa porta QUESTO PR (deterministico, testato golden 1:1)
//!
//! - **I 6 gate di skip** (`understanding_node.py:71-93`): flag OFF, depth guard
//!   sub-agent, token budget minimo, complessita' (`_is_complex`), tool runner
//!   assente, query troppo corta. Ogni gate produce lo stesso
//!   `understanding_skip_reason` del Python.
//! - **`_is_complex`** (`:46-58`): `task_complexity == "high"` OR `is_ambiguous`
//!   OR `agentic_score >= 0.7` (con la stessa tolleranza al cast del Python).
//! - **`_last_user_message`** (`:61-66`): ultimo messaggio (in reverse) con
//!   contenuto stringa non vuoto, trimmed.
//! - **Formattazione `<grounding>`** (`:113-122`): rendering deterministico delle
//!   hit (source/score/testo troncato a 300 char) a partire dal JSON del tool.
//! - **Sotto-query del fan-out** (`:135-139`): euristica leggera (no LLM),
//!   deterministica, derivata dal task e clampata a `understanding_max_explore`.
//! - **Formattazione `<esplorazioni>`** (`:160-163`): rendering deterministico
//!   dei summary (troncati a 400 char) dal JSON dei sub-agent.
//! - **Gate `no_context_found`** (`:167-168`) e **assemblaggio `raw_brief`**
//!   (`:170`): concatenazione strutturata dei due block.
//!
//! ## Cosa NON porta (I/O delegato dietro i trait, TODO espliciti)
//!
//! - L'**esecuzione** dei tool `nexus_search_semantic` e `dispatch_subagent`
//!   passa per `ctx.tools` (`ToolExecutor`): il nodo costruisce l'input,
//!   l'esecutore concreto (mcp-core) fa l'I/O. I test usano lo stub.
//! - La **sintesi LLM** (`:172-197`) richiede la risoluzione `purpose_model`
//!   ("understanding") + il clamp `clamp_single_prompt` + `ctx.llm`: e' un TODO
//!   delegato. Il provider/modello vanno RISOLTI A MONTE (regola G); finche' non
//!   c'e' la porta che li fornisce, il nodo usa il `raw_brief` strutturato (ramo
//!   identico al Python quando `understanding_synthesize_enabled` e' OFF), che e'
//!   il default DB documentato.
//!
//! Il nodo NON instrada (l'edge e' fuori, in `edge.rs`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, StateDelta, ToolUse};

/// Lunghezza minima della query (ultimo messaggio utente) sotto la quale il
/// nodo salta: replica `len(query) < 10` (`understanding_node.py:92`).
const MIN_QUERY_LEN: usize = 10;

/// Soglia di `agentic_score` oltre la quale il task e' considerato complesso
/// (`understanding_node.py:54`).
const AGENTIC_SCORE_THRESHOLD: f64 = 0.7;

/// Troncamento del testo di una hit di grounding (`understanding_node.py:117`).
const HIT_TEXT_MAX: usize = 300;

/// Troncamento del summary di un'esplorazione (`understanding_node.py:161`).
const EXPLORE_SUMMARY_MAX: usize = 400;

/// Config DB-driven del nodo understanding, PASSATA (regola G: nessuna lettura
/// DB nel nodo, nessun fallback hardcoded dentro la logica decisionale).
///
/// Mappa i settings letti dal brain via `orchestrator_config.get()`
/// (`orchestrator_config.py:79-84,192`):
///   - `understanding_enabled`            -> `enabled` (default false)
///   - `understanding_fanout_enabled`     -> `fanout_enabled` (default false)
///   - `understanding_synthesize_enabled` -> `synthesize_enabled` (default false)
///   - `understanding_topk`               -> `topk` (default 8)
///   - `understanding_min_token_budget`   -> `min_token_budget` (default 3000)
///   - `understanding_max_explore`        -> `max_explore` (default 3)
///   - `subagents_enabled`                -> `subagents_enabled` (default true)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnderstandingConfig {
    /// Nodo understanding attivo (OFF di default -> pass-through totale).
    pub enabled: bool,
    /// Fan-out `explore` via `dispatch_subagent` abilitato.
    pub fanout_enabled: bool,
    /// Sintesi LLM economica del brief abilitata (TODO I/O delegato).
    pub synthesize_enabled: bool,
    /// Numero di hit richieste a `nexus_search_semantic` (e cap sul rendering).
    pub topk: i64,
    /// Token budget minimo del turno sotto cui il nodo salta.
    pub min_token_budget: i64,
    /// Numero massimo di sotto-query `explore` lanciate in parallelo.
    pub max_explore: i64,
    /// Sub-agent globalmente abilitati (gate condiviso col fan-out).
    pub subagents_enabled: bool,
}

impl Default for UnderstandingConfig {
    fn default() -> Self {
        // Default IDENTICI ai `_SAFE_DEFAULTS` del brain
        // (`orchestrator_config.py:205-211` + `subagents_enabled` a 192). Valgono
        // SOLO se il DB e' irraggiungibile, mai come magic fallback nella logica.
        Self {
            enabled: false,
            fanout_enabled: false,
            synthesize_enabled: false,
            topk: 8,
            min_token_budget: 3000,
            max_explore: 3,
            subagents_enabled: true,
        }
    }
}

/// Nodo di comprensione pre-planning. Stateless: legge lo stato + la config
/// passata e fa I/O tramite le porte del `AgentNodeCtx` (RAG/fan-out via
/// `ctx.tools`). La sintesi LLM e' un TODO delegato (vedi doc del modulo).
pub struct UnderstandingNode {
    /// Config DB-driven (regola G: passata, mai letta dal nodo).
    cfg: UnderstandingConfig,
}

impl UnderstandingNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante.
    pub fn new(cfg: UnderstandingConfig) -> Self {
        Self { cfg }
    }

    /// Delta di skip con motivo dato: `understanding_active=false` +
    /// `understanding_skip_reason=reason`. Punto unico (regola L): tutti i gate
    /// che NON sono il pass-through totale passano da qui.
    fn skip(reason: &str) -> OpaqueDelta {
        StateDelta {
            understanding_active: Some(Some(false)),
            understanding_skip_reason: Some(Some(reason.to_string())),
            ..Default::default()
        }
        .into_opaque()
    }

    /// `true` se il task e' complesso secondo i segnali del classifier adattivo.
    /// Replica `_is_complex` (`understanding_node.py:46-58`): high OR ambiguo OR
    /// score >= 0.7. La tolleranza al cast (`agentic_score` non numerico ignorato)
    /// e' implicita: nello stato Rust `agentic_score` e' gia' `Option<f64>`.
    fn is_complex(state: &AgentState) -> bool {
        use crate::state::TaskComplexity;
        if matches!(state.task_complexity, Some(TaskComplexity::High)) {
            return true;
        }
        if state.is_ambiguous.unwrap_or(false) {
            return true;
        }
        if let Some(score) = state.agentic_score {
            if score >= AGENTIC_SCORE_THRESHOLD {
                return true;
            }
        }
        false
    }

    /// Ultimo messaggio (in reverse) con contenuto testuale non vuoto, trimmed.
    /// Replica `_last_user_message` (`understanding_node.py:61-66`): NON filtra
    /// per ruolo (il Python itera su tutti i messaggi e prende il primo, dal
    /// fondo, con `content` stringa non vuota).
    fn last_user_message(messages: &[Message]) -> String {
        for m in messages.iter().rev() {
            let content = match m {
                Message::Human { content } => content,
                Message::Ai { content, .. } => content,
                Message::Tool { content, .. } => content,
            };
            let flat = content.flatten_text();
            let trimmed = flat.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        String::new()
    }

    /// Tronca una stringa a `max` caratteri (semantica `str[:max]` del Python,
    /// che taglia per code point Unicode, non per byte).
    fn truncate_chars(s: &str, max: usize) -> String {
        s.chars().take(max).collect()
    }

    /// Costruisce il `<grounding>` block dal JSON ritornato da
    /// `nexus_search_semantic`. Deterministico (`understanding_node.py:111-122`):
    /// prende le prime `topk` hit, per ognuna source/score/testo (trim + 300 char),
    /// scarta le hit a testo vuoto, e wrappa in `<grounding>...</grounding>`.
    /// Stringa vuota se nessuna hit utile (il chiamante decide il fallback).
    fn render_grounding(result_json: &str, topk: usize) -> String {
        let parsed: Value = serde_json::from_str(result_json).unwrap_or(Value::Null);
        let hits = parsed
            .get("hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        for h in hits.into_iter().take(topk) {
            let sk = h
                .get("source_kind")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let txt_raw = h.get("chunk_text").and_then(Value::as_str).unwrap_or("");
            let txt = Self::truncate_chars(txt_raw.trim(), HIT_TEXT_MAX);
            // `float(... or 0)` Python: numero o 0.0; le stringhe numeriche del
            // tool sono fuori contratto (hits.score e' numerico), quindi qui
            // accettiamo solo numeri JSON, coerente con il payload reale.
            let score = h.get("score").and_then(Value::as_f64).unwrap_or(0.0);
            if !txt.is_empty() {
                lines.push(format!(
                    "  <hit source=\"{sk}\" score=\"{score:.2}\">{txt}</hit>"
                ));
            }
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("<grounding>\n{}\n</grounding>", lines.join("\n"))
        }
    }

    /// Sotto-query deterministiche del fan-out (`understanding_node.py:135-139`):
    /// euristica leggera (no LLM) clampata a `max_explore`.
    fn explore_subqueries(query: &str, max_explore: usize) -> Vec<String> {
        let all = [
            format!("Come funziona e dove si trova nel codebase: {query}"),
            format!("Test, vincoli e casi limite rilevanti per: {query}"),
            format!("Dipendenze e impatti di: {query}"),
        ];
        all.into_iter().take(max_explore).collect()
    }

    /// Costruisce il `<esplorazioni>` block dai summary dei sub-agent
    /// (`understanding_node.py:151-163`): per ogni risultato non-errore estrae
    /// `summary` (troncato a 400 char), scarta i vuoti, wrappa. Gli errori sono
    /// gia' filtrati a monte dal chiamante (le esecuzioni fallite non arrivano).
    fn render_explore(summaries_json: &[String]) -> String {
        let mut lines: Vec<String> = Vec::new();
        for raw in summaries_json {
            let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
            let summary = parsed
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !summary.is_empty() {
                let truncated = Self::truncate_chars(summary, EXPLORE_SUMMARY_MAX);
                lines.push(format!("  <explore>{truncated}</explore>"));
            }
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("<esplorazioni>\n{}\n</esplorazioni>", lines.join("\n"))
        }
    }

    /// Assembla il `raw_brief` concatenando i due block non vuoti con doppia
    /// riga vuota (`understanding_node.py:170`): `"\n\n".join(...)`.
    fn assemble_raw_brief(grounding: &str, explore: &str) -> String {
        [grounding, explore]
            .into_iter()
            .filter(|b| !b.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for UnderstandingNode {
    fn id(&self) -> NodeId {
        NodeId::Understanding
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        // ── Gate 0: flag OFF -> pass-through TOTALE (delta vuoto) ─────────────
        // `understanding_node.py:71-73`. NON e' uno skip "attivo": il delta e'
        // vuoto, lo stato resta identico a oggi (nessun campo understanding_*).
        if !self.cfg.enabled {
            return Ok(StateDelta::default().into_opaque());
        }

        // ── Gate 1: depth guard sub-agent (anti-esplosione, :80-81) ───────────
        if state.subagent_depth.unwrap_or(0) >= 1 {
            return Ok(Self::skip("skip_in_subagent"));
        }

        // ── Gate 2: token budget minimo (:83-85) ──────────────────────────────
        let token_budget = state.token_budget.unwrap_or(0);
        if token_budget < self.cfg.min_token_budget {
            return Ok(Self::skip("budget_too_low"));
        }

        // ── Gate 3: complessita' (:86-87) ──────────────────────────────────────
        if !Self::is_complex(state) {
            return Ok(Self::skip("not_complex"));
        }

        // NB il gate `tool_runner_missing` (:88-89) e' implicito nel runtime Rust:
        // `ctx.tools` e' sempre presente (porta iniettata da mcp-core, mai None).

        // ── Gate 4: query troppo corta (:91-93) ────────────────────────────────
        let query = Self::last_user_message(&state.messages);
        if query.chars().count() < MIN_QUERY_LEN {
            return Ok(Self::skip("query_too_short"));
        }

        let session_id = state.session_id.clone().unwrap_or_default();
        let topk = self.cfg.topk.max(0) as usize;
        let mode = ctx.exec_mode();

        // ── 1. Grounding semantico via nexus_search_semantic (:98-124) ────────
        // I/O dietro la porta `ToolExecutor`. Best-effort: errore -> block vuoto.
        let grounding_block = {
            let call = ToolUse {
                id: Uuid::new_v4().to_string(),
                name: "nexus_search_semantic".to_string(),
                input: json!({
                    "query": query,
                    "source_kinds": ["code", "kb", "chat_history"],
                    "top_k": self.cfg.topk,
                }),
            };
            match ctx.tools.execute(call, mode).await {
                Ok(outcome) => {
                    let raw = Self::outcome_result_json(&outcome.content);
                    Self::render_grounding(&raw, topk)
                }
                Err(err) => {
                    tracing::debug!(
                        target: "nexus_agent_graph::understanding",
                        error = %err,
                        session_id = %session_id,
                        "grounding semantico fallito (best-effort, degrado a no-op)"
                    );
                    String::new()
                }
            }
        };

        // ── 2. Fan-out explore opzionale via dispatch_subagent (:126-165) ─────
        // I/O parallelo dietro la porta. Best-effort. Le sotto-query sono
        // deterministiche (euristica leggera, no LLM).
        let explore_block = if self.cfg.fanout_enabled && self.cfg.subagents_enabled {
            let max_explore = self.cfg.max_explore.max(0) as usize;
            let subqueries = Self::explore_subqueries(&query, max_explore);
            // Esecuzione concorrente (replica `asyncio.gather`); gli errori per
            // singolo sub-agent sono scartati (return_exceptions=True nel Python).
            let mut futs = Vec::with_capacity(subqueries.len());
            for sq in &subqueries {
                let call = ToolUse {
                    id: Uuid::new_v4().to_string(),
                    name: "dispatch_subagent".to_string(),
                    input: json!({ "kind": "explore", "task": sq }),
                };
                futs.push(ctx.tools.execute(call, mode));
            }
            let results = futures::future::join_all(futs).await;
            let summaries: Vec<String> = results
                .into_iter()
                .filter_map(|r| match r {
                    Ok(outcome) => Some(Self::outcome_result_json(&outcome.content)),
                    Err(err) => {
                        tracing::debug!(
                            target: "nexus_agent_graph::understanding",
                            error = %err,
                            "fan-out explore: un sub-agent fallito (scartato)"
                        );
                        None
                    }
                })
                .collect();
            Self::render_explore(&summaries)
        } else {
            String::new()
        };

        // ── Gate 5: nessun contesto trovato (:167-168) ────────────────────────
        if grounding_block.is_empty() && explore_block.is_empty() {
            return Ok(Self::skip("no_context_found"));
        }

        let raw_brief = Self::assemble_raw_brief(&grounding_block, &explore_block);

        // ── 3. Sintesi LLM economica (:172-197) — TODO I/O delegato ───────────
        // Il ramo sintesi richiede: (a) la risoluzione `purpose_model`
        //   ("understanding") -> provider/model RISOLTI A MONTE (regola G),
        //   tramite una porta dedicata o `ctx.cfg`; (b) il clamp difensivo
        //   `clamp_single_prompt` (punto unico Python, regola L), non ancora
        //   portato. Finche' non c'e' la porta che fornisce provider/model
        //   risolti, il nodo usa il `raw_brief` STRUTTURATO: e' esattamente il
        //   ramo del Python quando `understanding_synthesize_enabled` e' OFF (il
        //   DEFAULT DB), quindi nessun comportamento divergente, solo una feature
        //   opzionale non ancora attivabile dal lato Rust.
        if self.cfg.synthesize_enabled {
            tracing::warn!(
                target: "nexus_agent_graph::understanding",
                "sintesi LLM del context_brief non ancora portata (TODO) -> uso il brief strutturato"
            );
        }
        let context_brief = raw_brief;

        tracing::info!(
            target: "nexus_agent_graph::understanding",
            chars = context_brief.chars().count(),
            grounding = !grounding_block.is_empty(),
            explore = !explore_block.is_empty(),
            "context_brief prodotto"
        );

        Ok(StateDelta {
            understanding_active: Some(Some(true)),
            context_brief: Some(Some(context_brief)),
            ..Default::default()
        }
        .into_opaque())
    }
}

impl UnderstandingNode {
    /// Estrae la stringa `result_json` dal contenuto di un `ToolOutcome`.
    ///
    /// Replica `getattr(res, "result_json", None) or "{}"` del Python: il
    /// ToolRunner Nexus ritorna un oggetto con campo `result_json` (stringa JSON).
    /// Sul canale astratto `ToolOutcome.content` e' `Value`: se e' una stringa la
    /// usiamo tale e quale (e' gia' il `result_json`), se e' un oggetto con campo
    /// `result_json` lo estraiamo, altrimenti serializziamo l'oggetto (il parser
    /// a valle leggera' `hits`/`summary` direttamente). Fallback "{}" = nessun
    /// campo utile, mai un magic value mascherato (e' l'oggetto vuoto neutro).
    fn outcome_result_json(content: &Value) -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Object(map) => {
                if let Some(Value::String(rj)) = map.get("result_json") {
                    rj.clone()
                } else {
                    Value::Object(map.clone()).to_string()
                }
            }
            Value::Null => "{}".to_string(),
            other => other.to_string(),
        }
    }
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
    use crate::runtime::ports::{
        EventSink, ExecMode, PortError, SseEvent, ToolCall, ToolExecutor, ToolOutcome,
    };
    use crate::runtime::test_doubles::{NullEventSink, StubLlmGateway};
    use crate::runtime::AgentNodeCtx;
    use crate::state::{AgentState, Message, MessageContent, TaskComplexity};

    /// Applica il delta opaco prodotto dal nodo a uno stato e lo ritorna.
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

    /// Esecutore di tool programmabile per-nome: ritorna un `result_json`
    /// (stringa) diverso a seconda del nome del tool richiesto. Registra anche le
    /// modalita' usate (per verificare lo shadow -> Replay).
    struct ScriptedTools {
        /// JSON ritornato (come stringa) per `nexus_search_semantic`.
        grounding_json: String,
        /// JSON ritornato per ogni `dispatch_subagent`.
        explore_json: String,
        /// Modalita' osservate.
        modes: std::sync::Mutex<Vec<ExecMode>>,
        /// Nomi tool osservati.
        names: std::sync::Mutex<Vec<String>>,
    }

    impl ScriptedTools {
        fn new(grounding_json: &str, explore_json: &str) -> Self {
            Self {
                grounding_json: grounding_json.to_string(),
                explore_json: explore_json.to_string(),
                modes: std::sync::Mutex::new(vec![]),
                names: std::sync::Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl ToolExecutor for ScriptedTools {
        async fn execute(
            &self,
            call: ToolCall,
            mode: ExecMode,
        ) -> Result<ToolOutcome, PortError> {
            self.modes.lock().unwrap().push(mode);
            self.names.lock().unwrap().push(call.name.clone());
            let rj = match call.name.as_str() {
                "nexus_search_semantic" => self.grounding_json.clone(),
                "dispatch_subagent" => self.explore_json.clone(),
                other => panic!("tool inatteso: {other}"),
            };
            Ok(ToolOutcome {
                tool_call_id: call.id,
                // Il contenuto e' la stringa `result_json` (forma reale del tool).
                content: Value::String(rj),
                is_error: false,
                ..Default::default()
            })
        }
    }

    /// Esecutore che fallisce SEMPRE (per il path best-effort degradato).
    struct FailingTools;

    #[async_trait]
    impl ToolExecutor for FailingTools {
        async fn execute(
            &self,
            _call: ToolCall,
            _mode: ExecMode,
        ) -> Result<ToolOutcome, PortError> {
            Err(PortError::Tool("simulato".to_string()))
        }
    }

    /// Sink eventi no-op riusabile (lo shadow non emette).
    struct Sink;
    impl EventSink for Sink {
        fn emit(&self, _ev: SseEvent) {}
    }

    /// Costruisce un ctx di test con tool programmabili; il `PgPool` e' lazy
    /// (`connect_lazy` non apre connessioni: il nodo non interroga il DB).
    fn ctx_with_tools(tools: Arc<dyn ToolExecutor>, shadow: bool) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("irrilevante")),
            tools,
            emit: Arc::new(NullEventSink),
            cfg: crate::routing::config::RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            shadow,
        }
    }

    /// Config "tutto attivo" (enabled + fanout) con budget basso per i test.
    fn cfg_active() -> UnderstandingConfig {
        UnderstandingConfig {
            enabled: true,
            fanout_enabled: true,
            synthesize_enabled: false,
            topk: 8,
            min_token_budget: 3000,
            max_explore: 3,
            subagents_enabled: true,
        }
    }

    /// Stato complesso "tipo" con query lunga e budget sufficiente.
    fn complex_state() -> AgentState {
        AgentState {
            messages: vec![human("Rifattorizza il modulo di autenticazione del progetto")],
            task_complexity: Some(TaskComplexity::High),
            token_budget: Some(5000),
            session_id: Some("sess-test".to_string()),
            ..Default::default()
        }
    }

    /// Gate 0: flag OFF -> pass-through TOTALE (delta vuoto, nessun campo).
    #[tokio::test]
    async fn flag_off_pass_through_totale() {
        let node = UnderstandingNode::new(UnderstandingConfig::default()); // enabled=false
        let ctx = ctx_with_tools(Arc::new(FailingTools), false);
        let delta = node.run(&complex_state(), &ctx).await.expect("run ok");
        assert_eq!(delta.as_map().len(), 0, "flag OFF: delta vuoto, zero chiavi");
        // Lo Sink esiste solo per evitare warning di import non usato.
        let _ = Sink;
    }

    /// Gate 1: dentro un sub-agent (depth>=1) -> skip_in_subagent.
    #[tokio::test]
    async fn gate_skip_in_subagent() {
        let node = UnderstandingNode::new(cfg_active());
        let ctx = ctx_with_tools(Arc::new(FailingTools), false);
        let mut state = complex_state();
        state.subagent_depth = Some(1);
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        assert_eq!(out.understanding_active, Some(false));
        assert_eq!(
            out.understanding_skip_reason.as_deref(),
            Some("skip_in_subagent")
        );
    }

    /// Gate 2: token budget sotto la soglia -> budget_too_low.
    #[tokio::test]
    async fn gate_budget_too_low() {
        let node = UnderstandingNode::new(cfg_active());
        let ctx = ctx_with_tools(Arc::new(FailingTools), false);
        let mut state = complex_state();
        state.token_budget = Some(100); // < 3000
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        assert_eq!(
            out.understanding_skip_reason.as_deref(),
            Some("budget_too_low")
        );
    }

    /// Gate 3: task non complesso -> not_complex.
    #[tokio::test]
    async fn gate_not_complex() {
        let node = UnderstandingNode::new(cfg_active());
        let ctx = ctx_with_tools(Arc::new(FailingTools), false);
        let mut state = complex_state();
        state.task_complexity = Some(TaskComplexity::Low);
        state.is_ambiguous = Some(false);
        state.agentic_score = Some(0.3);
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        assert_eq!(out.understanding_skip_reason.as_deref(), Some("not_complex"));
    }

    /// `is_complex` per i tre segnali, incluso agentic_score >= soglia.
    #[test]
    fn is_complex_segnali() {
        let mut s = AgentState::default();
        assert!(!UnderstandingNode::is_complex(&s));
        s.agentic_score = Some(0.7);
        assert!(UnderstandingNode::is_complex(&s), "score >= 0.7 -> complesso");
        s.agentic_score = Some(0.69);
        assert!(!UnderstandingNode::is_complex(&s));
        s.is_ambiguous = Some(true);
        assert!(UnderstandingNode::is_complex(&s), "ambiguo -> complesso");
    }

    /// Gate 4: query troppo corta -> query_too_short.
    #[tokio::test]
    async fn gate_query_too_short() {
        let node = UnderstandingNode::new(cfg_active());
        let ctx = ctx_with_tools(Arc::new(FailingTools), false);
        let mut state = complex_state();
        state.messages = vec![human("corto")]; // 5 char < 10
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        assert_eq!(
            out.understanding_skip_reason.as_deref(),
            Some("query_too_short")
        );
    }

    /// Gate 5: nessun contesto trovato (tool ritornano hit/summary vuoti) ->
    /// no_context_found.
    #[tokio::test]
    async fn gate_no_context_found() {
        let node = UnderstandingNode::new(cfg_active());
        // grounding: nessuna hit; explore: nessun summary.
        let tools = ScriptedTools::new(r#"{"hits": []}"#, r#"{"summary": ""}"#);
        let ctx = ctx_with_tools(Arc::new(tools), false);
        let state = complex_state();
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        assert_eq!(
            out.understanding_skip_reason.as_deref(),
            Some("no_context_found")
        );
    }

    /// Happy path: grounding + explore producono il context_brief strutturato.
    #[tokio::test]
    async fn happy_path_context_brief() {
        let node = UnderstandingNode::new(cfg_active());
        let grounding = r#"{"hits": [
            {"source_kind": "code", "chunk_text": "fn login() {}", "score": 0.91},
            {"source_kind": "kb", "chunk_text": "doc autenticazione", "score": 0.42}
        ]}"#;
        let explore = r#"{"summary": "Il modulo auth usa JWT e bcrypt."}"#;
        let tools = ScriptedTools::new(grounding, explore);
        let ctx = ctx_with_tools(Arc::new(tools), false);
        let state = complex_state();
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());

        assert_eq!(out.understanding_active, Some(true));
        let brief = out.context_brief.expect("context_brief presente");
        assert!(brief.contains("<grounding>"), "brief: {brief}");
        assert!(brief.contains("<hit source=\"code\" score=\"0.91\">fn login() {}</hit>"));
        assert!(brief.contains("<esplorazioni>"));
        assert!(brief.contains("<explore>Il modulo auth usa JWT e bcrypt.</explore>"));
        // Il separatore tra i due block e' la doppia riga vuota.
        assert!(brief.contains("</grounding>\n\n<esplorazioni>"));
    }

    /// Solo grounding (fanout OFF): il brief contiene solo `<grounding>`.
    #[tokio::test]
    async fn solo_grounding_quando_fanout_off() {
        let mut cfg = cfg_active();
        cfg.fanout_enabled = false;
        let node = UnderstandingNode::new(cfg);
        let grounding = r#"{"hits": [{"source_kind": "code", "chunk_text": "x", "score": 0.5}]}"#;
        let tools = ScriptedTools::new(grounding, r#"{"summary": "non usato"}"#);
        let ctx = ctx_with_tools(Arc::new(tools), false);
        let state = complex_state();
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        let brief = out.context_brief.expect("brief");
        assert!(brief.contains("<grounding>"));
        assert!(!brief.contains("<esplorazioni>"), "fanout OFF -> niente explore");
    }

    /// Best-effort: se il grounding fallisce ma l'explore produce contenuto, il
    /// nodo NON aborta e produce comunque il brief (degrado parziale).
    #[tokio::test]
    async fn grounding_fallito_explore_ok() {
        // Tool che fallisce sul primo (grounding) ma non vogliamo far fallire
        // anche l'explore: usiamo un esecutore che fallisce solo nexus_search.
        struct PartialFail;
        #[async_trait]
        impl ToolExecutor for PartialFail {
            async fn execute(
                &self,
                call: ToolCall,
                _mode: ExecMode,
            ) -> Result<ToolOutcome, PortError> {
                if call.name == "nexus_search_semantic" {
                    return Err(PortError::Tool("grounding giu".to_string()));
                }
                Ok(ToolOutcome {
                    tool_call_id: call.id,
                    content: Value::String(r#"{"summary": "trovato qualcosa"}"#.to_string()),
                    is_error: false,
                    ..Default::default()
                })
            }
        }
        let node = UnderstandingNode::new(cfg_active());
        let ctx = ctx_with_tools(Arc::new(PartialFail), false);
        let state = complex_state();
        let out = apply(state.clone(), node.run(&state, &ctx).await.unwrap());
        assert_eq!(out.understanding_active, Some(true));
        let brief = out.context_brief.expect("brief");
        assert!(!brief.contains("<grounding>"), "grounding fallito -> assente");
        assert!(brief.contains("<explore>trovato qualcosa</explore>"));
    }

    /// Shadow: i tool sono eseguiti in modalita' Replay (zero side-effect).
    #[tokio::test]
    async fn shadow_usa_replay() {
        let node = UnderstandingNode::new(cfg_active());
        let tools = Arc::new(ScriptedTools::new(
            r#"{"hits": [{"source_kind": "code", "chunk_text": "x", "score": 0.5}]}"#,
            r#"{"summary": "s"}"#,
        ));
        let ctx = ctx_with_tools(tools.clone(), true);
        let state = complex_state();
        let _ = node.run(&state, &ctx).await.unwrap();
        let modes = tools.modes.lock().unwrap();
        assert!(!modes.is_empty(), "almeno una chiamata tool");
        assert!(
            modes.iter().all(|m| *m == ExecMode::Replay),
            "shadow -> tutte le chiamate in Replay"
        );
    }

    /// Rendering grounding: troncamento a 300 char e scarto delle hit vuote.
    #[test]
    fn render_grounding_tronca_e_scarta() {
        let lungo = "a".repeat(500);
        let raw = format!(
            r#"{{"hits": [
                {{"source_kind": "code", "chunk_text": "{lungo}", "score": 0.5}},
                {{"source_kind": "kb", "chunk_text": "   ", "score": 0.9}}
            ]}}"#
        );
        let out = UnderstandingNode::render_grounding(&raw, 8);
        // La hit a soli spazi (trim -> vuota) e' scartata: una sola <hit>.
        assert_eq!(out.matches("<hit").count(), 1);
        // Il testo lungo e' troncato a 300 char.
        let inner = "a".repeat(300);
        assert!(out.contains(&inner));
        assert!(!out.contains(&"a".repeat(301)));
    }

    /// Sotto-query del fan-out clampate a max_explore.
    #[test]
    fn explore_subqueries_clamp() {
        let q = "ridisegna il login";
        assert_eq!(UnderstandingNode::explore_subqueries(q, 3).len(), 3);
        assert_eq!(UnderstandingNode::explore_subqueries(q, 1).len(), 1);
        assert_eq!(UnderstandingNode::explore_subqueries(q, 0).len(), 0);
        assert!(UnderstandingNode::explore_subqueries(q, 2)[0].contains(q));
    }
}

#[cfg(test)]
mod golden {
    //! Golden-test di PARITA' 1:1 vs Python sulla logica DETERMINISTICA del
    //! nodo. Lo script `/tmp/gen_golden_understanding.py` importa/replica le
    //! funzioni del brain, le esercita e salva `{case_id, function, input,
    //! output}` in `/tmp/golden_understanding.json`. Qui ricostruiamo l'input,
    //! chiamiamo la funzione Rust corrispondente e verifichiamo
    //! `output == golden Python`.
    //!
    //! `#[ignore]` perche' dipende dal file generato. Comando:
    //!   python3 /tmp/gen_golden_understanding.py
    //!   cargo test -p nexus-agent-graph golden_understanding_parita -- --ignored

    use serde::Deserialize;
    use serde_json::Value;

    use super::UnderstandingNode;
    use crate::state::{AgentState, ContentBlock, Message, MessageContent, TaskComplexity};

    /// Un caso golden: nome funzione + input (JSON) + output atteso (JSON).
    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    /// Ricostruisce un `AgentState` per `is_complex` dal dict `state` del golden.
    fn state_from_json(v: &Value) -> AgentState {
        let mut s = AgentState::default();
        if let Some(tc) = v.get("task_complexity").and_then(Value::as_str) {
            // Python confronta `.lower() == "high"`: solo "high" (case-insens.)
            // attiva il ramo; gli altri valori (incl. "low") non lo attivano.
            s.task_complexity = match tc.to_lowercase().as_str() {
                "high" => Some(TaskComplexity::High),
                "low" => Some(TaskComplexity::Low),
                "medium" => Some(TaskComplexity::Medium),
                _ => None,
            };
        }
        if let Some(b) = v.get("is_ambiguous").and_then(Value::as_bool) {
            s.is_ambiguous = Some(b);
        }
        if let Some(score) = v.get("agentic_score") {
            s.agentic_score = score.as_f64();
        }
        s
    }

    /// Ricostruisce i `messages` per `last_user_message`: una stringa -> blocco
    /// testo; qualunque altro tipo JSON (es. una lista) -> content NON testuale
    /// (`Blocks` senza blocchi `Text`), che `last_user_message` salta come il
    /// Python salta i content non-stringa.
    fn messages_from_json(v: &Value) -> Vec<Message> {
        v.as_array()
            .map(|arr| {
                arr.iter()
                    .map(|c| {
                        let content = match c {
                            Value::String(s) => MessageContent::Text(s.clone()),
                            // Non-stringa -> nessun blocco Text (flatten -> "").
                            _ => MessageContent::Blocks(Vec::<ContentBlock>::new()),
                        };
                        Message::Human { content }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    #[ignore = "richiede /tmp/golden_understanding.json generato da gen_golden_understanding.py"]
    fn golden_understanding_parita() {
        let Some(raw) = crate::golden_util::load_golden(
            "golden_understanding.json",
            "gen_golden_understanding.py",
        ) else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(!cases.is_empty(), "golden vuoto");

        let mut checked = 0usize;
        for c in &cases {
            let got: Value = match c.function.as_str() {
                "is_complex" => {
                    let st = state_from_json(c.input.get("state").expect("state"));
                    Value::Bool(UnderstandingNode::is_complex(&st))
                }
                "last_user_message" => {
                    let msgs = messages_from_json(c.input.get("messages").expect("messages"));
                    Value::String(UnderstandingNode::last_user_message(&msgs))
                }
                "render_grounding" => {
                    let rj = c.input.get("result_json").and_then(Value::as_str).unwrap_or("");
                    let topk = c.input.get("topk").and_then(Value::as_i64).unwrap_or(0) as usize;
                    Value::String(UnderstandingNode::render_grounding(rj, topk))
                }
                "explore_subqueries" => {
                    let q = c.input.get("query").and_then(Value::as_str).unwrap_or("");
                    let me = c.input.get("max_explore").and_then(Value::as_i64).unwrap_or(0) as usize;
                    serde_json::to_value(UnderstandingNode::explore_subqueries(q, me))
                        .expect("serialize subqueries")
                }
                "render_explore" => {
                    let sums: Vec<String> = c
                        .input
                        .get("summaries_json")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    Value::String(UnderstandingNode::render_explore(&sums))
                }
                "assemble_raw_brief" => {
                    let g = c.input.get("grounding").and_then(Value::as_str).unwrap_or("");
                    let e = c.input.get("explore").and_then(Value::as_str).unwrap_or("");
                    Value::String(UnderstandingNode::assemble_raw_brief(g, e))
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
        println!("golden understanding: {checked} casi verificati, tutti verdi");
    }
}
