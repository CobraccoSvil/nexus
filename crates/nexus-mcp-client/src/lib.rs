//! Client MCP (Model Context Protocol) universale: punto unico (regola L /
//! ADR 0026).
//!
//! Prima `mcp_client.rs` viveva DUPLICATO in `crates/mcp-core/src/mcp_client.rs`
//! e `crates/plugin-service/src/mcp_client.rs` (~510 righe ognuno, cluster top
//! del jscpd report: 4 cloni grossi 79+62+46+40L). Era nato cosi' perche'
//! plugin-service e mcp-core sono binari indipendenti che non dipendono l'uno
//! dall'altro per design; il codice MCP era stato copiato invece di estratto.
//! Ora vive qui una volta sola e entrambi i binari lo importano.
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

pub mod server_endpoints;
pub mod server_storage;

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
    /// Tool eseguiti in-process (dispatch fuori da questo modulo).
    /// Questo variant non viene mai passato a `call_tool`/`list_tools`.
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
    ToolNotFound(String),
    #[error("Timeout")]
    Timeout,
}

// -- HTTP JSON-RPC Client --

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    REQUEST_ID.fetch_add(1, Ordering::SeqCst)
}

/// Costruisce un client HTTP "default" per chiamate MCP. I call site che hanno
/// bisogno di un client preconfigurato (es. con tracing/headers globali) possono
/// usare le funzioni `*_with_client`.
pub fn default_client() -> reqwest::Client {
    reqwest::Client::new()
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
        .replace(['\n', '\r'], " ")
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
    // Caso standard: body JSON puro.
    if let Ok(value) = serde_json::from_str::<Value>(response_text) {
        return Ok(value);
    }

    // Fallback SSE: alcuni endpoint MCP HTTP rispondono con stream
    // ("event: ...", "data: {...}") anche su richieste singole.
    // Cerchiamo il primo payload JSON valido nelle righe "data:".
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

    // Ultimo tentativo: cerca il primo oggetto JSON nel testo grezzo.
    if let Some(start_idx) = response_text.find('{') {
        let candidate = &response_text[start_idx..];
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            return Ok(value);
        }
    }

    // Ritorna l'errore originale del parser JSON per mantenere il dettaglio.
    serde_json::from_str::<Value>(response_text)
}

// -- Public MCP operations --

/// Recupera la lista dei tool da un server MCP HTTP, usando il client fornito.
pub async fn list_tools_http_with_client(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<Vec<McpTool>, McpError> {
    // Handshake initialize (obbligatorio per MCP)
    let _ = http_jsonrpc(
        client,
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

    // tools/list
    let result = http_jsonrpc(client, url, headers, "tools/list", json!({})).await?;

    parse_tools_response(result)
}

/// Recupera la lista dei tool da un server MCP HTTP (usa il client di default).
pub async fn list_tools_http(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<Vec<McpTool>, McpError> {
    let client = default_client();
    list_tools_http_with_client(&client, url, headers).await
}

/// Esegue un tool su un server MCP HTTP, usando il client fornito.
pub async fn call_tool_http_with_client(
    client: &reqwest::Client,
    url: &str,
    headers: &HashMap<String, String>,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolResult, McpError> {
    let result = http_jsonrpc(
        client,
        url,
        headers,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await?;

    parse_tool_result(result)
}

/// Esegue un tool su un server MCP HTTP (usa il client di default).
pub async fn call_tool_http(
    url: &str,
    headers: &HashMap<String, String>,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolResult, McpError> {
    let client = default_client();
    call_tool_http_with_client(&client, url, headers, tool_name, arguments).await
}

/// Recupera la lista dei tool da un server MCP stdio.
///
/// `timeout` e' la finestra massima per ricevere la risposta `tools/list`; va
/// risolto dal chiamante (DB-driven, regola G). Vedi `DEFAULT_STDIO_TIMEOUT`.
pub async fn list_tools_stdio(
    command: &str,
    args: &[String],
    env_vars: &HashMap<String, String>,
    timeout: std::time::Duration,
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
    // Handshake MCP completo: initialize -> notifications/initialized -> tools/list.
    // Senza la notifica i server strict ignorano il tools/list (regola H).
    let initialized = build_jsonrpc_notification("notifications/initialized", json!({}));
    let list_msg = build_jsonrpc_with_id(2, "tools/list", json!({}));
    let output = run_stdio_jsonrpc(
        command,
        args,
        env_vars,
        2,
        &[init_msg, initialized, list_msg],
        timeout,
    )
    .await?;

    // Seleziona la risposta con l'id atteso (2 = tools/list) via punto unico,
    // poi verifica che porti il campo "tools".
    if let Some(result) = select_result_by_id(&output, 2) {
        if result.get("tools").is_some() {
            return parse_tools_response(result);
        }
    }

    if output.trim().is_empty() {
        Ok(vec![])
    } else {
        Err(McpError::Protocol(output))
    }
}

/// Esegue un tool su un server MCP stdio.
///
/// `timeout` e' la finestra massima per ricevere la risposta `tools/call`; va
/// risolto dal chiamante (DB-driven, regola G), con default `DEFAULT_STDIO_TIMEOUT`.
/// Deve essere ampio abbastanza da coprire l'avvio di server lenti come
/// `@playwright/mcp` (che lancia un browser): vedi BUG d2(A).
pub async fn call_tool_stdio(
    command: &str,
    args: &[String],
    env_vars: &HashMap<String, String>,
    tool_name: &str,
    arguments: Value,
    timeout: std::time::Duration,
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
    // Handshake MCP completo: initialize -> notifications/initialized -> tools/call.
    // Senza la notifica i server strict ignorano il tools/call (regola H).
    let initialized = build_jsonrpc_notification("notifications/initialized", json!({}));
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
        &[init_msg, initialized, call_msg],
        timeout,
    )
    .await?;

    // Seleziona la risposta con l'id atteso (2 = tools/call) via punto unico:
    // evita di scambiare la risposta di initialize (id=1) per il risultato del
    // tool quando il server e' lento (BUG d2 cause A).
    if let Some(result) = select_result_by_id(&output, 2) {
        return parse_tool_result(result);
    }

    Err(McpError::Protocol(
        "Nessun risultato dal server stdio".to_string(),
    ))
}

// -- Internal helpers --

fn build_jsonrpc_with_id(id: u64, method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
    .to_string()
}

/// Notifica JSON-RPC (SENZA campo `id`: per le notification MCP non si attende
/// risposta). Necessaria per `notifications/initialized`, che i server MCP strict
/// (es. @playwright/mcp) esigono DOPO `initialize` e PRIMA di qualunque
/// `tools/call` / `tools/list`: senza, ignorano le richieste e rispondono solo
/// all'handshake (l'agente vedeva solo {protocolVersion,serverInfo} e il tool non
/// veniva mai eseguito). Regola H: conformita' alla spec MCP, non un workaround.
fn build_jsonrpc_notification(method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
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
    timeout: std::time::Duration,
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
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpError::Protocol("child stdin non disponibile dopo spawn".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpError::Protocol("child stdout non disponibile dopo spawn".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| McpError::Protocol("child stderr non disponibile dopo spawn".into()))?;

    for msg in messages {
        stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(McpError::Io)?;
        stdin.write_all(b"\n").await.map_err(McpError::Io)?;
    }
    // BUG d2(A) LIFECYCLE STDIO: NON si chiude stdin qui. Prima `drop(stdin)`
    // avveniva SUBITO dopo l'invio dei messaggi: per server che avviano un
    // browser lento (@playwright/mcp) la chiusura immediata di stdin + finestra
    // breve faceva sì che si leggesse SOLO la risposta id=1 (initialize ->
    // serverInfo) e mai la id=2 (tools/call). Ora stdin resta APERTO finche'
    // non arriva la risposta con l'expected_response_id (o scade il timeout),
    // poi viene chiuso. CAUTELA: lo `stdin` NON viene rimosso, viene solo
    // spostato (drop esplicito DOPO il loop): i server che si chiudono su EOF
    // (filesystem/github/postgres...) ricevono comunque l'EOF al termine, quindi
    // non si deadlocka nessuno. La differenza e' solo QUANDO arriva l'EOF.
    let mut out_lines: Vec<String> = Vec::new();
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    let mut saw_expected_response = false;

    loop {
        tokio::select! {
            _ = &mut deadline => {
                break;
            }
            line = stdout_lines.next_line() => {
                if let Ok(Some(l)) = line {
                    if !l.trim().is_empty() {
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
                // next_line() ha ritornato Ok(None) (EOF) o Err: nessun break
                // automatico, restiamo nel loop finche' deadline non scade. Va
                // bene: se il processo e' morto, le branch successive degenerano
                // e il timeout chiude pulito.
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

    // Chiusura stdin DOPO aver ricevuto la risposta attesa (o dopo il timeout):
    // segnala EOF al server stdio in modo che possa terminare ordinatamente.
    drop(stdin);
    let _ = child.kill().await;
    Ok(out_lines.join("\n"))
}

/// Seleziona il `result` della risposta JSON-RPC con l'`id` atteso da un output
/// multi-linea (una riga = un messaggio JSON-RPC). Punto unico (regola L) per il
/// post-processing delle risposte stdio: prima `call_tool_stdio` e
/// `list_tools_stdio` prendevano l'ULTIMA riga con un `result` qualsiasi, senza
/// verificare l'id; con server lenti come @playwright/mcp la prima/unica riga
/// poteva essere la risposta a `initialize` (id=1) invece che a `tools/call`
/// (id=2). Qui filtriamo per id atteso (BUG d2 cause A).
///
/// Strategia: scorre in ordine inverso (l'ultima risposta valida vince) e
/// ritorna il `result` della prima riga con id == `expected_id` e un `result`.
/// Se nessuna riga matcha l'id (server che non echeggia l'id come numero),
/// ripiega sull'ultima riga con un `result` per retrocompatibilita'.
fn select_result_by_id(output: &str, expected_id: u64) -> Option<Value> {
    let mut fallback: Option<Value> = None;
    for line in output.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("result").is_none() {
            continue;
        }
        if v.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return v.get("result").cloned();
        }
        if fallback.is_none() {
            fallback = v.get("result").cloned();
        }
    }
    fallback
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

/// Timeout di default per le chiamate stdio quando il chiamante non risolve il
/// setting DB. NON e' un "magic fallback" di configurazione (regola G): la
/// configurazione vera vive nel DB e viene passata esplicitamente da mcp-core /
/// plugin-service; questo e' solo un floor di sicurezza per i pochi call site
/// che non hanno accesso al pool DB. >=60s per coprire l'avvio di server lenti
/// (browser di @playwright/mcp).
pub const DEFAULT_STDIO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Chiave settings (DB) per il timeout delle chiamate stdio MCP.
pub const STDIO_TIMEOUT_SETTING_KEY: &str = "agent.mcp.stdio_call_timeout_seconds";

/// Risolve il timeout stdio dal DB (regola G). PUNTO UNICO (regola L): tutti i
/// chiamanti della facade `call_tool`/`list_tools` su transport stdio risolvono
/// il timeout qui, invece di hardcodarlo. Se il setting manca o non e' parsabile
/// o il DB e' irraggiungibile, ripiega su `DEFAULT_STDIO_TIMEOUT` (floor di
/// sicurezza documentato, non una scelta di modello/config nascosta).
pub async fn resolve_stdio_timeout(db: &sqlx::PgPool) -> std::time::Duration {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
            .bind(STDIO_TIMEOUT_SETTING_KEY)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    match raw.as_deref().map(str::trim).map(str::parse::<u64>) {
        Some(Ok(secs)) if secs > 0 => std::time::Duration::from_secs(secs),
        _ => DEFAULT_STDIO_TIMEOUT,
    }
}

/// Chiama un tool su qualsiasi server MCP (HTTP o stdio) dato il config.
///
/// `timeout` si applica SOLO al transport stdio (HTTP ha il suo timeout fisso).
pub async fn call_tool(
    config: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
    timeout: std::time::Duration,
) -> Result<McpToolResult, McpError> {
    match &config.transport {
        McpTransport::Http { url, headers } => {
            call_tool_http(url, headers, tool_name, arguments).await
        }
        McpTransport::Stdio {
            command,
            args,
            env_vars,
        } => call_tool_stdio(command, args, env_vars, tool_name, arguments, timeout).await,
        McpTransport::Builtin => Err(McpError::Protocol(
            "Builtin transport non deve passare per mcp_client".to_string(),
        )),
    }
}

/// Lista i tool da qualsiasi server MCP dato il config.
///
/// `timeout` si applica SOLO al transport stdio (HTTP ha il suo timeout fisso).
pub async fn list_tools(
    config: &McpServerConfig,
    timeout: std::time::Duration,
) -> Result<Vec<McpTool>, McpError> {
    match &config.transport {
        McpTransport::Http { url, headers } => list_tools_http(url, headers).await,
        McpTransport::Stdio {
            command,
            args,
            env_vars,
        } => list_tools_stdio(command, args, env_vars, timeout).await,
        McpTransport::Builtin => Ok(vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BUG d2(A): con piu' risposte JSON-RPC sullo stdout, va scelta quella con
    /// l'id atteso, NON la prima/ultima a caso. Qui id=1 e' initialize, id=2 e'
    /// tools/call: il selettore deve ritornare il result di id=2.
    #[test]
    fn select_result_picks_expected_id() {
        let line1 =
            r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"Playwright"}}}"#;
        let line2 =
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"Result 42"}]}}"#;
        let output = format!("{line1}\n{line2}");
        let result = select_result_by_id(&output, 2).expect("deve trovare id=2");
        assert!(result.get("content").is_some());
        let parsed = parse_tool_result(result).unwrap();
        assert!(parsed.content.contains("Result 42"));
        assert!(!parsed.is_error);
    }

    /// Se arriva SOLO la risposta di initialize (id=1) e si attende id=2, NON
    /// deve essere scambiata per il risultato del tool: fallback all'unica
    /// risposta con result, ma e' chiaramente il serverInfo (non un tool result).
    #[test]
    fn select_result_does_not_confuse_initialize_when_only_one() {
        let output = r#"{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"Playwright"}}}"#;
        // expected_id=2 non presente: fallback ritorna l'unico result (id=1).
        let result = select_result_by_id(output, 2).expect("fallback all'unico result");
        // Il chiamante (list_tools_stdio) verifica poi la presenza di "tools":
        // qui non c'e', quindi non verrebbe scambiato per una tool list.
        assert!(result.get("tools").is_none());
        assert!(result.get("content").is_none());
    }

    #[test]
    fn select_result_none_when_no_result() {
        let output = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"x"}}"#;
        assert!(select_result_by_id(output, 2).is_none());
    }

    #[test]
    fn select_result_skips_non_json_lines() {
        let json = r#"{"jsonrpc":"2.0","id":2,"result":{"content":[]}}"#;
        let output = format!("stderr noise non-json\n{json}");
        assert!(select_result_by_id(&output, 2).is_some());
    }
}
