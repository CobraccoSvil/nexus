//! Serializzazione nel formato `langchain_core.load.dumps` (punto unico, regola L).
//!
//! PUNTO UNICO che conosce il formato di serializzazione LangChain
//! (`{"lc":1,"type":"constructor","id":[...],"kwargs":{...}}`). Serve SOLO ai
//! confini gRPC col brain Python durante la coesistenza (resume cross-runtime):
//! il checkpointer Rust usa il `serde_json` nativo dello struct, NON questo
//! formato. Tenere questa conoscenza in un solo modulo evita che il formato
//! LangChain "trapeli" nei nodi (regola L).
//!
//! Formato di riferimento (prodotto da `langchain_core.load.dumps`):
//! ```json
//! {"lc": 1, "type": "constructor",
//!  "id": ["langchain", "schema", "messages", "HumanMessage"],
//!  "kwargs": {"content": "...", "type": "human"}}
//! ```
//! AIMessage con tool_calls aggiunge `kwargs.tool_calls = [{"name","args","id","type":"tool_call"}]`;
//! ToolMessage aggiunge `kwargs.tool_call_id`. Il round-trip Python<->Rust e'
//! verificato dal test `#[ignore] round_trip_lc_vs_python` (vedi modulo `tests`).

use serde_json::{json, Map, Value};
use thiserror::Error;

use super::message::{ContentBlock, Message, MessageContent, ToolUse};

/// Errore di (de)serializzazione LangChain. Niente `unwrap`: ogni forma
/// inattesa risale al chiamante con un messaggio diagnostico (regola H).
#[derive(Debug, Error)]
pub enum LcSerdeError {
    /// Il valore non e' un oggetto `{lc,type,id,kwargs}` ben formato.
    #[error("formato LangChain non valido: {0}")]
    Shape(String),
    /// Il tipo di messaggio nell'`id` non e' fra quelli supportati.
    #[error("tipo di messaggio LangChain non supportato: '{0}'")]
    UnknownType(String),
    /// Un campo richiesto manca o ha tipo errato.
    #[error("campo '{0}' mancante o di tipo errato")]
    MissingField(&'static str),
}

/// Suffisso dell'`id` LangChain (il prefisso `langchain.schema.messages` e'
/// implicito): l'ultimo elemento e' il nome della classe.
fn lc_id(class_name: &str) -> Value {
    json!(["langchain", "schema", "messages", class_name])
}

/// Converte un `Message` nel formato `langchain_core.dumps`.
///
/// La forma del contenuto (`MessageContent`) viene serializzata cosi' com'e':
/// stringa per `Text`, array di blocchi per `Blocks` — identico a come LangChain
/// rappresenta `content` (str | list).
pub fn to_lc(message: &Message) -> Value {
    match message {
        Message::Human { content } => {
            let mut kwargs = Map::new();
            kwargs.insert("content".to_string(), content_to_value(content));
            kwargs.insert("type".to_string(), json!("human"));
            constructor("HumanMessage", kwargs)
        }
        Message::Ai {
            content,
            tool_calls,
            reasoning,
        } => {
            let mut kwargs = Map::new();
            kwargs.insert("content".to_string(), content_to_value(content));
            // tool_calls in formato LangChain: {name, args, id, type:"tool_call"}.
            let lc_tool_calls: Vec<Value> = tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "name": tc.name,
                        "args": tc.input,
                        "id": tc.id,
                        "type": "tool_call",
                    })
                })
                .collect();
            kwargs.insert("tool_calls".to_string(), Value::Array(lc_tool_calls));
            // Reasoning DeepSeek (thinking mode) in `additional_kwargs.reasoning_content`,
            // la chiave usata dal brain Python: cosi' sopravvive al round-trip gRPC e
            // puo' essere ri-passato all'API (vincolo HTTP 400). Omesso se assente.
            if let Some(r) = reasoning {
                kwargs.insert(
                    "additional_kwargs".to_string(),
                    json!({ "reasoning_content": r }),
                );
            }
            kwargs.insert("type".to_string(), json!("ai"));
            constructor("AIMessage", kwargs)
        }
        Message::Tool {
            tool_call_id,
            content,
        } => {
            let mut kwargs = Map::new();
            kwargs.insert("content".to_string(), content_to_value(content));
            kwargs.insert("tool_call_id".to_string(), json!(tool_call_id));
            kwargs.insert("type".to_string(), json!("tool"));
            constructor("ToolMessage", kwargs)
        }
    }
}

/// Costruisce l'involucro `{"lc":1,"type":"constructor","id":[...],"kwargs":{...}}`.
fn constructor(class_name: &str, kwargs: Map<String, Value>) -> Value {
    json!({
        "lc": 1,
        "type": "constructor",
        "id": lc_id(class_name),
        "kwargs": Value::Object(kwargs),
    })
}

/// Serializza il contenuto: stringa o array di blocchi (come LangChain).
fn content_to_value(content: &MessageContent) -> Value {
    match content {
        MessageContent::Text(s) => json!(s),
        MessageContent::Blocks(blocks) => {
            // I blocchi seguono il formato Anthropic content block, che e' anche
            // quello accettato da LangChain come `content` lista.
            serde_json::to_value(blocks).unwrap_or(Value::Null)
        }
    }
}

/// Converte un valore nel formato LangChain in un `Message`.
///
/// Tollerante in lettura (la coesistenza riceve messaggi prodotti dal brain
/// Python): legge il tipo dall'`id` LangChain e ricostruisce la variante.
pub fn from_lc(value: &Value) -> Result<Message, LcSerdeError> {
    let obj = value
        .as_object()
        .ok_or_else(|| LcSerdeError::Shape("atteso oggetto JSON".to_string()))?;

    let id = obj
        .get("id")
        .and_then(|v| v.as_array())
        .ok_or_else(|| LcSerdeError::Shape("campo 'id' assente o non array".to_string()))?;
    let class_name = id
        .last()
        .and_then(|v| v.as_str())
        .ok_or_else(|| LcSerdeError::Shape("'id' senza nome di classe".to_string()))?;

    let kwargs = obj
        .get("kwargs")
        .and_then(|v| v.as_object())
        .ok_or_else(|| LcSerdeError::Shape("campo 'kwargs' assente o non oggetto".to_string()))?;

    match class_name {
        "HumanMessage" => {
            let content = value_to_content(kwargs.get("content"))?;
            Ok(Message::Human { content })
        }
        "AIMessage" | "AIMessageChunk" => {
            let content = value_to_content(kwargs.get("content"))?;
            let tool_calls = parse_lc_tool_calls(kwargs.get("tool_calls"))?;
            // Reasoning DeepSeek: il brain Python lo mette in
            // `additional_kwargs.reasoning_content`. Tollerante: accetta anche un
            // `reasoning_content` top-level. Vuoto/assente -> None.
            let reasoning = kwargs
                .get("additional_kwargs")
                .and_then(|v| v.as_object())
                .and_then(|ak| ak.get("reasoning_content"))
                .or_else(|| kwargs.get("reasoning_content"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Ok(Message::Ai {
                content,
                tool_calls,
                reasoning,
            })
        }
        "ToolMessage" => {
            let content = value_to_content(kwargs.get("content"))?;
            let tool_call_id = kwargs
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .ok_or(LcSerdeError::MissingField("tool_call_id"))?
                .to_string();
            Ok(Message::Tool {
                tool_call_id,
                content,
            })
        }
        other => Err(LcSerdeError::UnknownType(other.to_string())),
    }
}

/// Ricostruisce `MessageContent` da `kwargs.content` (stringa o array).
fn value_to_content(value: Option<&Value>) -> Result<MessageContent, LcSerdeError> {
    match value {
        None => Err(LcSerdeError::MissingField("content")),
        Some(Value::String(s)) => Ok(MessageContent::Text(s.clone())),
        Some(Value::Array(_)) => {
            let blocks: Vec<ContentBlock> = serde_json::from_value(value.cloned().unwrap_or(Value::Null))
                .map_err(|e| LcSerdeError::Shape(format!("content a blocchi non valido: {e}")))?;
            Ok(MessageContent::Blocks(blocks))
        }
        Some(_) => Err(LcSerdeError::Shape(
            "content non e' stringa ne array".to_string(),
        )),
    }
}

/// Ricostruisce i `ToolUse` da `kwargs.tool_calls` (formato LangChain).
fn parse_lc_tool_calls(value: Option<&Value>) -> Result<Vec<ToolUse>, LcSerdeError> {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        // Assente o vuoto: nessun tool_call (turno testuale).
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for tc in arr {
        let name = tc
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or(LcSerdeError::MissingField("tool_calls[].name"))?
            .to_string();
        let id = tc
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or(LcSerdeError::MissingField("tool_calls[].id"))?
            .to_string();
        // LangChain usa `args`; in lettura tolleriamo anche `input`.
        let input = tc
            .get("args")
            .or_else(|| tc.get("input"))
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        out.push(ToolUse { id, name, input });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MessageContent;

    /// Round-trip Rust-puro `to_lc -> from_lc` per i tre tipi di messaggio:
    /// non serve Python, valida che la nostra serializzazione sia
    /// auto-consistente (il formato e' lo stesso prodotto da LangChain).
    #[test]
    fn round_trip_to_lc_from_lc_rust() {
        let human = Message::Human {
            content: MessageContent::text("ciao"),
        };
        let ai = Message::Ai {
            content: MessageContent::text("rispondo e chiamo un tool"),
            tool_calls: vec![ToolUse {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                input: json!({"path": "/tmp/x"}),
            }],
            reasoning: None,
        };
        let tool = Message::Tool {
            tool_call_id: "call_1".to_string(),
            content: MessageContent::text("contenuto del file"),
        };

        for msg in [&human, &ai, &tool] {
            let lc = to_lc(msg);
            let back = from_lc(&lc).expect("from_lc deve ricostruire il messaggio");
            assert_eq!(&back, msg, "round-trip to_lc/from_lc deve essere identita'");
        }
    }

    /// Verifica la forma del JSON LangChain prodotto da `to_lc` per un AIMessage
    /// con tool_calls (campo `args`, non `input`; involucro `{lc,type,id,kwargs}`).
    #[test]
    fn to_lc_ai_forma_langchain() {
        let ai = Message::Ai {
            content: MessageContent::text(""),
            tool_calls: vec![ToolUse {
                id: "c1".to_string(),
                name: "edit_file".to_string(),
                input: json!({"a": 1}),
            }],
            reasoning: None,
        };
        let lc = to_lc(&ai);
        assert_eq!(lc["lc"], json!(1));
        assert_eq!(lc["type"], json!("constructor"));
        assert_eq!(lc["id"].as_array().unwrap().last().unwrap(), &json!("AIMessage"));
        let tc = &lc["kwargs"]["tool_calls"][0];
        assert_eq!(tc["name"], json!("edit_file"));
        assert_eq!(tc["args"], json!({"a": 1}));
        assert_eq!(tc["id"], json!("c1"));
        assert_eq!(tc["type"], json!("tool_call"));
    }

    /// Round-trip cross-runtime Python<->Rust.
    ///
    /// PREREQUISITO: generare il file con `langchain_core.load.dumps` lato Python.
    /// Comando (eseguire dentro il venv del brain che ha langchain_core):
    /// ```sh
    /// python - <<'PY'
    /// import json
    /// from langchain_core.load import dumps
    /// from langchain_core.messages import HumanMessage, AIMessage, ToolMessage
    /// msgs = [
    ///     HumanMessage(content="ciao da python"),
    ///     AIMessage(content="ok", tool_calls=[
    ///         {"name": "read_file", "args": {"path": "/tmp/x"}, "id": "call_1", "type": "tool_call"}
    ///     ]),
    ///     ToolMessage(content="contenuto", tool_call_id="call_1"),
    /// ]
    /// with open("/tmp/lc_messages.json", "w") as f:
    ///     json.dump([json.loads(dumps(m)) for m in msgs], f)
    /// PY
    /// ```
    /// Poi: `cargo test -p nexus-agent-graph -- --ignored round_trip_lc_vs_python`
    ///
    /// Il test legge ogni messaggio LangChain, lo converte in `Message` con
    /// `from_lc`, lo ri-serializza con `to_lc` e verifica che i campi semantici
    /// (tipo, content, tool_calls con args/id, tool_call_id) coincidano: cioe'
    /// che il giro Rust preservi la semantica del messaggio prodotto da Python.
    #[test]
    #[ignore = "richiede /tmp/lc_messages.json generato da langchain_core (vedi doc del test)"]
    fn round_trip_lc_vs_python() {
        let raw = std::fs::read_to_string("/tmp/lc_messages.json")
            .expect("manca /tmp/lc_messages.json: generarlo con lo script Python nel doc del test");
        let arr: Vec<Value> = serde_json::from_str(&raw).expect("/tmp/lc_messages.json non e' JSON valido");
        assert_eq!(arr.len(), 3, "atteso esattamente 3 messaggi (Human, AI, Tool)");

        // 1) HumanMessage: content testuale.
        let human = from_lc(&arr[0]).expect("from_lc Human");
        match &human {
            Message::Human { content } => {
                assert_eq!(*content, MessageContent::text("ciao da python"));
            }
            other => panic!("atteso Human, trovato {other:?}"),
        }

        // 2) AIMessage con tool_calls: name/args/id preservati nel round-trip.
        let ai = from_lc(&arr[1]).expect("from_lc Ai");
        match &ai {
            Message::Ai {
                content,
                tool_calls,
                ..
            } => {
                assert_eq!(*content, MessageContent::text("ok"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].name, "read_file");
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].input, json!({"path": "/tmp/x"}));
            }
            other => panic!("atteso Ai, trovato {other:?}"),
        }
        // to_lc deve riprodurre la forma LangChain (args, type=tool_call).
        let ai_lc = to_lc(&ai);
        assert_eq!(ai_lc["kwargs"]["tool_calls"][0]["args"], json!({"path": "/tmp/x"}));
        assert_eq!(ai_lc["kwargs"]["tool_calls"][0]["type"], json!("tool_call"));

        // 3) ToolMessage: tool_call_id preservato.
        let tool = from_lc(&arr[2]).expect("from_lc Tool");
        match &tool {
            Message::Tool {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(*content, MessageContent::text("contenuto"));
            }
            other => panic!("atteso Tool, trovato {other:?}"),
        }
    }
}
