//! Messaggi della conversazione agentica (replica `langchain_core.messages`).
//!
//! Modello dei messaggi scambiati col modello, tipizzato. Replica il dualismo
//! Python `content`-stringa / `additional_kwargs["anthropic_content"]`-blocchi:
//! `MessageContent` puo' essere testo semplice oppure una lista di
//! `ContentBlock` (la forma autoritativa quando ci sono tool_use/tool_result).
//!
//! NB: questa e' la forma "canale interno" (tag `role` user/assistant/tool),
//! quella che il gateway Rust gia' produce/consuma. La traduzione al formato di
//! serializzazione `langchain_core.dumps` (`{lc,type,id,kwargs}`) vive
//! ESCLUSIVAMENTE in `lc_serde.rs` (punto unico, regola L), usata solo ai confini
//! gRPC col brain Python durante la coesistenza.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Un messaggio della conversazione, discriminato dal ruolo.
///
/// Il tag serde `role` produce `{"role":"user",...}` / `{"role":"assistant",...}`
/// / `{"role":"tool",...}`, coerente col formato OpenAI-compat usato dal gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    /// Messaggio dell'utente.
    #[serde(rename = "user")]
    Human {
        /// Contenuto: testo semplice o blocchi (raro per l'utente, ma ammesso).
        content: MessageContent,
    },
    /// Risposta del modello. Puo' includere richieste di tool (`tool_calls`).
    #[serde(rename = "assistant")]
    Ai {
        /// Contenuto testuale o a blocchi (con eventuali ToolUse inline).
        content: MessageContent,
        /// Richieste di tool emesse dal modello. Vuoto se il turno e' testuale.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolUse>,
    },
    /// Risultato dell'esecuzione di un tool, riferito alla richiesta via id.
    #[serde(rename = "tool")]
    Tool {
        /// Id della `ToolUse` a cui questo risultato risponde.
        tool_call_id: String,
        /// Contenuto del risultato (testo o blocchi).
        content: MessageContent,
    },
}

/// Contenuto di un messaggio: o testo semplice, o lista di blocchi tipizzati.
///
/// `untagged`: serde prova prima `Text` (stringa JSON) poi `Blocks` (array). La
/// stringa e l'array non sono ambigui (tipi JSON diversi), quindi il round-trip
/// e' deterministico. La forma `Blocks` e' autoritativa quando il turno contiene
/// tool_use/tool_result; `Text` e' la forma derivata/semplice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Contenuto testuale semplice.
    Text(String),
    /// Contenuto strutturato a blocchi (Anthropic-style).
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    /// Helper: contenuto testuale da una stringa.
    pub fn text(s: impl Into<String>) -> Self {
        MessageContent::Text(s.into())
    }
}

/// Un blocco di contenuto strutturato (Anthropic content block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Blocco di testo.
    Text {
        /// Testo del blocco.
        text: String,
    },
    /// Richiesta di tool inline nel contenuto dell'assistente.
    ToolUse {
        /// Id univoco della richiesta (referenziato dal ToolResult).
        id: String,
        /// Nome del tool da invocare.
        name: String,
        /// Argomenti del tool (JSON arbitrario).
        input: Value,
    },
    /// Risultato di un tool inline (Anthropic-style).
    ToolResult {
        /// Id della ToolUse a cui risponde.
        tool_use_id: String,
        /// Contenuto del risultato (JSON arbitrario: stringa o struttura).
        content: Value,
        /// `true` se il tool ha fallito (errore applicativo).
        #[serde(default)]
        is_error: bool,
    },
}

/// Richiesta di tool emessa dal modello (forma OpenAI-compat `tool_calls`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUse {
    /// Id univoco della chiamata.
    pub id: String,
    /// Nome del tool richiesto.
    pub name: String,
    /// Argomenti del tool (JSON arbitrario).
    pub input: Value,
}
