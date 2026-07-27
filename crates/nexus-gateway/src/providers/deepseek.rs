//! Provider DeepSeek (OpenAI-compatibile) con fix XML tool-call + reasoning.
//!
//! Porting di `packages/llm-gateway/src/providers/deepseek.ts` + parita' con
//! `brain/providers/deepseek_provider.py`. DeepSeek a volte emette le tool-call
//! in formato XML Anthropic-style dentro il campo `content` invece del formato
//! strutturato OpenAI `tool_calls`. Questo modulo intercetta il blocco
//! `<tool_calls>...</tool_calls>`, lo converte in [`LlmToolCall`] strutturati e
//! ripulisce il content.
//!
//! Reasoning: i dual-mode V4 girano in thinking mode di default; il thinking si
//! governa col parametro ufficiale `extra_body.thinking.type` (enabled/disabled)
//! e il reasoning torna nel campo separato `reasoning_content` (response/stream),
//! mappato in `reasoning`/`reasoning_delta` dal client compat (punto unico).
//!
//! La logica di parsing XML e' un punto unico ([`parse_xml_tool_calls`] /
//! [`strip_xml_tool_calls`]) riusata sia da `complete` (post-processing) sia da
//! `stream` (accumulo + emissione tool-call al termine).

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use regex::Regex;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::{OpenAiCompatClient, ReasoningDialect, ResolvedReasoning};
use crate::types::{
    LlmRequest, LlmResponse, LlmStreamChunk, LlmToolCall, SensitivityTier, ToolCallDelta,
    ToolCallDeltaFunction, ToolFunctionCall,
};

const TIERS: &[SensitivityTier] = &[0, 1, 2];

/// Endpoint DeepSeek di default (override via costruttore).
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

// Regex compilate una sola volta in static `LazyLock<Regex>`. Porting fedele
// dei pattern TS:
//   TOOL_CALLS_XML_RE = /<tool_calls>\s*([\s\S]*?)\s*<\/tool_calls>/
//   INVOKE_RE         = /<invoke\s+name=["']([^"']+)["']\s*>([\s\S]*?)<\/invoke>/g
//   PARAM_RE          = /<parameter\s+name=["']([^"']+)["'](?:\s+[^>]*)?>([^<]*)<\/parameter>/g
// In Rust il flag `s` (dotAll) e' `(?s)`; `[\s\S]` resta valido ed equivalente.
// safety: i pattern sono literal hardcoded e validi a build-time; l'`unwrap`
// e' infallibile (stesso pattern di `crates/mcp-ast/src/lib.rs`), non opera su
// input runtime.
static TOOL_CALLS_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_calls>\s*(.*?)\s*</tool_calls>").unwrap());

static INVOKE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<invoke\s+name=["']([^"']+)["']\s*>(.*?)</invoke>"#).unwrap());

static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<parameter\s+name=["']([^"']+)["'](?:\s+[^>]*)?>([^<]*)</parameter>"#).unwrap()
});

/// Tenta di estrarre tool-call XML dal `content`. Ritorna `None` se assenti.
///
/// Per ogni `<parameter>` prova a interpretare il valore come JSON (numeri,
/// booleani, oggetti); fallback a stringa grezza (parita' col `JSON.parse` con
/// try/catch del TS). Gli argomenti vengono poi serializzati come stringa JSON,
/// come richiede il contratto [`ToolFunctionCall::arguments`].
fn parse_xml_tool_calls(content: &str) -> Option<Vec<LlmToolCall>> {
    let block = TOOL_CALLS_BLOCK_RE.captures(content)?.get(1)?.as_str();

    let mut calls = Vec::new();
    for (i, inv) in INVOKE_RE.captures_iter(block).enumerate() {
        let tool_name = inv.get(1).map(|m| m.as_str()).unwrap_or_default();
        let params_body = inv.get(2).map(|m| m.as_str()).unwrap_or_default();

        let mut args = serde_json::Map::new();
        for p in PARAM_RE.captures_iter(params_body) {
            let key = p.get(1).map(|m| m.as_str()).unwrap_or_default();
            let raw_value = p.get(2).map(|m| m.as_str().trim()).unwrap_or_default();
            let value = serde_json::from_str::<serde_json::Value>(raw_value)
                .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
            args.insert(key.to_string(), value);
        }

        let arguments = serde_json::to_string(&serde_json::Value::Object(args))
            .unwrap_or_else(|_| "{}".to_string());

        calls.push(LlmToolCall {
            id: synthetic_id(tool_name, i),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: tool_name.to_string(),
                arguments,
            },
            // Firma per-call specifica di Gemini: assente su DeepSeek.
            thought_signature: None,
        });
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

/// Genera un id sintetico per una tool-call XML (parita' col TS
/// `xmltc_{name}_{Date.now()}_{idx}`).
fn synthetic_id(tool_name: &str, idx: usize) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("xmltc_{tool_name}_{ts}_{idx}")
}

/// Rimuove il blocco `<tool_calls>...</tool_calls>` dal content, lasciando il
/// testo prima/dopo (trim finale come nel TS).
fn strip_xml_tool_calls(content: &str) -> String {
    TOOL_CALLS_BLOCK_RE.replace(content, "").trim().to_string()
}

/// Risolve il dialetto reasoning per la richiesta.
///
/// GATE THINKING+TOOL_CHOICE (incidente HTTP 400 "Thinking mode does not support
/// tool_choice"): i dual-mode V4 girano in thinking ON di default, ma l'API
/// DeepSeek RIFIUTA la combinazione thinking + `tool_choice`. Nei run agentici il
/// force-action invia `tool_choice` ("required"/"none"/funzione) e, senza
/// preferenza esplicita di thinking, il default ON produceva un 400 sistematico
/// (status failed, iteration 1, 0 token). Quindi: SE la richiesta porta un
/// `tool_choice` riconosciuto, FORZIAMO `thinking.type=disabled` (dialetto
/// DeepSeek con `enabled=false`), a prescindere da `req.thinking`. E' la
/// traduzione, al confine col provider, della policy catalog
/// `agentic_thinking_policy='disable_for_tools'`: deepseek non puo' pensare
/// MENTRE gli si vincola la scelta del tool. Regola G: nessun nome modello
/// hardcoded — il gate scatta sul dialetto reasoning del provider (DeepSeek) +
/// la presenza del vincolo, non su una stringa modello.
///
/// Senza `tool_choice`: comportamento storico. Inviamo `extra_body.thinking.type`
/// SOLO quando il chiamante esprime una preferenza esplicita via `req.thinking`;
/// altrimenti DeepSeek usa il suo default (thinking ON sui dual-mode V4) ->
/// dialetto base, niente extra_body. Il riconoscimento del vincolo `tool_choice`
/// delega al PUNTO UNICO di mapping ([`super::tool_choice::ToolChoice::from_openai`],
/// regola L): non re-ispezioniamo il JSON grezzo qui.
fn resolve_reasoning(req: &LlmRequest) -> ResolvedReasoning {
    // Thinking OFF forzato quando la richiesta contiene TOOL (a prescindere da
    // tool_choice). L'API DeepSeek in thinking mode impone due vincoli che il
    // nostro agent-loop non soddisfa: (1) rifiuta thinking + `tool_choice`
    // ("Thinking mode does not support tool_choice"); (2) pretende che il
    // `reasoning_content` prodotto venga rimandato in ogni iterazione successiva
    // ("The reasoning_content in the thinking mode must be passed back to the
    // API"). Nei run agentici i tool sono SEMPRE presenti e con `tool_choice`
    // anche solo "auto" il default thinking ON genera reasoning_content che il
    // loop non ripropaga -> 400 al turno dopo. Disabilitare il thinking ogni
    // volta che ci sono tool e' la traduzione al confine col provider della policy
    // catalog `agentic_thinking_policy='disable_for_tools'` e azzera entrambi i
    // 400. Punto unico per complete + stream.
    let has_tools = req.tools.as_ref().is_some_and(|t| !t.is_empty());
    let has_tool_choice_constraint = req
        .tool_choice
        .as_ref()
        .and_then(super::tool_choice::ToolChoice::from_openai)
        .is_some();
    if has_tools || has_tool_choice_constraint {
        return ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: false,
            effort: None,
        };
    }

    match req.thinking.as_ref() {
        Some(t) => ResolvedReasoning {
            dialect: ReasoningDialect::DeepSeek,
            enabled: t.enabled,
            effort: None,
        },
        None => ResolvedReasoning::none(),
    }
}

/// Post-processa una risposta DeepSeek: converte eventuali tool-call XML se la
/// risposta non ne ha gia' di native.
fn fixup_response(mut resp: LlmResponse) -> LlmResponse {
    // Tool-call native gia' presenti: non toccare nulla.
    if resp.tool_calls.as_ref().is_some_and(|v| !v.is_empty()) {
        return resp;
    }
    if !resp.content.contains("<tool_calls>") {
        return resp;
    }
    if let Some(parsed) = parse_xml_tool_calls(&resp.content) {
        resp.content = strip_xml_tool_calls(&resp.content);
        resp.tool_calls = Some(parsed);
        resp.finish_reason = "tool_calls".to_string();
    }
    resp
}

pub struct DeepSeekProvider {
    client: OpenAiCompatClient,
}

impl DeepSeekProvider {
    pub fn new(http: Client, api_key: impl Into<String>, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            client: OpenAiCompatClient::new(http, base_url, api_key, "deepseek"),
        }
    }
}

#[async_trait]
impl LlmProvider for DeepSeekProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn max_context_tokens(&self) -> u32 {
        65_536
    }

    fn tier_compatibility(&self) -> &[SensitivityTier] {
        TIERS
    }

    async fn complete(&self, req: &LlmRequest) -> anyhow::Result<LlmResponse> {
        let reasoning = resolve_reasoning(req);
        let resp = self.client.complete_with_reasoning(req, &reasoning).await?;
        Ok(fixup_response(resp))
    }

    /// Streaming con detection XML: accumula i chunk; se al termine non ci sono
    /// tool-call native ma il content contiene XML, le emette come delta
    /// strutturati (parita' col generatore TS). I chunk di reasoning
    /// (`reasoning_delta`) viaggiano invariati nel ramo normale.
    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let reasoning = resolve_reasoning(req);
        let mut inner = self.client.stream_with_reasoning(req, &reasoning).await?;

        let mut accumulated = String::new();
        let mut has_native_tool_calls = false;
        let mut buffered: Vec<LlmStreamChunk> = Vec::new();
        let mut last_usage = None;

        while let Some(item) = inner.next().await {
            let chunk = item?;
            if chunk.tool_call_delta.is_some() {
                has_native_tool_calls = true;
            }
            accumulated.push_str(&chunk.delta);
            if chunk.usage.is_some() {
                last_usage = chunk.usage;
            }
            buffered.push(chunk);
        }

        // Tool-call native: ri-emette i chunk accumulati invariati.
        if has_native_tool_calls || !accumulated.contains("<tool_calls>") {
            let out = futures::stream::iter(buffered.into_iter().map(Ok));
            return Ok(out.boxed());
        }

        // XML tool-call: ricostruisce uno stream pulito.
        if let Some(parsed) = parse_xml_tool_calls(&accumulated) {
            let mut rebuilt: Vec<LlmStreamChunk> = Vec::new();

            let clean = strip_xml_tool_calls(&accumulated);
            if !clean.is_empty() {
                rebuilt.push(LlmStreamChunk {
                    delta: clean,
                    tool_call_delta: None,
                    finish_reason: None,
                    usage: None,
                    provider_used: Some("deepseek".to_string()),
                    model_used: None,
                    reasoning_delta: None,
                });
            }

            for (i, tc) in parsed.iter().enumerate() {
                rebuilt.push(LlmStreamChunk {
                    delta: String::new(),
                    tool_call_delta: Some(ToolCallDelta {
                        index: i as u32,
                        id: Some(tc.id.clone()),
                        function: Some(ToolCallDeltaFunction {
                            name: Some(tc.function.name.clone()),
                            arguments: Some(tc.function.arguments.clone()),
                        }),
                    }),
                    finish_reason: None,
                    usage: None,
                    provider_used: Some("deepseek".to_string()),
                    model_used: None,
                    reasoning_delta: None,
                });
            }

            rebuilt.push(LlmStreamChunk {
                delta: String::new(),
                tool_call_delta: None,
                finish_reason: Some("tool_calls".to_string()),
                usage: last_usage,
                provider_used: Some("deepseek".to_string()),
                model_used: None,
                reasoning_delta: None,
            });

            let out = futures::stream::iter(rebuilt.into_iter().map(Ok));
            return Ok(out.boxed());
        }

        // Nessuna XML tool-call valida: ri-emette i chunk originali.
        let out = futures::stream::iter(buffered.into_iter().map(Ok));
        Ok(out.boxed())
    }

    async fn healthcheck(&self) -> bool {
        self.client.healthcheck().await
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.client.list_models().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        LlmMessage, LlmToolDefinition, LlmUsage, MessageContent, RequestMetadata, ThinkingConfig,
        ToolFunctionDef,
    };

    fn base_response(content: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            tool_calls: None,
            usage: LlmUsage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            },
            model_used: "deepseek-x".to_string(),
            provider_used: "deepseek".to_string(),
            latency_ms: 1,
            finish_reason: "stop".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger_entry: None,
        }
    }

    fn req_with_thinking(thinking: Option<ThinkingConfig>) -> LlmRequest {
        LlmRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![LlmMessage {
                role: "user".to_string(),
                content: MessageContent::Text("ciao".to_string()),
                tool_call_id: None,
                tool_calls: None,
                name: None,
                thinking_signature: None,
                reasoning: None,
            }],
            temperature: None,
            max_tokens: Some(1024),
            tools: None,
            response_format: None,
            stream: None,
            thinking,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".to_string(),
                user_id: "u".to_string(),
                request_id: "r".to_string(),
                sensitivity_tier: 0,
                feature: "f".to_string(),
            },
            run_timeout_secs: None,
        }
    }

    #[test]
    fn capacita_dichiarate() {
        let p = DeepSeekProvider::new(Client::new(), "k", None);
        assert_eq!(p.name(), "deepseek");
        assert_eq!(p.max_context_tokens(), 65_536);
        assert_eq!(p.tier_compatibility(), &[0, 1, 2]);
    }

    #[test]
    fn reasoning_assente_senza_preferenza() {
        // Nessun req.thinking -> dialetto base, niente extra_body (default DeepSeek).
        let r = resolve_reasoning(&req_with_thinking(None));
        assert_eq!(r.dialect, ReasoningDialect::None);
    }

    #[test]
    fn reasoning_enabled_invia_dialetto_deepseek() {
        let r = resolve_reasoning(&req_with_thinking(Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        })));
        assert_eq!(r.dialect, ReasoningDialect::DeepSeek);
        assert!(r.enabled);
    }

    #[test]
    fn reasoning_disabled_invia_dialetto_deepseek_off() {
        let r = resolve_reasoning(&req_with_thinking(Some(ThinkingConfig {
            enabled: false,
            budget_tokens: None,
            mandatory: false,
        })));
        assert_eq!(r.dialect, ReasoningDialect::DeepSeek);
        assert!(!r.enabled);
    }

    #[test]
    fn tool_choice_required_forza_thinking_off() {
        // GATE: il force-action (tool_choice "required") DEVE disabilitare il
        // thinking anche SENZA preferenza esplicita -> niente piu' HTTP 400
        // "Thinking mode does not support tool_choice". Senza il gate qui il
        // default DeepSeek (thinking ON) + tool_choice generava il 400.
        let mut req = req_with_thinking(None);
        req.tool_choice = Some(serde_json::json!("required"));
        let r = resolve_reasoning(&req);
        assert_eq!(r.dialect, ReasoningDialect::DeepSeek);
        assert!(!r.enabled, "tool_choice presente -> thinking forzato OFF");
    }

    #[test]
    fn tool_choice_funzione_forza_thinking_off() {
        // Anche il forcing di un tool nominato innesca il gate (l'API rifiuta
        // QUALSIASI tool_choice in thinking mode).
        let mut req = req_with_thinking(None);
        req.tool_choice =
            Some(serde_json::json!({"type": "function", "function": {"name": "edit_file"}}));
        let r = resolve_reasoning(&req);
        assert!(!r.enabled);
    }

    #[test]
    fn tool_choice_none_forza_thinking_off() {
        // tool_choice="none" (turno testuale forzato) e' comunque un vincolo che
        // l'API DeepSeek non accetta in thinking mode: thinking OFF.
        let mut req = req_with_thinking(None);
        req.tool_choice = Some(serde_json::json!("none"));
        let r = resolve_reasoning(&req);
        assert!(!r.enabled);
    }

    #[test]
    fn tool_choice_vince_sulla_preferenza_thinking_on() {
        // Anche se il chiamante ha chiesto thinking ON, la presenza di tool_choice
        // ha la precedenza (l'API rifiuterebbe la combinazione): thinking OFF.
        let mut req = req_with_thinking(Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        }));
        req.tool_choice = Some(serde_json::json!("required"));
        let r = resolve_reasoning(&req);
        assert!(!r.enabled);
    }

    #[test]
    fn senza_tool_choice_comportamento_storico_invariato() {
        // Nessun tool_choice: il gate non scatta, resta il comportamento storico
        // (preferenza esplicita rispettata; assente -> default DeepSeek).
        let r_on = resolve_reasoning(&req_with_thinking(Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        })));
        assert!(r_on.enabled);
        let r_none = resolve_reasoning(&req_with_thinking(None));
        assert_eq!(r_none.dialect, ReasoningDialect::None);
    }

    /// Un tool qualunque, per esercitare il ramo `has_tools` di
    /// `resolve_reasoning`. Schema `object` vuoto ma valido.
    fn tool_qualunque() -> LlmToolDefinition {
        LlmToolDefinition {
            kind: "function".to_string(),
            function: ToolFunctionDef {
                name: "read_file".to_string(),
                description: None,
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                strict: None,
            },
        }
    }

    #[test]
    fn tool_presenti_senza_tool_choice_forzano_thinking_off() {
        // GATE, ramo `has_tools`: e' QUESTO il ramo che scatta in produzione, dove
        // i run agentici mandano sempre i tool e spesso senza `tool_choice`.
        // Spegnere il thinking qui e' cio' che azzera il 400 "The
        // reasoning_content in the thinking mode must be passed back to the API":
        // senza thinking non nasce `reasoning_content`, quindi non c'e' niente da
        // rimandare al turno successivo.
        //
        // Il ramo era SCOPERTO: gli altri otto test del gate passano tutti da
        // `tool_choice` e la fixture `req_with_thinking` ha `tools: None`, quindi
        // togliendo `has_tools ||` dalla condizione restavano tutti verdi e il 400
        // tornava in produzione. Il test di parita' live
        // (`tests/provider_tool_loop.rs`) non puo' sostituirlo: senza thinking non
        // riesce a provocare quel 400 nemmeno con la chiave (regola O).
        let mut req = req_with_thinking(None);
        req.tools = Some(vec![tool_qualunque()]);
        assert!(req.tool_choice.is_none(), "premessa: nessun tool_choice");

        let r = resolve_reasoning(&req);
        assert_eq!(r.dialect, ReasoningDialect::DeepSeek);
        assert!(
            !r.enabled,
            "tool presenti (anche senza tool_choice) -> thinking forzato OFF"
        );
    }

    #[test]
    fn tool_presenti_vincono_sulla_preferenza_thinking_on() {
        // Anche una richiesta esplicita di thinking ON cede alla presenza di tool:
        // l'alternativa e' un `reasoning_content` che il loop dovrebbe ripropagare
        // a ogni iterazione, e che al primo turno mancante da' 400.
        let mut req = req_with_thinking(Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        }));
        req.tools = Some(vec![tool_qualunque()]);

        let r = resolve_reasoning(&req);
        assert!(!r.enabled, "tool presenti -> thinking OFF nonostante enabled");
    }

    #[test]
    fn lista_tool_vuota_non_innesca_il_gate() {
        // `Some(vec![])` non e' "ci sono tool": senza tool veri il vincolo
        // dell'API non esiste e vale il comportamento storico (preferenza
        // esplicita rispettata). Fissa il confine del ramo.
        let mut req = req_with_thinking(Some(ThinkingConfig {
            enabled: true,
            budget_tokens: None,
            mandatory: false,
        }));
        req.tools = Some(vec![]);

        let r = resolve_reasoning(&req);
        assert!(r.enabled, "lista tool vuota -> gate non scatta");
    }

    #[test]
    fn tool_choice_non_riconosciuto_non_innesca_gate() {
        // Una stringa tool_choice ignota non e' un vincolo riconosciuto dal punto
        // unico: il gate non scatta, vale il comportamento storico (default).
        let mut req = req_with_thinking(None);
        req.tool_choice = Some(serde_json::json!("boh"));
        let r = resolve_reasoning(&req);
        assert_eq!(r.dialect, ReasoningDialect::None);
    }

    #[test]
    fn fixup_converte_xml_tool_call() {
        let content = "Penso di usare un tool.\n\
            <tool_calls>\n\
            <invoke name=\"read_file\">\n\
            <parameter name=\"path\">/tmp/x.txt</parameter>\n\
            <parameter name=\"limit\">10</parameter>\n\
            </invoke>\n\
            </tool_calls>";
        let resp = fixup_response(base_response(content));

        assert_eq!(resp.finish_reason, "tool_calls");
        assert_eq!(resp.content, "Penso di usare un tool.");
        let calls = resp.tool_calls.expect("tool_calls convertite");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");

        // Gli argomenti sono JSON valido: path stringa, limit numero.
        let args: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "/tmp/x.txt");
        assert_eq!(args["limit"], 10);
    }

    #[test]
    fn fixup_non_tocca_risposta_senza_xml() {
        let resp = fixup_response(base_response("risposta normale senza tool"));
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.content, "risposta normale senza tool");
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn fixup_rispetta_tool_calls_native() {
        let mut resp = base_response("<tool_calls><invoke name=\"x\"></invoke></tool_calls>");
        resp.tool_calls = Some(vec![LlmToolCall {
            id: "native_1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "native".to_string(),
                arguments: "{}".to_string(),
            },
            thought_signature: None,
        }]);
        let out = fixup_response(resp);
        // Tool-call native presenti: il content XML NON viene ripulito.
        let calls = out.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "native_1");
    }

    #[test]
    fn fixup_preserva_reasoning() {
        // Il fixup XML non deve perdere il reasoning gia' estratto dal content.
        let mut resp = base_response("risposta normale");
        resp.reasoning = Some("ho ragionato".to_string());
        let out = fixup_response(resp);
        assert_eq!(out.reasoning.as_deref(), Some("ho ragionato"));
    }

    #[test]
    fn parse_xml_multi_invoke() {
        let content = "<tool_calls>\n\
            <invoke name=\"a\"><parameter name=\"k\">1</parameter></invoke>\n\
            <invoke name=\"b\"><parameter name=\"k\">\"due\"</parameter></invoke>\n\
            </tool_calls>";
        let calls = parse_xml_tool_calls(content).expect("due invoke");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "a");
        assert_eq!(calls[1].function.name, "b");

        let args_b: serde_json::Value =
            serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(args_b["k"], "due");
    }

    #[test]
    fn strip_lascia_testo_circostante() {
        let content = "prima <tool_calls><invoke name=\"x\"></invoke></tool_calls> dopo";
        assert_eq!(strip_xml_tool_calls(content), "prima  dopo");
    }
}
