//! Client MCP (Model Context Protocol) universale.
//!
//! Supporta due transport:
//!   - HTTP: invia richieste JSON-RPC 2.0 a un endpoint HTTP/HTTPS
//!   - Stdio: avvia un processo locale e comunica via stdin/stdout JSON-RPC
//!
//! Protocollo MCP essenziale implementato:
//!   - `initialize` (handshake)
//!   - `tools/list`  -> lista tool disponibili con schema
//!   - `tools/call`  -> esecuzione tool con parametri

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// -- Public types --

/// Definizione di un tool esposto da un server MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Risultato di una chiamata tool MCP.
#[derive(Debug, Clone)]
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env_vars: HashMap<String, String>,
    },
    /// Tool eseguiti in-process da nexus_builtin::execute().
    Builtin,
}

// -- Errors --

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP protocol error: {0}")]
    Protocol(String),
    #[error("Tool not found: {0}")]
    #[allow(dead_code)]
    ToolNotFound(String),
    #[error("Timeout")]
    Timeout,
}

// -- HTTP JSON-RPC Client --

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::SeqCst)
}

/// Invia una richiesta JSON-RPC 2.0 a un server MCP HTTP.
async fn http_jsonrpc(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    method: &str,
    params: Value,
) -> Result<Value, McpError> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": next_id(),
        "method": method,
        "params": params
    });

    let mut req = client
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .json(&body);
    for (k, v) in headers {
        req = req.header(k, v);
    }

    let resp = tokio::time::timeout(std::time::Duration::from_secs(30), req.send())
        .await
        .map_err(|_| McpError::Timeout)?
        .map_err(McpError::Http)?;

    let status = resp.status();
    let response_text = resp.text().await.map_err(McpError::Http)?;
    if !status.is_success() {
        return Err(McpError::Protocol(format!(
            "HTTP {} {}",
            status.as_u16(),
            compact_response_message(&response_text)
        )));
    }

    let json: Value = parse_jsonrpc_response(&response_text).map_err(|error| {
        McpError::Protocol(format!(
            "Risposta non JSON dal server MCP: {} (body: {})",
            error,
            compact_response_message(&response_text)
        ))
    })?;

    if let Some(err) = json.get("error") {
        return Err(McpError::Protocol(err.to_string()));
    }

    Ok(json.get("result").cloned().unwrap_or(Value::Null))
}

fn compact_response_message(text: &str) -> String {
    let compact = text
        .replace('\n', " ")
        .replace('\r', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return "(risposta vuota)".to_string();
    }
    let mut out = trimmed.to_string();
    if out.chars().count() > 240 {
        out = out.chars().take(240).collect::<String>() + "...";
    }
    out
}

fn parse_jsonrpc_response(response_text: &str) -> Result<Value, serde_json::Error> {
    if let Ok(value) = serde_json::from_str::<Value>(response_text) {
        return Ok(value);
    }

    // Fallback SSE: some MCP HTTP endpoints respond with stream
    for line in response_text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let payload = trimmed.trim_start_matches("data:").trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(payload) {
            return Ok(value);
        }
    }

    if let Some(start_idx) = response_text.find('{') {
        let candidate = &response_text[start_idx..];
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            return Ok(value);
        }
    }

    serde_json::from_str::<Value>(response_text)
}

// -- Public MCP operations --

/// Recupera la lista dei tool da un server MCP HTTP.
pub async fn list_tools_http(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<Vec<McpTool>, McpError> {
    let client = nexus_http::build_client();

    let _ = http_jsonrpc(
        &client,
        url,
        headers,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "ideai-mcp-client", "version": "1.0" }
        }),
    )
    .await;

    let result = http_jsonrpc(&client, url, headers, "tools/list", json!({})).await?;
    parse_tools_response(result)
}

/// Esegue un tool su un server MCP HTTP.
pub async fn call_tool_http(
    url: &str,
    headers: &HashMap<String, String>,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolResult, McpError> {
    let client = nexus_http::build_client();

    let result = http_jsonrpc(
        &client,
        url,
        headers,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await?;

    parse_tool_result(result)
}

/// Recupera la lista dei tool da un server MCP stdio.
pub async fn list_tools_stdio(
    command: &str,
    args: &[String],
    env_vars: &HashMap<String, String>,
) -> Result<Vec<McpTool>, McpError> {
    let init_msg = build_jsonrpc_with_id(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "ideai-mcp-client", "version": "1.0" }
        }),
    );
    let list_msg = build_jsonrpc_with_id(2, "tools/list", json!({}));
    let output = run_stdio_jsonrpc(
        command,
        args,
        env_vars,
        2,
        &[init_msg, list_msg],
    )
    .await?;

    for line in output.lines().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("result").is_some() && v["result"].get("tools").is_some() {
                return parse_tools_response(v["result"].clone());
            }
        }
    }

    // Se non troviamo nessun JSON-RPC valido, non nascondiamo l'errore:
    // altrimenti il test risulta "ok" con 0 tool / health unknown.
    if output.trim().is_empty() {
        Ok(vec![])
    } else {
        Err(McpError::Protocol(output))
    }
}

/// Esegue un tool su un server MCP stdio.
pub async fn call_tool_stdio(
    command: &str,
    args: &[String],
    env_vars: &HashMap<String, String>,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolResult, McpError> {
    let init_msg = build_jsonrpc_with_id(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "ideai-mcp-client", "version": "1.0" }
        }),
    );
    let call_msg = build_jsonrpc_with_id(
        2,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    );
    let output = run_stdio_jsonrpc(
        command,
        args,
        env_vars,
        2,
        &[init_msg, call_msg],
    )
    .await?;

    for line in output.lines().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("result").is_some() {
                return parse_tool_result(v["result"].clone());
            }
        }
    }

    Err(McpError::Protocol(
        "Nessun risultato dal server stdio".to_string(),
    ))
}

// -- Internal helpers --

fn build_jsonrpc(method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": next_id(),
        "method": method,
        "params": params
    })
    .to_string()
}

fn build_jsonrpc_with_id(id: u64, method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
    .to_string()
}

async fn run_stdio_jsonrpc(
    command: &str,
    args: &[String],
    env_vars: &HashMap<String, String>,
    expected_response_id: u64,
    messages: &[String],
) -> Result<String, McpError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .envs(env_vars)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(McpError::Io)?;
    // stdin/stdout/stderr sono Some per costruzione (Stdio::piped() impostato
    // sopra), ma esprimiamo l'invariante esplicitamente per non panicare.
    let mut stdin = child.stdin.take()
        .ok_or_else(|| McpError::Protocol("child stdin non disponibile dopo spawn".into()))?;
    let stdout = child.stdout.take()
        .ok_or_else(|| McpError::Protocol("child stdout non disponibile dopo spawn".into()))?;
    let stderr = child.stderr.take()
        .ok_or_else(|| McpError::Protocol("child stderr non disponibile dopo spawn".into()))?;

    for msg in messages {
        stdin.write_all(msg.as_bytes()).await.map_err(McpError::Io)?;
        stdin.write_all(b"\n").await.map_err(McpError::Io)?;
    }
    drop(stdin);

    // Alcuni server MCP stdio (es. Redis) restano in ascolto e non terminano.
    // Ci fermiamo quando otteniamo la risposta JSON-RPC relativa ALLA richiesta attesa (id).
    let mut out_lines: Vec<String> = Vec::new();
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    let deadline = tokio::time::sleep(std::time::Duration::from_secs(30));
    tokio::pin!(deadline);

    let mut saw_expected_response = false;

    loop {
        tokio::select! {
            _ = &mut deadline => {
                break;
            }
            line = stdout_lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if !l.trim().is_empty() {
                            // Molti server scrivono 1 JSON per riga
                            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                                if v.get("id").and_then(Value::as_u64) == Some(expected_response_id)
                                    && (v.get("result").is_some() || v.get("error").is_some())
                                {
                                    saw_expected_response = true;
                                }
                            }
                            out_lines.push(l);
                            if saw_expected_response {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            line = stderr_lines.next_line() => {
                if let Ok(Some(l)) = line {
                    if !l.trim().is_empty() {
                        out_lines.push(l);
                    }
                }
            }
        }
    }

    // Best-effort terminate: evita test appesi indefinitamente.
    let _ = child.kill().await;

    Ok(out_lines.join("\n"))
}

fn parse_tools_response(result: Value) -> Result<Vec<McpTool>, McpError> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(tools
        .into_iter()
        .map(|t| McpTool {
            name: t["name"].as_str().unwrap_or("").to_string(),
            description: t["description"].as_str().map(str::to_string),
            input_schema: t
                .get("inputSchema")
                .cloned()
                .unwrap_or(json!({ "type": "object", "properties": {} })),
        })
        .collect())
}

fn parse_tool_result(result: Value) -> Result<McpToolResult, McpError> {
    let is_error = result["isError"].as_bool().unwrap_or(false);

    let content = if let Some(arr) = result.get("content").and_then(Value::as_array) {
        arr.iter()
            .filter_map(|c| {
                if c["type"] == "text" {
                    c["text"].as_str().map(str::to_string)
                } else {
                    Some(c.to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        result.to_string()
    };

    Ok(McpToolResult { content, is_error })
}

// -- Unified facade --

/// Chiama un tool su qualsiasi server MCP (HTTP o stdio) dato il config.
pub async fn call_tool(
    config: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolResult, McpError> {
    match &config.transport {
        McpTransport::Http { url, headers } => {
            call_tool_http(url, headers, tool_name, arguments).await
        }
        McpTransport::Stdio {
            command,
            args,
            env_vars,
        } => call_tool_stdio(command, args, env_vars, tool_name, arguments).await,
        McpTransport::Builtin => Err(McpError::Protocol(
            "Builtin transport non deve passare per mcp_client".to_string(),
        )),
    }
}

/// Lista i tool da qualsiasi server MCP dato il config.
pub async fn list_tools(config: &McpServerConfig) -> Result<Vec<McpTool>, McpError> {
    match &config.transport {
        McpTransport::Http { url, headers } => list_tools_http(url, headers).await,
        McpTransport::Stdio {
            command,
            args,
            env_vars,
        } => list_tools_stdio(command, args, env_vars).await,
        McpTransport::Builtin => Ok(vec![]),
    }
}
