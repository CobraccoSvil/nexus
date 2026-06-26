//! Adapter del trait [`nexus_agent_graph::runtime::ports::LlmGateway`].
//!
//! Implementa `LlmGateway::complete` delegando al gateway LLM concreto di mcp-core
//! ([`crate::nexus_gateway::NexusGatewayClient`], HTTP verso il Nexus LLM Gateway —
//! catena Fallback DB-driven). Il provider/model arrivano gia' risolti nella
//! [`LlmRequest`] (regola G): l'adapter li inoltra al gateway via `pin_provider`,
//! mai li sceglie/hardcoda.
//!
//! RISCHIO NOTO (memoria progetto "Gateway droppava tool_choice" / "google tool
//! monco"): l'impl DEVE onorare `force_tool_choice` end-to-end. La mappatura
//! `force_tool_choice -> tool_choice` viaggia nel campo `GwRequest::tool_choice`
//! (stringa OpenAI `"required"`/`"none"`, omesso = `auto`) che il server gateway
//! propaga al provider; le `tool_calls` tornano in `GwResponse::tool_calls` e
//! diventano `LlmResponse::tool_calls` + `assistant_content` (blocchi
//! `anthropic_content`) per ricostruire il `Message::Ai` con i tool_use originali.
//! Se uno dei due lati si "perdesse" il force-action anti-loop sarebbe
//! neutralizzato: i test before/after lo coprono esplicitamente.

use async_trait::async_trait;
use serde_json::{json, Value};

use nexus_agent_graph::runtime::ports::{
    LlmGateway, LlmRequest, LlmResponse, LlmUsage, PortError,
};
use nexus_agent_graph::state::ToolUse;

use crate::nexus_gateway::{
    GwMessage, GwMetadata, GwRequest, GwResponse, GwToolCall, GwToolFunctionCall,
    NexusGatewayClient,
};

/// Adapter [`LlmGateway`] -> [`NexusGatewayClient`].
pub struct GatewayLlmAdapter {
    /// Client del gateway LLM concreto a cui la `complete` delega.
    gateway: NexusGatewayClient,
}

impl GatewayLlmAdapter {
    /// Costruisce l'adapter sul client gateway concreto.
    pub fn new(gateway: NexusGatewayClient) -> Self {
        Self { gateway }
    }
}

#[async_trait]
impl LlmGateway for GatewayLlmAdapter {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, PortError> {
        let gw_req = build_gw_request(&req);
        let resp = self
            .gateway
            .complete(gw_req)
            .await
            .map_err(|e| PortError::Llm(e.to_string()))?;
        Ok(map_gw_response(resp))
    }
}

/// Mappa una [`LlmRequest`] (porta) nel [`GwRequest`] del client gateway.
///
/// - `provider` -> `pin_provider` (provider gia' risolto a monte dalla routing
///   matrix, regola G: il gateway non deve re-instradare) + `model` prefissato
///   `provider/model` (il pin server strippa il prefisso);
/// - `system_text` -> primo messaggio `role:"system"` (il server estrae il system
///   come campo separato per i provider che lo richiedono, es. Anthropic);
/// - `force_tool_choice` -> `tool_choice` (`Some(true)`->`"required"`,
///   `Some(false)`->`"none"`, `None`->omesso = `auto`);
/// - `tools` -> schema OpenAI atteso dal server (conversione da Anthropic-style se
///   necessario, vedi [`tools_to_openai_schema`]).
fn build_gw_request(req: &LlmRequest) -> GwRequest {
    let mut messages: Vec<GwMessage> = Vec::with_capacity(req.messages.len() + 1);

    // System come PRIMO messaggio con role "system" (forma che il server normalizza
    // per provider). Solo se non vuoto e se non c'e' gia' un system nei messaggi.
    let has_system_msg = req.messages.iter().any(|m| m.role == "system");
    if let Some(sys) = req.system_text.as_ref() {
        if !sys.is_empty() && !has_system_msg {
            messages.push(GwMessage {
                role: "system".to_string(),
                content: Value::String(sys.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }
    }
    for m in &req.messages {
        // CONTINUITA' TOOL MULTI-TURN (regola L): preserva tool_calls (turno
        // assistant con tool) e tool_call_id (turno tool/risultato) end-to-end.
        // I `ToolUse` della porta (id/name/input) diventano `GwToolCall` in forma
        // OpenAI Chat Completions (`{id, type:"function", function:{name,
        // arguments-stringa-JSON}}`), che il server mappa nei block `tool_use`
        // Anthropic. Senza questo i tool_use finivano appiattiti in content e i
        // tool_result perdevano l'id -> HTTP 400.
        let tool_calls = m.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|tc| GwToolCall {
                    id: tc.id.clone(),
                    kind: "function".to_string(),
                    function: GwToolFunctionCall {
                        name: tc.name.clone(),
                        // arguments e' una STRINGA JSON (contratto OpenAI).
                        arguments: serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".to_string()),
                    },
                })
                .collect::<Vec<_>>()
        });
        messages.push(GwMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls,
            tool_call_id: m.tool_call_id.clone(),
        });
    }

    // Modello prefissato col provider risolto: il pin server fa lo strip del
    // prefisso "provider/". Coerente con il path pin di routes.rs.
    let model = if req.provider.is_empty() {
        req.model.clone()
    } else {
        format!("{}/{}", req.provider, req.model)
    };

    GwRequest {
        model,
        messages,
        max_tokens: req.max_tokens.and_then(|m| u32::try_from(m).ok()),
        temperature: None,
        tools: req.tools.as_ref().map(|t| tools_to_openai_schema(t)),
        tool_choice: force_tool_choice_to_value(req.force_tool_choice),
        pin_provider: if req.provider.is_empty() {
            None
        } else {
            Some(req.provider.clone())
        },
        metadata: GwMetadata {
            tenant_id: String::new(),
            user_id: "system".to_string(),
            request_id: req.run_id.clone().unwrap_or_default(),
            sensitivity_tier: 0,
            feature: req.intent.clone().unwrap_or_else(|| "agent".to_string()),
        },
    }
}

/// Mappa `force_tool_choice` al valore `tool_choice` in stile OpenAI atteso dal
/// gateway. PUNTO CRITICO del force-action: `Some(true)` DEVE produrre
/// `"required"` (il modello e' OBBLIGATO a chiamare un tool); `Some(false)` ->
/// `"none"` (turno puramente testuale); `None` -> omesso (= `auto`).
fn force_tool_choice_to_value(force: Option<bool>) -> Option<Value> {
    match force {
        Some(true) => Some(Value::String("required".to_string())),
        Some(false) => Some(Value::String("none".to_string())),
        None => None,
    }
}

/// Normalizza i tool al formato OpenAI Chat Completions atteso dal server gateway
/// (`[{type:"function", function:{name, description?, parameters}}]`).
///
/// I tool del sistema Nexus sono Anthropic-style (`{name, description,
/// input_schema}`, vedi `AGENT_TOOLS_JSON`): vanno convertiti
/// (`input_schema`->`function.parameters`). Un tool gia' in formato OpenAI
/// (chiave `function` presente) e' lasciato invariato. PUNTO UNICO della
/// conversione (regola L): un solo posto traduce lo schema verso il gateway.
fn tools_to_openai_schema(tools: &[Value]) -> Value {
    let converted: Vec<Value> = tools
        .iter()
        .map(|t| {
            // Gia' OpenAI-style: passthrough.
            if t.get("function").is_some() {
                return t.clone();
            }
            // Anthropic-style -> OpenAI.
            let name = t.get("name").cloned().unwrap_or(Value::Null);
            let description = t.get("description").cloned();
            let parameters = t
                .get("input_schema")
                .or_else(|| t.get("parameters"))
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            let mut function = json!({ "name": name, "parameters": parameters });
            if let Some(desc) = description {
                function["description"] = desc;
            }
            json!({ "type": "function", "function": function })
        })
        .collect();
    Value::Array(converted)
}

/// Mappa la [`GwResponse`] del gateway nella [`LlmResponse`] della porta.
///
/// - `tool_calls`: `GwToolCall` -> [`ToolUse`] (l'`arguments` stringa JSON e'
///   parsato in `input`); inoltre ricostruisce `assistant_content` (blocchi
///   `anthropic_content`: `{type:"text",...}` + `{type:"tool_use", id, name,
///   input}`) per il `Message::Ai` con i tool_use originali (continuita'
///   tool_use/tool_result);
/// - `provider_used`/`model_used`: gli EFFETTIVI post cascade/sticky del gateway;
/// - `usage`: cache_read/creation mappati (telemetria token);
/// - `finish_reason` -> `stop_reason`.
fn map_gw_response(resp: GwResponse) -> LlmResponse {
    let tool_calls: Vec<ToolUse> = resp
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tc| ToolUse {
            id: tc.id,
            name: tc.function.name,
            // arguments e' una STRINGA JSON: la parsiamo in Value; se vuota/invalida -> {}.
            input: serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({})),
        })
        .collect();

    // assistant_content (blocchi anthropic_content) per la continuita': blocco
    // text (se non vuoto) seguito dai blocchi tool_use. Vuoto quando non c'e' ne'
    // testo ne' tool_call (l'executor ricostruisce dai campi base).
    let mut assistant_content: Vec<Value> = Vec::new();
    if !resp.content.is_empty() {
        assistant_content.push(json!({ "type": "text", "text": resp.content }));
    }
    for tc in &tool_calls {
        assistant_content.push(json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.input,
        }));
    }
    // Se non c'e' alcun tool_use, NON forziamo un blocco text isolato: lasciamo
    // l'executor costruire dal solo `content` (forma minimale). assistant_content
    // serve quando ci sono tool_use da preservare.
    if tool_calls.is_empty() {
        assistant_content.clear();
    }

    LlmResponse {
        content: resp.content,
        tool_calls,
        usage: LlmUsage {
            prompt_tokens: resp.usage.input_tokens as i64,
            completion_tokens: resp.usage.output_tokens as i64,
            total_tokens: (resp.usage.input_tokens + resp.usage.output_tokens) as i64,
            cache_creation_tokens: resp.usage.cache_creation_tokens.map(|v| v as i64),
            cache_read_tokens: resp.usage.cache_read_tokens.map(|v| v as i64),
            // Il /v1/complete del gateway NON riporta il costo del turno (il ledger
            // lo calcola lato gateway, non e' nella response). Resta None: il
            // chiamante lo derivera' altrove se serve (regola G: niente magic).
            total_cost_usd: None,
        },
        provider_used: Some(resp.provider_used),
        model_used: Some(resp.model_used),
        assistant_content,
        stop_reason: Some(resp.finish_reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexus_gateway::{GwToolCall, GwToolFunctionCall, GwUsage};
    use nexus_agent_graph::runtime::ports::LlmMessage;

    fn base_req() -> LlmRequest {
        LlmRequest {
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: json!("ciao"),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    // ── force_tool_choice end-to-end (BEFORE/AFTER) ───────────────────────────

    #[test]
    fn force_tool_choice_some_true_arriva_required() {
        // AFTER: force_tool_choice=Some(true) -> tool_choice "required" nel GwRequest.
        let mut req = base_req();
        req.force_tool_choice = Some(true);
        req.tools = Some(vec![json!({"name": "edit_file", "input_schema": {"type": "object"}})]);

        let gw = build_gw_request(&req);
        // Il vincolo DEVE essere presente e valere "required" (non droppato).
        assert_eq!(gw.tool_choice, Some(json!("required")));
        // E deve serializzare nel JSON inviato al gateway (prova che ARRIVA al server).
        let body = serde_json::to_value(&gw).unwrap();
        assert_eq!(body["tool_choice"], json!("required"));
    }

    #[test]
    fn force_tool_choice_none_omette_tool_choice() {
        // BEFORE (comportamento auto): force_tool_choice=None -> nessun tool_choice.
        let req = base_req(); // force_tool_choice = None (Default)
        let gw = build_gw_request(&req);
        assert_eq!(gw.tool_choice, None);
        // Omesso nel JSON (skip_serializing_if): il gateway tratta come `auto`.
        let body = serde_json::to_value(&gw).unwrap();
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn force_tool_choice_some_false_diventa_none_testuale() {
        let mut req = base_req();
        req.force_tool_choice = Some(false);
        let gw = build_gw_request(&req);
        assert_eq!(gw.tool_choice, Some(json!("none")));
    }

    // ── provider/model pin + system ───────────────────────────────────────────

    #[test]
    fn provider_va_in_pin_e_model_prefissato() {
        let req = base_req();
        let gw = build_gw_request(&req);
        // provider risolto -> pin_provider esplicito (no re-routing nel gateway).
        assert_eq!(gw.pin_provider.as_deref(), Some("anthropic"));
        // model prefissato provider/model (il pin server strippa il prefisso).
        assert_eq!(gw.model, "anthropic/claude-x");
    }

    #[test]
    fn system_text_diventa_primo_messaggio_system() {
        let mut req = base_req();
        req.system_text = Some("sei un assistente".to_string());
        let gw = build_gw_request(&req);
        assert_eq!(gw.messages[0].role, "system");
        assert_eq!(gw.messages[0].content, json!("sei un assistente"));
        assert_eq!(gw.messages[1].role, "user");
    }

    #[test]
    fn system_text_non_duplicato_se_gia_presente() {
        let mut req = base_req();
        req.system_text = Some("X".to_string());
        req.messages.insert(
            0,
            LlmMessage {
                role: "system".to_string(),
                content: json!("system esistente"),
                ..Default::default()
            },
        );
        let gw = build_gw_request(&req);
        // Un solo messaggio system (quello gia' presente), il system_text non lo duplica.
        let n_system = gw.messages.iter().filter(|m| m.role == "system").count();
        assert_eq!(n_system, 1);
        assert_eq!(gw.messages[0].content, json!("system esistente"));
    }

    // ── tools: conversione Anthropic-style -> OpenAI ──────────────────────────

    #[test]
    fn tools_anthropic_convertiti_in_openai() {
        let req = {
            let mut r = base_req();
            r.tools = Some(vec![json!({
                "name": "read_file",
                "description": "legge un file",
                "input_schema": {"type": "object", "properties": {}}
            })]);
            r
        };
        let gw = build_gw_request(&req);
        let tools = gw.tools.unwrap();
        let t = &tools[0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["function"]["name"], "read_file");
        assert_eq!(t["function"]["description"], "legge un file");
        // input_schema -> function.parameters
        assert_eq!(t["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn tools_gia_openai_passthrough() {
        let req = {
            let mut r = base_req();
            r.tools = Some(vec![json!({
                "type": "function",
                "function": {"name": "x", "parameters": {"type": "object"}}
            })]);
            r
        };
        let gw = build_gw_request(&req);
        let tools = gw.tools.unwrap();
        assert_eq!(tools[0]["function"]["name"], "x");
    }

    // ── tool_calls round-trip (GwResponse -> LlmResponse) ─────────────────────

    fn gw_resp_with_tool_call() -> GwResponse {
        GwResponse {
            content: "procedo".to_string(),
            tool_calls: Some(vec![GwToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: GwToolFunctionCall {
                    name: "edit_file".to_string(),
                    arguments: r#"{"path":"a.rs","content":"x"}"#.to_string(),
                },
            }]),
            usage: GwUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: Some(3),
                cache_creation_tokens: Some(7),
            },
            model_used: "claude-real".to_string(),
            provider_used: "anthropic".to_string(),
            latency_ms: 42,
            finish_reason: "tool_calls".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
        }
    }

    #[test]
    fn tool_calls_round_trip_popolato_e_assistant_content() {
        let out = map_gw_response(gw_resp_with_tool_call());
        // tool_calls popolato.
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].id, "call_1");
        assert_eq!(out.tool_calls[0].name, "edit_file");
        // arguments (stringa JSON) parsato in input.
        assert_eq!(out.tool_calls[0].input["path"], "a.rs");

        // assistant_content: blocco text + blocco tool_use (per il Message::Ai).
        assert_eq!(out.assistant_content.len(), 2);
        assert_eq!(out.assistant_content[0]["type"], "text");
        assert_eq!(out.assistant_content[0]["text"], "procedo");
        assert_eq!(out.assistant_content[1]["type"], "tool_use");
        assert_eq!(out.assistant_content[1]["id"], "call_1");
        assert_eq!(out.assistant_content[1]["name"], "edit_file");
        assert_eq!(out.assistant_content[1]["input"]["content"], "x");

        // I blocchi assistant_content sono deserializzabili in ContentBlock
        // (forma autoritativa attesa da build_assistant_message dell'executor).
        for b in &out.assistant_content {
            serde_json::from_value::<nexus_agent_graph::state::ContentBlock>(b.clone())
                .expect("assistant_content deserializzabile in ContentBlock");
        }

        // provider/model EFFETTIVI + stop_reason.
        assert_eq!(out.provider_used.as_deref(), Some("anthropic"));
        assert_eq!(out.model_used.as_deref(), Some("claude-real"));
        assert_eq!(out.stop_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn usage_cache_mappato() {
        let out = map_gw_response(gw_resp_with_tool_call());
        assert_eq!(out.usage.prompt_tokens, 10);
        assert_eq!(out.usage.completion_tokens, 5);
        assert_eq!(out.usage.total_tokens, 15);
        assert_eq!(out.usage.cache_read_tokens, Some(3));
        assert_eq!(out.usage.cache_creation_tokens, Some(7));
        // Il /v1/complete non riporta il costo: None (niente magic).
        assert_eq!(out.usage.total_cost_usd, None);
    }

    #[test]
    fn turno_testuale_senza_tool_calls_assistant_content_vuoto() {
        let mut resp = gw_resp_with_tool_call();
        resp.tool_calls = None;
        resp.content = "solo testo".to_string();
        let out = map_gw_response(resp);
        assert!(out.tool_calls.is_empty());
        // Nessun tool_use da preservare -> assistant_content vuoto (l'executor
        // ricostruisce dal solo content).
        assert!(out.assistant_content.is_empty());
        assert_eq!(out.content, "solo testo");
    }

    // ── continuita' tool multi-turn: LlmMessage -> GwMessage (bug 2026-06-26) ──

    #[test]
    fn multi_turn_assistant_tool_calls_e_tool_role_id_preservati() {
        use nexus_agent_graph::state::ToolUse;

        // Sequenza multi-step: [Human, Ai-con-tool_use(id=X), Tool(tool_call_id=X)].
        let req = LlmRequest {
            provider: "anthropic".to_string(),
            model: "claude-x".to_string(),
            messages: vec![
                LlmMessage {
                    role: "user".to_string(),
                    content: json!("leggi a.rs"),
                    ..Default::default()
                },
                LlmMessage {
                    role: "assistant".to_string(),
                    content: json!("procedo a leggere"),
                    tool_calls: Some(vec![ToolUse {
                        id: "call_X".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.rs"}),
                    }]),
                    tool_call_id: None,
                },
                LlmMessage {
                    role: "tool".to_string(),
                    content: json!("contenuto di a.rs"),
                    tool_calls: None,
                    tool_call_id: Some("call_X".to_string()),
                },
            ],
            ..Default::default()
        };

        let gw = build_gw_request(&req);
        // messages[0] = user (nessun tool).
        assert_eq!(gw.messages[0].role, "user");
        assert!(gw.messages[0].tool_calls.is_none());

        // messages[1] = assistant con tool_calls NON vuoto contenente id=call_X.
        // Il tool_use NON deve essere appiattito in content (resta il testo).
        let ai = &gw.messages[1];
        assert_eq!(ai.role, "assistant");
        let calls = ai.tool_calls.as_ref().expect("assistant deve avere tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_X");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "read_file");
        // arguments e' una STRINGA JSON (contratto OpenAI).
        let args: Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.rs");
        // Il content NON contiene il tool_use appiattito.
        assert_eq!(ai.content, json!("procedo a leggere"));

        // messages[2] = role "tool" con tool_call_id = call_X (round-trip).
        let tool = &gw.messages[2];
        assert_eq!(tool.role, "tool");
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_X"));
        assert_eq!(tool.content, json!("contenuto di a.rs"));

        // Coerenza id: tool_use dell'assistant == tool_call_id del messaggio tool.
        assert_eq!(calls[0].id, tool.tool_call_id.clone().unwrap());

        // Serializzazione wire: il JSON inviato al gateway porta i campi (prova che
        // ARRIVANO al server `to_anthropic_messages`, che riconosce la coppia).
        let body = serde_json::to_value(&gw).unwrap();
        let m1 = &body["messages"][1];
        assert_eq!(m1["role"], "assistant");
        assert_eq!(m1["tool_calls"][0]["id"], "call_X");
        assert_eq!(m1["tool_calls"][0]["type"], "function");
        // L'assistant NON serializza tool_call_id (None -> omesso).
        assert!(m1.get("tool_call_id").is_none());
        let m2 = &body["messages"][2];
        assert_eq!(m2["role"], "tool");
        assert_eq!(m2["tool_call_id"], "call_X");
        // Il messaggio tool NON serializza tool_calls (None -> omesso).
        assert!(m2.get("tool_calls").is_none());
    }
}
