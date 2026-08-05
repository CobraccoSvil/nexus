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
//! - `decisions::predictive_cap`: [`predictive_cap_check`] + [`is_cap_exempt`].
//!   Il guard "blocked-da-cap" NON riconosce piu' il proprio messaggio: il gate
//!   che rifiuta la chiamata lo DICHIARA in [`ToolResultBlock::motivo_blocco`].
//! - `decisions::m16`: [`build_m16_allowed`]/[`is_tool_allowed`]/
//!   [`parse_discovered_tools`]/[`merge_discovered_run`] (il parser usa gia' il
//!   fix ensure_ascii, PR-G).
//! - `decisions::tool_dispatch`: [`apply_run_notes`]/[`normalize_declared_outcome`]/
//!   [`estimate_tool_result_size_bytes`]/[`extract_returned_bytes`]/
//!   [`estimate_context_chars`]/[`current_context_token_estimate`]/
//!   [`append_reminder_block`] + le costanti di cap.
//! - Trait `runtime::ports`: [`ToolExecutor`] (esecuzione),
//!   [`AgentStepStore`] (persist step), [`RunControlStore`] (superseded +
//!   heartbeat), [`TodoStore`] (reminder), [`ContextOffload`] (offload RAG). Sono
//!   CAMPI del nodo (coerente con `FinalGateNode`/`TodoRunnerNode`).
//!
//! ## Ordine dei gate (1:1 col Python, load-bearing)
//!
//! 1. `ctx.cancel` / `RunControlStore::is_superseded` -> early return
//!    `stop_reason=superseded` (uscita cooperativa, mig 0370).
//! 2. `pending_tool_uses` vuoto -> `{pending_tool_uses:[], stop_reason:end_turn}`.
//! 2b. HITL Conferma: pending mutativi + `automation_mode=confirm` + `!approved`
//!     -> `awaiting_confirmation=true` + pending_actions in extra, NESSUN tool
//!     eseguito (interrupt-resume prima dell'executor, parita' graph.py).
//! 3. per ogni pending, NELL'ORDINE: predictive_cap_check (priorita') ->
//!    SYNTHETIC-blocked (NON eseguito); M16 `is_tool_allowed` -> SYNTHETIC error
//!    (forza nexus_mcp_tool_search); budget allegati -> SYNTHETIC error;
//!    altrimenti KEPT. Ogni synthetic porta il gate che l'ha prodotto in
//!    [`ToolResultBlock::motivo_blocco`].
//! 4. esecuzione: `join_all` dei KEPT via `ToolExecutor::execute`. Il nodo
//!    PRESERVA l'ordine ORIGINALE dei pending nella ricomposizione (allineamento
//!    per POSIZIONE, non per id): load-bearing.
//! 5. exit_code: fluisce da `ToolOutcome::exit_code` al `ContentBlock::ToolResult`
//!    INVARIATO (segnale anti-stallo).
//! 6. aggiorna `attachment_read_bytes` dai tool_result attachment.
//! 7. guard "blocked-da-cap": se task_complete outcome=blocked + una chiamata
//!    del turno rifiutata dal cap (campo, non testo) -> rifiuta la
//!    dichiarazione UNA volta.
//! 8. persist step via `AgentStepStore::persist_step`.
//! 9. context-budget cap: se `ctx_chars + new_chars > max_context_chars`, tronca
//!    ogni tool_result a `budget_per_tool` con offload best-effort
//!    (`ContextOffload`), degrado a troncamento testa+coda.
//! 10. reminder TODO via `TodoStore::build_reminder_text` + `append_reminder_block`
//!     (se `plan_phase_active` e soglia raggiunta).
//! 11. `_dispatch_updates`: `discovered_tools_next_turn` SEMPRE scritto, ANCHE `[]`
//!     (distinzione `None`=no-op vs `Some(vec![])`=azzera, load-bearing) +
//!     `merge_discovered_run` per `discovered_tools_run`. Heartbeat best-effort.
//!
//! ## TURN_FOCUS
//!
//! Il `tool_dispatch_node` NON tocca `turn_focus` (lo inietta l'executor nel
//! prompt): nessuna replica qui.
//!
//! Va pero' saputo che il messaggio prodotto da [`build_tool_message`] ha ruolo
//! `user` sul canale interno pur non venendo dall'utente: porta i tool_result e,
//! in coda, i promemoria `<system-reminder>` della barriera advisory e dei todo.
//! Finche' il focus del turno lo leggeva come "ultimo messaggio dell'utente",
//! dal secondo turno in poi dichiarava al modello che la richiesta ADESSO era un
//! promemoria di sistema. La richiesta si legge da `decisions::turn_task`, non
//! da qui: chi aggiunge un consumatore della cronologia se lo ricordi.
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

use crate::decisions::hitl::{
    build_pending_actions_json, pending_contains_mutator, should_suspend_for_hitl,
    HITL_PENDING_ACTIONS_EXTRA_KEY,
};
use crate::decisions::m16::DiscoveredTool;
use crate::decisions::predictive_cap::{is_cap_exempt, predictive_cap_check};
use crate::decisions::tool_dispatch::{
    append_reminder_block, apply_run_notes, current_context_token_estimate, estimate_context_chars,
    estimate_tool_result_size_bytes, extract_returned_bytes, normalize_advisory_verdict,
    normalize_debate_position, normalize_declared_outcome, normalize_review_verdict,
    ContextMessage, DeclarationRejected,
};
use crate::decisions::{build_m16_allowed, is_tool_allowed, merge_discovered_run, M16_META_TOOLS};
use crate::py_json::{py_json_dumps, SortKeys};
use crate::runtime::ports::{
    self as ports, AgentStepStore, ContextOffload, MetaStepStore, OffloadKind, PersistedStep,
    RunControlStore, SseEvent, StepStatus, TodoStore, ToolCall, ToolExecutor,
};
use crate::runtime::AgentNodeCtx;
use crate::state::{AgentState, Message, MessageContent, MetaStep, StateDelta, StopReason};

/// Tool brain-only `task_complete` (`TASK_COMPLETE_TOOL_NAME`, helpers.py:413):
/// non eseguito via ToolExecutor, registra l'esito dichiarato.
const TASK_COMPLETE_TOOL_NAME: &str = "task_complete";

/// Cap di default (byte) dello schema di un tool scoperto via M16: oltre, lo
/// schema si scarta (safe-default identico al brain; il valore vero e' il
/// setting `agent.tools.discovery_schema_max_bytes`).
const DEFAULT_DISCOVERY_SCHEMA_MAX_BYTES: usize = 8192;

/// Attesa di default (s) della barriera advisory = il timeout tipico di un
/// panel a monte (mig 0546: 240-300s per figura). Vale solo se il DB tace; il
/// valore vero e' `orchestrator.advisory_gate_timeout_s` (mig 0606).
const DEFAULT_ADVISORY_GATE_TIMEOUT_S: u64 = 300;

/// Tool brain-only `nexus_run_notes` (`RUN_NOTES_TOOL_NAME`, helpers.py:450):
/// aggiorna il taccuino del run nello stato, non eseguito via ToolExecutor.
const RUN_NOTES_TOOL_NAME: &str = "nexus_run_notes";

/// Tool brain-only `review_verdict` (Fase B ultracode): il REVISORE dichiara il
/// verdetto strutturato della review (gemello di `task_complete` per il canale
/// del giudice). Non eseguito via ToolExecutor; registrato nello stato e
/// propagato oltre il confine sub-run via `structured_verdict` (regola M).
const REVIEW_VERDICT_TOOL_NAME: &str = "review_verdict";

/// Tool brain-only `advisory_verdict` (consiglio di figure a monte): una FIGURA
/// di analisi dichiara il parere strutturato con la sua lente (gemello di
/// `review_verdict` per il canale del consiglio). Non eseguito via ToolExecutor;
/// registrato nello stato e propagato oltre il confine sub-run via
/// `structured_verdict` (campo `advisory`, regola M).
const ADVISORY_VERDICT_TOOL_NAME: &str = "advisory_verdict";

/// Tool brain-only `debate_position` (tesi contrapposte): un AVVOCATO dichiara
/// la posizione strutturata sulla tesi che gli e' stata assegnata (gemello di
/// `advisory_verdict` per il canale del dibattito). Non eseguito via
/// ToolExecutor; registrato nello stato e propagato oltre il confine sub-run via
/// `structured_verdict` (campo `debate`, regola M).
const DEBATE_POSITION_TOOL_NAME: &str = "debate_position";

/// Tool di dispatch sub-agente (singolo/batch). Il loro tool_result puo' portare
/// il SEGNALE STRUTTURATO `background_dispatched: true` (Fase D fan-in): il padre
/// NON attende l'esito, si sospende e riprende al fan-in dei sub-run background.
const DISPATCH_SUBAGENT_TOOLS: &[&str] = &["dispatch_subagent", "dispatch_subagents"];

/// Chiave extra nello stato: enforcement deterministico dei verdetti aggregati
/// prodotti da `dispatch_subagent(s)` (`panel_verdict` / `advisory_synthesis`).
pub const PANEL_ENFORCEMENT_KEY: &str = "agentic_panel_enforcement";

/// Chiave extra nello stato: sintesi advisory strutturata prodotta PRIMA del run
/// (panel multi-provider o consiglio a monte). Il coordinatore legge questo
/// segnale (regola M), non il blocco testuale in `initial_msg`.
pub const PRE_RUN_ADVISORY_SYNTHESIS_KEY: &str = "pre_run_advisory_synthesis";

/// Chiave extra nello stato: esito della BARRIERA DI SCRITTURA advisory
/// (overlap consiglio ∥ run, mig 0606). Segnale strutturato (regola M) con la
/// forma `{status, reason_code?}`: la UI e il resoconto sanno DA QUI se il run
/// ha atteso il consiglio, se e' partito perche' il panel non ha risposto, o se
/// e' stato fermato dal veto — senza dedurlo dalla prosa.
pub const ADVISORY_GATE_KEY: &str = "advisory_gate";

/// Nota iniettata quando la barriera si scioglie SENZA un verdetto utilizzabile
/// (panel morto o timeout): il modello deve sapere che sta procedendo senza
/// approvazione, non credere di averla ricevuta (regola M — un'assenza di
/// verdetto non e' un verdetto favorevole).
const GATE_UNAVAILABLE_NOTE: &str = "Il consiglio di analisi NON ha prodotto un parere \
utilizzabile in tempo per questa modifica: procedi pure, ma sappi che NON hai \
un'approvazione — nessuno ha validato l'approccio. Sii conservativo: preferisci la \
modifica minima e reversibile, e dichiara nel resoconto che il parere e' mancato.";

/// Esito della barriera per il chiamante.
enum AdvisoryBarrier {
    /// Nessuna barriera applicabile: il turno prosegue invariato (bit-identico).
    Inert,
    /// Barriera sciolta: il turno prosegue con `extra` aggiornato (esito del gate
    /// + eventuale enforcement) e un promemoria da mettere davanti al modello
    /// PRIMA che scriva.
    Proceed {
        extra: serde_json::Map<String, Value>,
        reminder: Option<String>,
    },
    /// Veto: il turno si chiude qui.
    Veto(OpaqueDelta),
}

/// Promemoria dei vincoli del consiglio, reso dai campi STRUTTURATI
/// dell'enforcement (regola M). Il modello sta per SCRIVERE: i requisiti gli
/// servono ora, non a fine run.
fn render_gate_requirements(enforcement: &Value) -> String {
    let verdict = enforcement
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let summary = enforcement
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!(
        "Il consiglio di analisi ha deliberato mentre lavoravi (verdetto: {verdict}). \
         Prima di modificare il codice, incorpora i suoi vincoli: {summary}"
    )
}

/// Stato della barriera di scrittura advisory, osservato dal ToolDispatchNode
/// via `watch::Receiver` (vedi [`crate::runtime::ctx::AgentNodeCtx::advisory_gate`]).
///
/// Perche' un `watch` e non un `mpsc`: e' uno STATO CORRENTE, non una coda di
/// eventi. Il nodo puo' osservarlo a ogni iterazione (o mai, se il run non
/// scrive nulla) e i late-joiner leggono l'ultimo valore senza consumarlo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisoryGateState {
    /// I panel a monte stanno ancora deliberando: la ricognizione read-only
    /// procede, la prima SCRITTURA attende.
    Pending,
    /// Il consiglio ha deliberato e non veta: si puo' scrivere. `enforcement`
    /// porta i requisiti da iniettare come promemoria prima della modifica
    /// (`None` = via libera piena, nessun vincolo da ricordare).
    Released { enforcement: Option<Value> },
    /// Veto del consiglio: il run va fermato PRIMA della prima modifica.
    /// `enforcement` e' gia' nella forma di `PANEL_ENFORCEMENT_KEY` (terminal),
    /// cosi' l'edge esistente `terminal_panel_veto` lo instrada al Learner senza
    /// routing nuovo.
    Vetoed { enforcement: Value },
    /// Il panel non e' arrivato a un esito utilizzabile (roster morto, errore di
    /// convocazione, kill-switch): il run PROSEGUE, ma il motivo e' DICHIARATO
    /// (regola M) e va detto al modello — inconclusive non e' un'approvazione.
    Unavailable { reason_code: String },
}

/// Chiave del segnale strutturato (regola M) emesso da `dispatch_subagent(s)` in
/// modalita' background: `true` -> il padre si sospende in attesa del fan-in.
const BACKGROUND_DISPATCHED_KEY: &str = "background_dispatched";

/// Tool di lettura allegato soggetti al budget cumulativo della sessione
/// (`_ATTACHMENT_READ_TOOLS`, helpers.py:3647).
const ATTACHMENT_READ_TOOLS: &[&str] = &["nexus_read_attachment", "nexus_read_archive_entry"];

/// Tool brain-only sempre ammessi da M16 oltre la whitelist DB (Python:
/// `{TASK_COMPLETE_TOOL_NAME, RUN_NOTES_TOOL_NAME}`; `review_verdict` aggiunto
/// in Fase B: e' nel catalogo SOLO per i kind che lo whitelistano, ma quando
/// c'e' M16 non deve mai filtrarlo — e' il canale di chiusura del revisore).
const M16_BRAIN_TOOLS: &[&str] = &[
    TASK_COMPLETE_TOOL_NAME,
    RUN_NOTES_TOOL_NAME,
    REVIEW_VERDICT_TOOL_NAME,
    ADVISORY_VERDICT_TOOL_NAME,
    DEBATE_POSITION_TOOL_NAME,
];

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
    /// Tool mutativi (setting `agent.tools.result_cache_mutators`): usati dal
    /// gate HITL in modalita' Conferma e dalla BARRIERA DI SCRITTURA advisory
    /// (punto unico `decisions::hitl`, stessa domanda: "questo tool muta lo
    /// stato?").
    pub fs_mutator_tools: Vec<String>,
    /// Attesa massima (secondi) della barriera di scrittura advisory prima di
    /// procedere senza il verdetto del consiglio
    /// (`orchestrator.advisory_gate_timeout_s`, mig 0606). Il chiamante lo
    /// clampa alla deadline residua del run: una barriera che attende oltre la
    /// deadline produrrebbe un `time_budget` mascherato da gate.
    pub advisory_gate_timeout_s: u64,
    /// Mode del gate duale sui passi critici
    /// (`orchestrator.critical_step_gate_mode`, mig 0677). `Off` = passo 2a
    /// inerte, dispatch bit-identico.
    pub step_gate_mode: crate::decisions::step_gate::StepGateMode,
    /// Regole di criticita' gia' PARSE a monte
    /// (`orchestrator.critical_step_rules`, JSON in settings; le voci rotte
    /// sono gia' state scartate una a una con WARN da `parse_rules`).
    pub step_gate_rules: Vec<crate::decisions::step_gate::CriticalityRule>,
    /// Rimandi massimi del gate duale prima di degradare a NeedsHuman
    /// (`orchestrator.critical_step_max_rejections`): il cap anti ping-pong
    /// fra modello e validatori.
    pub step_gate_max_rejections: u32,
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
            discovery_schema_max_bytes: DEFAULT_DISCOVERY_SCHEMA_MAX_BYTES,
            todo_reminder_every_n_steps: 5,
            max_context_chars: crate::decisions::tool_dispatch::MAX_CONTEXT_CHARS,
            fs_mutator_tools: crate::routing::RoutingConfig::default()
                .fs_mutator_tools
                .clone(),
            advisory_gate_timeout_s: DEFAULT_ADVISORY_GATE_TIMEOUT_S,
            // Gate duale spento di default: si accende SOLO dal DB (mig 0677).
            step_gate_mode: crate::decisions::step_gate::StepGateMode::Off,
            step_gate_rules: Vec::new(),
            step_gate_max_rejections: 2,
        }
    }
}

/// Quale GATE ha rifiutato una chiamata prima di eseguirla. Vocabolario chiuso
/// (regola Q): il fatto lo conosce chi rifiuta, e lo DICHIARA qui invece di
/// affidarlo a una sottostringa del messaggio destinato al modello.
///
/// Il guard blocked-da-cap decideva cercando [`PREDICTIVE_CAP_SENTINEL`] dentro
/// il testo di un qualunque tool_result del turno: un `read_file` su questo
/// sorgente, un grep, il resoconto di un sub-run che cita il cap bastavano ad
/// ANNULLARE una dichiarazione `blocked` legittima del modello — cioe' a
/// rimettere in moto un run che si era fermato per una ragione vera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MotivoBlocco {
    /// Predictive context cap: la proiezione del risultato sforava il budget
    /// (blocco della SINGOLA chiamata, mai del task).
    PredictiveCap,
    /// M16: tool non in whitelist e non ancora scoperto in questo run.
    ToolNonScoperto,
    /// Budget di letture allegati della sessione esaurito.
    BudgetAllegati,
    /// La porta ha sollevato un errore invece di rispondere (guasto infra o
    /// eccezione applicativa): la chiamata e' partita, non e' stata rifiutata.
    PortaInErrore,
    /// task_complete ripetuto nello stesso turno (mig 0676, GAP-9): la
    /// dichiarazione e' terminale, la seconda e' un'anomalia dichiarata.
    DichiarazioneRipetuta,
    /// Gate duale sui passi critici (mig 0677): almeno un validatore ha
    /// rifiutato il batch. Il tool_result porta i motivi e l'eventuale
    /// alternativa piu' sicura: e' un rimando al modello, mai una chiusura.
    StepGateRejected,
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
    /// Il gate che ha rifiutato la chiamata, quando non e' stata eseguita.
    /// `None` = il tool ha girato (o e' brain-only). E' un campo INTERNO al
    /// turno: nasce qui e muore qui, quindi non ha bisogno di attraversare il
    /// `ContentBlock::ToolResult` ne' la persistenza — il suo unico consumatore,
    /// [`should_reject_blocked_from_cap`], guarda i risultati di QUESTO turno.
    motivo_blocco: Option<MotivoBlocco>,
}

#[derive(Debug, Clone)]
struct PanelEnforcement {
    source: &'static str,
    verdict: String,
    terminal: bool,
    declared_outcome: Option<Value>,
    summary: String,
    payload: Value,
}

impl PanelEnforcement {
    fn to_value(&self) -> Value {
        json!({
            "source": self.source,
            "verdict": self.verdict,
            "terminal": self.terminal,
            "summary": self.summary,
            "payload": self.payload,
            "declared_outcome": self.declared_outcome,
        })
    }

    fn prompt_block(&self) -> String {
        let payload = py_dumps(&self.payload);
        format!(
            "<panel_enforcement>\nsource={}\nverdict={}\nterminal={}\nsummary={}\npayload={}\n</panel_enforcement>",
            self.source, self.verdict, self.terminal, self.summary, payload
        )
    }
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

    /// BARRIERA DI SCRITTURA advisory (overlap consiglio ∥ run, mig 0606).
    ///
    /// Inerte (zero costo) quando: non c'e' canale (ramo legacy: il run e'
    /// partito DOPO i panel), la barriera si e' gia' sciolta in questo run,
    /// oppure il batch non contiene tool mutativi — la ricognizione read-only
    /// non aspetta nessuno, ed e' proprio questo che rende l'overlap utile.
    async fn advisory_barrier(
        &self,
        state: &AgentState,
        pending: &[Value],
        ctx: &AgentNodeCtx,
    ) -> AdvisoryBarrier {
        let Some(mut rx) = ctx.advisory_gate.clone() else {
            return AdvisoryBarrier::Inert;
        };
        // La barriera scatta al massimo UNA volta per run: dopo lo scioglimento
        // lo stato porta l'esito e non si attende piu'.
        if state.extra.contains_key(ADVISORY_GATE_KEY) {
            return AdvisoryBarrier::Inert;
        }
        // Punto unico "tool mutativo" (regola L): lo STESSO del gate HITL.
        if !pending_contains_mutator(pending, &self.cfg.fs_mutator_tools) {
            return AdvisoryBarrier::Inert;
        }
        let resolved = self.await_gate(&mut rx).await;
        self.apply_gate_outcome(state, resolved)
    }

    /// Attende che la barriera esca da `Pending`, entro il timeout configurato.
    /// Non attende mai per sempre: un panel che non risponde NON deve congelare
    /// il run (sarebbe un deadlock mascherato da attesa).
    async fn await_gate(
        &self,
        rx: &mut tokio::sync::watch::Receiver<AdvisoryGateState>,
    ) -> AdvisoryGateState {
        {
            let now = rx.borrow_and_update().clone();
            if now != AdvisoryGateState::Pending {
                return now;
            }
        }
        tracing::info!(
            target: "nexus_agent_graph::tool_dispatch",
            timeout_s = self.cfg.advisory_gate_timeout_s,
            "barriera advisory: prima scrittura in attesa del verdetto dei panel a monte"
        );
        let wait = rx.wait_for(|s| *s != AdvisoryGateState::Pending);
        match tokio::time::timeout(
            std::time::Duration::from_secs(self.cfg.advisory_gate_timeout_s),
            wait,
        )
        .await
        {
            Ok(Ok(v)) => v.clone(),
            // Sender droppato: il task dei panel e' morto senza dichiarare nulla.
            // Non e' un'approvazione: e' un'assenza, e va detta (regola M).
            Ok(Err(_)) => AdvisoryGateState::Unavailable {
                reason_code: "advisory_channel_closed".to_string(),
            },
            Err(_) => AdvisoryGateState::Unavailable {
                reason_code: "advisory_gate_timeout".to_string(),
            },
        }
    }

    /// Traduce l'esito della barriera: veto -> chiusura terminale; release ->
    /// enforcement + promemoria dei vincoli PRIMA della scrittura; indisponibile
    /// -> si procede, ma dichiarandolo al modello (inconclusive non e' un via
    /// libera).
    fn apply_gate_outcome(&self, state: &AgentState, resolved: AdvisoryGateState) -> AdvisoryBarrier {
        let mut extra = state.extra.clone();
        match resolved {
            // Non raggiungibile (await_gate esce solo su non-Pending), difensivo.
            AdvisoryGateState::Pending => AdvisoryBarrier::Inert,
            AdvisoryGateState::Vetoed { enforcement } => {
                extra.insert(PANEL_ENFORCEMENT_KEY.to_string(), enforcement);
                extra.insert(ADVISORY_GATE_KEY.to_string(), json!({ "status": "vetoed" }));
                tracing::warn!(
                    target: "nexus_agent_graph::tool_dispatch",
                    "barriera advisory: VETO del consiglio prima della prima scrittura -> run fermato"
                );
                // L'edge esistente (graph.rs: terminal_panel_veto -> Learner)
                // instrada la chiusura: nessun routing nuovo (regola L).
                AdvisoryBarrier::Veto(
                    StateDelta {
                        extra: Some(extra),
                        stop_reason: Some(Some(StopReason::EndTurn)),
                        ..Default::default()
                    }
                    .into_opaque(),
                )
            }
            AdvisoryGateState::Released { enforcement } => {
                let reminder = enforcement.as_ref().map(render_gate_requirements);
                if let Some(e) = enforcement {
                    extra.insert(PANEL_ENFORCEMENT_KEY.to_string(), e);
                }
                extra.insert(ADVISORY_GATE_KEY.to_string(), json!({ "status": "released" }));
                tracing::info!(
                    target: "nexus_agent_graph::tool_dispatch",
                    "barriera advisory: verdetto arrivato, la scrittura procede"
                );
                AdvisoryBarrier::Proceed { extra, reminder }
            }
            AdvisoryGateState::Unavailable { reason_code } => {
                tracing::warn!(
                    target: "nexus_agent_graph::tool_dispatch",
                    reason_code = %reason_code,
                    "barriera advisory: nessun verdetto utilizzabile, il run procede SENZA approvazione"
                );
                extra.insert(
                    ADVISORY_GATE_KEY.to_string(),
                    json!({ "status": "unavailable", "reason_code": reason_code }),
                );
                AdvisoryBarrier::Proceed {
                    extra,
                    reminder: Some(GATE_UNAVAILABLE_NOTE.to_string()),
                }
            }
        }
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
    ///
    /// `motivo` e' obbligatorio: chi fabbrica un risultato al posto del tool sa
    /// PERCHE' lo sta facendo, e dichiararlo qui e' cio' che evita a valle di
    /// dover riconoscere il proprio stesso messaggio (regola Q).
    /// L'anomalia della SECONDA task_complete nello stesso turno (GAP-9):
    /// costruita qui perche' il testo abbia un solo produttore e il ramo nel
    /// dispatch resti piatto.
    fn dichiarazione_ripetuta(tool_use_id: &str) -> ToolResultBlock {
        Self::synthetic_error(
            tool_use_id,
            json!(
                "task_complete e' terminale: hai gia' dichiarato l'esito in questo \
                 turno. La dichiarazione precedente resta valida; se l'esito e' \
                 cambiato, prosegui il lavoro e dichiara al turno successivo."
            ),
            MotivoBlocco::DichiarazioneRipetuta,
        )
    }

    fn synthetic_error(
        tool_use_id: &str,
        content: Value,
        motivo: MotivoBlocco,
    ) -> ToolResultBlock {
        ToolResultBlock {
            tool_use_id: tool_use_id.to_string(),
            content,
            is_error: true,
            exit_code: None,
            raw_content: None,
            motivo_blocco: Some(motivo),
        }
    }

    /// Esegue UN tool (parita' con la closure `_run` Python, righe 3786-3896).
    /// `task_complete`/`nexus_run_notes` sono brain-only (non via ToolExecutor):
    /// ritornano un ack e raccolgono outcome/notes nel `RunCollector`. Gli altri
    /// vanno via `ToolExecutor::execute`. Il `try/except` Python e'
    /// ONNICOMPRENSIVO (qualunque errore -> tool_result d'errore, niente
    /// propagazione): qui un `Err(PortError)` (anche infra) diventa un
    /// ToolResult `is_error=true` (NON un `NodeError`), 1:1 col Python.
    async fn run_one(
        &self,
        block: &Value,
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
            let notes_chars = new_notes
                .as_deref()
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0);
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
                // Brain-only: nessun gate ha rifiutato nulla.
                motivo_blocco: None,
            };
        }

        // ── task_complete (brain-only) ────────────────────────────────────────
        if name == TASK_COMPLETE_TOOL_NAME {
            // task_complete e' TERMINALE per il turno (mig 0676, GAP-9): la
            // seconda chiamata nello stesso turno e' un'anomalia STRUTTURATA
            // (tool_result di errore col motivo), mai una seconda valutazione
            // silenziosa in cui "l'ultimo prevale" — era il ciclo
            // dell'incidente e2e-bacheca: tre dichiarazioni senza chiudere,
            // ognuna sovrascriveva la precedente e nessuno lo diceva al
            // modello.
            // La normalizzazione e' SINCRONA: si calcola prima, cosi' il
            // check-e-push sotto sta in UNA sezione critica (review W2,
            // rilievo 13: due lock separati erano corretti solo per la
            // concorrenza cooperativa di oggi — un await aggiunto in mezzo
            // domani avrebbe riaperto la corsa senza che nessun test lo
            // vedesse).
            let decl = normalize_declared_outcome(&input);
            {
                let mut outcomes = collector.declared_outcomes.lock().expect("lock outcomes");
                if !outcomes.is_empty() {
                    return Self::dichiarazione_ripetuta(&tool_use_id);
                }
                if let Ok(d) = &decl {
                    outcomes.push(d.clone());
                }
            }
            collector
                .task_complete_ids
                .lock()
                .expect("lock tc ids")
                .push(tool_use_id.clone());
            return declarative_tool_result(tool_use_id, "outcome", decl.as_ref());
        }

        // ── review_verdict (brain-only, Fase B ultracode) ─────────────────────
        if name == REVIEW_VERDICT_TOOL_NAME {
            let decl = normalize_review_verdict(&input);
            if let Ok(d) = &decl {
                collector
                    .review_verdicts
                    .lock()
                    .expect("lock review verdicts")
                    .push(d.clone());
            }
            return declarative_tool_result(tool_use_id, "verdict", decl.as_ref());
        }

        // ── advisory_verdict (brain-only, consiglio di figure a monte) ────────
        if name == ADVISORY_VERDICT_TOOL_NAME {
            let decl = normalize_advisory_verdict(&input);
            if let Ok(d) = &decl {
                collector
                    .advisory_verdicts
                    .lock()
                    .expect("lock advisory verdicts")
                    .push(d.clone());
            }
            return declarative_tool_result(tool_use_id, "verdict", decl.as_ref());
        }

        // ── debate_position (brain-only, avvocato del dibattito) ──────────────
        if name == DEBATE_POSITION_TOOL_NAME {
            let decl = normalize_debate_position(&input);
            if let Ok(d) = &decl {
                collector
                    .debate_positions
                    .lock()
                    .expect("lock debate positions")
                    .push(d.clone());
            }
            return declarative_tool_result(tool_use_id, "stance", decl.as_ref());
        }

        // ── tool generico via ToolExecutor ────────────────────────────────────
        let call = ToolCall {
            id: tool_use_id.clone(),
            name: name.to_string(),
            input,
            thought_signature: None,
        };
        match self.tools.execute(call).await {
            Ok(outcome) => {
                // WAVE 2.2: errore infrastrutturale (ToolRunner gRPC down)
                // segnalato strutturato (mcp-core NON scala i provider).
                if outcome.is_infrastructure {
                    *collector.infra_error.lock().expect("lock infra") = true;
                }
                // Fase D fan-in (regola M): un `dispatch_subagent(s)` in modalita'
                // background risponde subito col segnale strutturato
                // `background_dispatched` (mai prosa). Raccolto nel collector come
                // gli esiti brain-only; a WAVE 3 diventa `awaiting_subagents` nel
                // delta -> il motore sospende il padre fino al fan-in.
                if DISPATCH_SUBAGENT_TOOLS.contains(&name)
                    && !outcome.is_error
                    && content_signals_background(&outcome.content)
                {
                    *collector.awaiting_subagents.lock().expect("lock awaiting") = true;
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
                    // Il tool ha girato: nessun gate lo ha rifiutato.
                    motivo_blocco: None,
                }
            }
            // try/except onnicomprensivo: QUALSIASI errore (infra incluso) ->
            // tool_result d'errore, niente NodeError (il run non fallisce).
            // Python: json.dumps({"error": str(exc)}) (separatori con spazio).
            Err(exc) => Self::synthetic_error(
                &tool_use_id,
                Value::String(py_dumps(&json!({"error": exc.to_string()}))),
                MotivoBlocco::PortaInErrore,
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
    /// Verdetti dichiarati via review_verdict (l'ultimo prevale, Fase B).
    review_verdicts: std::sync::Mutex<Vec<Value>>,
    /// Pareri dichiarati via advisory_verdict (l'ultimo prevale, consiglio a monte).
    advisory_verdicts: std::sync::Mutex<Vec<Value>>,
    /// Posizioni dichiarate via debate_position (l'ultima prevale, dibattito).
    debate_positions: std::sync::Mutex<Vec<Value>>,
    /// tool_use_id dei task_complete del turno (guard blocked-da-cap).
    task_complete_ids: std::sync::Mutex<Vec<String>>,
    /// Taccuino del run (holder mutabile, P4). Inizializzato al valore di stato.
    run_notes: std::sync::Mutex<Option<String>>,
    /// `true` se almeno un tool e' fallito per infrastruttura (WAVE 2.2).
    infra_error: std::sync::Mutex<bool>,
    /// `true` se almeno un `dispatch_subagent(s)` ha risposto col segnale
    /// strutturato `background_dispatched` (Fase D fan-in): il padre si sospende
    /// (`awaiting_subagents`) e il motore lo interrompe fino al fan-in.
    awaiting_subagents: std::sync::Mutex<bool>,
}

/// Effetti dei tool brain-only del turno, estratti dal [`RunCollector`] a
/// `join_all` concluso (nessuna `run_one` in volo: i `Mutex` sono consumati).
struct TurnEffects {
    /// Esiti dichiarati via task_complete (l'ultimo prevale).
    declared_outcomes: Vec<Value>,
    /// Verdetti dichiarati via review_verdict (l'ultimo prevale, Fase B).
    review_verdicts: Vec<Value>,
    /// Pareri dichiarati via advisory_verdict (l'ultimo prevale, consiglio a monte).
    advisory_verdicts: Vec<Value>,
    /// Posizioni dichiarate via debate_position (l'ultima prevale, tesi contrapposte).
    debate_positions: Vec<Value>,
    /// tool_use_id dei task_complete del turno (guard blocked-da-cap).
    task_complete_ids: Vec<String>,
    /// Taccuino del run a fine turno (P4).
    run_notes: Option<String>,
    /// `true` se almeno un tool e' fallito per infrastruttura (WAVE 2.2).
    infra_error: bool,
    /// `true` se almeno un `dispatch_subagent(s)` e' andato in background (Fase D).
    awaiting_subagents: bool,
}

impl RunCollector {
    /// Consuma il collector e ne estrae gli effetti del turno.
    fn into_turn_effects(self) -> TurnEffects {
        TurnEffects {
            declared_outcomes: self.declared_outcomes.into_inner().expect("outcomes"),
            review_verdicts: self.review_verdicts.into_inner().expect("review verdicts"),
            advisory_verdicts: self
                .advisory_verdicts
                .into_inner()
                .expect("advisory verdicts"),
            debate_positions: self
                .debate_positions
                .into_inner()
                .expect("debate positions"),
            task_complete_ids: self.task_complete_ids.into_inner().expect("tc ids"),
            run_notes: self.run_notes.into_inner().expect("run_notes"),
            infra_error: self.infra_error.into_inner().expect("infra"),
            awaiting_subagents: self.awaiting_subagents.into_inner().expect("awaiting"),
        }
    }
}

/// Soglie del pre-filtro dei pending (sezione 3), risolte UNA volta per turno:
/// restano costanti per tutti i pending del turno (il budget allegati si
/// aggiorna solo a fine turno, 1:1 col Python).
struct PrefilterGates {
    /// Finestra (token) per il predictive cap. `<= 0` = cap non applicabile.
    cap_window: i64,
    /// Stima del contesto corrente (token) per la proiezione del cap.
    predictive_tokens: i64,
    /// Byte allegati gia' letti nella sessione.
    attachment_bytes_read: i64,
    /// Budget cumulativo letture allegati della sessione.
    attachment_budget: i64,
    /// Tool ammessi da M16 (meta + whitelist + always-on + brain-only).
    allowed: std::collections::HashSet<String>,
    /// Tool scoperti nel turno precedente (`discovered_tools_next_turn`).
    discovered_now: std::collections::HashSet<String>,
}

/// Segnali strutturati del turno da riversare nel delta (regola M).
struct TurnSignals<'a> {
    /// Errore di infrastruttura di almeno un tool (WAVE 2.2).
    infra_error: bool,
    /// Sospensione del padre in attesa del fan-in dei sub-run (Fase D).
    awaiting_subagents: bool,
    /// `extra` gia' aggiornato dalla BARRIERA advisory se si e' sciolta in questo
    /// turno: l'enforcement dei panel vi si somma sopra invece di sovrascriverlo
    /// (altrimenti l'esito della barriera andrebbe perso).
    gate_extra: Option<Map<String, Value>>,
    /// Dichiarazione `blocked` rifiutata dal guard blocked-da-cap in questo turno.
    blocked_cap_rejected: bool,
    /// Enforcement deterministico dei verdetti aggregati del panel.
    panel_enforcement: Option<&'a PanelEnforcement>,
    /// Taccuino del run a fine turno (scritto solo se cambiato).
    run_notes: Option<String>,
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

        // Heartbeat best-effort (anti-recovery prematuro).
        let _ = self.run_control.heartbeat(&run_id).await;

        // ── (2a) GATE DUALE sui passi critici (mig 0677) ──────────────────────
        // PRIMA di HITL (2b): le macchine filtrano, l'umano vede il filtrato
        // coi verdetti allegati. In Confirm il costo di validare un passo che
        // l'umano potrebbe bocciare e' accettato (rilievo A3: i critici sono
        // pochi per run); in Automatic e' l'unica barriera sui passi critici.
        if let Some(delta) = self
            .step_gate_barrier(state, &pending, ctx, &run_id)
            .await
        {
            return Ok(delta);
        }

        // ── (2b) HITL Conferma: sospensione strutturale prima dei mutators ───
        if should_suspend_for_hitl(
            state.automation_mode,
            state.approved,
            &pending,
            &self.cfg.fs_mutator_tools,
        ) {
            return Ok(self.hitl_suspend_delta(state, &pending));
        }

        // ── (2c) BARRIERA DI SCRITTURA advisory (overlap, mig 0606) ───────────
        // Il run e' partito mentre i panel a monte deliberavano: la ricognizione
        // read-only e' gia' andata avanti, ma la prima MODIFICA deve attendere il
        // verdetto. Gemello del gate HITL qui sopra: stessa domanda ("questo
        // batch muta lo stato?"), stesso punto unico (`fs_mutator_tools`).
        let (gate_extra, gate_reminder) = match self.advisory_barrier(state, &pending, ctx).await {
            AdvisoryBarrier::Veto(delta) => return Ok(delta),
            AdvisoryBarrier::Proceed { extra, reminder } => (Some(extra), reminder),
            AdvisoryBarrier::Inert => (None, None),
        };

        // ── (3) Pre-filtro dei pending: cap predittivo / M16 / budget allegati ─
        let ctx_chars = estimate_context_chars(
            &state
                .messages
                .iter()
                .map(message_to_ctx)
                .collect::<Vec<_>>(),
        );
        let gates = self.prefilter_gates(state);
        let (slots, kept_indices) = self.prefilter_pending(&pending, &gates);

        // ── (4) Esecuzione dei KEPT (join_all), ordine preservato per POSIZIONE ─
        let collector = RunCollector {
            run_notes: std::sync::Mutex::new(state.run_notes.clone()),
            ..Default::default()
        };
        let kept_futs = kept_indices
            .iter()
            .map(|&i| self.run_one(&pending[i], &collector));
        let kept_results: Vec<ToolResultBlock> = join_all(kept_futs).await;
        let mut results = recompose_results(slots, kept_results);

        // ── (5) Tronca i singoli tool_result a tool_result_max_chars (offload) ─
        self.truncate_kept_results(&mut results, &kept_indices).await;

        // ── (6) Aggiorna attachment_read_bytes (py:3909-3914) ─────────────────
        let new_attachment_read_bytes =
            gates.attachment_bytes_read + added_attachment_bytes(&pending, &results);

        // ── (7) Guard blocked-da-cap (py:3924-3953) ───────────────────────────
        let turn = collector.into_turn_effects();
        let mut declared_outcomes = turn.declared_outcomes;
        let blocked_cap_rejected_now = reject_blocked_from_cap(
            &mut declared_outcomes,
            &mut results,
            &turn.task_complete_ids,
            state.blocked_cap_rejected.unwrap_or(false),
        );

        let panel_enforcement = panel_enforcement_from_results(&pending, &results);
        if let Some(declared) = panel_enforcement
            .as_ref()
            .and_then(|e| e.declared_outcome.as_ref())
        {
            declared_outcomes.push(declared.clone());
        }

        // ── (8) Persist step incrementale (gata Real, py:3956-4007) ───────────
        // PRIMA del cap (9): lo storico conserva il tool_result non compresso.
        self.persist_turn_steps(&run_id, state, &pending, &results)
            .await;

        // ── (9) Context-budget cap (py:4009-4032) ─────────────────────────────
        self.apply_context_budget_cap(&mut results, ctx_chars).await;

        // ── (10) Reminder TODO (anti-amnesia, py:4034-4059) ───────────────────
        let (reminder_text, new_reminder_counter) = self
            .todo_reminder(state, &run_id, pending.len())
            .await;

        // ── Costruzione del HumanMessage coi blocchi tool_result ──────────────
        let tool_msg = build_tool_message(
            &results,
            panel_enforcement.as_ref(),
            gate_reminder.as_deref(),
            reminder_text.as_deref(),
        );

        // ── (11) M16: parse dei tool scoperti dal search (py:4116-4196) ───────
        let discovered_next =
            parse_discovered_next(&pending, &results, self.cfg.discovery_schema_max_bytes);

        // ── meta_steps "tool_executed" + narrazione live (py:4065-4114) ───────
        let tool_steps = build_tool_steps(state, &pending, &results);
        self.narrate_turn(ctx, &tool_steps, &results).await;

        // ── _dispatch_updates ─────────────────────────────────────────────────
        let mut delta = StateDelta {
            messages: Some(vec![tool_msg]),
            meta_steps: Some(tool_steps),
            pending_tool_uses: Some(Some(vec![])),
            stop_reason: Some(Some(StopReason::ToolUse)),
            since_last_todo_reminder: Some(Some(new_reminder_counter)),
            attachment_read_bytes: Some(Some(new_attachment_read_bytes)),
            // CONSUMO del permesso umano: vale per il solo batch appena
            // eseguito. Senza, resterebbe acceso per il resto del run e ogni
            // batch critico successivo salterebbe il gate — l'approvazione di
            // UN passo diventerebbe un lasciapassare permanente.
            step_gate_human_ok: (state.step_gate_human_ok == Some(true)).then_some(Some(false)),
            // SEMPRE scritto (anche []): durata esatta 1 turno (overwrite reducer).
            discovered_tools_next_turn: Some(Some(
                discovered_next.iter().map(discovered_to_value).collect(),
            )),
            ..Default::default()
        };
        delta.discovered_tools_run = merged_discovered_run(state, &discovered_next).map(Some);
        apply_declared_outcome(&mut delta, state, &declared_outcomes);
        // Canali di RUOLO (punto unico `set_role_channel`): l'ultima dichiarazione
        // prevale, ma un turno che non dichiara NON li azzera — al contrario di
        // `declared_outcome` sopra, che e' lo STATO del run e che il lavoro
        // successivo puo' davvero smentire.
        set_role_channel(&mut delta.review_verdict, turn.review_verdicts.last());
        set_role_channel(&mut delta.advisory_verdict, turn.advisory_verdicts.last());
        set_role_channel(&mut delta.debate_position, turn.debate_positions.last());
        apply_turn_signals(
            &mut delta,
            state,
            TurnSignals {
                infra_error: turn.infra_error,
                awaiting_subagents: turn.awaiting_subagents,
                blocked_cap_rejected: blocked_cap_rejected_now,
                panel_enforcement: panel_enforcement.as_ref(),
                run_notes: turn.run_notes,
                gate_extra,
            },
        );

        Ok(delta.into_opaque())
    }
}

impl ToolDispatchNode {
    /// (2b) Delta di sospensione HITL: il run si ferma PRIMA di eseguire i tool
    /// mutativi e pubblica le azioni pendenti in `extra` per la conferma utente
    /// (interrupt-resume prima dell'executor, parita' graph.py). NESSUN tool
    /// eseguito.
    fn hitl_suspend_delta(&self, state: &AgentState, pending: &[Value]) -> OpaqueDelta {
        self.hitl_suspend_delta_con_validazioni(state, pending, None)
    }

    /// Come [`Self::hitl_suspend_delta`], con i verdetti del gate duale
    /// allegati quando la sospensione nasce da un suo `NeedsHuman`: l'umano
    /// decide VEDENDO cosa hanno detto i validatori (chiave
    /// [`step_gate::STEP_GATE_VERDICTS_EXTRA_KEY`]), anche in Automatic.
    fn hitl_suspend_delta_con_validazioni(
        &self,
        state: &AgentState,
        pending: &[Value],
        step_validations: Option<Value>,
    ) -> OpaqueDelta {
        let actions = build_pending_actions_json(pending, &self.cfg.fs_mutator_tools);
        let mut extra = state.extra.clone();
        extra.insert(
            HITL_PENDING_ACTIONS_EXTRA_KEY.to_string(),
            Value::Array(actions),
        );
        // CHI sospende lo DICHIARA (rilievo A4, punto unico
        // `decisions::suspension_watch`): a valle serve per sapere quale
        // `blocker` dichiarare se la sospensione scade senza che nessuno la
        // sciolga. Scritta a OGNI sospensione, cosi' non puo' restare dietro
        // come fossile di una sospensione gia' risolta.
        let origin = if step_validations.is_some() {
            crate::decisions::SuspensionOrigin::StepGate
        } else {
            crate::decisions::SuspensionOrigin::HumanReview
        };
        extra.insert(
            crate::decisions::SUSPENSION_ORIGIN_EXTRA_KEY.to_string(),
            Value::String(origin.as_str().to_string()),
        );
        if let Some(v) = step_validations {
            extra.insert(
                crate::decisions::step_gate::STEP_GATE_VERDICTS_EXTRA_KEY.to_string(),
                v,
            );
        }
        // QUI NON si marca nulla come deliberato. Il permesso di eseguire il
        // batch nasce dalla RISPOSTA dell'umano (campo `step_gate_human_ok`,
        // scritto dal resume), mai dalla domanda: marcarlo qui significava
        // dichiarare deliberato un batch nel momento in cui se ne CHIEDEVA la
        // decisione, e al rientro nel dispatch quel marker faceva saltare la
        // rivalidazione — misurato in esercizio il 05/08/2026 (run 77fcff4a),
        // dove un `rm -rf` irreversibile veniva eseguito 482ms dopo il
        // `NeedsHuman` che avrebbe dovuto fermarlo.
        tracing::info!(
            target: "nexus_agent_graph::tool_dispatch",
            pending_mutators = pending.len(),
            "HITL: run sospeso in attesa di conferma utente (tool mutativi pendenti)"
        );
        StateDelta {
            awaiting_confirmation: Some(Some(true)),
            extra: Some(extra),
            ..Default::default()
        }
        .into_opaque()
    }

    /// (2a) Gate duale sui passi critici (mig 0677). `None` = si procede (gate
    /// spento, batch sotto soglia, batch gia' deliberato o validatori
    /// unanimi); `Some(delta)` = il turno finisce qui (rimando o sospensione).
    ///
    /// La decisione umana su un batch e' FINALE (rilievo A3): quando l'umano
    /// approva, il resume scrive [`AgentState::step_gate_human_ok`] e QUEL
    /// giro del dispatch procede senza riconvocare i validatori (che
    /// altrimenti ribalterebbero l'approvazione, o la riproporrebbero
    /// all'infinito). Il permesso vale per UN solo giro e il turno lo consuma.
    ///
    /// Due letture SBAGLIATE, entrambe misurate: `state.approved` (seminato a
    /// `true` in Automatic/Continuous per saltare l'HITL — spegneva il gate
    /// nella modalita' in cui e' l'unica barriera) e un marker scritto alla
    /// SOSPENSIONE (dichiarava deliberato il batch mentre se ne chiedeva la
    /// decisione: il rientro nel dispatch lo faceva passare, e il `rm -rf` del
    /// run 77fcff4a partiva 482ms dopo il proprio `NeedsHuman`).
    async fn step_gate_barrier(
        &self,
        state: &AgentState,
        pending: &[Value],
        ctx: &AgentNodeCtx,
        run_id: &str,
    ) -> Option<OpaqueDelta> {
        use crate::decisions::step_gate::{self, StepGateMode};
        let mode = self.cfg.step_gate_mode;
        if mode == StepGateMode::Off {
            return None;
        }
        // Permesso umano fresco per QUESTO giro (consumato dal turno).
        if state.step_gate_human_ok == Some(true) {
            return None;
        }

        // Classificazione in-memory (costo zero per i batch ordinari).
        let (level, critici, steps_slim) = self.classifica_batch(pending)?;

        if !mode.convoca(level) {
            self.osserva_batch(ctx, level, steps_slim).await;
            return None;
        }

        let prior_rejections = state
            .extra
            .get(step_gate::STEP_GATE_REJECTIONS_EXTRA_KEY)
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;

        let report = Self::convoca_porta(ctx, state, run_id, critici, level, prior_rejections)
            .await;

        let (decision, cap_raggiunto) = self.decidi_con_cap(&report, level, prior_rejections);
        let payload = payload_convocazione(
            &decision,
            level,
            steps_slim,
            &report,
            prior_rejections,
            cap_raggiunto,
        );
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            step_gate::STEP_VALIDATION_META_KIND,
            titolo_per_umano(&decision, level, &report),
            payload.clone(),
        )
        .await;

        self.esito_decisione(decision, state, pending, &report, prior_rejections, run_id, payload)
            .await
    }

    /// (2a) La CONSEGUENZA della decisione sul flusso del turno: `None` = i
    /// tool si eseguono; `Some(delta)` = rimando o sospensione.
    #[allow(clippy::too_many_arguments)]
    async fn esito_decisione(
        &self,
        decision: crate::decisions::step_gate::StepGateDecision,
        state: &AgentState,
        pending: &[Value],
        report: &ports::StepValidationReport,
        prior_rejections: u32,
        run_id: &str,
        payload: Value,
    ) -> Option<OpaqueDelta> {
        use crate::decisions::step_gate::StepGateDecision;
        match decision {
            StepGateDecision::Approved => None,
            StepGateDecision::UnavailableDeclared => {
                tracing::warn!(
                    target: "nexus_agent_graph::tool_dispatch",
                    "gate duale senza verdetti utilizzabili su un Critical: si procede DICHIARANDOLO"
                );
                None
            }
            // IN AUTONOMIA NON SI CHIEDE (regola D): in Automatic/Continuous
            // l'utente ha scelto di non essere interrotto, e non c'e' nessuno
            // che sciolga la sospensione — una domanda li' e' un run appeso
            // fino alla scadenza, non una difesa. Il gate dice NO al passo e
            // lo rimanda al modello coi motivi: l'agente cambia strada o
            // chiude `blocked`, che e' l'esito onesto. La sospensione resta
            // dove qualcuno la puo' sciogliere: Confirm e Studio, dove
            // `automation_requires_hitl` (punto unico) e' vero.
            StepGateDecision::NeedsHuman => {
                self.esito_needs_human(state, pending, report, prior_rejections, run_id, payload)
                    .await
            }
            StepGateDecision::Rejected => Some(
                self.step_gate_reject_delta(state, pending, report, prior_rejections, run_id)
                    .await,
            ),
        }
    }

    /// (2a) Telemetria di taratura (observe / Critical in
    /// enforce_irreversible): la classificazione si PERSISTE come meta_step,
    /// il batch procede senza costo LLM.
    async fn osserva_batch(
        &self,
        ctx: &AgentNodeCtx,
        level: crate::decisions::step_gate::StepCriticality,
        steps_slim: Vec<Value>,
    ) {
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            crate::decisions::step_gate::STEP_VALIDATION_META_KIND,
            format!(
                "Passo {} osservato (gate in sola osservazione, nessun blocco)",
                if level == crate::decisions::step_gate::StepCriticality::Irreversible {
                    "irreversibile"
                } else {
                    "critico"
                }
            ),
            json!({
                "decision": "observed",
                "level": level.as_str(),
                "steps": steps_slim,
            }),
        )
        .await;
    }

    /// (2a) Classifica il batch: `None` = sotto soglia (nessun passo >=
    /// Critical), il gate non ha nulla da dire. Ritorna il livello massimo, i
    /// passi al livello alto (quelli che i validatori vedono) e la loro forma
    /// slim per il payload del meta_step.
    fn classifica_batch(
        &self,
        pending: &[Value],
    ) -> Option<(
        crate::decisions::step_gate::StepCriticality,
        Vec<ports::PendingStepInfo>,
        Vec<Value>,
    )> {
        use crate::decisions::step_gate::{self, StepCriticality};
        let classificati: Vec<step_gate::StepClassification> = pending
            .iter()
            .map(|p| {
                step_gate::classify_step(
                    p.get("name").and_then(Value::as_str).unwrap_or(""),
                    p.get("input").unwrap_or(&Value::Null),
                    &self.cfg.fs_mutator_tools,
                    &self.cfg.step_gate_rules,
                )
            })
            .collect();
        let level = classificati.iter().map(|c| c.level).max()?;
        if level < StepCriticality::Critical {
            return None;
        }
        let critici: Vec<ports::PendingStepInfo> = pending
            .iter()
            .zip(&classificati)
            .filter(|(_, c)| c.level >= StepCriticality::Critical)
            .map(|(p, c)| ports::PendingStepInfo {
                tool_use_id: p
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_name: p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_input: p.get("input").cloned().unwrap_or(Value::Null),
                matched_category: c.matched_category.clone(),
            })
            .collect();
        let steps_slim: Vec<Value> = critici
            .iter()
            .map(|s| json!({"tool_name": s.tool_name, "category": s.matched_category}))
            .collect();
        Some((level, critici, steps_slim))
    }

    /// (2a) La decisione dai verdetti + il cap anti ping-pong: un ennesimo
    /// rimando non riparte verso il modello, degrada alla decisione umana
    /// (dichiarato nel payload con `cap_reached`).
    fn decidi_con_cap(
        &self,
        report: &ports::StepValidationReport,
        level: crate::decisions::step_gate::StepCriticality,
        prior_rejections: u32,
    ) -> (crate::decisions::step_gate::StepGateDecision, bool) {
        use crate::decisions::step_gate::{decide_step_gate, StepGateDecision, StepVerdict};
        let verdetti: Vec<StepVerdict> = report.verdicts.iter().map(|v| v.verdict).collect();
        let decision = decide_step_gate(&verdetti, level);
        let cap_raggiunto = decision == StepGateDecision::Rejected
            && prior_rejections + 1 >= self.cfg.step_gate_max_rejections.max(1);
        if cap_raggiunto {
            (StepGateDecision::NeedsHuman, true)
        } else {
            (decision, false)
        }
    }

    /// (2a) Convoca la porta. La porta assente con mode acceso e' un report
    /// vuoto (degrado DICHIARATO): la matrice della doppia astensione decide,
    /// mai un salto silenzioso.
    async fn convoca_porta(
        ctx: &AgentNodeCtx,
        state: &AgentState,
        run_id: &str,
        critici: Vec<ports::PendingStepInfo>,
        level: crate::decisions::step_gate::StepCriticality,
        prior_rejections: u32,
    ) -> ports::StepValidationReport {
        let Some(gate) = ctx.step_gate.as_ref() else {
            return report_degradato("porta di validazione non cablata (setup non armato)");
        };
        let req = ports::StepValidationRequest {
            run_id: run_id.to_string(),
            executor_provider: state.sticky_provider.clone().unwrap_or_default(),
            steps: critici,
            level,
            plan_excerpt: crate::decisions::turn_task::current_turn_task(state)
                .map(str::to_string),
            prior_rejections,
        };
        match gate.validate(req).await {
            Ok(r) => r,
            Err(e) => report_degradato(&format!("porta di validazione in errore: {e}")),
        }
    }

    /// (2a) Che fare quando il gate rimanda la decisione a un umano: dipende
    /// da CHI puo' rispondere. In Conferma e Studio l'utente e' al terminale e
    /// la sospensione ha un destinatario; in Automatic/Continuous ha scelto di
    /// non essere interrotto (regola D) e nessuno la scioglierebbe — li' il
    /// gate dice NO al passo e lo rimanda al modello, che cambia strada o
    /// chiude `blocked`. La domanda «questa modalita' vuole l'umano?» ha gia'
    /// il suo punto unico: `hitl::automation_requires_hitl`.
    #[allow(clippy::too_many_arguments)]
    async fn esito_needs_human(
        &self,
        state: &AgentState,
        pending: &[Value],
        report: &ports::StepValidationReport,
        prior_rejections: u32,
        run_id: &str,
        payload: Value,
    ) -> Option<OpaqueDelta> {
        if crate::decisions::hitl::automation_requires_hitl(state.automation_mode) {
            return Some(self.hitl_suspend_delta_con_validazioni(state, pending, Some(payload)));
        }
        Some(
            self.step_gate_reject_delta(state, pending, report, prior_rejections, run_id)
                .await,
        )
    }

    /// Il delta del RIMANDO: nessun tool eseguito, ogni pending riceve un
    /// tool_result sintetico coi motivi dei validatori (e l'alternativa piu'
    /// sicura, se proposta). Il turno torna al modello (`ToolUse`), il
    /// contatore dei rimandi sale nello stato.
    async fn step_gate_reject_delta(
        &self,
        state: &AgentState,
        pending: &[Value],
        report: &ports::StepValidationReport,
        prior_rejections: u32,
        run_id: &str,
    ) -> OpaqueDelta {
        use crate::decisions::step_gate::STEP_GATE_REJECTIONS_EXTRA_KEY;
        let testo = testo_rimando(report);
        let results: Vec<ToolResultBlock> = pending
            .iter()
            .map(|p| {
                Self::synthetic_error(
                    p.get("id").and_then(Value::as_str).unwrap_or_default(),
                    json!(testo.clone()),
                    MotivoBlocco::StepGateRejected,
                )
            })
            .collect();
        // Lo storico conserva i rifiuti come passi persistiti (stesso canale
        // dei turni normali: `agent_steps` vede il batch respinto).
        self.persist_turn_steps(run_id, state, pending, &results)
            .await;
        let tool_msg = build_tool_message(&results, None, None, None);
        let tool_steps = build_tool_steps(state, pending, &results);
        let mut extra = state.extra.clone();
        extra.insert(
            STEP_GATE_REJECTIONS_EXTRA_KEY.to_string(),
            json!(prior_rejections + 1),
        );
        StateDelta {
            messages: Some(vec![tool_msg]),
            meta_steps: Some(tool_steps),
            pending_tool_uses: Some(Some(vec![])),
            stop_reason: Some(Some(StopReason::ToolUse)),
            extra: Some(extra),
            ..Default::default()
        }
        .into_opaque()
    }

    /// (3) Risolve UNA volta per turno le soglie del pre-filtro dei pending.
    fn prefilter_gates(&self, state: &AgentState) -> PrefilterGates {
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
        PrefilterGates {
            cap_window,
            predictive_tokens: self.predictive_tokens(state),
            attachment_bytes_read: state.attachment_read_bytes.unwrap_or(0),
            attachment_budget: self.cfg.attachment_budget_bytes,
            allowed: self.m16_allowed(),
            discovered_now: Self::discovered_now(state),
        }
    }

    /// (3) Applica il pre-filtro a TUTTI i pending. Ritorna gli slot per
    /// POSIZIONE (`Some` = synthetic gia' pronto, tool NON eseguito; `None` =
    /// KEPT da eseguire) e gli indici dei KEPT nell'ordine originale.
    fn prefilter_pending(
        &self,
        pending: &[Value],
        gates: &PrefilterGates,
    ) -> (Vec<Option<ToolResultBlock>>, Vec<usize>) {
        let slots: Vec<Option<ToolResultBlock>> = pending
            .iter()
            .map(|b| self.prefilter_block(b, gates))
            .collect();
        let kept_indices: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_none())
            .map(|(i, _)| i)
            .collect();
        (slots, kept_indices)
    }

    /// (3) Verdetto del pre-filtro per UN pending: `Some` = tool NON eseguito
    /// (tool_result synthetic), `None` = KEPT. L'ordine dei gate e' load-bearing
    /// (1:1 col Python): predictive cap (priorita') -> M16 -> budget allegati.
    /// `or_else` e' lazy: il primo gate che blocca ferma la catena.
    fn prefilter_block(&self, block: &Value, gates: &PrefilterGates) -> Option<ToolResultBlock> {
        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
        let tool_use_id = block.get("id").and_then(Value::as_str).unwrap_or("");
        let input = block.get("input").cloned().unwrap_or(json!({}));
        self.cap_block(name, tool_use_id, &input, gates)
            .or_else(|| self.m16_block(name, tool_use_id, gates))
            .or_else(|| attachment_budget_block(name, tool_use_id, gates))
    }

    /// (3a) Predictive cap: esente -> nessun blocco; altrimenti la proiezione e'
    /// valutata SOLO se la finestra e' nota (>0). Il content del synthetic e' il
    /// messaggio col SENTINEL in testa (stringa nuda, NON un JSON {error:...}),
    /// 1:1 col Python.
    fn cap_block(
        &self,
        name: &str,
        tool_use_id: &str,
        input: &Value,
        gates: &PrefilterGates,
    ) -> Option<ToolResultBlock> {
        if gates.cap_window <= 0 || is_cap_exempt(name) {
            return None;
        }
        let expected = estimate_tool_result_size_bytes(name, input);
        let msg = predictive_cap_check(
            self.cfg.predictive_cap_ratio,
            gates.cap_window,
            expected,
            gates.predictive_tokens,
        )?;
        Some(Self::synthetic_error(
            tool_use_id,
            Value::String(msg),
            MotivoBlocco::PredictiveCap,
        ))
    }

    /// (3b) M16: tool non ammesso e non scoperto -> SYNTHETIC error che forza il
    /// giro via `nexus_mcp_tool_search`.
    fn m16_block(
        &self,
        name: &str,
        tool_use_id: &str,
        gates: &PrefilterGates,
    ) -> Option<ToolResultBlock> {
        if !self.cfg.discovery_first_enabled
            || is_tool_allowed(name, &gates.allowed, &gates.discovered_now)
        {
            return None;
        }
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
        Some(Self::synthetic_error(
            tool_use_id,
            Value::String(py_dumps(&err)),
            MotivoBlocco::ToolNonScoperto,
        ))
    }

    /// (5) Tronca i tool_result dei soli KEPT a `tool_result_max_chars` (i
    /// synthetic sono brevi e restano intatti).
    async fn truncate_kept_results(&self, results: &mut [ToolResultBlock], kept_indices: &[usize]) {
        for (idx, r) in results.iter_mut().enumerate() {
            if kept_indices.contains(&idx) {
                self.truncate_content(&mut r.content, self.cfg.tool_result_max_chars)
                    .await;
            }
        }
    }

    /// (8) Persistenza incrementale degli step del turno (gata Real). No-op senza
    /// run_id. Best-effort (errore DB loggato dall'impl, `Ok(())` ritornato): un
    /// guasto della persistenza NON deve far fallire il run.
    async fn persist_turn_steps(
        &self,
        run_id: &str,
        state: &AgentState,
        pending: &[Value],
        results: &[ToolResultBlock],
    ) {
        if run_id.is_empty() {
            return;
        }
        let iteration = state.iterations.unwrap_or(0);
        for (idx, (b, r)) in pending.iter().zip(results.iter()).enumerate() {
            // I campi vanno alla persistenza come campi (regola Q). L'esito e' il
            // flag STRUTTURATO `is_error` (regola M), non una lettura del testo
            // prodotto dal tool.
            let step = PersistedStep {
                tool_name: b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                tool_input: b.get("input").cloned().unwrap_or(json!({})),
                tool_result: Some(value_as_json_string(&r.content)),
                status: StepStatus::from_is_error(r.is_error),
            };
            let _ = self
                .steps
                .persist_step(run_id, iteration, idx as i64, step)
                .await;
        }
    }

    /// (9) Context-budget cap: se il turno sfonda `max_context_chars`, comprime
    /// OGNI tool_result (KEPT e synthetic) a una quota equa, con offload
    /// best-effort e degrado a troncamento testa+coda.
    async fn apply_context_budget_cap(
        &self,
        results: &mut [ToolResultBlock],
        ctx_chars: i64,
    ) {
        let new_chars: i64 = results
            .iter()
            .map(|r| value_as_json_string(&r.content).chars().count() as i64)
            .sum();
        if ctx_chars + new_chars <= self.cfg.max_context_chars as i64 {
            return;
        }
        let span = self.cfg.max_context_chars as i64 - ctx_chars;
        let budget_per_tool =
            std::cmp::max(1500i64, span / std::cmp::max(results.len() as i64, 1)) as usize;
        for r in results.iter_mut() {
            self.truncate_content(&mut r.content, budget_per_tool).await;
        }
        tracing::warn!(
            target: "nexus_agent_graph::tool_dispatch",
            ctx_chars,
            new_chars,
            budget_per_tool,
            "contesto vicino al limite, troncamento aggressivo"
        );
    }

    /// (10) Promemoria dei todo (anti-amnesia): solo in plan phase, alla soglia di
    /// tool-use e con run_id. Ritorna il testo del reminder (se prodotto) e il
    /// contatore aggiornato, azzerato SOLO quando il reminder parte davvero.
    async fn todo_reminder(
        &self,
        state: &AgentState,
        run_id: &str,
        pending_len: usize,
    ) -> (Option<String>, i64) {
        let counter = state.since_last_todo_reminder.unwrap_or(0) + pending_len as i64;
        let every_n = std::cmp::max(1, self.cfg.todo_reminder_every_n_steps);
        if !state.plan_phase_active.unwrap_or(false) || counter < every_n || run_id.is_empty() {
            return (None, counter);
        }
        let text = self.todos.build_reminder_text(run_id).await.unwrap_or(None);
        if text.is_none() {
            return (None, counter);
        }
        // Best-effort: traccia che i todos sono stati "visti".
        let _ = self.todos.increment_iteration_seen(run_id).await;
        (text, 0)
    }

    /// NARRAZIONE LIVE: ogni tool eseguito diventa una riga della cronaca in chat
    /// ("tool edit_file — src/x.ts", "errore run_command — pnpm build") e un
    /// evento SSE `tool_result` verso il frontend (parita' 1:1 con run_via_brain,
    /// correlato alla ToolUse via `tool_call_id`).
    ///
    /// Prima i meta-step restavano solo nel canale stato (delta) e NON arrivavano
    /// mai al frontend ne' al DB: la timeline mostrava solo gli executor_call
    /// ("quale modello") — incidente narrazione 2026-07-02. Pattern emit (live,
    /// sink no-op in shadow) + persist (storico, gata Real), identico
    /// all'executor_call (regola L).
    ///
    /// Chiamata DOPO i cap/troncamenti (5/9), cosi' il contenuto inviato
    /// all'utente coincide con quello consegnato al modello. Best-effort e
    /// infallibile: in shadow il sink iniettato nel ctx e' `NullEventSink` (no-op)
    /// -> nessun evento esce in Replay (gate gia' assicurato a monte da
    /// `build_native_engine`, qui niente ramo per lo shadow).
    async fn narrate_turn(
        &self,
        ctx: &AgentNodeCtx,
        tool_steps: &[MetaStep],
        results: &[ToolResultBlock],
    ) {
        for ms in tool_steps {
            crate::nodes::emit_phase_meta(
                ctx.emit.as_ref(),
                self.meta_steps.as_ref(),
                &ms.kind,
                ms.title.clone(),
                ms.payload.clone(),
            )
            .await;
        }
        for r in results {
            ctx.emit.emit(SseEvent::ToolResult {
                tool_call_id: r.tool_use_id.clone(),
                content: r.content.clone(),
                is_error: r.is_error,
            });
        }
    }

    /// Tronca `content` (se stringa) a `max_chars` con offload best-effort in RAG
    /// e degrado a troncamento testa+coda (`_smart_truncate_lossless`,
    /// `__init__.py:153-181`).
    ///
    /// PURO se il content non e' stringa o e' sotto soglia (no-op). Sopra soglia:
    /// head = `max_chars/5`, tail = `max(200, max_chars - head - 200)`, pointer
    /// in mezzo (col pointer RAG se l'offload riesce, un marker di troncamento
    /// altrimenti).
    async fn truncate_content(&self, content: &mut Value, max_chars: usize) {
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
        // Offload best-effort: pointer RAG se la porta riesce, altrimenti marker
        // di troncamento (guasto Qdrant).
        let pointer = match self.try_offload(text).await {
            Some(ptr) => format!(
                "\n\n[...troncato: {total} char totali offloadati in RAG, recupera con \
                 nexus_search_semantic (pointer={ptr})...]\n\n"
            ),
            None => format!("\n\n[...troncato: {total} char totali, coda preservata sotto...]\n\n"),
        };
        let head: String = chars.iter().take(head_size).collect();
        let tail: String = chars.iter().skip(total.saturating_sub(tail_size)).collect();
        *content = Value::String(format!("{head}{pointer}{tail}"));
    }

    /// Offload best-effort verso RAG. `None` su errore della porta (degrado a
    /// troncamento).
    async fn try_offload(&self, text: &str) -> Option<String> {
        // Tool_result grande al dispatch: collection ToolResult, senza filtro
        // session/project (comportamento storico, cache del contesto offloadato).
        self.offload
            .offload_to_rag(
                json!({"text": text}),
                OffloadKind::ToolResult,
                None,
                None,
            )
            .await
            .ok()
    }
}

/// (3c) Budget allegati: tool di lettura allegato oltre il budget cumulativo
/// della sessione -> SYNTHETIC error che indirizza agli estrattori strutturati.
fn attachment_budget_block(
    name: &str,
    tool_use_id: &str,
    gates: &PrefilterGates,
) -> Option<ToolResultBlock> {
    if !ATTACHMENT_READ_TOOLS.contains(&name)
        || gates.attachment_bytes_read < gates.attachment_budget
    {
        return None;
    }
    let current_bytes = gates.attachment_bytes_read;
    let budget_total = gates.attachment_budget;
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
    Some(ToolDispatchNode::synthetic_error(
        tool_use_id,
        Value::String(py_dumps(&err)),
        MotivoBlocco::BudgetAllegati,
    ))
}

/// (4) Ricompone i risultati nell'ordine ORIGINALE dei pending: ogni slot vuoto
/// (KEPT) prende il prossimo esito eseguito, gli altri tengono il synthetic gia'
/// pronto. Allineamento per POSIZIONE (load-bearing, NON per id).
fn recompose_results(
    slots: Vec<Option<ToolResultBlock>>,
    kept_results: Vec<ToolResultBlock>,
) -> Vec<ToolResultBlock> {
    let mut kept_iter = kept_results.into_iter();
    slots
        .into_iter()
        .map(|slot| {
            slot.unwrap_or_else(|| {
                kept_iter
                    .next()
                    .expect("ogni slot KEPT ha un risultato corrispondente")
            })
        })
        .collect()
}

/// (6) Byte letti dai tool allegato RIUSCITI del turno: il budget cumulativo
/// della sessione avanza solo sui successi (py:3909-3914).
fn added_attachment_bytes(pending: &[Value], results: &[ToolResultBlock]) -> i64 {
    pending
        .iter()
        .zip(results.iter())
        .filter(|(b, r)| {
            let name = b.get("name").and_then(Value::as_str).unwrap_or("");
            ATTACHMENT_READ_TOOLS.contains(&name) && !r.is_error
        })
        .map(|(_, r)| extract_returned_bytes(&value_as_json_string(&r.content)))
        .sum()
}

/// (7) Predicato del guard blocked-da-cap: l'ultima dichiarazione del turno e'
/// `blocked`, non e' gia' stata rifiutata in questo run e almeno una chiamata di
/// QUESTO turno e' stata rifiutata dal predictive cap.
///
/// Il criterio e' il campo [`ToolResultBlock::motivo_blocco`], che il gate
/// compila quando rifiuta: annullare una dichiarazione d'esito del modello e'
/// un'azione forte, e non puo' poggiare sul ritrovamento di una stringa dentro
/// un testo che qualunque tool puo' aver RESTITUITO invece che SUBITO.
fn should_reject_blocked_from_cap(
    declared_outcomes: &[Value],
    results: &[ToolResultBlock],
    already_rejected: bool,
) -> bool {
    let last_blocked = declared_outcomes
        .last()
        .and_then(|d| d.get("outcome").and_then(Value::as_str))
        == Some("blocked");
    let qualcuno_bloccato_dal_cap = results
        .iter()
        .any(|r| r.motivo_blocco == Some(MotivoBlocco::PredictiveCap));
    !declared_outcomes.is_empty() && last_blocked && !already_rejected && qualcuno_bloccato_dal_cap
}

/// (7) Guard blocked-da-cap (py:3924-3953): una dichiarazione `blocked` il cui
/// UNICO blocco e' il predictive cap su una singola chiamata NON e' un blocco del
/// task -> la dichiarazione viene rifiutata (UNA volta per run: il flag
/// `blocked_cap_rejected` onora la seconda). Ritorna `true` se ha rifiutato.
fn reject_blocked_from_cap(
    declared_outcomes: &mut Vec<Value>,
    results: &mut [ToolResultBlock],
    task_complete_ids: &[String],
    already_rejected: bool,
) -> bool {
    if !should_reject_blocked_from_cap(declared_outcomes, results, already_rejected) {
        return false;
    }
    // La `reason` e' una stringa SINGOLA (spazi singoli, niente indentazione
    // spuria): 1:1 col Python (concatenazione implicita di literal adiacenti).
    // py_dumps -> separatori con spazio come json.dumps.
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
    tracing::warn!(
        target: "nexus_agent_graph::tool_dispatch",
        "task_complete blocked RIFIUTATO (blocco era del predictive cap, non del task)"
    );
    true
}

/// Costruisce il HumanMessage coi blocchi del turno: i tool_result (senza
/// `raw_content`) + l'eventuale blocco di enforcement del panel + l'eventuale
/// promemoria della barriera advisory + l'eventuale promemoria dei todo.
///
/// Il Python costruisce un HumanMessage con content="" e i blocchi in
/// additional_kwargs["anthropic_content"]. In Rust la forma autoritativa del
/// contenuto a blocchi e' MessageContent::Blocks: deserializziamo i blocchi JSON
/// in ContentBlock (un blocco non riconosciuto, es. il reminder text, cade su
/// ContentBlock::Text).
fn build_tool_message(
    results: &[ToolResultBlock],
    panel_enforcement: Option<&PanelEnforcement>,
    gate_reminder: Option<&str>,
    reminder_text: Option<&str>,
) -> Message {
    let mut final_blocks: Vec<Value> = results.iter().map(tool_result_to_block).collect();
    if let Some(enforcement) = panel_enforcement {
        final_blocks.push(json!({"type": "text", "text": enforcement.prompt_block()}));
    }
    // Promemoria della BARRIERA: i vincoli del consiglio (o la sua assenza)
    // arrivano al modello nello stesso turno in cui ha scritto, non a fine
    // run — e' l'unico momento in cui puo' ancora tenerne conto.
    if let Some(txt) = gate_reminder {
        append_reminder_block(&mut final_blocks, txt);
    }
    if let Some(txt) = reminder_text {
        append_reminder_block(&mut final_blocks, txt);
    }
    human_message_from_blocks(final_blocks)
}

/// (11) M16: tool scoperti dai `nexus_mcp_tool_search` RIUSCITI del turno. Il
/// parse gira sul JSON INTEGRO (il `raw_content` pre-troncamento, altrimenti il
/// content) e `parse_discovered_tools` usa gia' il fix ensure_ascii (PR-G).
/// Dedup cross-search nel turno: la prima occorrenza vince (come Python
/// `if not any(d.name == ...)`).
fn parse_discovered_next(
    pending: &[Value],
    results: &[ToolResultBlock],
    schema_max_bytes: usize,
) -> Vec<DiscoveredTool> {
    let mut discovered_next: Vec<DiscoveredTool> = Vec::new();
    for (b, r) in pending.iter().zip(results.iter()) {
        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
        if name != "nexus_mcp_tool_search" || r.is_error {
            continue;
        }
        let raw = r
            .raw_content
            .clone()
            .unwrap_or_else(|| value_as_json_string(&r.content));
        for t in crate::decisions::parse_discovered_tools(&raw, schema_max_bytes) {
            if !discovered_next.iter().any(|d| d.name == t.name) {
                discovered_next.push(t);
            }
        }
    }
    discovered_next
}

/// meta_steps `tool_executed` del turno (live UX, py:4065-4114): un MetaStep per
/// OGNI tool (KEPT, synthetic-blocked, brain-only), allineato per POSIZIONE ai
/// pending (`zip(pending, results)`, 1:1 col Python). provider/model emittenti
/// del turno (UI badge): catena di fallback identica al Python
/// (provider_used -> sticky -> override).
fn build_tool_steps(
    state: &AgentState,
    pending: &[Value],
    results: &[ToolResultBlock],
) -> Vec<MetaStep> {
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
    pending
        .iter()
        .zip(results.iter())
        .map(|(b, r)| tool_executed_meta_step(b, r.is_error, exec_provider, exec_model))
        .collect()
}

/// P3: accumulo persistente dei tool scoperti nel run (merge dedup, l'ultimo
/// schema vince). `None` = il turno non ha scoperto nulla e il campo NON va
/// toccato (l'accumulo del run resta quello dello stato).
fn merged_discovered_run(
    state: &AgentState,
    discovered_next: &[DiscoveredTool],
) -> Option<Vec<Value>> {
    if discovered_next.is_empty() {
        return None;
    }
    let previous: Vec<DiscoveredTool> = state
        .discovered_tools_run
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| serde_json::from_value::<DiscoveredTool>(v.clone()).ok())
        .collect();
    let merged = merge_discovered_run(&previous, discovered_next);
    Some(merged.iter().map(discovered_to_value).collect())
}

/// WAVE 3: esito dichiarato del turno (l'ultimo prevale) + conteggio cumulativo
/// dei `done`.
///
/// Senza dichiarazioni nel turno ma con una dichiarazione nello stato scatta
/// l'INVALIDAZIONE STANTIA (ADR 0034): il run PROSEGUE con altri tool DOPO una
/// dichiarazione precedente -> quella dichiarazione era intermedia, non l'esito
/// finale. Senza questo azzeramento, un "partial"/"blocked" dichiarato a meta'
/// run falsava lo status canonico FINALE anche a lavoro poi completato (il
/// finalizzatore legge l'ULTIMA dichiarazione dallo stato). `declared_done_count`
/// resta cumulativo (gate done>=3 in testa).
fn apply_declared_outcome(delta: &mut StateDelta, state: &AgentState, declared_outcomes: &[Value]) {
    let Some(last) = declared_outcomes.last() else {
        if state.declared_outcome.is_some() {
            delta.declared_outcome = Some(None);
            delta.declared_outcome_iteration = Some(None);
        }
        return;
    };
    delta.declared_outcome = Some(Some(last.clone()));
    // Timbro di freschezza (vedi doc `AgentState::declared_outcome_iteration`):
    // `state.iterations` e' il valore che il turno SUCCESSIVO ricevera' come
    // `iters_in` (l'executor l'ha gia' scritto nel proprio delta, gia' applicato
    // qui). Chi consuma la dichiarazione per riscrivere testo visibile
    // all'utente si fida solo se il confronto e' un'uguaglianza esatta.
    delta.declared_outcome_iteration = Some(Some(state.iterations.unwrap_or(0)));
    let done_now = declared_outcomes
        .iter()
        .filter(|d| d.get("outcome").and_then(Value::as_str) == Some("done"))
        .count() as i64;
    if done_now > 0 {
        let prev = state.declared_done_count.unwrap_or(0);
        delta.declared_done_count = Some(Some(prev + done_now));
    }
}

/// Riversa nel delta i segnali strutturati del turno (regola M).
///
/// - `infra_error` (WAVE 2.2): errore infrastruttura tool (mcp-core NON scala i
///   provider).
/// - `panel_enforcement`: pubblicato in `extra`; se terminale chiude il turno.
/// - `awaiting_subagents` (Fase D fan-in): almeno un `dispatch_subagent(s)`
///   background ha risposto col segnale strutturato -> il padre si sospende. Il
///   motore interrompe su `is_awaiting_interrupt` (Slice 1) e riprende al fan-in
///   (Slice 3). Scritto solo quando true (nessun azzeramento qui: il resume lo
///   gestisce Slice 3).
/// - `blocked_cap_rejected`: marca il flag (la 2a dichiarazione sara' onorata).
/// - `run_notes` (P4): persiste il taccuino aggiornato SOLO se cambiato
///   (py:4219).
fn apply_turn_signals(delta: &mut StateDelta, state: &AgentState, signals: TurnSignals<'_>) {
    if signals.infra_error {
        delta.tool_infra_error = Some(Some(true));
    }
    if let Some(enforcement) = signals.panel_enforcement {
        let mut extra = signals.gate_extra.unwrap_or_else(|| state.extra.clone());
        extra.insert(PANEL_ENFORCEMENT_KEY.to_string(), enforcement.to_value());
        delta.extra = Some(extra);
        if enforcement.terminal {
            delta.result = Some(Some(enforcement.summary.clone()));
            delta.stop_reason = Some(Some(StopReason::EndTurn));
        }
    } else if let Some(extra) = signals.gate_extra {
        delta.extra = Some(extra);
    }
    if signals.awaiting_subagents {
        delta.awaiting_subagents = Some(Some(true));
    }
    if signals.blocked_cap_rejected {
        delta.blocked_cap_rejected = Some(Some(true));
    }
    if signals.run_notes != state.run_notes {
        delta.run_notes = Some(signals.run_notes);
    }
}

/// Converte un [`ToolResultBlock`] nel `ContentBlock::tool_result` JSON (forma
/// anthropic_content), SENZA `raw_content` (rimosso: non arriva al modello).
/// `exit_code` e' incluso solo se presente (tool-comando), 1:1 col Python che
/// aggiunge la chiave solo `if result.exit_code is not None`.
/// Costruisce il `tool_result` di un canale dichiarativo di RUOLO.
///
/// PUNTO UNICO (regola L) della forma `{"acknowledged": bool, <chiave>: ...}`, e
/// del fatto che un rifiuto porti con se' la RAGIONE: prima i tre canali
/// ripetevano la stessa costruzione e il modello riceveva solo
/// `{"acknowledged": false, "verdict": null}`, senza sapere cosa correggere.
///
/// Copre anche `task_complete`: l'esclusione originaria proteggeva la parita'
/// col golden Python, ma il golden file non esiste nel repo e il generatore e'
/// stato rimosso col brain (`load_golden` salta sempre) — il vincolo era un
/// fossile. Il reperto che ha motivato l'estensione: 16 rifiuti muti su 132,
/// con retry alla cieca provati ("success" dichiarato identico 3 volte).
///
/// Regola M: per il CODICE il segnale resta `is_error`; `reason` e' prosa per il
/// modello e nessun ramo del programma deve parsarla.
fn declarative_tool_result(
    tool_use_id: String,
    payload_key: &'static str,
    decl: Result<&Value, &DeclarationRejected>,
) -> ToolResultBlock {
    let mut content = serde_json::Map::new();
    content.insert("acknowledged".to_string(), Value::Bool(decl.is_ok()));
    match decl {
        Ok(d) => {
            content.insert(
                payload_key.to_string(),
                d.get(payload_key).cloned().unwrap_or(Value::Null),
            );
        }
        Err(r) => {
            content.insert(payload_key.to_string(), Value::Null);
            content.insert("reason".to_string(), Value::String(r.explain()));
        }
    }
    ToolResultBlock {
        tool_use_id,
        content: Value::String(py_dumps(&Value::Object(content))),
        is_error: decl.is_err(),
        exit_code: None,
        raw_content: None,
        // Canale dichiarativo (brain-only): il rifiuto riguarda la
        // DICHIARAZIONE, non una chiamata che un gate ha impedito.
        motivo_blocco: None,
    }
}

/// Scrive nel delta un canale di RUOLO: parere della figura del consiglio,
/// verdetto del revisore, posizione dell'avvocato del dibattito.
///
/// PUNTO UNICO (regola L) della loro semantica, che e' l'OPPOSTO di quella di
/// `declared_outcome` (ADR 0034): un canale di ruolo e' un CONTRIBUTO gia'
/// consegnato su un oggetto ESTERNO al run — la richiesta da valutare, il diff da
/// revisionare, l'opzione da difendere — non un'asserzione sullo stato del run.
/// Proseguire con altri tool non lo smentisce, quindi il turno successivo non lo
/// azzera: solo una nuova dichiarazione lo sostituisce.
///
/// Perche' l'azzeramento faceva danno: l'unico consumatore legge lo stato FINALE,
/// quindi cancellare a meta' non "corregge" nulla — fabbrica un'ASTENSIONE
/// indistinguibile dal ruolo che non ha mai parlato. `classify_council_figure_result`
/// (mcp-core), `extract_vote` (decisions/adversarial_review.rs) ed `extract_position`
/// (decisions/debate_panel.rs) la contano come voto mancante, e sotto quorum il
/// panel esce `inconclusive`. Nel caso del revisore e' peggio: con zero review
/// `compose_panel_verdict` non emette alcuna nota, quindi un `fail` con findings
/// gravi diventa silenzio totale invece che prudenza.
///
/// Regola M: il segnale e' la presenza di una dichiarazione ACCETTATA dal
/// normalizzatore, mai la prosa del turno.
fn set_role_channel(target: &mut Option<Option<Value>>, declared: Option<&Value>) {
    if let Some(last) = declared {
        *target = Some(Some(last.clone()));
    }
    // Nessun ramo `else`: l'assenza di dichiarazione in QUESTO turno non ritratta
    // quella dei turni precedenti. Leggere quel silenzio come ritrattazione
    // sarebbe inferenza, non segnale.
}

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
/// Il titolo del meta_step, scritto per CHI LEGGE nel nastro attivita' e non
/// per chi ha scritto il codice: e' la sola riga che l'utente vede quando il
/// run si ferma, e «Gate duale su passo irreversible: NeedsHuman» non gli
/// dice ne' cosa e' successo ne' cosa deve fare. Composto DAI campi del
/// report (regola Q punto 3: struttura -> prosa, mai il contrario).
///
/// I tre casi che l'utente puo' incontrare sono diversi fra loro e vanno
/// distinti: nessun giudice disponibile (fatto d'ambiente, oggi il caso piu'
/// frequente per credito esaurito), giudizi non unanimi, rifiuto motivato.
fn titolo_per_umano(
    decision: &crate::decisions::step_gate::StepGateDecision,
    level: crate::decisions::step_gate::StepCriticality,
    report: &ports::StepValidationReport,
) -> String {
    use crate::decisions::step_gate::{StepGateDecision, StepVerdict};
    let passo = if level == crate::decisions::step_gate::StepCriticality::Irreversible {
        "irreversibile"
    } else {
        "critico"
    };
    match decision {
        StepGateDecision::Approved => {
            format!("Passo {passo} approvato dai validatori: l'esecuzione prosegue")
        }
        StepGateDecision::Rejected => {
            let motivo = report
                .verdicts
                .iter()
                .filter(|v| v.verdict == StepVerdict::Reject)
                .flat_map(|v| v.reasons.iter())
                .find_map(|r| r.get("description").and_then(Value::as_str))
                .unwrap_or("motivo non dettagliato");
            format!("Passo {passo} rifiutato dai validatori: {motivo}")
        }
        StepGateDecision::UnavailableDeclared => format!(
            "Passo {passo}: nessun verdetto utilizzabile, l'esecuzione prosegue dichiarandolo"
        ),
        StepGateDecision::NeedsHuman => titolo_serve_conferma(passo, report),
    }
}

/// Il caso che l'utente incontra piu' spesso: va detto PERCHE' tocca a lui
/// decidere, distinguendo l'indisponibilita' dei giudici dal loro disaccordo
/// (sono due situazioni diverse e portano a due azioni diverse).
fn titolo_serve_conferma(passo: &str, report: &ports::StepValidationReport) -> String {
    if report.verdicts.is_empty() {
        let perche = report
            .degraded
            .as_deref()
            .unwrap_or("nessun validatore convocabile");
        return format!(
            "Passo {passo}: serve la tua conferma perche' nessun validatore \
             indipendente ha potuto giudicarlo ({perche})"
        );
    }
    let riassunto: Vec<String> = report.verdicts.iter().map(riassunto_verdetto).collect();
    format!(
        "Passo {passo}: serve la tua conferma, i validatori non sono unanimi ({})",
        riassunto.join(", ")
    )
}

/// Come si racconta UN verdetto a chi legge: nome del provider e cosa ha
/// detto, con la causa quando non ha risposto (un «non ha risposto» senza il
/// perche' non aiuta a decidere).
fn riassunto_verdetto(v: &ports::ValidatorVerdict) -> String {
    use crate::decisions::step_gate::StepVerdict;
    match v.verdict {
        StepVerdict::Abstained => format!(
            "{} non ha risposto ({})",
            v.provider,
            v.abstain_cause.as_deref().unwrap_or("causa non dichiarata")
        ),
        StepVerdict::Approve => format!("{} approva", v.provider),
        StepVerdict::Reject => format!("{} rifiuta", v.provider),
        StepVerdict::NeedsHuman => format!("{} rimanda a te", v.provider),
    }
}

/// Un report senza convocati col degrado DICHIARATO (GAP-2: la matrice della
/// doppia astensione decide, mai un salto silenzioso).
fn report_degradato(motivo: &str) -> ports::StepValidationReport {
    ports::StepValidationReport {
        verdicts: Vec::new(),
        degraded: Some(motivo.to_string()),
    }
}

/// Payload slim del meta_step `step_validation` (i lettori sono il replay SSE
/// e le query di taratura: per ogni validatore provider, modello,
/// verdetto|astensione+causa e costo — GAP-2, il denominatore resta visibile).
fn payload_convocazione(
    decision: &crate::decisions::step_gate::StepGateDecision,
    level: crate::decisions::step_gate::StepCriticality,
    steps_slim: Vec<Value>,
    report: &ports::StepValidationReport,
    prior_rejections: u32,
    cap_raggiunto: bool,
) -> Value {
    let validators_slim: Vec<Value> = report
        .verdicts
        .iter()
        .map(|v| {
            json!({
                "role": v.role,
                "provider": v.provider,
                "model": v.model,
                "verdict": v.verdict,
                "abstain_cause": v.abstain_cause,
                "reasons": v.reasons,
                "safer_alternative": v.safer_alternative,
                "cost_usd": v.cost_usd,
            })
        })
        .collect();
    json!({
        "decision": decision,
        "level": level.as_str(),
        "steps": steps_slim,
        "validators": validators_slim,
        "degraded": report.degraded,
        "prior_rejections": prior_rejections,
        "cap_reached": cap_raggiunto,
    })
}

/// L'intestazione del rimando dice il motivo VERO: se nessun giudice ha
/// espresso un verdetto, «rifiutato dai validatori» sarebbe falso e non
/// aiuterebbe l'agente a cambiare strada. In autonomia questo testo e' tutto
/// cio' che riceve per decidere cosa fare dopo.
fn intestazione_rimando(report: &ports::StepValidationReport) -> String {
    use crate::decisions::step_gate::StepVerdict;
    let nessun_rifiuto = !report
        .verdicts
        .iter()
        .any(|v| v.verdict == StepVerdict::Reject);
    if nessun_rifiuto {
        let perche = report
            .degraded
            .as_deref()
            .unwrap_or("nessun validatore indipendente disponibile");
        return format!(
            "Il gate sui passi critici NON ha potuto autorizzare questo batch e \
             nessun tool e' stato eseguito: {perche}. In modalita' autonoma non \
             si chiede conferma all'utente, quindi il passo resta non eseguito."
        );
    }
    "Il gate di validazione sui passi critici ha RIFIUTATO questo batch: \
     nessun tool e' stato eseguito."
        .to_string()
}

/// Il testo del rimando del gate duale, composto DAI campi del report (regola
/// Q, punto 3: renderer struttura->prosa; il consumatore e' il modello).
fn testo_rimando(report: &ports::StepValidationReport) -> String {
    use crate::decisions::step_gate::StepVerdict;
    let mut motivi: Vec<String> = Vec::new();
    for v in report.verdicts.iter().filter(|v| v.verdict == StepVerdict::Reject) {
        for r in &v.reasons {
            motivi.push(format!(
                "[{}] {} ({})",
                r.get("severity").and_then(Value::as_str).unwrap_or("-"),
                r.get("description").and_then(Value::as_str).unwrap_or("-"),
                v.provider
            ));
        }
    }
    let mut testo = format!(
        "{}\nMotivi:\n{}",
        intestazione_rimando(report),
        if motivi.is_empty() {
            "- (nessun verdetto espresso dai validatori)".to_string()
        } else {
            motivi
                .iter()
                .map(|m| format!("- {m}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    );
    if let Some(a) = report
        .verdicts
        .iter()
        .find_map(|v| v.safer_alternative.clone())
    {
        testo.push_str(&format!("\nAlternativa piu' sicura proposta: {a}"));
    }
    testo.push_str(
        "\nRivedi il passo: proponi una variante piu' sicura o motiva nel piano \
         perche' il passo e' necessario cosi'.",
    );
    testo
}

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

fn tool_content_as_object(content: &Value) -> Option<Value> {
    match content {
        Value::Object(_) => Some(content.clone()),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_object),
        _ => None,
    }
}

/// PUNTO UNICO (regola L) dell'enforcement advisory: usato da `dispatch_subagent(s)`
/// e dal seed pre-run in `build_initial_state` (panel multi-provider / consiglio).
pub fn panel_enforcement_from_advisory_synthesis(
    advisory: &Value,
    source: &'static str,
) -> Option<Value> {
    build_advisory_enforcement(advisory, source).map(|e| e.to_value())
}

fn build_advisory_enforcement(advisory: &Value, source: &'static str) -> Option<PanelEnforcement> {
    let verdict = advisory.get("verdict").and_then(Value::as_str)?;
    match verdict {
        "block" | "inconclusive" => {
            // ADVISORY, NON BLOCCANTE (decisione prodotto 2026-07-13): un verdetto
            // "block"/"inconclusive" del consiglio/panel a monte e' un PARERE, non un
            // cancello. Il coordinatore LEGGE il segnale e PROCEDE incorporando i vincoli
            // (il consiglio e' advisory). PRIMA era `terminal: true` -> il veto di UNA
            // sola figura (es. sysadmin, 5/6 volevano procedere) fermava l'intero task
            // dopo la sola pianificazione, e il modello chiudeva con "Iniziero'..."
            // venendo troncato senza implementare nulla. Ora e' un vincolo FORTE ma non
            // bloccante: affronta PRIMA i blocker/rischi critici, poi implementa.
            // `declared_outcome: None` (niente pre-dichiarazione "blocked" che, col fix
            // graph 9e26d2a7, instraderebbe subito a FinalGate chiudendo il run).
            let summary = match source {
                "multi_provider_synthesis" => format!(
                    "Panel multi-provider: verdict={verdict}; affronta PRIMA i rischi critici/blocker \
                     segnalati (requisiti obbligatori), poi procedi con l'implementazione."
                ),
                _ => format!(
                    "Consiglio delle Competenze: verdict={verdict}; affronta PRIMA i blocker/rischi \
                     critici segnalati (requisiti obbligatori), poi procedi con l'implementazione."
                ),
            };
            Some(PanelEnforcement {
                source,
                verdict: verdict.to_string(),
                terminal: false,
                declared_outcome: None,
                summary,
                payload: advisory.clone(),
            })
        }
        "proceed_with_changes" => {
            let summary = match source {
                "multi_provider_synthesis" => {
                    "Panel multi-provider: procedere rispettando i requisiti obbligatori convergenti."
                        .to_string()
                }
                _ => {
                    "Consiglio delle Competenze: procedere rispettando i requisiti obbligatori del panel."
                        .to_string()
                }
            };
            Some(PanelEnforcement {
                source,
                verdict: verdict.to_string(),
                terminal: false,
                declared_outcome: None,
                summary,
                payload: advisory.clone(),
            })
        }
        _ => None,
    }
}

fn build_review_panel_enforcement(panel: &Value) -> Option<PanelEnforcement> {
    let verdict = panel.get("verdict").and_then(Value::as_str)?;
    if verdict == "pass" {
        return None;
    }
    let summary = format!(
        "Panel di review: verdict={verdict}; il lavoro non puo' essere trattato come approvato."
    );
    Some(PanelEnforcement {
        source: "panel_verdict",
        verdict: verdict.to_string(),
        terminal: false,
        declared_outcome: Some(json!({
            "outcome": "partial",
            "summary": summary,
            "next_step": "Correggere i findings del panel prima di dichiarare completato.",
        })),
        summary,
        payload: panel.clone(),
    })
}

/// Estrae l'enforcement dai verdict aggregati prodotti dai sub-agent. Priorita':
/// un veto advisory terminale prevale su qualunque review; poi review non-pass;
/// infine advisory proceed_with_changes come vincolo non bloccante.
fn panel_enforcement_from_results(
    pending: &[Value],
    results: &[ToolResultBlock],
) -> Option<PanelEnforcement> {
    let mut deferred_advisory: Option<PanelEnforcement> = None;
    for (block, result) in pending.iter().zip(results.iter()) {
        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
        if !DISPATCH_SUBAGENT_TOOLS.contains(&name) || result.is_error {
            continue;
        }
        let Some(content) = tool_content_as_object(&result.content) else {
            continue;
        };
        if let Some(advisory) = content.get("advisory_synthesis") {
            if let Some(enforcement) = build_advisory_enforcement(advisory, "advisory_synthesis")
            {
                if enforcement.terminal {
                    return Some(enforcement);
                }
                deferred_advisory = Some(enforcement);
            }
        }
        if let Some(panel) = content.get("panel_verdict") {
            if let Some(enforcement) = build_review_panel_enforcement(panel) {
                return Some(enforcement);
            }
        }
    }
    deferred_advisory
}

/// `true` se il content di un tool_result di `dispatch_subagent(s)` porta il
/// SEGNALE STRUTTURATO `background_dispatched: true` (regola M: campo booleano,
/// MAI parsing di prosa). Il content di questi tool arriva dal `ToolExecutor` come
/// stringa JSON (il tool ritorna `String`): si parsa la stringa in `Value` e si
/// legge il campo. Se il content e' gia' un oggetto (forma strutturata) lo si
/// legge diretto. Qualunque forma inattesa (parse fallito, campo assente) -> false
/// (fail-safe: nessuna sospensione -> comportamento bloccante storico).
fn content_signals_background(content: &Value) -> bool {
    let field = match content {
        Value::Object(_) => content.get(BACKGROUND_DISPATCHED_KEY).cloned(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.get(BACKGROUND_DISPATCHED_KEY).cloned()),
        _ => None,
    };
    field.and_then(|v| v.as_bool()).unwrap_or(false)
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
    // Prosa del messaggio al modello: i test la controllano perche' e' cio' che
    // il modello legge, MAI perche' il programma la usi per decidere.
    use crate::decisions::predictive_cap::PREDICTIVE_CAP_SENTINEL;
    use crate::routing::config::RoutingConfig;
    use crate::runtime::ports::{PortError, SseEvent, ToolOutcome};
    use crate::runtime::test_doubles::{
        NullEventSink, RecordingEventSink, StubAgentStepStore, StubContextOffload, StubLlmGateway,
        StubMetaStepStore, StubRunControlStore, StubTodoStore,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::state::{ContentBlock, Message, MessageContent};

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    /// Fase D fan-in (regola M): `content_signals_background` legge il segnale
    /// STRUTTURATO `background_dispatched` da entrambe le forme di content
    /// (stringa JSON come la produce il tool, oggetto JSON), MAI dalla prosa.
    /// Forme inattese / assenza / `false` -> `false` (fail-safe: nessuna
    /// sospensione, comportamento bloccante storico).
    #[test]
    fn content_signals_background_da_segnale_strutturato() {
        // Forma reale: il tool ritorna una String JSON.
        let as_string = Value::String(
            json!({"background_dispatched": true, "subagent_run_id": "abc", "kind": "coder"})
                .to_string(),
        );
        assert!(content_signals_background(&as_string));

        // Forma oggetto (content gia' strutturato).
        let as_object = json!({"background_dispatched": true, "kind": "review"});
        assert!(content_signals_background(&as_object));

        // Batch: segnale presente accanto agli altri campi.
        let batch = Value::String(
            json!({"count": 2, "background_dispatched": true, "child_run_ids": ["a", "b"]})
                .to_string(),
        );
        assert!(content_signals_background(&batch));

        // Assenza del campo -> false (dispatch sincrono classico).
        let sync_result =
            Value::String(json!({"subagent_run_id": "x", "status": "completed"}).to_string());
        assert!(!content_signals_background(&sync_result));

        // Segnale esplicitamente false -> false.
        let explicit_false = json!({"background_dispatched": false});
        assert!(!content_signals_background(&explicit_false));

        // Stringa non-JSON (prosa) -> false, MAI matching di testo.
        let prose = Value::String("dispatch background completato con successo".into());
        assert!(!content_signals_background(&prose));

        // Forma inattesa (numero) -> false.
        assert!(!content_signals_background(&json!(42)));
    }

    #[test]
    fn panel_enforcement_advisory_block_e_non_terminale() {
        // Il verdetto "block" del consiglio e' ADVISORY (fix 7a311454): produce un
        // enforcement (vincolo/nudge iniettato nel turno) ma NON ferma il run
        // (terminal=false, declared_outcome=None). Prima era terminal=true e il veto di
        // UNA figura chiudeva il task dopo la sola pianificazione ("Iniziero'...").
        let pending = vec![json!({"name": "dispatch_subagents"})];
        let results = vec![ToolResultBlock {
            tool_use_id: "t1".into(),
            content: Value::String(
                json!({
                    "advisory_synthesis": {
                        "verdict": "block",
                        "veto": true,
                        "risks": [{"severity": "alta", "description": "rischio"}]
                    }
                })
                .to_string(),
            ),
            is_error: false,
            exit_code: None,
            raw_content: None,
            motivo_blocco: None,
        }];

        let enforcement = panel_enforcement_from_results(&pending, &results)
            .expect("advisory block deve produrre un enforcement (vincolo advisory)");
        assert_eq!(enforcement.source, "advisory_synthesis");
        assert!(
            !enforcement.terminal,
            "il block del consiglio e' advisory: NON ferma il run"
        );
        assert!(
            enforcement.declared_outcome.is_none(),
            "nessuna pre-dichiarazione 'blocked' (instraderebbe subito a FinalGate)"
        );
    }

    #[test]
    fn panel_enforcement_review_non_pass_non_terminale() {
        let pending = vec![json!({"name": "dispatch_subagents"})];
        let results = vec![ToolResultBlock {
            tool_use_id: "t1".into(),
            content: json!({
                "panel_verdict": {
                    "verdict": "needs_changes",
                    "approved": false,
                    "findings": [{"file": "src/lib.rs", "severity": "media", "description": "bug"}]
                }
            }),
            is_error: false,
            exit_code: None,
            raw_content: None,
            motivo_blocco: None,
        }];

        let enforcement = panel_enforcement_from_results(&pending, &results)
            .expect("review non-pass deve produrre enforcement");
        assert_eq!(enforcement.source, "panel_verdict");
        assert!(!enforcement.terminal);
        assert_eq!(
            enforcement
                .declared_outcome
                .as_ref()
                .and_then(|v| v.get("outcome"))
                .and_then(Value::as_str),
            Some("partial")
        );
    }

    #[test]
    fn panel_enforcement_advisory_vincoli_non_blocca() {
        let pending = vec![json!({"name": "dispatch_subagents"})];
        let results = vec![ToolResultBlock {
            tool_use_id: "t1".into(),
            content: json!({
                "advisory_synthesis": {
                    "verdict": "proceed_with_changes",
                    "clear": false,
                    "requirements": ["usa il punto unico"]
                }
            }),
            is_error: false,
            exit_code: None,
            raw_content: None,
            motivo_blocco: None,
        }];

        let enforcement = panel_enforcement_from_results(&pending, &results)
            .expect("proceed_with_changes deve produrre vincolo");
        assert_eq!(enforcement.source, "advisory_synthesis");
        assert!(!enforcement.terminal);
        assert!(enforcement.declared_outcome.is_none());
    }

    /// Esecutore di tool a coda per il dispatch: mappa per nome del tool a un
    /// `ToolOutcome` (content) e registra le chiamate. Cosi' un test puo'
    /// restituire payload diversi per tool diversi e verificare l'ordine.
    struct MapToolExecutor {
        by_name: std::collections::HashMap<String, ToolOutcome>,
        default: ToolOutcome,
        pub seen: std::sync::Mutex<Vec<ToolCall>>,
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
        async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
            self.seen.lock().unwrap().push(call.clone());
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
        async fn execute(
            &self,
            _call: ToolCall,
        ) -> Result<ToolOutcome, PortError> {
            Err(PortError::Tool("grpc down".into()))
        }
    }

    fn ctx_with(cancel: CancellationToken) -> AgentNodeCtx {
        ctx_with_emit(cancel, Arc::new(NullEventSink))
    }

    /// Come [`ctx_with`] ma con un [`EventSink`] iniettabile (per asserire gli
    /// emit `ToolResult` del nodo): i test passano un `RecordingEventSink`.
    fn ctx_with_emit(
        cancel: CancellationToken,
        emit: Arc<dyn crate::runtime::ports::EventSink>,
    ) -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(MapToolExecutor::new()),
            emit,
            cfg: RoutingConfig::default(),
            cancel,
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
            step_gate: None,
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
            automation_mode: Some(crate::state::AutomationMode::Automatic),
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
            } => blocks
                .iter()
                .map(|b| serde_json::to_value(b).unwrap())
                .collect(),
            _ => vec![],
        }
    }

    // ── (2) pending vuoto -> end_turn ────────────────────────────────────────────

    #[tokio::test]
    async fn hitl_confirm_sospende_prima_dei_mutators() {
        let (n, steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        st.automation_mode = Some(crate::state::AutomationMode::Confirm);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.awaiting_confirmation, Some(true));
        assert!(
            out.extra
                .get(HITL_PENDING_ACTIONS_EXTRA_KEY)
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
        );
        assert!(steps.steps.lock().unwrap().is_empty(), "nessun tool eseguito");

        // L'ORIGINE della sospensione (rilievo A4): questa e' una revisione
        // umana ordinaria, e va dichiarata come tale — da lei dipende quale
        // `blocker` verrebbe dichiarato a una scadenza, e in Confirm dipende
        // che la scadenza non ci sia affatto.
        assert_eq!(
            out.extra
                .get(crate::decisions::SUSPENSION_ORIGIN_EXTRA_KEY)
                .and_then(|v| v.as_str()),
            Some(crate::decisions::SuspensionOrigin::HumanReview.as_str()),
            "una sospensione HITL ordinaria non e' del gate duale"
        );
        // La trappola che questo test blindava — usare il marker di batch come
        // firma del gate duale, dando una scadenza anche alle sospensioni di
        // Confirm — non e' piu' rappresentabile: il marker non esiste piu'.
        // Scritto ALLA SOSPENSIONE, faceva passare il batch al rientro nel
        // dispatch (`rm -rf` eseguito 482ms dopo il proprio NeedsHuman, run
        // 77fcff4a); il permesso e' ora il campo tipizzato
        // `AgentState::step_gate_human_ok`, scritto dal RESUME. Resta cio' che
        // il test prova davvero: l'ORIGINE della sospensione, asserita sopra.
        assert!(
            !out.extra
                .contains_key(crate::decisions::step_gate::STEP_GATE_VERDICTS_EXTRA_KEY),
            "nessun verdetto: qui il gate duale non ha deliberato"
        );
    }

    // ── Barriera di scrittura advisory (overlap, mig 0606) ───────────────────

    /// Ctx con la barriera armata sullo stato dato.
    fn ctx_with_gate(
        state: AdvisoryGateState,
    ) -> (
        AgentNodeCtx,
        tokio::sync::watch::Sender<AdvisoryGateState>,
    ) {
        let (tx, rx) = tokio::sync::watch::channel(state);
        let mut ctx = ctx_with(CancellationToken::new());
        ctx.advisory_gate = Some(rx);
        (ctx, tx)
    }

    fn enforcement_terminale() -> Value {
        json!({
            "source": "council_synthesis",
            "verdict": "block",
            "terminal": true,
            "summary": "Il consiglio veta: il fix e' una toppa.",
            "payload": {},
        })
    }

    fn writer_node() -> (ToolDispatchNode, Arc<StubAgentStepStore>) {
        let (n, steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(
                MapToolExecutor::new().with(
                    "write_file",
                    ToolOutcome {
                        tool_call_id: "w1".into(),
                        content: json!("scritto"),
                        is_error: false,
                        ..Default::default()
                    },
                ),
            ),
        );
        (n, steps)
    }

    #[tokio::test]
    async fn barriera_i_tool_read_only_non_attendono_il_consiglio() {
        // E' il senso dell'overlap: la ricognizione parte subito. Con la barriera
        // ancora Pending un read_file DEVE eseguire senza attendere (se
        // attendesse, il test andrebbe in timeout).
        let (n, steps, _rc) = node(
            ToolDispatchConfig {
                // Se per errore attendesse, si vedrebbe: 1s e non 300.
                advisory_gate_timeout_s: 1,
                ..ToolDispatchConfig::default()
            },
            Arc::new(
                MapToolExecutor::new().with(
                    "read_file",
                    ToolOutcome {
                        tool_call_id: "r1".into(),
                        content: json!("contenuto"),
                        is_error: false,
                        ..Default::default()
                    },
                ),
            ),
        );
        let (ctx, _tx) = ctx_with_gate(AdvisoryGateState::Pending);
        let st = state_with_pending(vec![pending_tool(
            "r1",
            "read_file",
            json!({"path": "src/main.rs"}),
        )]);
        let started = std::time::Instant::now();
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(900),
            "un tool read-only NON deve attendere la barriera"
        );
        assert_eq!(steps.steps.lock().unwrap().len(), 1, "il read ha eseguito");
        assert!(
            !out.extra.contains_key(ADVISORY_GATE_KEY),
            "la barriera non si e' nemmeno consultata"
        );
    }

    #[tokio::test]
    async fn barriera_la_prima_scrittura_attende_e_poi_procede() {
        let (n, steps) = writer_node();
        let (ctx, tx) = ctx_with_gate(AdvisoryGateState::Pending);
        // Il consiglio delibera mentre il nodo e' gia' in attesa.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            let _ = tx.send(AdvisoryGateState::Released {
                enforcement: Some(json!({
                    "verdict": "proceed_with_changes",
                    "summary": "Usa il punto unico esistente.",
                    "terminal": false,
                })),
            });
        });
        let st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(steps.steps.lock().unwrap().len(), 1, "la scrittura procede");
        assert_eq!(
            out.extra
                .get(ADVISORY_GATE_KEY)
                .and_then(|v| v.get("status"))
                .and_then(Value::as_str),
            Some("released")
        );
        // I requisiti arrivano al modello NELLO STESSO turno in cui ha scritto.
        let blocks = blocks_of(out.messages.last().expect("messaggio"));
        let testo = serde_json::to_string(&blocks).unwrap();
        assert!(
            testo.contains("punto unico esistente"),
            "il promemoria dei vincoli deve raggiungere il modello: {testo}"
        );
    }

    #[tokio::test]
    async fn barriera_il_veto_ferma_prima_della_prima_modifica() {
        let (n, steps) = writer_node();
        let (ctx, _tx) = ctx_with_gate(AdvisoryGateState::Vetoed {
            enforcement: enforcement_terminale(),
        });
        let st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert!(
            steps.steps.lock().unwrap().is_empty(),
            "NESSUN file deve essere toccato dopo un veto"
        );
        // L'edge esistente (graph.rs: terminal_panel_veto -> Learner) legge questo.
        assert_eq!(
            out.extra
                .get(PANEL_ENFORCEMENT_KEY)
                .and_then(|v| v.get("terminal"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            out.extra
                .get(ADVISORY_GATE_KEY)
                .and_then(|v| v.get("status"))
                .and_then(Value::as_str),
            Some("vetoed")
        );
        assert_eq!(out.stop_reason, Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn barriera_il_timeout_non_blocca_mai_il_run() {
        // Un panel che non risponde NON deve congelare il run: si procede, ma
        // dichiarando che NON c'e' approvazione (regola M).
        let (n, steps) = writer_node();
        let (ctx, _tx) = ctx_with_gate(AdvisoryGateState::Pending);
        let n = ToolDispatchNode {
            cfg: ToolDispatchConfig {
                advisory_gate_timeout_s: 1,
                ..ToolDispatchConfig::default()
            },
            ..n
        };
        let st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(steps.steps.lock().unwrap().len(), 1, "il run prosegue");
        let gate = out.extra.get(ADVISORY_GATE_KEY).expect("esito dichiarato");
        assert_eq!(gate.get("status").and_then(Value::as_str), Some("unavailable"));
        assert_eq!(
            gate.get("reason_code").and_then(Value::as_str),
            Some("advisory_gate_timeout"),
            "il motivo e' un segnale, non una deduzione"
        );
        let testo = serde_json::to_string(&blocks_of(out.messages.last().expect("msg"))).unwrap();
        assert!(
            testo.contains("NON hai"),
            "il modello deve sapere che procede senza approvazione: {testo}"
        );
    }

    #[tokio::test]
    async fn barriera_sender_droppato_non_appende_il_run() {
        // Il task dei panel muore senza dichiarare nulla: il canale si chiude.
        // Un'assenza non e' un'approvazione, ma non deve nemmeno bloccare.
        let (n, steps) = writer_node();
        let (ctx, tx) = ctx_with_gate(AdvisoryGateState::Pending);
        drop(tx);
        let st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            n.run(&st, &ctx),
        )
        .await
        .expect("il run non deve appendere sul sender caduto")
        .expect("run ok");
        let out = apply(st, out);
        assert_eq!(steps.steps.lock().unwrap().len(), 1);
        assert_eq!(
            out.extra
                .get(ADVISORY_GATE_KEY)
                .and_then(|v| v.get("reason_code"))
                .and_then(Value::as_str),
            Some("advisory_channel_closed")
        );
    }

    #[tokio::test]
    async fn barriera_assente_e_bit_identica_al_ramo_classico() {
        // Flag OFF (nessun canale nel ctx): il gate non esiste, la scrittura
        // procede come prima e nessuna chiave sporca lo stato.
        let (n, steps) = writer_node();
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(steps.steps.lock().unwrap().len(), 1);
        assert!(!out.extra.contains_key(ADVISORY_GATE_KEY));
    }

    #[tokio::test]
    async fn hitl_automatico_non_sospende() {
        let (n, steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(
                MapToolExecutor::new().with(
                    "write_file",
                    ToolOutcome {
                        tool_call_id: "w1".into(),
                        content: json!("ok"),
                        is_error: false,
                        ..Default::default()
                    },
                ),
            ),
        );
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "src/main.rs"}),
        )]);
        st.automation_mode = Some(crate::state::AutomationMode::Automatic);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_ne!(out.awaiting_confirmation, Some(true));
        assert!(!steps.steps.lock().unwrap().is_empty(), "tool eseguito");
    }

    #[tokio::test]
    async fn pending_vuoto_end_turn() {
        let (n, steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
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
        let ctx = ctx_with_emit(CancellationToken::new(), sink.clone());
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
        assert_eq!(
            results.len(),
            2,
            "un ToolResult per tool, eventi: {events:?}"
        );
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
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("a", "read_file", json!({}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.stop_reason, Some(StopReason::Superseded));
        assert_eq!(out.pending_tool_uses, Some(vec![]));
    }

    // ── (1b) cancel del ctx -> superseded (senza interrogare lo store) ───────────

    #[tokio::test]
    async fn cancel_token_esce_cooperativo() {
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = ctx_with(cancel);
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
        let ctx = ctx_with(CancellationToken::new());
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
        let ctx = ctx_with(CancellationToken::new());
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

    /// Il PRODUTTORE REALE davanti alla persistenza: si esegue il nodo e si
    /// guarda cosa arriva allo store, non un record costruito a mano.
    ///
    /// E' la lezione della regola O su questo difetto. I test dell'impl
    /// (`agent_step_store.rs`) fabbricavano il record nella forma che l'impl si
    /// aspettava — `{"name","input"}` — mentre questo percorso ne ha sempre
    /// prodotta un'altra: i due lati erano incompatibili dal primo giorno e i
    /// test restavano verdi, perche' misuravano un'imitazione del produttore.
    /// Misurato il 02/08/2026 sul DB di bacheca-attivita: 8860 righe su 8860 con
    /// `tool_name` vuoto e `status='completed'`, 536 fallimenti reali compresi.
    ///
    /// PROVE DI MUTAZIONE, in `persist_turn_steps`:
    /// - `tool_name: b.get("nome_sbagliato")...` -> la prima asserzione rosseggia
    ///   con `""`, il valore che il difetto scriveva su ogni riga;
    /// - `status: StepStatus::Completed` fisso -> l'ultima rosseggia con
    ///   `Completed` su un tool fallito, che e' esattamente cio' che i quattro
    ///   consumatori a valle leggevano.
    #[tokio::test]
    async fn persistenza_porta_nome_del_tool_ed_esito_del_risultato() {
        let tools = Arc::new(
            MapToolExecutor::new()
                .with(
                    "read_file",
                    ToolOutcome {
                        tool_call_id: "c1".into(),
                        content: json!("{\"text\":\"contenuto\"}"),
                        is_error: false,
                        ..Default::default()
                    },
                )
                .with(
                    "edit_file",
                    ToolOutcome {
                        tool_call_id: "c2".into(),
                        content: json!("{\"error\":\"file non trovato\"}"),
                        is_error: true,
                        ..Default::default()
                    },
                ),
        );
        let (n, steps, _rc) = node(ToolDispatchConfig::default(), tools);
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![
            pending_tool("c1", "read_file", json!({"path": "a.rs"})),
            pending_tool("c2", "edit_file", json!({"path": "b.rs"})),
        ]);
        let _ = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        let persisted = steps.steps.lock().unwrap();
        assert_eq!(persisted.len(), 2, "un passo persistito per tool eseguito");
        assert_eq!(
            persisted[0].2.tool_name, "read_file",
            "il nome del tool arriva alla persistenza"
        );
        assert_eq!(persisted[1].2.tool_name, "edit_file");
        assert_eq!(
            persisted[1].2.tool_input,
            json!({"path": "b.rs"}),
            "l'input arriva PIATTO: e' la forma che i consumatori interrogano"
        );
        assert_eq!(persisted[0].2.status, StepStatus::Completed);
        assert_eq!(
            persisted[1].2.status,
            StepStatus::Failed,
            "un tool fallito si persiste come fallito, non come riuscito"
        );
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
        let ctx = ctx_with(CancellationToken::new());
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
        let ctx = ctx_with(CancellationToken::new());
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
        let ctx = ctx_with(CancellationToken::new());
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
        let ctx = ctx_with(CancellationToken::new());
        // Una chiamata bloccata dal cap + un task_complete outcome=blocked.
        let st = state_with_pending(vec![
            pending_tool("c1", "nexus_read_attachment", json!({"length": 100000})),
            pending_tool(
                "c2",
                "task_complete",
                json!({"outcome": "blocked", "summary": "stop"}),
            ),
        ]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // La dichiarazione blocked viene rifiutata (declared_outcome NON settato).
        assert_eq!(out.declared_outcome, None);
        assert_eq!(out.blocked_cap_rejected, Some(true));
        // Il tool_result del task_complete e' marcato is_error con la reason.
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let tc = blocks
            .iter()
            .find(|b| b["tool_use_id"] == json!("c2"))
            .unwrap();
        assert!(tc["is_error"].as_bool().unwrap());
        assert!(tc["content"].as_str().unwrap().contains("RIFIUTATA"));
    }

    /// Un tool che RESTITUISCE il testo del cap non ha SUBITO il cap: la
    /// dichiarazione `blocked` del modello resta in piedi.
    ///
    /// E' la trappola armata dal criterio testuale: il guard cercava
    /// [`PREDICTIVE_CAP_SENTINEL`] in un qualunque tool_result del turno, e quel
    /// testo puo' arrivare da un `read_file` su questo sorgente, da un grep, dal
    /// resoconto di un sub-run che cita il cap. Chi lo restituiva vedeva
    /// annullata la propria dichiarazione di blocco e il run ripartiva su un
    /// blocco vero.
    ///
    /// La finestra qui e' ampia (nessuna chiamata viene rifiutata): l'unica
    /// occorrenza della stringa e' nel CONTENUTO di un tool riuscito.
    ///
    /// PROVA DI MUTAZIONE: rimettendo in `should_reject_blocked_from_cap` il
    /// criterio testuale (`results.iter().any(|r|
    /// value_as_json_string(&r.content).contains(PREDICTIVE_CAP_SENTINEL))`) al
    /// posto del campo, questo test rosseggia su `declared_outcome == None` e
    /// `blocked_cap_rejected == Some(true)`.
    #[tokio::test]
    async fn un_tool_che_cita_il_cap_non_annulla_la_dichiarazione_di_blocco() {
        let cfg = ToolDispatchConfig {
            context_window: 10_000_000,
            predictive_cap_ratio: 0.8,
            ..Default::default()
        };
        // Il sorgente letto contiene la frase del cap: e' un CONTENUTO, non un
        // rifiuto. Passa dal produttore vero (la porta `ToolExecutor`).
        let tools = Arc::new(MapToolExecutor::new().with(
            "read_file",
            ToolOutcome {
                tool_call_id: "r1".into(),
                content: Value::String(format!(
                    "pub const PREDICTIVE_CAP_SENTINEL: &str = \"{PREDICTIVE_CAP_SENTINEL}\";"
                )),
                is_error: false,
                ..Default::default()
            },
        ));
        let (n, _steps, _rc) = node(cfg, tools.clone());
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![
            pending_tool("r1", "read_file", json!({"path": "predictive_cap.rs"})),
            pending_tool(
                "c2",
                "task_complete",
                json!({"outcome": "blocked", "summary": "manca la credenziale del provider"}),
            ),
        ]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        assert_eq!(tools.seen.lock().expect("lock").len(), 1, "read_file eseguito");
        assert_eq!(
            out.declared_outcome
                .as_ref()
                .and_then(|v| v.get("outcome"))
                .and_then(Value::as_str),
            Some("blocked"),
            "nessuna chiamata e' stata rifiutata dal cap: la dichiarazione vale"
        );
        assert_eq!(out.blocked_cap_rejected, None);
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
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({"path": "a"}))]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        assert!(blocks[0]["content"]
            .as_str()
            .unwrap()
            .contains("nexus_mcp_tool_search"));
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
        let ctx = ctx_with(CancellationToken::new());
        let mut st =
            state_with_pending(vec![pending_tool("c1", "nexus_read_attachment", json!({}))]);
        st.attachment_read_bytes = Some(2000); // gia' oltre il budget.
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        assert!(blocks[0]["content"]
            .as_str()
            .unwrap()
            .contains("budget letture allegati esaurito"));
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    // ── run_notes / task_complete brain-only (non via ToolExecutor) ──────────────

    #[tokio::test]
    async fn brain_only_run_notes_e_task_complete() {
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), tools.clone());
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![
            pending_tool(
                "c1",
                "nexus_run_notes",
                json!({"action": "set", "content": "appunto"}),
            ),
            pending_tool(
                "c2",
                "task_complete",
                json!({"outcome": "done", "summary": "ok"}),
            ),
        ]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // run_notes persistito; declared_outcome=done; declared_done_count=1.
        assert_eq!(out.run_notes.as_deref(), Some("appunto"));
        assert_eq!(
            out.declared_outcome.as_ref().unwrap()["outcome"],
            json!("done")
        );
        assert_eq!(out.declared_done_count, Some(1));
        // Timbro di freschezza (regola O: verificato sul PRODUTTORE reale, non
        // costruito a mano dal test del consumatore in executor/tests.rs):
        // state_with_pending non imposta iterations -> unwrap_or(0) = 0.
        assert_eq!(out.declared_outcome_iteration, Some(0));
        // Nessun tool eseguito via ToolExecutor (entrambi brain-only).
        assert!(tools.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dichiarazione_stantia_invalidata_da_lavoro_successivo() {
        // ADR 0034: se il run PROSEGUE con altri tool dopo una dichiarazione
        // precedente, quella dichiarazione era intermedia -> azzerata. Senza,
        // un "partial"/"blocked" dichiarato a meta' run falsava lo status
        // canonico finale anche a lavoro poi completato.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.declared_outcome = Some(json!({"outcome": "partial", "summary": "meta'"}));
        st.declared_outcome_iteration = Some(0);
        st.declared_done_count = Some(0);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Dichiarazione stantia azzerata; il contatore done resta cumulativo.
        assert!(out.declared_outcome.is_none());
        // Il timbro di freschezza va azzerato INSIEME alla dichiarazione: se
        // restasse valorizzato, un futuro declared_outcome scritto altrove
        // potrebbe leggerlo per errore come ancora fresco.
        assert!(out.declared_outcome_iteration.is_none());
    }

    #[tokio::test]
    async fn parere_consultivo_sopravvive_al_lavoro_successivo() {
        // A DIFFERENZA di `declared_outcome` (test sopra): il parere di una figura
        // e' un CONTRIBUTO, non lo stato del run. Sequenza reale del 20/07 che
        // questo test riproduce: `functional_analyst` dichiara alle 06:12:25, poi
        // legge cinque file alle 06:12:35, e finiva `CompletedNoAdvisory` col
        // parere buttato — sotto quorum per un voto che il sistema aveva ricevuto.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.advisory_verdict = Some(json!({"verdict": "proceed", "summary": "gia' dichiarato"}));

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        // Mutazione che rende rosso: rimettere il ramo
        // `else if state.advisory_verdict.is_some() { delta.advisory_verdict = Some(None) }`.
        assert!(
            out.advisory_verdict.is_some(),
            "il parere non va cancellato da un tool successivo"
        );
        assert_eq!(
            out.advisory_verdict.as_ref().expect("parere")["verdict"],
            json!("proceed")
        );
    }

    #[tokio::test]
    async fn una_nuova_dichiarazione_sostituisce_la_precedente() {
        // L'unica cosa che cambia un parere e' un altro parere: la figura che si
        // ricrede resta libera di farlo, e vale l'ULTIMA dichiarazione.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool(
            "c1",
            "advisory_verdict",
            json!({
                "verdict": "block",
                "summary": "ho trovato un difetto",
                "risks": [{"description": "segreto in chiaro nel repo", "severity": "alta"}]
            }),
        )]);
        st.advisory_verdict = Some(json!({"verdict": "proceed", "summary": "parere precedente"}));

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        assert_eq!(
            out.advisory_verdict.as_ref().expect("parere")["verdict"],
            json!("block"),
            "l'ultima dichiarazione prevale"
        );
    }

    #[tokio::test]
    async fn anche_verdetto_revisore_e_posizione_avvocato_sopravvivono() {
        // Stessa classe del parere: sono contributi su un oggetto ESTERNO al run.
        // Per il revisore l'azzeramento era il piu' insidioso: con zero review
        // `compose_panel_verdict` non emette alcuna nota, quindi un `fail` con
        // findings gravi non diventava prudenza, diventava silenzio.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.review_verdict = Some(json!({"verdict": "fail", "summary": "difetto grave"}));
        st.debate_position = Some(json!({"stance": "oppose", "summary": "cedo la tesi"}));

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        // Mutazione che rende rosso: rimettere il ramo `else if ... = Some(None)`
        // su uno dei due canali in `set_role_channel`.
        assert_eq!(
            out.review_verdict.as_ref().expect("verdetto")["verdict"],
            json!("fail"),
            "il verdetto del revisore non va cancellato dal lavoro successivo"
        );
        assert_eq!(
            out.debate_position.as_ref().expect("posizione")["stance"],
            json!("oppose"),
            "l'oppose e' il segnale piu' prezioso del dibattito: non va perso"
        );
    }

    #[tokio::test]
    async fn un_parere_rifiutato_dice_al_modello_come_correggerlo() {
        // Prima il modello riceveva solo {"acknowledged": false, "verdict": null}:
        // non sapendo COSA fosse sbagliato riprovava alla cieca. Il test arriva al
        // contenuto del tool_result, cioe' a cio' che il modello legge davvero.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool(
            "c1",
            "advisory_verdict",
            json!({"verdict": "proceed_with_caution", "summary": "quasi giusto"}),
        )]);

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().expect("content");

        assert!(
            blocks[0]["is_error"].as_bool().unwrap_or(false),
            "il rifiuto resta un segnale strutturato per il codice"
        );
        // Mutazione che rende rosso: togliere l'inserimento di "reason" in
        // `declarative_tool_result`, o riportare normalize_* a `Option` (che
        // perde la ragione alla fonte).
        assert!(
            content.contains("proceed_with_caution"),
            "il messaggio deve citare il valore rifiutato: {content}"
        );
        for ammesso in ["proceed", "proceed_with_changes", "block"] {
            assert!(
                content.contains(ammesso),
                "il messaggio deve elencare i valori ammessi, manca {ammesso}: {content}"
            );
        }
        // Il parere non e' stato acquisito: il canale resta muto.
        assert!(out.advisory_verdict.is_none());
    }

    #[tokio::test]
    async fn task_complete_rifiutato_elenca_gli_outcome_ammessi() {
        // Caso reale: un modello ha dichiarato outcome="success" (fuori enum)
        // tre volte identiche, perche' riceveva solo acknowledged=false senza
        // sapere che i valori ammessi sono done|blocked|needs_input|partial.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool(
            "c1",
            "task_complete",
            json!({"outcome": "success", "summary": "fatto tutto"}),
        )]);

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().expect("content");

        assert!(blocks[0]["is_error"].as_bool().unwrap_or(false));
        // Mutazione che rende rosso: riportare il call site alla vecchia forma
        // {"acknowledged": false, "outcome": null} senza reason.
        assert!(
            content.contains("success"),
            "il valore rifiutato va citato: {content}"
        );
        for ammesso in ["done", "blocked", "needs_input", "partial"] {
            assert!(content.contains(ammesso), "manca {ammesso}: {content}");
        }
        assert!(out.declared_outcome.is_none(), "la dichiarazione non e' acquisita");
    }

    #[tokio::test]
    async fn un_veto_senza_evidenza_spiega_cosa_manca() {
        // Regola non ovvia: `block` esige almeno un rischio con descrizione. Se il
        // rifiuto non lo dice, il modello rilegge l'enum, lo trova corretto, e
        // ripete lo stesso errore.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool(
            "c1",
            "advisory_verdict",
            json!({"verdict": "block", "summary": "non si puo' fare"}),
        )]);

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().expect("content");

        assert!(blocks[0]["is_error"].as_bool().unwrap_or(false));
        assert!(
            content.contains("risks"),
            "il messaggio deve nominare la lista mancante: {content}"
        );
    }

    // ── (5) errore infrastruttura -> tool_result d'errore, niente NodeError ──────

    #[tokio::test]
    async fn errore_porta_non_propaga_node_error() {
        let (n, _steps, _rc) = node(ToolDispatchConfig::default(), Arc::new(FailingToolExecutor));
        let ctx = ctx_with(CancellationToken::new());
        let st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        let out = apply(
            st.clone(),
            n.run(&st, &ctx).await.expect("run NON deve fallire"),
        );
        let blocks = blocks_of(out.messages.last().expect("msg"));
        assert!(blocks[0]["is_error"].as_bool().unwrap());
        assert!(blocks[0]["content"].as_str().unwrap().contains("grpc down"));
    }

    // ── (11) discovered_tools_next_turn SEMPRE scritto (anche []) ────────────────

    #[tokio::test]
    async fn discovered_sempre_scritto_anche_vuoto() {
        // Un read_file qualunque: nessun search -> discovered_next = [] ma SCRITTO.
        let (n, _steps, _rc) = node(
            ToolDispatchConfig::default(),
            Arc::new(MapToolExecutor::new()),
        );
        let ctx = ctx_with(CancellationToken::new());
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
        let ctx = ctx_with(CancellationToken::new());
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

    // ── (10) reminder TODO iniettato alla soglia ─────────────────────────────────

    #[tokio::test]
    async fn reminder_todo_iniettato_alla_soglia() {
        // StubTodoStore default ritorna None da build_reminder_text; serve uno
        // store che ritorna un testo. Usiamo un piccolo stub locale.
        struct ReminderStore;
        #[async_trait]
        impl TodoStore for ReminderStore {
            async fn list_todos(
                &self,
                _r: &str,
            ) -> Result<Vec<crate::decisions::dag_scheduler::Todo>, PortError> {
                Ok(vec![])
            }
            async fn mark_status(
                &self,
                _id: &str,
                _s: crate::decisions::dag_scheduler::TodoStatus,
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
        let ctx = ctx_with(CancellationToken::new());
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.plan_phase_active = Some(true);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        // Counter resettato a 0 dopo l'injection.
        assert_eq!(out.since_last_todo_reminder, Some(0));
        // Il blocco reminder e' appeso ai blocchi (ContentBlock::Text col tag).
        let last = out.messages.last().expect("msg");
        if let Message::Human {
            content: MessageContent::Blocks(blocks),
        } = last
        {
            let has_reminder = blocks.iter().any(
                |b| matches!(b, ContentBlock::Text { text } if text.contains("system-reminder")),
            );
            assert!(has_reminder, "il reminder deve essere appeso ai blocchi");
        } else {
            panic!("atteso HumanMessage a blocchi");
        }
    }

    // ── Il focus del turno NON segue i messaggi che questo nodo produce ─────────

    /// REGRESSIONE: la directive "FOCUS DEL TURNO CORRENTE" dichiara al modello
    /// quale sia la richiesta dell'utente ADESSO. Era costruita sull'ULTIMO
    /// `Message::Human` della cronologia — che da questo nodo in poi non e' piu'
    /// l'utente: e' il messaggio di risultati che il nodo stesso produce, con i
    /// tool_result e i promemoria `<system-reminder>` appesi in coda.
    ///
    /// La history del test la scrive il NODO REALE (regola O): niente
    /// `Message::Human` fabbricato a mano, altrimenti si verificherebbe la
    /// propria idea del messaggio di risultati invece di quello vero — e la
    /// forma esatta (blocchi tipizzati + blocco di testo col tag) e' proprio cio'
    /// che rendeva il difetto invisibile.
    ///
    /// Verifica per mutazione: rimettendo l'euristica "ultimo Human" la directive
    /// cita il promemoria dei todo al posto della richiesta.
    #[tokio::test]
    async fn il_focus_del_turno_ignora_i_messaggi_prodotti_dal_dispatch() {
        use crate::decisions::turn_focus::build_turn_focus_directive;
        use crate::decisions::turn_task::ORIGINAL_TASK_KEY;

        struct ReminderStore;
        #[async_trait]
        impl TodoStore for ReminderStore {
            async fn list_todos(
                &self,
                _r: &str,
            ) -> Result<Vec<crate::decisions::dag_scheduler::Todo>, PortError> {
                Ok(vec![])
            }
            async fn mark_status(
                &self,
                _id: &str,
                _s: crate::decisions::dag_scheduler::TodoStatus,
            ) -> Result<(), PortError> {
                Ok(())
            }
            async fn build_reminder_text(&self, _r: &str) -> Result<Option<String>, PortError> {
                Ok(Some("CHECKLIST: 1) rifai il layout della dashboard".to_string()))
            }
        }

        let cfg = ToolDispatchConfig {
            todo_reminder_every_n_steps: 1,
            ..Default::default()
        };
        let tools = Arc::new(MapToolExecutor::new().with(
            "read_file",
            ToolOutcome {
                tool_call_id: "c1".into(),
                content: Value::String("export const dashboard = 1;".into()),
                is_error: false,
                ..Default::default()
            },
        ));
        let n = ToolDispatchNode::new(
            cfg,
            tools,
            Arc::new(StubAgentStepStore::default()),
            Arc::new(StubRunControlStore::default()),
            Arc::new(ReminderStore),
            Arc::new(StubContextOffload::default()),
            Arc::new(StubMetaStepStore::default()),
        );
        let ctx = ctx_with(CancellationToken::new());

        // Stato come lo fissa `native_engine::build_initial_state`: la richiesta
        // del turno in `extra`, e la stessa in coda ai messaggi.
        const RICHIESTA: &str = "Aggiungi il filtro per data alla lista spese";
        let mut st = state_with_pending(vec![pending_tool("c1", "read_file", json!({}))]);
        st.plan_phase_active = Some(true);
        st.extra
            .insert(ORIGINAL_TASK_KEY.to_string(), json!(RICHIESTA));
        st.messages = vec![Message::Human {
            content: MessageContent::text(RICHIESTA),
        }];

        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        // PREMESSA del test, verificata invece che assunta: il nodo ha davvero
        // messo in coda un Human coi risultati e col promemoria.
        let ultimo = out.messages.last().expect("il nodo appende un messaggio");
        assert!(
            matches!(ultimo, Message::Human { .. }),
            "il messaggio dei risultati ha ruolo user sul canale interno"
        );
        let blocchi = blocks_of(ultimo);
        assert!(
            blocchi.iter().any(|b| b["type"] == "tool_result"),
            "atteso il tool_result nel messaggio prodotto: {blocchi:?}"
        );
        assert!(
            blocchi.iter().any(|b| b["text"]
                .as_str()
                .is_some_and(|t| t.contains("system-reminder"))),
            "atteso il promemoria appeso dal nodo: {blocchi:?}"
        );

        // La directive resta ancorata alla richiesta.
        let focus = build_turn_focus_directive(&out, false).expect("directive");
        assert!(
            focus.contains(RICHIESTA),
            "il focus deve citare la richiesta dell'utente, era: {focus}"
        );
        assert!(
            !focus.contains("system-reminder"),
            "il focus dichiarava un promemoria di sistema come richiesta: {focus}"
        );
        assert!(
            !focus.contains("CHECKLIST"),
            "il focus dichiarava il promemoria dei todo come richiesta: {focus}"
        );
        assert!(
            !focus.contains("export const dashboard"),
            "il focus dichiarava l'output di un tool come richiesta: {focus}"
        );
    }

    // ── (9) context-budget cap: troncamento aggressivo con offload ───────────────

    #[tokio::test]
    async fn context_budget_cap_tronca_e_offloada() {
        let cfg = ToolDispatchConfig {
            max_context_chars: 100,           // soglia minuscola -> forza il cap.
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
        let ctx = ctx_with(CancellationToken::new());
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

    // ── (2a) Gate duale sui passi critici (mig 0677) ─────────────────────────

    /// Porta stub del gate duale: risponde col report configurato.
    struct StubStepGate {
        report: ports::StepValidationReport,
        chiamate: std::sync::Mutex<u32>,
    }

    #[async_trait]
    impl ports::StepValidationPort for StubStepGate {
        async fn validate(
            &self,
            _req: ports::StepValidationRequest,
        ) -> Result<ports::StepValidationReport, PortError> {
            *self.chiamate.lock().unwrap() += 1;
            Ok(self.report.clone())
        }
    }

    fn verdetto(role: &str, v: crate::decisions::step_gate::StepVerdict) -> ports::ValidatorVerdict {
        ports::ValidatorVerdict {
            role: role.into(),
            provider: if role == "gatekeeper" { "openai" } else { "google" }.into(),
            model: "m".into(),
            verdict: v,
            reasons: vec![json!({"severity": "alta", "description": "bersaglio fuori progetto"})],
            safer_alternative: Some("usa il filtro label del progetto".into()),
            abstain_cause: None,
            cost_usd: Some(0.001),
        }
    }

    fn regole_kill() -> Vec<crate::decisions::step_gate::CriticalityRule> {
        crate::decisions::step_gate::parse_rules(
            r#"[{"matcher_kind":"command_token","pattern":"rm -rf","level":"irreversible","category":"destructive_fs"}]"#,
        )
    }

    fn cfg_gate() -> ToolDispatchConfig {
        ToolDispatchConfig {
            step_gate_mode: crate::decisions::step_gate::StepGateMode::EnforceIrreversible,
            step_gate_rules: regole_kill(),
            ..ToolDispatchConfig::default()
        }
    }

    fn ctx_con_gate(gate: Arc<StubStepGate>) -> AgentNodeCtx {
        AgentNodeCtx {
            step_gate: Some(gate),
            ..ctx_with(CancellationToken::new())
        }
    }

    /// REJECT: nessun tool eseguito, ogni pending riceve il synthetic coi
    /// motivi, il contatore dei rimandi sale. Mutazione: eseguire comunque i
    /// tool col reject -> l'esecutore stub registrerebbe la chiamata -> rosso.
    #[tokio::test]
    async fn gate_duale_reject_non_esegue_e_rimanda_con_motivi() {
        use crate::decisions::step_gate::StepVerdict;
        let gate = Arc::new(StubStepGate {
            report: ports::StepValidationReport {
                verdicts: vec![
                    verdetto("gatekeeper", StepVerdict::Approve),
                    verdetto("challenger", StepVerdict::Reject),
                ],
                degraded: None,
            },
            chiamate: std::sync::Mutex::new(0),
        });
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg_gate(), tools.clone());
        let ctx = ctx_con_gate(gate.clone());
        let st = state_with_pending(vec![pending_tool(
            "k1",
            "run_command",
            json!({"command": "rm -rf /srv/dati"}),
        )]);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(*gate.chiamate.lock().unwrap(), 1, "porta convocata una volta");
        assert!(tools.seen.lock().unwrap().is_empty(), "NESSUN tool eseguito");
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().unwrap();
        assert!(content.contains("RIFIUTATO"));
        assert!(content.contains("bersaglio fuori progetto"));
        assert!(content.contains("usa il filtro label del progetto"));
        assert_eq!(
            out.extra
                .get(crate::decisions::step_gate::STEP_GATE_REJECTIONS_EXTRA_KEY)
                .and_then(Value::as_u64),
            Some(1),
            "il contatore dei rimandi sale nello stato"
        );
    }

    /// IN AUTONOMIA NON SI CHIEDE (regola D): con lo stesso disaccordo che in
    /// Confirm sospende, in Automatic il gate RIFIUTA il passo e lo rimanda al
    /// modello — nessuna domanda a cui nessuno risponderebbe, nessun run
    /// appeso. Mutazione: far sospendere anche in Automatic (com'era) ->
    /// `awaiting_confirmation` torna `Some(true)` e il rimando sparisce.
    #[tokio::test]
    async fn in_automatico_il_gate_rifiuta_invece_di_chiedere() {
        use crate::decisions::step_gate::StepVerdict;
        let mut astenuto = verdetto("challenger", StepVerdict::Abstained);
        astenuto.abstain_cause = Some("billing".into());
        let gate = Arc::new(StubStepGate {
            report: ports::StepValidationReport {
                verdicts: vec![verdetto("gatekeeper", StepVerdict::Approve), astenuto],
                degraded: None,
            },
            chiamate: std::sync::Mutex::new(0),
        });
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg_gate(), tools.clone());
        let ctx = ctx_con_gate(gate);
        // `state_with_pending` costruisce gia' un run in Automatic.
        let st = batch_rm_rf();
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));

        assert_ne!(
            out.awaiting_confirmation,
            Some(true),
            "in autonomia il gate non mette il run in attesa di un umano"
        );
        assert!(tools.seen.lock().unwrap().is_empty(), "il passo resta NON eseguito");
        let blocks = blocks_of(out.messages.last().expect("msg"));
        let content = blocks[0]["content"].as_str().unwrap();
        assert!(
            content.contains("non si chiede conferma all'utente"),
            "il rimando dice all'agente perche' il passo non e' passato: {content}"
        );
    }

    /// NEEDS_HUMAN (approve + astensione, GAP-2: l'astensione non e' un si'):
    /// in CONFERMA — dove qualcuno puo' scioglierla — la sospensione resta, coi
    /// verdetti allegati in extra.
    #[tokio::test]
    async fn gate_duale_astensione_sospende_con_verdetti_allegati() {
        use crate::decisions::step_gate::StepVerdict;
        let mut astenuto = verdetto("challenger", StepVerdict::Abstained);
        astenuto.abstain_cause = Some("timeout".into());
        let gate = Arc::new(StubStepGate {
            report: ports::StepValidationReport {
                verdicts: vec![verdetto("gatekeeper", StepVerdict::Approve), astenuto],
                degraded: None,
            },
            chiamate: std::sync::Mutex::new(0),
        });
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg_gate(), tools.clone());
        let ctx = ctx_con_gate(gate);
        let mut st = state_with_pending(vec![pending_tool(
            "k1",
            "run_command",
            json!({"command": "rm -rf /srv/dati"}),
        )]);
        // In CONFERMA la sospensione ha un destinatario: l'utente e' al
        // terminale e la card gli mostra i verdetti. In Automatic il gate
        // rifiuta invece di chiedere (test `in_automatico_il_gate_rifiuta`).
        st.automation_mode = Some(crate::state::AutomationMode::Confirm);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert!(tools.seen.lock().unwrap().is_empty(), "NESSUN tool eseguito");
        assert_eq!(out.awaiting_confirmation, Some(true), "sospeso in Conferma");
        let verdetti = out
            .extra
            .get(crate::decisions::step_gate::STEP_GATE_VERDICTS_EXTRA_KEY)
            .expect("verdetti allegati alla sospensione");
        assert_eq!(verdetti["validators"][1]["abstain_cause"], "timeout");
    }

    /// Gate con verdetti unanimi Approve, gia' costruito su un batch rm -rf.
    fn scenario_approve() -> (Arc<StubStepGate>, Arc<MapToolExecutor>, ToolDispatchNode) {
        use crate::decisions::step_gate::StepVerdict;
        let gate = Arc::new(StubStepGate {
            report: ports::StepValidationReport {
                verdicts: vec![
                    verdetto("gatekeeper", StepVerdict::Approve),
                    verdetto("challenger", StepVerdict::Approve),
                ],
                degraded: None,
            },
            chiamate: std::sync::Mutex::new(0),
        });
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg_gate(), tools.clone());
        (gate, tools, n)
    }

    fn batch_rm_rf() -> AgentState {
        state_with_pending(vec![pending_tool(
            "k1",
            "run_command",
            json!({"command": "rm -rf /srv/dati"}),
        )])
    }

    /// APPROVE unanime: i tool si eseguono (il gate non ferma il flusso).
    #[tokio::test]
    async fn gate_duale_approve_esegue_il_batch() {
        let (gate, tools, n) = scenario_approve();
        let ctx = ctx_con_gate(gate.clone());
        let st = batch_rm_rf();
        let _ = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(*gate.chiamate.lock().unwrap(), 1);
        assert_eq!(tools.seen.lock().unwrap().len(), 1, "tool eseguito dopo l'unanimita'");
    }

    /// Il permesso umano fresco (scritto dal RESUME) fa procedere QUEL giro
    /// senza riconvocare, e il turno lo CONSUMA: il batch successivo riceve
    /// validazione fresca. Mutazione: togliere il consumo dal delta ->
    /// l'approvazione di un passo diventa un lasciapassare permanente e la
    /// seconda asserzione cade.
    #[tokio::test]
    async fn permesso_umano_vale_un_giro_solo_e_viene_consumato() {
        let (gate, tools, n) = scenario_approve();
        let ctx = ctx_con_gate(gate.clone());
        let mut st = batch_rm_rf();
        st.step_gate_human_ok = Some(true);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(*gate.chiamate.lock().unwrap(), 0, "permesso fresco: non si ri-valida");
        assert_eq!(tools.seen.lock().unwrap().len(), 1, "il batch approvato si esegue");
        assert_eq!(
            out.step_gate_human_ok,
            Some(false),
            "il permesso e' consumato dal turno che lo usa"
        );

        // Batch successivo senza permesso: la porta viene convocata.
        let st2 = state_with_pending(vec![pending_tool(
            "k2",
            "run_command",
            json!({"command": "rm -rf /srv/dati"}),
        )]);
        let _ = apply(st2.clone(), n.run(&st2, &ctx).await.expect("run ok"));
        assert_eq!(*gate.chiamate.lock().unwrap(), 1, "batch nuovo: validazione fresca");
    }

    /// IL DIFETTO MISURATO IN ESERCIZIO (run 77fcff4a, 05/08/2026): dopo un
    /// `NeedsHuman` il grafo rientra nel dispatch con gli STESSI pending, e il
    /// gate deve riconvocare — non trovare un lasciapassare che si era
    /// scritto da solo sospendendo. Mutazione: far scrivere alla sospensione
    /// un marker di batch deliberato -> la seconda convocazione sparisce e
    /// il `rm -rf` passa, esattamente come in produzione.
    #[tokio::test]
    async fn dopo_la_sospensione_il_rientro_riconvoca_e_non_esegue() {
        use crate::decisions::step_gate::StepVerdict;
        let mut astenuto = verdetto("challenger", StepVerdict::Abstained);
        astenuto.abstain_cause = Some("billing".into());
        let gate = Arc::new(StubStepGate {
            report: ports::StepValidationReport {
                verdicts: vec![verdetto("gatekeeper", StepVerdict::Approve), astenuto],
                degraded: None,
            },
            chiamate: std::sync::Mutex::new(0),
        });
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg_gate(), tools.clone());
        let ctx = ctx_con_gate(gate.clone());

        // Primo giro: il gate sospende. In CONFERMA, che e' dove la
        // sospensione esiste — ed e' esattamente li' che il marker scritto
        // alla sospensione faceva passare il batch al rientro.
        let mut st = batch_rm_rf();
        st.automation_mode = Some(crate::state::AutomationMode::Confirm);
        let out = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(out.awaiting_confirmation, Some(true));
        assert!(tools.seen.lock().unwrap().is_empty(), "nulla eseguito");

        // Rientro nel dispatch con lo stato PRODOTTO dal giro precedente e gli
        // stessi pending: nessun permesso umano e' arrivato, quindi il gate
        // riconvoca e il comando resta fermo.
        let mut rientro = out.clone();
        rientro.pending_tool_uses = st.pending_tool_uses.clone();
        let _ = apply(rientro.clone(), n.run(&rientro, &ctx).await.expect("run ok"));
        assert_eq!(
            *gate.chiamate.lock().unwrap(),
            2,
            "senza decisione umana il batch si rivalida, non passa"
        );
        assert!(
            tools.seen.lock().unwrap().is_empty(),
            "il comando irreversibile NON deve essere eseguito"
        );
    }

    /// REGOLA O (il test attraversa il PRODUTTORE dello stato): in Automatic
    /// lo stato iniziale semina `approved=Some(true)` per saltare HITL — il
    /// gate DEVE convocare lo stesso, perche' in quella modalita' e' l'unica
    /// barriera sui passi critici. Mutazione: reintrodurre il corto-circuito
    /// su `state.approved` nel barrier -> chiamate=0 -> rosso (il difetto
    /// trovato dalla review avversaria del 05/08).
    #[tokio::test]
    async fn gate_duale_convoca_anche_con_approved_seminato_dal_mode() {
        let (gate, tools, n) = scenario_approve();
        let ctx = ctx_con_gate(gate.clone());
        let mut st = batch_rm_rf();
        st.approved = Some(true); // come build_initial_state in Automatic/Continuous
        let _ = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(
            *gate.chiamate.lock().unwrap(),
            1,
            "approved del MODE non e' un'approvazione del batch: il gate convoca"
        );
        assert_eq!(tools.seen.lock().unwrap().len(), 1, "unanimita': tool eseguito");
    }

    /// Un batch sotto soglia (Mutating) non convoca MAI la porta: la
    /// classificazione in-memory e' l'unico costo. Mutazione: convocare su
    /// Mutating -> chiamate=1 -> rosso.
    #[tokio::test]
    async fn gate_duale_sotto_soglia_non_convoca() {
        use crate::decisions::step_gate::StepVerdict;
        let gate = Arc::new(StubStepGate {
            report: ports::StepValidationReport {
                verdicts: vec![verdetto("gatekeeper", StepVerdict::Reject)],
                degraded: None,
            },
            chiamate: std::sync::Mutex::new(0),
        });
        let tools = Arc::new(MapToolExecutor::new());
        let (n, _steps, _rc) = node(cfg_gate(), tools.clone());
        let ctx = ctx_con_gate(gate.clone());
        // write_file e' un mutatore ordinario: HITL/review lo coprono, il gate no.
        let st = state_with_pending(vec![pending_tool(
            "w1",
            "write_file",
            json!({"path": "a.txt", "content": "x"}),
        )]);
        let _ = apply(st.clone(), n.run(&st, &ctx).await.expect("run ok"));
        assert_eq!(*gate.chiamate.lock().unwrap(), 0, "porta mai convocata sotto soglia");
        assert_eq!(tools.seen.lock().unwrap().len(), 1, "tool eseguito normalmente");
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
    use crate::routing::config::RoutingConfig;
    use crate::runtime::ports::{PortError, ToolOutcome};
    use crate::runtime::test_doubles::{
        NullEventSink, StubAgentStepStore, StubContextOffload, StubLlmGateway, StubRunControlStore,
        StubTodoStore,
    };
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
        async fn execute(&self, call: ToolCall) -> Result<ToolOutcome, PortError> {
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
        if let Some(d) = v
            .get("discovered_tools_next_turn")
            .and_then(Value::as_array)
        {
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
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or(d.discovery_first_whitelist),
            always_on_tools: v
                .get("always_on_tools")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
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
            fs_mutator_tools: d.fs_mutator_tools,
            // Il golden non esercita la barriera advisory (nessun canale nel ctx
            // -> gate inerte): il valore e' irrilevante.
            advisory_gate_timeout_s: d.advisory_gate_timeout_s,
            // Il golden non esercita il gate duale (mode Off di default:
            // passo 2a inerte, dispatch bit-identico ai fixture).
            step_gate_mode: d.step_gate_mode,
            step_gate_rules: d.step_gate_rules,
            step_gate_max_rejections: d.step_gate_max_rejections,
        }
    }

    /// Esiti stubati dei tool (`tool_results`: id -> {content, is_error, exit_code}).
    fn tool_results_from(v: &Value) -> std::collections::HashMap<String, ToolOutcome> {
        let mut map = std::collections::HashMap::new();
        if let Some(obj) = v.as_object() {
            for (id, stub) in obj {
                let content = stub.get("content").cloned().unwrap_or(json!("{}"));
                let is_error = stub
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
            isolation_available: false,
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
            advisory_gate: None,
        step_gate: None,
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
        assert!(
            cases.len() >= 8,
            "attesi >= 8 casi, trovati {}",
            cases.len()
        );

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
