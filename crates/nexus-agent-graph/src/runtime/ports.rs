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
use crate::decisions::escalation::{ChainEntry, CrossProviderCandidate};
use crate::state::ToolUse;

/// Errore di una porta I/O. Opaco al runtime: messaggio + classe sintetica per
/// permettere ai nodi di distinguere un guasto infrastrutturale (gateway/tool
/// down) da un errore applicativo, senza accoppiarsi al dettaglio concreto.
#[derive(Debug, Error)]
pub enum PortError {
    /// Il gateway LLM ha risposto con un errore (provider down, billing, 4xx).
    #[error("gateway LLM: {0}")]
    Llm(String),
    /// Il gateway LLM ha risposto che il/i provider risolti per la richiesta NON
    /// sono disponibili (cooldown billing/transient o `PROVIDER_ERROR` aggregato:
    /// "tutti i provider hanno fallito"). E' un SEGNALE STRUTTURATO distinto da
    /// [`PortError::Llm`] generico (regola L): il nodo executor lo matcha per
    /// tentare il FALLBACK cross-provider (escalation) invece di chiudere il run
    /// con `StopReason::Error`. Il discriminante e' il CODICE errore del gateway
    /// (`PROVIDER_ERROR` nel body del 500), NON il testo lessicale "in cooldown"
    /// (fragile): la mappatura vive nell'adapter concreto (`llm_gateway.rs`).
    #[error("provider non disponibile: {0}")]
    ProviderUnavailable(String),
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
///
/// CONTINUITA' TOOL MULTI-TURN (regola L, un solo formato messaggio wire): per
/// un turno `assistant` che ha chiamato tool i `tool_use` vanno in
/// [`LlmMessage::tool_calls`] (NON appiattiti in `content`); per un turno `tool`
/// (risultato) il `role` e' `"tool"` e [`LlmMessage::tool_call_id`] referenzia la
/// chiamata. Il server (`to_anthropic_messages`) riconosce la coppia tool_use /
/// tool_result SOLO da questi campi: senza di essi Anthropic risponde HTTP 400
/// (`tool_use ids without tool_result`). I due campi sono `Option` additivi:
/// `None` su tutti i messaggi testuali (retrocompatibile coi call site esistenti).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LlmMessage {
    /// Ruolo del messaggio (`system` | `user` | `assistant` | `tool`).
    pub role: String,
    /// Contenuto: stringa o struttura a blocchi (JSON opaco al runtime).
    pub content: Value,
    /// Tool-call emesse da un turno `assistant` (continuita' tool_use). `None` /
    /// vuoto per un turno testuale. Gli id qui DEVONO combaciare col
    /// [`LlmMessage::tool_call_id`] del messaggio `tool` che ne porta il risultato.
    pub tool_calls: Option<Vec<ToolUse>>,
    /// Id della tool-call a cui un messaggio `tool` (risultato) risponde
    /// (round-trip). `None` su tutti gli altri ruoli.
    pub tool_call_id: Option<String>,
}

/// Richiesta minimale al gateway LLM.
///
/// `provider`/`model` sono RISOLTI A MONTE dalla routing matrix (regola G: il
/// nodo non li sceglie e non li hardcoda, li riceve gia' decisi). `tools` e'
/// opzionale: assente per un turno puramente testuale.
///
/// I campi estesi sono tutti `Option`/`Default` per non rompere i call site
/// esistenti (i nodi gia' portati costruiscono `LlmRequest` con i soli campi
/// base + `..Default::default()`): un turno minimale resta valido senza
/// valorizzarli.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmRequest {
    /// Provider risolto dalla routing matrix (es. valore opaco "anthropic").
    pub provider: String,
    /// Modello risolto dalla routing matrix (mai hardcoded qui, regola G).
    pub model: String,
    /// Messaggi della conversazione nel formato minimale.
    pub messages: Vec<LlmMessage>,
    /// Tool dichiarati al modello (schema JSON). `None` = turno senza tool.
    pub tools: Option<Vec<Value>>,
    /// Forza il modello a chiamare un tool (`tool_choice` non-`auto`).
    ///
    /// RISCHIO NOTO (memoria progetto, "Gateway droppava tool_choice"): in una
    /// migrazione passata il gateway PERDEVA il `tool_choice`, neutralizzando il
    /// force-action anti-loop (l'agente descriveva il fix ma non chiamava
    /// `edit_file` -> abort). Per questo `force_tool_choice` e' un campo
    /// ESPLICITO del contratto: l'impl gateway concreta (Fase 6) DEVE onorarlo
    /// end-to-end (`tool_choice` propagato al provider), mai droppato. `None` =
    /// lascia la scelta al modello (`auto`); `Some(true)` = forza un tool;
    /// `Some(false)` = vieta i tool (turno puramente testuale).
    pub force_tool_choice: Option<bool>,
    /// System prompt del turno (forma testuale provider-agnostica). `None` se il
    /// system viaggia gia' come primo `LlmMessage` con `role="system"` (forma
    /// usata da planner/clarify): le due forme sono alternative, l'impl concreta
    /// le normalizza nel payload del provider.
    pub system_text: Option<String>,
    /// Tetto di token di output per il turno (`max_tokens`). `None` = default del
    /// provider risolto a monte.
    pub max_tokens: Option<i64>,
    /// Metadati per la registrazione usage lato gateway concreto (telemetria
    /// token/costo per run): `run_id` del run corrente. `None` per le chiamate
    /// fuori-run (es. test). Opaco al nodo.
    pub run_id: Option<String>,
    /// Iterazione del run a cui la chiamata appartiene (telemetria usage). `None`
    /// fuori-run.
    pub iteration: Option<i64>,
    /// Intent classificato del turno (telemetria usage / routing osservabile).
    /// `None` se non disponibile. Opaco al nodo.
    pub intent: Option<String>,
    /// Nodo CHIAMANTE della completion (`"executor"` / `"planner"` /
    /// `"reflection"` / `"clarify_expand"`). `None` (Default) per i call site che
    /// non lo valorizzano (test, turni minimali).
    ///
    /// SCOPO (regola L): e' un discriminante OPACO al gateway concreto. L'impl di
    /// produzione [`LlmGateway`] (`GatewayLlmAdapter` di mcp-core) lo IGNORA
    /// completamente — un turno REAL non cambia comportamento. Serve SOLO al
    /// decorator di REPLAY usato dallo shadow (`ReplayLlmGateway`), che distingue
    /// la chiamata dell'executor (da RIGIOCARE sulla sequenza tool del primario
    /// letta da `agent_steps`) da quelle ausiliarie (planner/reflection/
    /// clarify_expand, da NEUTRALIZZARE con una risposta neutra deterministica).
    /// Il trait [`LlmGateway`] resta invariato (firma `complete()` non cambia):
    /// `purpose` viaggia nel payload, l'impl decide se guardarlo.
    pub purpose: Option<String>,
}

/// Uso/consumo token riportato dal gateway (forma normalizzata cross-provider).
///
/// I campi cache/costo sono `Option`: `None` = il provider/gateway non li ha
/// riportati (compute_turn_cost resta lato gateway concreto, il nodo legge
/// l'usage gia' normalizzato; vedi memoria progetto "Token usage stream + live"
/// per la normalizzazione cross-provider `extract_usage_tokens`). `Eq` rimosso
/// perche' `total_cost_usd` e' un `f64` (non `Eq`); `PartialEq` basta per i test.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LlmUsage {
    /// Token di prompt (input).
    pub prompt_tokens: i64,
    /// Token di completion (output).
    pub completion_tokens: i64,
    /// Token totali.
    pub total_tokens: i64,
    /// Token usati per CREARE la cache di prompt (Anthropic
    /// `cache_creation_input_tokens`). `None` se il provider non la espone.
    pub cache_creation_tokens: Option<i64>,
    /// Token LETTI dalla cache di prompt (Anthropic `cache_read_input_tokens`):
    /// risparmio della KV-cache. `None` se il provider non la espone.
    pub cache_read_tokens: Option<i64>,
    /// Costo del turno in USD calcolato dal gateway (`compute_turn_cost`). `None`
    /// se non calcolato. Il nodo lo legge gia' pronto, non lo calcola.
    pub total_cost_usd: Option<f64>,
}

/// Risposta minimale del gateway LLM.
///
/// I campi estesi sono `Option`/`Vec` con `Default` per non rompere i call site
/// esistenti (i nodi gia' portati costruiscono `LlmResponse` con i soli campi
/// base + `..Default::default()`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmResponse {
    /// Contenuto testuale prodotto dal modello (vuoto se solo tool_calls).
    pub content: String,
    /// Richieste di tool emesse dal modello (vuoto se turno testuale).
    pub tool_calls: Vec<ToolUse>,
    /// Consumo token normalizzato.
    pub usage: LlmUsage,
    /// Provider EFFETTIVAMENTE usato dal gateway (puo' differire dal `provider`
    /// richiesto: il gateway fa cascade/sticky internamente). Il nodo legge
    /// l'effettivo per la telemetria e per `RunControlStore::set_effective_model`.
    /// `None` = il gateway non l'ha riportato (es. stub di test).
    pub provider_used: Option<String>,
    /// Modello EFFETTIVAMENTE usato dal gateway (vedi `provider_used`). `None` se
    /// non riportato.
    pub model_used: Option<String>,
    /// Blocchi grezzi del contenuto dell'assistente nel formato "anthropic_content"
    /// (testo + `tool_use`), per ricostruire fedelmente il `Message::Ai` con i
    /// blocchi `tool_use` originali. Serve la continuita' `tool_use`/`tool_result`
    /// gia' gestita nel planner (un `tool_result` deve referenziare il `tool_use`
    /// originale). `Vec` vuoto = il gateway ha riportato solo `content`/`tool_calls`
    /// (i nodi ricostruiscono dai campi base).
    pub assistant_content: Vec<Value>,
    /// Motivo di fine turno riportato dal provider (`stop_reason`/`finish_reason`
    /// normalizzato: `end_turn`/`tool_use`/`max_tokens`/...). `None` se non
    /// riportato. Segnale per la chiusura turno (vedi `routing::signals`).
    pub stop_reason: Option<String>,
    /// Ragionamento/pensiero intermedio aggregato dal gateway (reasoning_content
    /// OpenAI-compat, thoughts Gemini, thinking Anthropic). `None` se il provider
    /// non lo riporta. L'executor lo emette come `SseEvent::ThinkingDelta` per
    /// mostrare il pensiero del modello in chat (visibilita' pre-porting).
    pub reasoning: Option<String>,
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
///
/// COERENZA `exit_code` (regola L, un solo segnale d'esito): un `ToolOutcome`
/// diventa un [`crate::state::ContentBlock::ToolResult`] quando il nodo lo
/// appende ai messaggi (vedi `tool_call_id` -> `tool_use_id`, `content`,
/// `is_error`, `exit_code`). Il campo `exit_code` qui DEVE fluire INVARIATO in
/// quel `ContentBlock::ToolResult` (stesso tipo `Option<i64>`, stessa semantica:
/// `Some(0)` successo, `Some(!=0)` errore di comando, `None` tool non-comando):
/// e' il segnale PRIMARIO letto da
/// [`crate::routing::signals::tool_result_outcome_after`]. Non re-derivare
/// l'esito altrove.
///
/// I campi estesi sono `Option`/`bool` con `Default` per non rompere i call site
/// esistenti.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolOutcome {
    /// Id della `ToolCall` a cui questo esito risponde (round-trip). Diventa
    /// `tool_use_id` nel `ContentBlock::ToolResult`.
    pub tool_call_id: String,
    /// Contenuto del risultato (JSON: stringa o struttura).
    pub content: Value,
    /// `true` se il tool ha fallito (errore applicativo, non infrastrutturale).
    pub is_error: bool,
    /// Exit code STRUTTURATO del tool-comando: `Some(0)` successo, `Some(!=0)`
    /// errore, `None` tool non-comando. DEVE confluire INVARIATO nel campo
    /// `exit_code` di [`crate::state::ContentBlock::ToolResult`] (alimenta
    /// [`crate::routing::signals::tool_result_outcome_after`]).
    pub exit_code: Option<i64>,
    /// `true` se il fallimento e' INFRASTRUTTURALE (ToolRunner/gRPC down, non un
    /// errore applicativo del tool). Mappa il caso "gRPC-down -> degrada a
    /// executor" senza scalare provider (WAVE 2.2: mcp-core NON scala il provider
    /// su un guasto infra). Default `false` (errore applicativo / successo).
    pub is_infrastructure: bool,
    /// Classe sintetica dell'errore (`timeout`/`grpc_unavailable`/`tool_error`/...)
    /// per il mapping diagnostico fine. `None` su successo o quando non
    /// classificato. Opaco al nodo (eco diagnostica).
    pub error_class: Option<String>,
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

    /// Reminder testuale dei todo da iniettare nel prompt dell'executor quando la
    /// fase di piano e' attiva (`plan_phase_active`). L'impl concreta legge i
    /// todos del run e rende il testo (lista compatta stato/seq), `None` se non
    /// c'e' un piano attivo o nessun todo. Default `Ok(None)` per non obbligare
    /// gli store che non servono l'executor (es. il `TodoRunnerNode` esistente).
    /// SOLA LETTURA: nessun gate `mode` (non scrive).
    async fn build_reminder_text(&self, _run_id: &str) -> Result<Option<String>, PortError> {
        Ok(None)
    }

    /// Incrementa il contatore di iterazioni "viste" per il run (telemetria di
    /// avanzamento del piano). UPDATE best-effort gata `Real` (no-op in
    /// [`ExecMode::Replay`], stesso gate shadow di [`mark_status`](TodoStore::mark_status),
    /// regola L). Default no-op: gli store che non lo servono non lo
    /// sovrascrivono.
    async fn increment_iteration_seen(
        &self,
        _run_id: &str,
        _mode: ExecMode,
    ) -> Result<(), PortError> {
        Ok(())
    }
}

/// Controllo di run condiviso da `executor` e `tool_dispatch` (PUNTO UNICO,
/// regola L): la stessa domanda "il run e' stato superato?" e gli stessi
/// side-effect best-effort (heartbeat, modello effettivo) vivono in UN solo
/// trait, i due nodi delegano qui invece di re-implementare query/UPDATE.
///
/// Relazione con `ctx.cancel` (memoria progetto, "Single run per session" /
/// supersede last-wins): in Rust il segnale di run superato e' GIA' parzialmente
/// coperto dal `CancellationToken` del ctx (l'orchestratore lo cancella quando
/// un nuovo run supera il corrente). `is_superseded` e' il segnale ESPLICITO e
/// POLLABILE complementare: il nodo lo interroga ai checkpoint (inizio
/// iterazione) anche quando il token non e' ancora propagato (es. supersede
/// scritto su DB da un altro processo). Le due fonti convergono; l'impl concreta
/// puo' leggere il flag `superseded`/`supersede_active_runs` su `agent_runs`.
#[async_trait]
pub trait RunControlStore: Send + Sync {
    /// `true` se il run e' stato superato (last-wins) e deve fermarsi. FAIL-OPEN
    /// (regola di sicurezza): un errore di lettura DB ritorna `Ok(false)` (il run
    /// PROSEGUE), mai un `PortError` che lo bloccherebbe per un guasto
    /// infrastrutturale. SOLA LETTURA: nessun gate `mode`.
    async fn is_superseded(&self, run_id: &str) -> Result<bool, PortError>;

    /// Heartbeat del run (UPDATE `updated_at` best-effort): segnala che il run e'
    /// vivo (anti-recovery prematuro). Gata `Real` (no-op in [`ExecMode::Replay`]:
    /// il run shadow non tocca la telemetria del primario, regola L).
    /// Best-effort: l'impl logga e ritorna `Ok(())` su errore DB.
    async fn heartbeat(&self, _run_id: &str, _mode: ExecMode) -> Result<(), PortError>;

    /// Registra il modello EFFETTIVAMENTE usato dal gateway (da
    /// [`LlmResponse::provider_used`]/[`LlmResponse::model_used`]) sul run, per la
    /// telemetria/osservabilita' (la chat mostra il modello reale, non quello
    /// richiesto). Gata `Real` (no-op in [`ExecMode::Replay`]). Best-effort.
    async fn set_effective_model(
        &self,
        _run_id: &str,
        _provider: &str,
        _model: &str,
        _mode: ExecMode,
    ) -> Result<(), PortError>;
}

/// Persistenza dei singoli step dell'agente su `agent_steps`
/// (modello: [`VerifierRunStore`]). Ogni blocco prodotto in un'iterazione
/// (tool_use, tool_result, testo) e' uno step indicizzato. Confine d'inversione:
/// SOLO l'INSERT, nessuna logica.
///
/// `step_index` deterministico = `iteration * 1000 + idx` (idx = posizione del
/// blocco nell'iterazione): garantisce ordinamento globale stabile senza
/// contatore condiviso. L'impl concreta DEVE usare `ON CONFLICT DO NOTHING`
/// (idempotente sui retry) + guard `untracked_run` (non inserire step per run non
/// tracciati, evita FK orfane).
#[async_trait]
pub trait AgentStepStore: Send + Sync {
    /// Persiste UN blocco di una iterazione. `block` = il blocco emesso
    /// (tool_use/testo, JSON opaco), `result` = l'eventuale tool_result associato
    /// (`None` per i blocchi di testo o quando non ancora disponibile).
    ///
    /// Gata `Real` (punto unico gate shadow, regola L): INSERT solo in
    /// [`ExecMode::Real`]; no-op in [`ExecMode::Replay`] (il run shadow non scrive
    /// step). Best-effort come [`VerifierRunStore::record`]: errore DB loggato,
    /// `Ok(())` ritornato (il `PortError` resta per un contratto rotto).
    async fn persist_step(
        &self,
        run_id: &str,
        iteration: i64,
        idx: i64,
        block: Value,
        result: Option<Value>,
        mode: ExecMode,
    ) -> Result<(), PortError>;
}

/// Persistenza dei meta-step dell'agente su `agent_meta_steps` (plan/routing/
/// clarify/fallback/reflection persistiti per la cronologia, distinti dal canale
/// live SSE).
///
/// SCELTA `MetaStepStore` vs estendere [`EventSink`] (documentata, regola L):
/// resta un TRAIT SEPARATO. `EventSink::emit` e' il canale LIVE verso il
/// frontend — SINCRONO, infallibile, best-effort, NON gata `mode` (lo shadow usa
/// un sink no-op). La PERSISTENZA DB e' invece una scrittura ASYNC, FALLIBILE e
/// GATA `Real/Replay` (no-op shadow): semantica diversa. Fonderle in `EventSink`
/// costringerebbe a un `emit` async fallibile e a un gate `mode` sul canale live,
/// rompendo i call site `emit` esistenti e mescolando due concern (live vs
/// storico). I due trait sono complementari: un meta-step tipicamente si EMETTE
/// (live) e si PERSISTE (storico) nello stesso punto.
#[async_trait]
pub trait MetaStepStore: Send + Sync {
    /// Persiste un meta-step (`meta_step` = JSON `{kind,title,payload}` opaco allo
    /// store). Gata `Real` (INSERT solo in [`ExecMode::Real`], no-op in
    /// [`ExecMode::Replay`], regola L). Best-effort: errore DB loggato, `Ok(())`.
    async fn persist_meta_step(&self, meta_step: Value, mode: ExecMode) -> Result<(), PortError>;
}

/// Offload del contesto verso RAG (Qdrant + embeddings) quando un payload e'
/// troppo grande per restare inline nel contesto del modello.
///
/// CONFINE (regola L): la logica head/tail/pointer-size (DECIDERE cosa offloadare
/// e come troncare) e' PURA e NON vive qui — andra' nel modulo
/// `context_reduction` quando servira'. Questo trait espone SOLO l'I/O di
/// offload (scrittura su Qdrant + ritorno del pointer), che e' infrastrutturale.
/// Best-effort con DEGRADO A TRONCAMENTO: se l'offload fallisce (embed/Qdrant
/// down), l'impl ritorna un `PortError` e il chiamante degrada troncando inline
/// (non blocca il run).
///
/// Gata `Real` (PUNTO UNICO gate shadow, regola L; uniforme con
/// [`AgentStepStore`]/[`RunControlStore`]/[`TodoStore`]/[`VerifierRunStore`]/
/// [`MetaStepStore`]): la SCRITTURA su Qdrant e' un side-effect, quindi
/// `offload_to_rag` riceve `mode` e in [`ExecMode::Replay`] e' un NO-OP che
/// ritorna `PortError` (il chiamante degrada a troncamento testa+coda non-RAG).
/// Cosi' il gate vive nell'impl/porta (un solo punto), NON sparso nel nodo.
#[async_trait]
pub trait ContextOffload: Send + Sync {
    /// Scrive `payload` su RAG e ritorna un POINTER opaco (chiave per il recupero
    /// successivo). Gata `Real`: in [`ExecMode::Replay`] e' un no-op che ritorna
    /// `PortError` (il run shadow non scrive Qdrant). Su guasto infrastrutturale
    /// (anche in Real) ritorna `PortError` (il chiamante degrada a troncamento).
    async fn offload_to_rag(&self, payload: Value, mode: ExecMode) -> Result<String, PortError>;
}

/// Dati di INPUT dell'auto-escalation gia' risolti dall'impl (catena DB + gate
/// cooldown + router cross-provider): tutto cio' che serve a
/// [`crate::decisions::escalation::pick_escalation_model`] per DECIDERE in modo
/// PURO. Il confine d'inversione (regola L): l'I/O (lettura
/// `nexus_model_escalation_chain`, gate ADR 0020, purpose `loop_fallback_default`)
/// vive nell'impl della porta; la SELEZIONE resta nel modulo puro `escalation`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EscalationInputs {
    /// Catena intra-provider per `(provider, model)` correnti, gia' filtrata
    /// (`is_active = TRUE`) e ordinata per `escalation_position` ASC. Vuota se non
    /// c'e' catena per la coppia corrente.
    pub chain: Vec<ChainEntry>,
    /// `true` se il provider corrente e' in cooldown billing/quota (gate ADR 0020):
    /// in tal caso la SELEZIONE salta il Tier 1 intra-provider.
    pub provider_in_cooldown: bool,
    /// Candidato cross-provider (`loop_fallback_default`) risolto dal router, gia'
    /// con sentinelle escluse (`__router_unavailable__` / `__no_capable_provider__`
    /// NON arrivano: l'impl ritorna `None` in quel caso). `None` = nessun Tier 2.
    pub cross_provider: Option<CrossProviderCandidate>,
}

/// Una scelta di proseguimento derivata dal testo dell'assistente (meta_step
/// `next_actions`): `{label, prompt}` (`next_actions.py:11`, contratto frontend).
/// Forma minimale: lo store/sink concreto la serializza nel payload del meta_step
/// (`{"choices": [...]}`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NextActionChoice {
    /// Testo breve del pulsante (max 60 char lato Python).
    pub label: String,
    /// Prompt completo da inviare come messaggio utente proseguendo con la scelta.
    pub prompt: String,
}

/// Astrazione della DERIVAZIONE delle scelte di proseguimento dal testo
/// dell'assistente (`next_actions.derive`, `next_actions.py:451-484`). Confine
/// d'inversione (regola L): la RIMOZIONE deterministica del blocco
/// `<suggested_actions>` dal testo visibile e' PURA e vive in
/// [`crate::decisions::end_turn::strip_suggested_actions`]; QUESTA porta isola SOLO
/// l'I/O della derivazione delle scelte (parse blocco machine-readable -> fallback
/// deterministico "Prossimi passi" -> fallback LLM purpose `choices_extractor`).
///
/// BEST-EFFORT (parita' col try/except py:3401-3402): la derivazione NON deve mai
/// rompere il turno. Su qualunque errore (router giu', provider in cooldown, JSON
/// malformato) l'impl ritorna `Ok(vec![])` (nessuna scelta -> nessun meta_step),
/// MAI un `PortError`. Il blocco `<suggested_actions>` viene rimosso dal nodo a
/// prescindere dall'esito (punto unico deterministico), quindi il testo visibile
/// e' sempre pulito anche quando la derivazione fallisce.
///
/// SOLA LETTURA / nessun side-effect persistente: nessun gate `mode` (il
/// meta_step si persiste/emette a valle via [`MetaStepStore`]/[`EventSink`]).
#[async_trait]
pub trait NextActionsDeriver: Send + Sync {
    /// Deriva le scelte di proseguimento da `cleaned_text` (testo GIA' privo del
    /// blocco `<suggested_actions>`, rimosso a monte dal punto unico
    /// deterministico). Ritorna le scelte (vuoto = nessuna). Best-effort: errore
    /// -> `Ok(vec![])`, mai `PortError` nel flusso normale.
    async fn derive(&self, cleaned_text: &str)
        -> Result<Vec<NextActionChoice>, PortError>;
}

/// Astrazione della LISTA dei provider AI in cooldown billing/quota (fonte unica:
/// `brain.providers.registry.get_billing_cooldown_snapshot`, `__init__.py:1611-1620`).
/// Confine d'inversione: la DECISIONE fail-fast (gate soglia + messaggio) e' PURA
/// e vive in [`crate::decisions::end_turn::billing_fail_fast_message`]; questa
/// porta isola SOLO la lettura dello snapshot cooldown.
///
/// FAIL-OPEN (sicurezza, parita' col best-effort py:1619): un guasto di lettura
/// NON deve bloccare il run — l'impl ritorna `Ok(vec![])` (nessun provider
/// esausto -> nessun fail-fast, il run prosegue), MAI un `PortError`. La lista
/// DEVE arrivare GIA' ORDINATA (`sorted(snap.keys())`, py:1618) per parita' del
/// messaggio. SOLA LETTURA: nessun gate `mode`.
#[async_trait]
pub trait BillingCooldownPort: Send + Sync {
    /// Provider in cooldown billing/quota, ordinati alfabeticamente. Vuoto se
    /// nessuno. Fail-open: errore -> `Ok(vec![])`, mai `PortError`.
    async fn billing_exhausted_providers(&self) -> Result<Vec<String>, PortError>;
}

/// Esito dello smart-upscale risolto dall'I/O: il modello target promosso (con il
/// suo provider risolto via catalog) + reason diagnostica, oppure `None` se non
/// c'e' un candidato adeguato. Forma minimale: il nodo riassegna `provider`/`model`
/// del turno e logga `reason` (`_smart_upscale_model` + `_provider_from_model`,
/// `helpers.py:2742-2800` / `2719-2739`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpscalePick {
    /// Provider del modello promosso (risolto da `ai_price_catalog`).
    pub provider: String,
    /// Modello promosso (window piu' grande nel tier configurato).
    pub model: String,
    /// Reason diagnostica (`context_overflow:est=...:from_window=...:tier=...`).
    pub reason: String,
}

/// Astrazione dell'I/O dello smart-upscale del modello (`_smart_upscale_model`,
/// `helpers.py:2742-2800`). Confine d'inversione (regola L): la DECISIONE di SE
/// tentare l'upscale (`est_tokens >= window*0.9`) + il numero `required` di token
/// sono PURI ([`crate::decisions::end_turn::should_upscale`] /
/// [`crate::decisions::end_turn::upscale_required_tokens`]); questa porta isola
/// l'I/O: il lookup del context window del modello corrente E la SELEZIONE
/// dinamica dal catalog (tier + capability + window >= required) del modello
/// target con provider risolto.
///
/// Tier-based, DB-driven (regola G): nessun nome modello hardcoded; il tier
/// (`agent.upscale.target_tier`) e i flag (`agent.upscale.*`) sono settings letti
/// dall'impl. BEST-EFFORT (parita' col try/except py:2829-2830): un guasto NON
/// deve rompere il turno — l'impl ritorna `Ok(None)` (nessun upscale, prosegue
/// col modello corrente), MAI un `PortError`. SOLA LETTURA: nessun gate `mode`.
#[async_trait]
pub trait ModelUpscalePort: Send + Sync {
    /// Context window (token) del modello corrente (`_model_context_window`,
    /// `helpers.py:3309-3336`). `0` se ignoto: il chiamante salta l'upscale
    /// (`should_upscale` gata `current_window > 0`). Fail-open: errore -> `Ok(0)`.
    async fn context_window(&self, model: &str) -> Result<i64, PortError>;

    /// Seleziona dal catalog un modello con `context_window >= required_tokens`
    /// nel tier configurato (capable per tool use, escluso `agentic_thinking_policy
    /// = 'exclude'`), col provider risolto. `None` se nessun candidato o se il
    /// migliore coincide col modello corrente. Fail-open: errore -> `Ok(None)`.
    async fn select_upscale_model(
        &self,
        current_model: &str,
        required_tokens: i64,
    ) -> Result<Option<UpscalePick>, PortError>;
}

/// Astrazione dell'I/O dell'auto-escalation (catena DB + cooldown + router
/// cross-provider). mcp-core la implementera' leggendo
/// `nexus_model_escalation_chain` (mig 0128), consultando il gate cooldown
/// (ADR 0020, fonte unica) e risolvendo il purpose `loop_fallback_default` dalla
/// routing matrix (regola G). I nodi dipendono solo da questo trait, mai dal DB.
///
/// CONFINE (regola L): qui c'e' SOLO l'I/O che fornisce i dati; la DECISIONE
/// (quale modello promuovere o `None`) e' del modulo puro
/// [`crate::decisions::escalation`], golden-abile in isolamento.
#[async_trait]
pub trait EscalationPort: Send + Sync {
    /// Risolve gli input dell'escalation per il turno corrente: catena
    /// intra-provider di `(provider, model)`, stato cooldown del provider e
    /// candidato cross-provider per `intent`. SOLA LETTURA: nessun gate `mode`.
    ///
    /// FAIL-OPEN (sicurezza): un guasto di lettura (DB/router down) NON deve
    /// bloccare il run — l'impl ritorna `EscalationInputs` "vuoto" (catena vuota,
    /// `provider_in_cooldown=false`, `cross_provider=None`), che fa risolvere la
    /// selezione a `None` (chiusura secca come oggi), MAI un `PortError`. Il
    /// `PortError` resta per un contratto rotto (mai nel flusso normale).
    async fn escalation_inputs(
        &self,
        intent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<EscalationInputs, PortError>;
}
