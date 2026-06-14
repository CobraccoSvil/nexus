//! Provider DeepSeek (OpenAI-compatibile) con fix XML tool-call.
//!
//! Porting di `packages/llm-gateway/src/providers/deepseek.ts`. DeepSeek a volte
//! emette le tool-call in formato XML Anthropic-style dentro il campo `content`
//! invece del formato strutturato OpenAI `tool_calls`. Questo modulo intercetta
//! il blocco `<tool_calls>...</tool_calls>`, lo converte in [`LlmToolCall`]
//! strutturati e ripulisce il content.
//!
//! La logica di parsing e' un punto unico ([`parse_xml_tool_calls`] /
//! [`strip_xml_tool_calls`]) riusata sia da `complete` (post-processing) sia da
//! `stream` (accumulo + emissione tool-call al termine).

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use regex::Regex;
use reqwest::Client;

use crate::provider::{ChunkStream, LlmProvider};
use crate::providers::openai_compat::OpenAiCompatClient;
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
        let resp = self.client.complete(req).await?;
        Ok(fixup_response(resp))
    }

    /// Streaming con detection XML: accumula i chunk; se al termine non ci sono
    /// tool-call native ma il content contiene XML, le emette come delta
    /// strutturati (parita' col generatore TS).
    async fn stream(&self, req: &LlmRequest) -> anyhow::Result<ChunkStream> {
        let mut inner = self.client.stream(req).await?;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LlmUsage;

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
        }]);
        let out = fixup_response(resp);
        // Tool-call native presenti: il content XML NON viene ripulito.
        let calls = out.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "native_1");
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
