//! Mapping di `tool_choice` ai dialetti dei provider (punto unico, regola L).
//!
//! Il contratto del gateway riceve `tool_choice` in stile OpenAI Chat
//! Completions (vedi [`crate::types::LlmRequest::tool_choice`]): la stringa
//! `"auto"`/`"required"`/`"none"` oppure l'oggetto
//! `{"type":"function","function":{"name":"X"}}`. Ogni provider parla un
//! dialetto diverso; qui centralizziamo la traduzione cosi' che `complete` e
//! `stream` di ogni provider deleghino alla STESSA funzione (niente logica
//! duplicata tra i due path).
//!
//! Le funzioni sono PURE (input `&serde_json::Value`, output `Value`/`Option`),
//! testabili senza rete: i provider le invocano dentro il rispettivo
//! `build_request_body`.

use serde_json::{json, Value};

/// Forma normalizzata, indipendente dal provider, del vincolo `tool_choice`
/// ricevuto in formato OpenAI. Astrae i 4 casi del contratto cosi' ogni mapper
/// provider lavora su un enum chiuso invece di re-ispezionare il JSON grezzo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    /// `"auto"`: il modello sceglie se chiamare un tool.
    Auto,
    /// `"required"` (alias `"any"`): il modello DEVE chiamare almeno un tool.
    Required,
    /// `"none"`: il modello NON deve chiamare tool.
    None,
    /// `{"type":"function","function":{"name":X}}`: forza il tool nominato.
    Function(String),
}

impl ToolChoice {
    /// Interpreta il valore in stile OpenAI nella forma normalizzata. Ritorna
    /// `None` se il valore non e' riconducibile a un vincolo noto (in tal caso
    /// il provider non invia nulla: equivale ad `auto`, comportamento storico).
    ///
    /// Tollerante: la stringa `"any"` (sinonimo Anthropic talvolta usato dai
    /// chiamanti) e' trattata come `Required`.
    pub fn from_openai(value: &Value) -> Option<Self> {
        match value {
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "auto" => Some(ToolChoice::Auto),
                "required" | "any" => Some(ToolChoice::Required),
                "none" => Some(ToolChoice::None),
                _ => None,
            },
            Value::Object(_) => {
                // `{"type":"function","function":{"name":"X"}}` (forma OpenAI).
                // Tolleriamo anche `{"type":"tool","name":"X"}` (forma Anthropic)
                // per robustezza, ma il contratto canonico e' OpenAI.
                let name = value
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .or_else(|| value.get("name"))
                    .and_then(|n| n.as_str());
                name.filter(|n| !n.is_empty())
                    .map(|n| ToolChoice::Function(n.to_string()))
            }
            _ => None,
        }
    }
}

/// Dialetto OpenAI Chat Completions (OpenAI/DeepSeek/Mistral/vLLM): il valore
/// arriva gia' nel formato nativo, quindi lo inoltriamo TALE E QUALE. Ritorna
/// `None` se non e' un vincolo riconosciuto, cosi' il provider non aggiunge un
/// `tool_choice` ambiguo al body.
pub fn to_openai(value: &Value) -> Option<Value> {
    // Normalizziamo e ri-serializziamo nella forma canonica OpenAI: cosi'
    // `"any"` (sinonimo) diventa `"required"` e gli oggetti sono ripuliti.
    match ToolChoice::from_openai(value)? {
        ToolChoice::Auto => Some(json!("auto")),
        ToolChoice::Required => Some(json!("required")),
        ToolChoice::None => Some(json!("none")),
        ToolChoice::Function(name) => Some(json!({
            "type": "function",
            "function": { "name": name }
        })),
    }
}

/// Dialetto Anthropic Messages: il campo `tool_choice` nel body usa
/// `{"type": ...}`. Mapping:
///   - `required`/`any` -> `{"type":"any"}`;
///   - `auto`           -> `{"type":"auto"}`;
///   - `{function X}`   -> `{"type":"tool","name":"X"}`;
///   - `none`           -> via meno invasiva: Anthropic non ha un "none"
///     esplicito, quindi NON inviamo `tool_choice` (equivale ad `auto` lato
///     API). Ritorna `None` cosi' il provider omette il campo.
///
/// Ritorna `None` anche per valori non riconosciuti (nessun campo inviato).
pub fn to_anthropic(value: &Value) -> Option<Value> {
    match ToolChoice::from_openai(value)? {
        ToolChoice::Required => Some(json!({ "type": "any" })),
        ToolChoice::Auto => Some(json!({ "type": "auto" })),
        ToolChoice::Function(name) => Some(json!({ "type": "tool", "name": name })),
        // Anthropic non ha "none": omettiamo il campo (via meno invasiva).
        ToolChoice::None => None,
    }
}

/// Dialetto Google/Vertex: `tool_config.function_calling_config`. Mapping del
/// solo blocco `function_calling_config` (il provider lo incapsula in
/// `tool_config`):
///   - `required`/`any` -> `{"mode":"ANY"}`;
///   - `auto`           -> `{"mode":"AUTO"}`;
///   - `none`           -> `{"mode":"NONE"}`;
///   - `{function X}`   -> `{"mode":"ANY","allowedFunctionNames":["X"]}`.
///
/// Ritorna `None` per valori non riconosciuti (nessun `tool_config` inviato).
pub fn to_google_function_calling_config(value: &Value) -> Option<Value> {
    match ToolChoice::from_openai(value)? {
        ToolChoice::Required => Some(json!({ "mode": "ANY" })),
        ToolChoice::Auto => Some(json!({ "mode": "AUTO" })),
        ToolChoice::None => Some(json!({ "mode": "NONE" })),
        ToolChoice::Function(name) => Some(json!({
            "mode": "ANY",
            "allowedFunctionNames": [name]
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizza_stringhe_openai() {
        assert_eq!(ToolChoice::from_openai(&json!("auto")), Some(ToolChoice::Auto));
        assert_eq!(
            ToolChoice::from_openai(&json!("required")),
            Some(ToolChoice::Required)
        );
        // "any" sinonimo -> Required.
        assert_eq!(ToolChoice::from_openai(&json!("any")), Some(ToolChoice::Required));
        assert_eq!(ToolChoice::from_openai(&json!("none")), Some(ToolChoice::None));
        // Case-insensitive + trim.
        assert_eq!(
            ToolChoice::from_openai(&json!("  REQUIRED ")),
            Some(ToolChoice::Required)
        );
        // Stringa ignota -> None (nessun vincolo).
        assert_eq!(ToolChoice::from_openai(&json!("boh")), None);
    }

    #[test]
    fn normalizza_oggetto_funzione() {
        let v = json!({"type": "function", "function": {"name": "edit_file"}});
        assert_eq!(
            ToolChoice::from_openai(&v),
            Some(ToolChoice::Function("edit_file".to_string()))
        );
        // Forma Anthropic tollerata.
        let v2 = json!({"type": "tool", "name": "run_command"});
        assert_eq!(
            ToolChoice::from_openai(&v2),
            Some(ToolChoice::Function("run_command".to_string()))
        );
        // Nome vuoto/assente -> None.
        assert_eq!(
            ToolChoice::from_openai(&json!({"type": "function", "function": {"name": ""}})),
            None
        );
        assert_eq!(ToolChoice::from_openai(&json!({"type": "function"})), None);
    }

    #[test]
    fn openai_passthrough_canonico() {
        // "required" resta "required".
        assert_eq!(to_openai(&json!("required")), Some(json!("required")));
        // "any" -> canonicalizzato a "required".
        assert_eq!(to_openai(&json!("any")), Some(json!("required")));
        assert_eq!(to_openai(&json!("auto")), Some(json!("auto")));
        assert_eq!(to_openai(&json!("none")), Some(json!("none")));
        // Oggetto funzione ripulito nella forma OpenAI canonica.
        let f = to_openai(&json!({"type": "function", "function": {"name": "f"}})).unwrap();
        assert_eq!(f["type"], "function");
        assert_eq!(f["function"]["name"], "f");
        // Valore ignoto -> None (niente passthrough).
        assert_eq!(to_openai(&json!("boh")), None);
    }

    #[test]
    fn anthropic_required_diventa_any() {
        assert_eq!(to_anthropic(&json!("required")), Some(json!({"type": "any"})));
        assert_eq!(to_anthropic(&json!("any")), Some(json!({"type": "any"})));
        assert_eq!(to_anthropic(&json!("auto")), Some(json!({"type": "auto"})));
        // none -> omesso (Anthropic non ha "none").
        assert_eq!(to_anthropic(&json!("none")), None);
        // Funzione -> {type:tool, name}.
        assert_eq!(
            to_anthropic(&json!({"type": "function", "function": {"name": "calc"}})),
            Some(json!({"type": "tool", "name": "calc"}))
        );
        // Valore ignoto -> None.
        assert_eq!(to_anthropic(&json!("boh")), None);
    }

    #[test]
    fn google_mappa_mode() {
        assert_eq!(
            to_google_function_calling_config(&json!("required")),
            Some(json!({"mode": "ANY"}))
        );
        assert_eq!(
            to_google_function_calling_config(&json!("auto")),
            Some(json!({"mode": "AUTO"}))
        );
        assert_eq!(
            to_google_function_calling_config(&json!("none")),
            Some(json!({"mode": "NONE"}))
        );
        let f = to_google_function_calling_config(
            &json!({"type": "function", "function": {"name": "search"}}),
        )
        .unwrap();
        assert_eq!(f["mode"], "ANY");
        assert_eq!(f["allowedFunctionNames"][0], "search");
        // Valore ignoto -> None.
        assert_eq!(to_google_function_calling_config(&json!("boh")), None);
    }
}
