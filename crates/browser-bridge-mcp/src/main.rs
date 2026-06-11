//! Browser Bridge MCP daemon.
//!
//! Espone:
//!   - HTTP JSON-RPC 2.0 MCP endpoint  (POST /mcp)            -> per Nexus / plugin-service
//!   - WebSocket bridge                (GET  /ws?token=...)   -> per estensione Chrome MV3
//!   - HTTP handshake                  (GET  /handshake)      -> verifica token + restituisce capability
//!
//! Architettura runtime:
//!   - `BridgeHub` (Arc) mantiene lo stato di tutte le sessioni tab attached
//!     e le richieste MCP in volo correlate via `request_id`.
//!   - L'estensione si connette via WS, autentica con token (file in
//!     `$HOME/.ideai/browser-bridge.token`) e diventa l'unico produttore di
//!     `BridgeEvent` e consumatore di `BridgeRequest`.
//!   - I tool MCP (`browser.*`) traducono argomenti -> `BridgeRequest`,
//!     attendono la risposta correlata su un canale `oneshot` con timeout 15s.
//!
//! Sicurezza:
//!   - Bind solo su 127.0.0.1 (loopback).
//!   - Token random 32 byte rigenerato ad ogni avvio, scritto con permessi 600.
//!   - Logging tracing senza payload sensibili (CLAUDE.md sezione F): gli
//!     argomenti `expression` / `value` sono hashati (sha256 troncato 16 hex).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use dashmap::DashMap;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot, Mutex};

mod extension_assets;

const PROTOCOL_VERSION: &str = "2024-11-05";
const RING_CAPACITY: usize = 1000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

// ---------- Stato hub ----------

#[derive(Clone)]
struct BridgeHub {
    token: String,
    /// Sender verso il client WS attualmente connesso (None se disconnesso).
    ws_out: Arc<Mutex<Option<mpsc::UnboundedSender<BridgeRequest>>>>,
    /// Map request_id -> oneshot per correlare risposte.
    inflight: Arc<DashMap<String, oneshot::Sender<BridgeResponse>>>,
    /// Stato per-tab.
    tabs: Arc<DashMap<i64, TabSession>>,
}

#[derive(Default)]
struct TabSession {
    console: VecDeque<LogEntry>,
    network: VecDeque<NetworkEntry>,
    exceptions: VecDeque<LogEntry>,
}

#[derive(Clone, Serialize)]
struct LogEntry {
    seq: u64,
    ts_ms: i64,
    level: String,
    text: String,
}

#[derive(Clone, Serialize)]
struct NetworkEntry {
    seq: u64,
    ts_ms: i64,
    method: String,
    url: String,
    status: Option<u16>,
    failed: bool,
    error: Option<String>,
}

// ---------- Protocollo WS bridge ----------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BridgeRequest {
    Navigate { request_id: String, tab_id: Option<i64>, url: String },
    Click { request_id: String, tab_id: Option<i64>, selector: Option<String>, x: Option<f64>, y: Option<f64> },
    Fill { request_id: String, tab_id: Option<i64>, selector: String, value_b64: String },
    Scroll { request_id: String, tab_id: Option<i64>, selector: Option<String>, dx: f64, dy: f64 },
    Screenshot { request_id: String, tab_id: Option<i64>, full_page: bool },
    SnapshotDom { request_id: String, tab_id: Option<i64>, mode: String },
    Eval { request_id: String, tab_id: Option<i64>, expression_b64: String, await_promise: bool },
    ListTabs { request_id: String },
    AttachTab { request_id: String, tab_id: i64 },
    #[expect(
        dead_code,
        reason = "protocollo WS simmetrico: detach_tab gia' gestito dall'estensione (background.js), tool MCP browser.detach_tab da esporre"
    )]
    DetachTab { request_id: String, tab_id: i64 },
    Heartbeat,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BridgeMessage {
    Response(BridgeResponse),
    Event(BridgeEvent),
    Hello { ext_version: String },
}

#[derive(Debug, Clone, Deserialize)]
struct BridgeResponse {
    request_id: String,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum BridgeEvent {
    ConsoleLog { tab_id: i64, level: String, text: String, ts_ms: i64 },
    Exception { tab_id: i64, text: String, ts_ms: i64 },
    NetworkRequest { tab_id: i64, method: String, url: String, ts_ms: i64 },
    NetworkResponse { tab_id: i64, url: String, status: u16, ts_ms: i64 },
    NetworkFailed { tab_id: i64, url: String, error: String, ts_ms: i64 },
    TabDetached { tab_id: i64 },
}

// ---------- Errori ----------

#[derive(Debug, thiserror::Error)]
enum BridgeError {
    #[error("nessuna estensione connessa")]
    Disconnected,
    #[error("timeout in attesa di risposta dal browser")]
    Timeout,
    #[error("errore browser: {0}")]
    Browser(String),
    #[error("argomento mancante o invalido: {0}")]
    BadArgs(String),
}

impl BridgeError {
    fn to_mcp_content(&self) -> Value {
        json!({
            "content": [{ "type": "text", "text": self.to_string() }],
            "isError": true
        })
    }
}

// ---------- Bootstrap ----------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Porta dal DB (regola G: unica fonte di verita', niente env/hardcoded).
    // Questo servizio era DB-less: ci connettiamo solo per risolvere la porta.
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await?;
    let port: u16 = nexus_auth::resolve_port(&db, "browser_bridge_port").await;

    // Per WSL2: per permettere a Chrome su Windows di raggiungere l'update.xml,
    // bisogna ascoltare su 0.0.0.0 (WSL forward -> localhost Windows).
    // Default rimane loopback per sicurezza.
    let bind_host = std::env::var("BROWSER_BRIDGE_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    // Host "pubblico" usato per generare gli URL negli script/install e update.xml.
    // In WSL2 conviene usare 127.0.0.1 (lato Windows) o "localhost".
    let public_host = std::env::var("BROWSER_BRIDGE_PUBLIC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

    let token = generate_token();
    persist_token_and_port(&token, port)?;

    let hub = BridgeHub {
        token: token.clone(),
        ws_out: Arc::new(Mutex::new(None)),
        inflight: Arc::new(DashMap::new()),
        tabs: Arc::new(DashMap::new()),
    };

    let assets = extension_assets::ExtensionAssets::from_env(&public_host, port);
    let app = Router::new()
        .route("/handshake", get(handshake))
        .route("/ws", get(ws_upgrade))
        .route("/mcp", post(mcp_endpoint))
        .route("/health", get(health))
        .with_state(hub)
        .nest("/extension", extension_assets::router().with_state(assets));

    let addr: SocketAddr = format!("{bind_host}:{port}").parse()?;
    tracing::info!(%addr, bind_host = %bind_host, public_host = %public_host, "browser-bridge-mcp in ascolto");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn generate_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn persist_token_and_port(token: &str, port: u16) -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(home).join(".ideai");
    std::fs::create_dir_all(&dir)?;
    let token_path = dir.join("browser-bridge.token");
    let port_path = dir.join("browser-bridge.port");
    std::fs::write(&token_path, token)?;
    std::fs::write(&port_path, port.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&token_path)?.permissions();
        perm.set_mode(0o600);
        std::fs::set_permissions(&token_path, perm)?;
    }
    Ok(())
}

// ---------- Endpoint HTTP ----------

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

#[derive(Deserialize)]
struct HandshakeQuery {
    token: String,
}

async fn handshake(
    State(hub): State<BridgeHub>,
    Query(q): Query<HandshakeQuery>,
) -> impl IntoResponse {
    if !constant_time_eq(q.token.as_bytes(), hub.token.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false }))).into_response();
    }
    let connected = hub.ws_out.lock().await.is_some();
    Json(json!({
        "ok": true,
        "protocol_version": PROTOCOL_VERSION,
        "ws_path": "/ws",
        "ws_connected": connected,
        "heartbeat_ms": HEARTBEAT_INTERVAL.as_millis(),
    }))
    .into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------- WebSocket ----------

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(hub): State<BridgeHub>,
    Query(q): Query<HandshakeQuery>,
) -> impl IntoResponse {
    if !constant_time_eq(q.token.as_bytes(), hub.token.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "token invalido").into_response();
    }
    ws.on_upgrade(move |socket| handle_ws(socket, hub))
}

async fn handle_ws(socket: WebSocket, hub: BridgeHub) {
    let (sink, stream) = socket.split();
    let (tx_out, rx_out) = mpsc::unbounded_channel::<BridgeRequest>();

    {
        let mut slot = hub.ws_out.lock().await;
        if slot.is_some() {
            tracing::warn!("estensione gia` connessa, sostituisco la sessione");
        }
        *slot = Some(tx_out.clone());
    }

    let writer_hub = hub.clone();
    let reader_hub = hub.clone();
    let tx_out_hb = tx_out.clone();

    let writer = tokio::spawn(ws_writer(sink, rx_out));
    let reader = tokio::spawn(ws_reader(stream, reader_hub));
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            if tx_out_hb.send(BridgeRequest::Heartbeat).is_err() {
                break;
            }
        }
    });

    let _ = tokio::join!(writer, reader);
    heartbeat.abort();

    // Pulizia: scollega slot e fallisci eventuali request in volo.
    {
        let mut slot = writer_hub.ws_out.lock().await;
        *slot = None;
    }
    let inflight = writer_hub.inflight.clone();
    let pending: Vec<String> = inflight.iter().map(|e| e.key().clone()).collect();
    for k in pending {
        if let Some((_, sender)) = inflight.remove(&k) {
            let _ = sender.send(BridgeResponse {
                request_id: k,
                ok: false,
                data: Value::Null,
                error: Some("estensione disconnessa".into()),
            });
        }
    }
    tracing::info!("estensione disconnessa, hub pulito");
}

async fn ws_writer(
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: mpsc::UnboundedReceiver<BridgeRequest>,
) {
    while let Some(req) = rx.recv().await {
        let payload = match serde_json::to_string(&req) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "serializzazione BridgeRequest fallita");
                continue;
            }
        };
        if sink.send(Message::Text(payload)).await.is_err() {
            break;
        }
    }
    let _ = sink.close().await;
}

async fn ws_reader(mut stream: SplitStream<WebSocket>, hub: BridgeHub) {
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { break };
        match msg {
            Message::Text(text) => handle_ws_text(&hub, &text).await,
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => break,
        }
    }
}

async fn handle_ws_text(hub: &BridgeHub, text: &str) {
    let parsed: Result<BridgeMessage, _> = serde_json::from_str(text);
    match parsed {
        Ok(BridgeMessage::Hello { ext_version }) => {
            tracing::info!(ext_version = %ext_version, "handshake estensione completato");
        }
        Ok(BridgeMessage::Response(resp)) => {
            if let Some((_, sender)) = hub.inflight.remove(&resp.request_id) {
                let _ = sender.send(resp);
            } else {
                tracing::debug!(rid = %resp.request_id, "risposta senza richiesta in volo (scartata)");
            }
        }
        Ok(BridgeMessage::Event(evt)) => apply_event(hub, evt),
        Err(e) => tracing::warn!(error = %e, "messaggio WS non valido"),
    }
}

fn apply_event(hub: &BridgeHub, evt: BridgeEvent) {
    let mut tab = match &evt {
        BridgeEvent::TabDetached { tab_id } => {
            hub.tabs.remove(tab_id);
            return;
        }
        BridgeEvent::ConsoleLog { tab_id, .. }
        | BridgeEvent::Exception { tab_id, .. }
        | BridgeEvent::NetworkRequest { tab_id, .. }
        | BridgeEvent::NetworkResponse { tab_id, .. }
        | BridgeEvent::NetworkFailed { tab_id, .. } => hub.tabs.entry(*tab_id).or_default(),
    };

    let next_seq = tab.console.len() as u64 + tab.network.len() as u64 + tab.exceptions.len() as u64;

    match evt {
        BridgeEvent::ConsoleLog { level, text, ts_ms, .. } => {
            push_capped(
                &mut tab.console,
                LogEntry { seq: next_seq, ts_ms, level, text },
            );
        }
        BridgeEvent::Exception { text, ts_ms, .. } => {
            push_capped(
                &mut tab.exceptions,
                LogEntry { seq: next_seq, ts_ms, level: "error".into(), text },
            );
        }
        BridgeEvent::NetworkRequest { method, url, ts_ms, .. } => {
            push_capped(
                &mut tab.network,
                NetworkEntry {
                    seq: next_seq,
                    ts_ms,
                    method,
                    url,
                    status: None,
                    failed: false,
                    error: None,
                },
            );
        }
        BridgeEvent::NetworkResponse { url, status, ts_ms, .. } => {
            push_capped(
                &mut tab.network,
                NetworkEntry {
                    seq: next_seq,
                    ts_ms,
                    method: "RESP".into(),
                    url,
                    status: Some(status),
                    failed: status >= 500,
                    error: None,
                },
            );
        }
        BridgeEvent::NetworkFailed { url, error, ts_ms, .. } => {
            push_capped(
                &mut tab.network,
                NetworkEntry {
                    seq: next_seq,
                    ts_ms,
                    method: "FAIL".into(),
                    url,
                    status: None,
                    failed: true,
                    error: Some(error),
                },
            );
        }
        BridgeEvent::TabDetached { .. } => {}
    }
}

fn push_capped<T>(buf: &mut VecDeque<T>, item: T) {
    if buf.len() >= RING_CAPACITY {
        buf.pop_front();
    }
    buf.push_back(item);
}

// ---------- MCP HTTP endpoint ----------

#[derive(Deserialize)]
struct JsonRpcReq {
    #[expect(
        dead_code,
        reason = "campo del wire-format JSON-RPC 2.0, accettato ma non validato"
    )]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResp {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

async fn mcp_endpoint(State(hub): State<BridgeHub>, Json(req): Json<JsonRpcReq>) -> impl IntoResponse {
    let id = req.id.clone().unwrap_or(Value::Null);
    let result = match req.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "serverInfo": { "name": "browser-bridge-mcp", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "tools": {} }
        })),
        "tools/list" => Ok(json!({ "tools": tool_catalog() })),
        "tools/call" => Ok(handle_tool_call(&hub, req.params).await),
        other => Err((-32601, format!("metodo sconosciuto: {other}"))),
    };

    let resp = match result {
        Ok(v) => JsonRpcResp { jsonrpc: "2.0", id, result: Some(v), error: None },
        Err((code, msg)) => JsonRpcResp {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({ "code": code, "message": msg })),
        },
    };
    Json(resp)
}

fn tool_catalog() -> Value {
    json!([
        tool("browser.navigate", "Apre una URL nel tab attached.", json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "tab_id": { "type": "integer" }
            },
            "required": ["url"]
        })),
        tool("browser.click", "Click su selettore CSS oppure coordinate (x,y).", json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string" },
                "x": { "type": "number" },
                "y": { "type": "number" },
                "tab_id": { "type": "integer" }
            }
        })),
        tool("browser.fill", "Riempi un input/textarea (selettore CSS) con valore.", json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string" },
                "value": { "type": "string" },
                "tab_id": { "type": "integer" }
            },
            "required": ["selector", "value"]
        })),
        tool("browser.scroll", "Scroll del tab o di un elemento (dx, dy).", json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string" },
                "dx": { "type": "number" },
                "dy": { "type": "number" },
                "tab_id": { "type": "integer" }
            },
            "required": ["dx", "dy"]
        })),
        tool("browser.screenshot", "Screenshot PNG base64 della pagina.", json!({
            "type": "object",
            "properties": {
                "full_page": { "type": "boolean" },
                "tab_id": { "type": "integer" }
            }
        })),
        tool("browser.snapshot_dom", "Snapshot ARIA tree o HTML semplificato.", json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["aria", "html"] },
                "tab_id": { "type": "integer" }
            }
        })),
        tool("browser.eval", "Esegue JS arbitrario nel contesto della pagina.", json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string" },
                "await_promise": { "type": "boolean" },
                "tab_id": { "type": "integer" }
            },
            "required": ["expression"]
        })),
        tool("browser.console_logs", "Restituisce i log console bufferizzati (since=seq cursor).", json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "integer" },
                "since": { "type": "integer" }
            }
        })),
        tool("browser.network_log", "Restituisce le richieste di rete bufferizzate.", json!({
            "type": "object",
            "properties": {
                "tab_id": { "type": "integer" },
                "since": { "type": "integer" },
                "only_failed": { "type": "boolean" }
            }
        })),
        tool("browser.list_tabs", "Elenca i tab visibili dall'estensione.", json!({
            "type": "object", "properties": {}
        })),
        tool("browser.attach_tab", "Attiva chrome.debugger sul tab indicato.", json!({
            "type": "object", "properties": { "tab_id": { "type": "integer" } }, "required": ["tab_id"]
        })),
    ])
}

fn tool(name: &str, desc: &str, schema: Value) -> Value {
    json!({ "name": name, "description": desc, "inputSchema": schema })
}

async fn handle_tool_call(hub: &BridgeHub, params: Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let tab_id = args.get("tab_id").and_then(Value::as_i64);

    let result = match name {
        "browser.navigate" => {
            let url = match args.get("url").and_then(Value::as_str) {
                Some(u) => u.to_string(),
                None => return BridgeError::BadArgs("url mancante".into()).to_mcp_content(),
            };
            let req = BridgeRequest::Navigate { request_id: new_rid(), tab_id, url };
            dispatch_text(hub, req).await
        }
        "browser.click" => {
            let selector = args.get("selector").and_then(Value::as_str).map(str::to_string);
            let x = args.get("x").and_then(Value::as_f64);
            let y = args.get("y").and_then(Value::as_f64);
            if selector.is_none() && (x.is_none() || y.is_none()) {
                return BridgeError::BadArgs("serve selector oppure (x,y)".into()).to_mcp_content();
            }
            let req = BridgeRequest::Click { request_id: new_rid(), tab_id, selector, x, y };
            dispatch_text(hub, req).await
        }
        "browser.fill" => {
            let selector = match args.get("selector").and_then(Value::as_str) {
                Some(s) => s.to_string(),
                None => return BridgeError::BadArgs("selector mancante".into()).to_mcp_content(),
            };
            let value = args.get("value").and_then(Value::as_str).unwrap_or("");
            let value_b64 = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
            tracing::debug!(value_hash = %hash16(value), selector = %selector, "browser.fill");
            let req = BridgeRequest::Fill { request_id: new_rid(), tab_id, selector, value_b64 };
            dispatch_text(hub, req).await
        }
        "browser.scroll" => {
            let dx = args.get("dx").and_then(Value::as_f64).unwrap_or(0.0);
            let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(0.0);
            let selector = args.get("selector").and_then(Value::as_str).map(str::to_string);
            let req = BridgeRequest::Scroll { request_id: new_rid(), tab_id, selector, dx, dy };
            dispatch_text(hub, req).await
        }
        "browser.screenshot" => {
            let full_page = args.get("full_page").and_then(Value::as_bool).unwrap_or(false);
            let req = BridgeRequest::Screenshot { request_id: new_rid(), tab_id, full_page };
            dispatch_text(hub, req).await
        }
        "browser.snapshot_dom" => {
            let mode = args.get("mode").and_then(Value::as_str).unwrap_or("aria").to_string();
            let req = BridgeRequest::SnapshotDom { request_id: new_rid(), tab_id, mode };
            dispatch_text(hub, req).await
        }
        "browser.eval" => {
            let expression = match args.get("expression").and_then(Value::as_str) {
                Some(s) => s.to_string(),
                None => return BridgeError::BadArgs("expression mancante".into()).to_mcp_content(),
            };
            let await_promise = args.get("await_promise").and_then(Value::as_bool).unwrap_or(false);
            let expression_b64 = base64::engine::general_purpose::STANDARD.encode(expression.as_bytes());
            tracing::debug!(expr_hash = %hash16(&expression), "browser.eval");
            let req = BridgeRequest::Eval { request_id: new_rid(), tab_id, expression_b64, await_promise };
            dispatch_text(hub, req).await
        }
        "browser.list_tabs" => {
            let req = BridgeRequest::ListTabs { request_id: new_rid() };
            dispatch_text(hub, req).await
        }
        "browser.attach_tab" => {
            let tid = match tab_id {
                Some(t) => t,
                None => return BridgeError::BadArgs("tab_id mancante".into()).to_mcp_content(),
            };
            let req = BridgeRequest::AttachTab { request_id: new_rid(), tab_id: tid };
            dispatch_text(hub, req).await
        }
        "browser.console_logs" => Ok(read_console(hub, tab_id, args.get("since").and_then(Value::as_u64))),
        "browser.network_log" => Ok(read_network(
            hub,
            tab_id,
            args.get("since").and_then(Value::as_u64),
            args.get("only_failed").and_then(Value::as_bool).unwrap_or(false),
        )),
        other => Err(BridgeError::BadArgs(format!("tool sconosciuto: {other}"))),
    };

    match result {
        Ok(v) => json!({
            "content": [{ "type": "text", "text": serde_json::to_string(&v).unwrap_or_default() }],
            "isError": false
        }),
        Err(e) => e.to_mcp_content(),
    }
}

fn new_rid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn hash16(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    hex::encode_upper(&out[..8])
}

// minimal hex encoder per evitare la dipendenza esterna
mod hex {
    pub fn encode_upper(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        s
    }
}

async fn dispatch_text(hub: &BridgeHub, req: BridgeRequest) -> Result<Value, BridgeError> {
    let rid = match &req {
        BridgeRequest::Navigate { request_id, .. }
        | BridgeRequest::Click { request_id, .. }
        | BridgeRequest::Fill { request_id, .. }
        | BridgeRequest::Scroll { request_id, .. }
        | BridgeRequest::Screenshot { request_id, .. }
        | BridgeRequest::SnapshotDom { request_id, .. }
        | BridgeRequest::Eval { request_id, .. }
        | BridgeRequest::ListTabs { request_id }
        | BridgeRequest::AttachTab { request_id, .. }
        | BridgeRequest::DetachTab { request_id, .. } => request_id.clone(),
        BridgeRequest::Heartbeat => return Ok(json!({ "ok": true })),
    };

    let (tx, rx) = oneshot::channel();
    hub.inflight.insert(rid.clone(), tx);

    {
        let slot = hub.ws_out.lock().await;
        let sender = slot.as_ref().ok_or(BridgeError::Disconnected)?;
        sender.send(req).map_err(|_| BridgeError::Disconnected)?;
    }

    let resp = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            hub.inflight.remove(&rid);
            return Err(BridgeError::Disconnected);
        }
        Err(_) => {
            hub.inflight.remove(&rid);
            return Err(BridgeError::Timeout);
        }
    };

    if !resp.ok {
        return Err(BridgeError::Browser(resp.error.unwrap_or_default()));
    }
    Ok(resp.data)
}

fn read_console(hub: &BridgeHub, tab_id: Option<i64>, since: Option<u64>) -> Value {
    let since = since.unwrap_or(0);
    let mut out = Vec::<LogEntry>::new();
    for entry in hub.tabs.iter() {
        if let Some(filter) = tab_id {
            if *entry.key() != filter {
                continue;
            }
        }
        for log in entry.value().console.iter() {
            if log.seq >= since {
                out.push(log.clone());
            }
        }
        for ex in entry.value().exceptions.iter() {
            if ex.seq >= since {
                out.push(ex.clone());
            }
        }
    }
    json!({ "entries": out })
}

fn read_network(hub: &BridgeHub, tab_id: Option<i64>, since: Option<u64>, only_failed: bool) -> Value {
    let since = since.unwrap_or(0);
    let mut out = Vec::<NetworkEntry>::new();
    for entry in hub.tabs.iter() {
        if let Some(filter) = tab_id {
            if *entry.key() != filter {
                continue;
            }
        }
        for n in entry.value().network.iter() {
            if n.seq >= since && (!only_failed || n.failed) {
                out.push(n.clone());
            }
        }
    }
    json!({ "entries": out })
}

// ---------- Test ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash16_is_stable_and_redacts_input() {
        let h = hash16("ciao mondo");
        assert_eq!(h.len(), 16);
        assert_eq!(h, hash16("ciao mondo"));
        assert_ne!(h, hash16("ciao mondo!"));
    }

    #[test]
    fn constant_time_eq_handles_lengths() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn ring_buffer_drops_oldest() {
        let mut buf: VecDeque<u32> = VecDeque::new();
        for i in 0..(RING_CAPACITY as u32 + 5) {
            push_capped(&mut buf, i);
        }
        assert_eq!(buf.len(), RING_CAPACITY);
        assert_eq!(*buf.front().unwrap(), 5);
        assert_eq!(*buf.back().unwrap(), RING_CAPACITY as u32 + 4);
    }

    #[test]
    fn tool_catalog_is_well_formed() {
        let cat = tool_catalog();
        let arr = cat.as_array().expect("array");
        assert!(arr.iter().all(|t| t.get("name").is_some() && t.get("inputSchema").is_some()));
    }
}
