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
use serde_json::Value;
use thiserror::Error;

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
