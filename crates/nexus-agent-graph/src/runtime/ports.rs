//! Porte astratte delle dipendenze I/O dei nodi (inversione di dipendenza).
//!
//! VINCOLO ARCHITETTURALE: `nexus-agent-graph` NON deve dipendere da `mcp-core`
//! (mcp-core dipendera' da lui -> ciclo). Le dipendenze I/O (gateway LLM,
//! esecuzione tool, eventi SSE) sono qui espresse come TRAIT astratti; mcp-core
//! li implementera' in un PR futuro delegando alle sue infrastrutture concrete
//! (es. `nexus-gateway` per `LlmGateway`, il ToolRunner gRPC per `ToolExecutor`,
//! il canale SSE per `EventSink`). Questo e' il confine d'inversione: i nodi
//! dipendono dai trait, non dalle implementazioni.
//!
//! Le strutture dati (`LlmRequest`/`LlmResponse`/`ToolCall`/`ToolOutcome`/
//! `SseEvent`) sono MINIMALI e provider-agnostiche: trasportano solo cio' che
//! serve ai nodi. Nessun nome modello / URL provider e' hardcoded qui (regola
//! G): provider e model arrivano dal chiamante (risolti a monte dalla routing
//! matrix).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::decisions::dag_scheduler::{Todo, TodoStatus};
use crate::state::ToolUse;

/// Errore di una porta I/O. Opaco al runtime: messaggio + classe sintetica per
/// permettere ai nodi di distinguere un guasto infrastrutturale (gateway/tool
/// down) da un errore applicativo, senza accoppiarsi al dettaglio concreto.
#[derive(Debug, Error)]
pub enum PortError {
    /// Il gateway LLM ha risposto con un errore (provider down, billing, 4xx).
    #[error("gateway LLM: {0}")]
    Llm(String),
    /// L'esecuzione di un tool e' fallita (ToolRunner down o errore applicativo).
    #[error("esecuzione tool: {0}")]
    Tool(String),
    /// In modalita' Replay il tool_result del run primario non e' disponibile.
    #[error("replay non disponibile per la chiamata '{0}'")]
    ReplayMissing(String),
}

/// Messaggio nel formato minimale richiesto dal gateway (ruolo + contenuto).
///
/// Provider-agnostico: il gateway concreto (mcp-core) traduce questa forma nel
/// payload specifico del provider scelto. `content` e' JSON arbitrario per
/// ammettere sia testo semplice sia blocchi strutturati (tool_use/tool_result).
#[derive(Debug, Clone, PartialEq)]
pub struct LlmMessage {
    /// Ruolo del messaggio (`system` | `user` | `assistant` | `tool`).
    pub role: String,
    /// Contenuto: stringa o struttura a blocchi (JSON opaco al runtime).
    pub content: Value,
}

/// Richiesta minimale al gateway LLM.
///
/// `provider`/`model` sono RISOLTI A MONTE dalla routing matrix (regola G: il
/// nodo non li sceglie e non li hardcoda, li riceve gia' decisi). `tools` e'
/// opzionale: assente per un turno puramente testuale.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    /// Provider risolto dalla routing matrix (es. valore opaco "anthropic").
    pub provider: String,
    /// Modello risolto dalla routing matrix (mai hardcoded qui, regola G).
    pub model: String,
    /// Messaggi della conversazione nel formato minimale.
    pub messages: Vec<LlmMessage>,
    /// Tool dichiarati al modello (schema JSON). `None` = turno senza tool.
    pub tools: Option<Vec<Value>>,
}

/// Uso/consumo token riportato dal gateway (forma normalizzata cross-provider).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LlmUsage {
    /// Token di prompt (input).
    pub prompt_tokens: i64,
    /// Token di completion (output).
    pub completion_tokens: i64,
    /// Token totali.
    pub total_tokens: i64,
}

/// Risposta minimale del gateway LLM.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    /// Contenuto testuale prodotto dal modello (vuoto se solo tool_calls).
    pub content: String,
    /// Richieste di tool emesse dal modello (vuoto se turno testuale).
    pub tool_calls: Vec<ToolUse>,
    /// Consumo token normalizzato.
    pub usage: LlmUsage,
}

/// Astrazione del gateway LLM. mcp-core la implementera' delegando a
/// `nexus-gateway` (catena Fallback DB-driven). I nodi dipendono solo da questo
/// trait, mai dal client concreto.
#[async_trait]
pub trait LlmGateway: Send + Sync {
    /// Esegue una completion. L'I/O (HTTP, retry, cooldown) e' del concreto.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError>;
}

/// Una chiamata a tool da eseguire (riusa la forma `ToolUse` del canale interno
/// per non duplicare la struttura nome/args/id, regola L).
pub type ToolCall = ToolUse;

/// Modalita' d'esecuzione di un tool.
///
/// `Replay` e' il cuore della modalita' shadow: invece di RIESEGUIRE il tool
/// (che avrebbe side-effect sul filesystem/DB/container del progetto),
/// l'esecutore RILEGGE il `tool_result` registrato dal run PRIMARIO. Cosi' il
/// run shadow osserva gli stessi risultati senza causare effetti collaterali
/// (ZERO side-effect, requisito di safety per lo shadow read-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// Esecuzione reale: il tool viene eseguito davvero (side-effect possibili).
    Real,
    /// Replay: rilegge il tool_result del run primario, nessun side-effect.
    Replay,
}

/// Esito dell'esecuzione di un tool nel formato minimale richiesto dai nodi.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// Id della `ToolCall` a cui questo esito risponde (round-trip).
    pub tool_call_id: String,
    /// Contenuto del risultato (JSON: stringa o struttura).
    pub content: Value,
    /// `true` se il tool ha fallito (errore applicativo, non infrastrutturale).
    pub is_error: bool,
}

/// Astrazione dell'esecutore di tool. mcp-core la implementera' delegando al
/// ToolRunner gRPC (modalita' `Real`) e a un lettore dei tool_result del run
/// primario (modalita' `Replay`, per lo shadow).
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Esegue (o replaya) una chiamata a tool secondo `mode`.
    async fn execute(&self, call: ToolCall, mode: ExecMode) -> Result<ToolOutcome, PortError>;
}

/// Specifica di UN criterio di verifica costruita dal `FinalGateNode`
/// (`no_orphan_imported` / `outputs_exist` / `service_logs_clean` /
/// `run_command`-build). Replica i dict `{type, spec, expected, timeout_s}`
/// costruiti in `final_gate.py:316-377`.
///
/// E' la forma trasportata al sotto-sistema [`CriteriaRunner`]: il nodo NON
/// esegue i criteri (richiederebbero il ToolRunner gRPC + la logica di
/// `criteria_runner._check_*`, sotto-sistema separato come `closure_judge` per
/// il learner). Il nodo costruisce SOLO queste spec (deterministico,
/// golden-abile) e delega l'esecuzione.
///
/// `Serialize`/`Deserialize`: serve a [`crate::nodes::final_gate::FinalGateConfig`]
/// (che incapsula un `Option<CriterionSpec>` come criterio endpoint risolto a
/// monte) per restare serializzabile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionSpec {
    /// Tipo del criterio (`no_orphan_imported`, `outputs_exist`,
    /// `service_logs_clean`, `run_command`).
    pub criterion_type: String,
    /// Parametri del criterio (`spec` Python: staging_dir, command, ...).
    pub spec: Value,
    /// Atteso (`expected` Python: `{mounted:true}`, `{exit_code:0}`, ...).
    pub expected: Value,
    /// Timeout dedicato in secondi (solo il criterio build lo valorizza).
    pub timeout_s: Option<f64>,
}

/// Esito dell'esecuzione di UN criterio (`{type, passed, evidence}` Python,
/// `final_gate.py:386-390`). `evidence` e' JSON arbitrario: per il criterio
/// build contiene `output_excerpt`/`exit_code`/`output_total_chars`/
/// `output_truncated`, per gli altri `verdict`/`error`.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionResult {
    /// Tipo del criterio (eco dello spec).
    pub criterion_type: String,
    /// `true` se il criterio e' passato (`bool(ok)` Python).
    pub passed: bool,
    /// Evidenza diagnostica (JSON opaco al nodo).
    pub evidence: Value,
}

/// Astrazione del motore di verifica dei criteri generali del final gate
/// (`brain/agents/criteria_runner.py`). mcp-core la implementera' delegando ai
/// `_check_*` concreti (che a loro volta usano il ToolRunner gRPC).
///
/// E' un SOTTO-SISTEMA a se' (come `closure_judge` per il learner): la LOGICA
/// dei singoli criteri NON e' portata in questo PR (vedi TODO in
/// `nodes::final_gate`). Il confine e' pulito: il `FinalGateNode` costruisce le
/// [`CriterionSpec`] e ottiene i [`CriterionResult`]; in modalita' shadow passa
/// `ExecMode::Replay` (i criteri rileggono i tool_result del primario = zero
/// side-effect).
#[async_trait]
pub trait CriteriaRunner: Send + Sync {
    /// Esegue (o replaya) i criteri nell'ordine dato; un fallimento di un
    /// criterio NON deve propagare un errore: il concreto lo mappa su un
    /// [`CriterionResult`] con `passed=false` + `evidence.error` (parita' col
    /// try/except del Python, `final_gate.py:381-385`). L'errore di porta resta
    /// per un guasto infrastrutturale del runner stesso.
    async fn run(
        &self,
        criteria: Vec<CriterionSpec>,
        mode: ExecMode,
    ) -> Result<Vec<CriterionResult>, PortError>;
}

/// Esito di UN run del verifier da persistere su `nexus_agent_verifier_runs`
/// (`verifier_node._persist_verifier_run`, `verifier_node.py:584-601`). Forma
/// minimale: i campi della INSERT (`run_id`/`todo_id`/`cycle`/`criteria_results`/
/// `passed`/`duration_ms`). `criteria_results` e' JSON arbitrario (la lista dei
/// [`CriterionResult`] serializzata) opaco allo store.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifierRunRecord {
    /// Id del run (= thread_id).
    pub run_id: String,
    /// Id del todo verificato.
    pub todo_id: String,
    /// Ciclo di verifica (1-based).
    pub cycle: i64,
    /// Risultati dei criteri serializzati (JSON: lista `{type, passed, evidence}`).
    pub criteria_results: Value,
    /// `true` se la verifica e' passata.
    pub passed: bool,
    /// Durata dell'esecuzione criteri in millisecondi.
    pub duration_ms: i64,
}

/// Astrazione della persistenza degli esiti del verifier su
/// `nexus_agent_verifier_runs` (`verifier_node._persist_verifier_run`). mcp-core
/// la implementera' con `sqlx` (INSERT best-effort). E' una SCRITTURA DB: come
/// [`TodoStore::mark_status`] e [`ToolExecutor::execute`], il `mode` gata
/// l'effetto (punto unico del gate shadow, regola L): l'impl concreta DEVE
/// eseguire la INSERT solo in [`ExecMode::Real`]; in [`ExecMode::Replay`] (run
/// shadow read-only) la chiamata e' un NO-OP (zero scritture, il run shadow non
/// inquina la telemetria del primario).
///
/// Parita' col Python: la INSERT e' BEST-EFFORT (su errore DB il verifier
/// prosegue, `verifier_node.py:600`). L'impl concreta NON deve propagare un
/// `PortError` per un fallimento dell'INSERT (lo logga e ritorna `Ok(())`); il
/// `PortError` resta per un contratto rotto (mai usato nel flusso normale).
#[async_trait]
pub trait VerifierRunStore: Send + Sync {
    /// Persiste (best-effort) un esito del verifier. No-op in [`ExecMode::Replay`].
    async fn record(&self, run: VerifierRunRecord, mode: ExecMode) -> Result<(), PortError>;
}

/// Evento pubblicato verso il frontend chat (sottoinsieme del contratto SSE).
///
/// Solo le varianti che servono ai nodi di questo PR + quelle del contratto
/// gia' note. Allineato al canale SSE prodotto dal brain (`type` + payload). Lo
/// shadow NON emette eventi (l'`EventSink` no-op viene iniettato nel ctx shadow):
/// l'unica fonte di verita' verso l'utente resta il run primario.
#[derive(Debug, Clone, PartialEq)]
pub enum SseEvent {
    /// Delta di ragionamento/streaming testuale.
    ThinkingDelta {
        /// Frammento di testo.
        delta: String,
    },
    /// Meta-step semantico (plan/routing/clarify/fallback/reflection).
    MetaStep {
        /// Tipo dello step.
        kind: String,
        /// Titolo leggibile.
        title: String,
        /// Payload arbitrario.
        payload: Value,
    },
    /// Consumo token aggiornato (barra contesto).
    Usage {
        /// Token di prompt.
        prompt_tokens: i64,
        /// Token di completion.
        completion_tokens: i64,
        /// Token totali.
        total_tokens: i64,
    },
    /// Il modello ha richiesto un tool.
    ToolUse {
        /// Id della richiesta.
        id: String,
        /// Nome del tool.
        name: String,
        /// Argomenti.
        input: Value,
    },
    /// Risultato di un tool.
    ToolResult {
        /// Id della richiesta a cui risponde.
        tool_call_id: String,
        /// Contenuto del risultato.
        content: Value,
        /// `true` se il tool ha fallito.
        is_error: bool,
    },
    /// Fine del turno corrente (il modello ha terminato la generazione).
    EndTurn,
    /// Fine del run (terminatore dello stream).
    Done,
}

/// Astrazione del canale eventi verso il frontend. `emit` e' SINCRONO e
/// infallibile dal punto di vista del nodo (best-effort: il concreto bufferizza
/// / scarta se non ci sono subscriber). mcp-core la implementera' col canale SSE.
pub trait EventSink: Send + Sync {
    /// Pubblica un evento (best-effort, non blocca il nodo).
    fn emit(&self, ev: SseEvent);
}

/// Riga di piano (`nexus_agent_plans`) nella forma MINIMALE che serve al riuso
/// piano intent/mode-aware del planner (`planner_node.py:84-113`,
/// `todo_store.fetch_plan`). Trasporta SOLO i due campi su cui il planner decide
/// l'invalidazione (`user_intent` / `behavior_mode`): un piano esistente si
/// RIUSA se questi non sono cambiati (campo non-None e divergente -> rigenera).
/// I piani legacy (pre-mig 0328) hanno questi campi a `None` (intent non
/// tracciato): in quel caso il riuso storico e' mantenuto (vedi nota Python).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanRow {
    /// Intent con cui il piano e' stato creato (`None` per i piani legacy).
    pub user_intent: Option<String>,
    /// Behavior_mode con cui il piano e' stato creato (`None` per i legacy).
    pub behavior_mode: Option<String>,
}

/// Astrazione dell'I/O sui todo del DAG (`brain/agents/todo_store.py`). Confine
/// d'inversione: la LOGICA DAG (selezione, ready layer, discendenti) e' pura e
/// vive in [`crate::decisions::dag_scheduler`] (punto unico, regola L); questo
/// trait isola SOLO l'accesso DB. I nodi (`verifier`/`todo_runner`/`planner`)
/// leggono i todo da qui e delegano la decisione al modulo puro.
///
/// mcp-core la implementera' con `sqlx` su `nexus_agent_todos` (TODO: impl
/// concreta nel PR dei nodi). Il trait e' astratto: definisce il contratto, non
/// l'implementazione.
///
/// INVARIANTE (regola H, bug 2026-06-10): [`list_todos`](TodoStore::list_todos)
/// DEVE restituire `Todo::depends_on` come `Vec`, MAI una stringa `"{...}"`.
/// Lato impl significa il cast `depends_on::text[]` (come `todo_store.list_todos`
/// Python); il tipo Rust `Vec<String>` rende l'invariante non eludibile. Deve
/// inoltre restituire gli elementi GIA' ordinati per `seq` ascendente (le
/// funzioni pure si basano sull'ordine dello slice come tie-break).
#[async_trait]
pub trait TodoStore: Send + Sync {
    /// Tutti i todo del run, ordinati per `seq` ascendente, con `depends_on`
    /// come `Vec` (cast `::text[]`). 1:1 con `todo_store.list_todos`.
    async fn list_todos(&self, run_id: &str) -> Result<Vec<Todo>, PortError>;

    /// Il piano esistente per il run, se presente (1:1 con
    /// `todo_store.fetch_plan`). Usato dal `PlannerNode` per il riuso piano
    /// intent/mode-aware (`planner_node.py:84-113`): `None` = nessun piano
    /// (prima pianificazione del run). Default `Ok(None)` cosi' le impl che non
    /// servono il planner (es. il `TodoRunnerNode`, gia' esistente) non devono
    /// fornirlo; l'impl concreta in mcp-core e lo stub del planner lo
    /// sovrascrivono.
    async fn fetch_plan(&self, _run_id: &str) -> Result<Option<PlanRow>, PortError> {
        Ok(None)
    }

    /// Il todo "attivo": il primo `in_progress`, altrimenti il primo `pending`,
    /// altrimenti `None`. 1:1 con `todo_store.active_todo`. Default fornito sopra
    /// [`list_todos`](TodoStore::list_todos) per non duplicare la selezione.
    async fn active_todo(&self, run_id: &str) -> Result<Option<Todo>, PortError> {
        let todos = self.list_todos(run_id).await?;
        let by_status = |s: TodoStatus| todos.iter().find(|t| t.status == s).cloned();
        Ok(by_status(TodoStatus::InProgress).or_else(|| by_status(TodoStatus::Pending)))
    }

    /// Aggiorna lo status di un todo (UPDATE best-effort). 1:1 con i `_mark` /
    /// `_mark_todo_status` di `dag_scheduler.py` e `verifier_node.py`.
    ///
    /// `mode` gata la scrittura come [`ToolExecutor::execute`] (punto unico del
    /// gate shadow, regola L): l'impl concreta DEVE eseguire l'UPDATE solo in
    /// [`ExecMode::Real`]. In [`ExecMode::Replay`] (run shadow read-only) la
    /// chiamata e' un NO-OP: nessuna scrittura su `nexus_agent_todos`, cosi' il
    /// run shadow non corrompe il DAG del run primario (ZERO side-effect).
    async fn mark_status(
        &self,
        todo_id: &str,
        status: TodoStatus,
        mode: ExecMode,
    ) -> Result<(), PortError>;
}
