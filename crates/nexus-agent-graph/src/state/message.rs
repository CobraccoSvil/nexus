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
        /// Reasoning (`reasoning_content`) emesso dal modello in thinking mode
        /// (oggi DeepSeek). VA RI-PASSATO nelle richieste successive: l'API
        /// DeepSeek lo IMPONE per gli assistant message generati in thinking mode
        /// (altrimenti HTTP 400, "The reasoning_content in the thinking mode must
        /// be passed back to the API"). Trasportato fino al wire via
        /// `LlmMessage::reasoning` (porta) -> `GwMessage::reasoning` ->
        /// `reasoning_content` (solo dialetto DeepSeek). `None` per i turni senza
        /// reasoning / gli altri provider. Speculare al `thinking_signature`
        /// Anthropic. Additivo (`serde(default)`): retrocompatibile col round-trip
        /// dello stato persistito.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
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

    /// Estrae il testo "piatto" dal contenuto del messaggio. Per `Text`
    /// restituisce la stringa cosi' com'e'; per `Blocks` concatena i soli blocchi
    /// `Text` con spazio, ignorando `ToolUse`/`ToolResult`.
    ///
    /// Punto unico (regola L): consolidato qui come metodo sul tipo (calcolo puro
    /// stateless, relazione is-a) al posto delle copie locali nei nodi
    /// router/understanding/learner/reflection/clarify_or_expand.
    pub fn flatten_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
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
        /// Exit code STRUTTURATO dei tool-comando (contratto dati A): `Some(0)`
        /// successo, `Some(!=0)` errore. Segnale PRIMARIO per l'esito di un
        /// tool_result (vedi `routing::signals::tool_result_outcome_after`),
        /// equivalente alla chiave `exit_code` del blocco `tool_result` Python.
        /// `None` quando il tool non e' un comando (resta `is_error`/lessicale).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_text_punto_unico() {
        // Text: ritorna la stringa cosi' com'e'.
        assert_eq!(
            MessageContent::Text("ciao".to_string()).flatten_text(),
            "ciao"
        );

        // Blocks: concatena i soli Text con spazio, ignora ToolUse/ToolResult.
        let blocks = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "primo".to_string(),
            },
            ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "edit_file".to_string(),
                input: Value::Null,
            },
            ContentBlock::Text {
                text: "secondo".to_string(),
            },
        ]);
        assert_eq!(blocks.flatten_text(), "primo secondo");

        // Blocks senza alcun Text: stringa vuota.
        let solo_tool = MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "t1".to_string(),
            content: Value::Null,
            is_error: false,
            exit_code: None,
        }]);
        assert_eq!(solo_tool.flatten_text(), "");
    }
}
