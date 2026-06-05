//! Re-export del client MCP unico (regola L / ADR 0026, Wave C1).
//!
//! Prima questo file era una copia di ~510 righe duplicata identicamente in
//! `crates/mcp-core/src/mcp_client.rs`. Ora la logica vive nel crate condiviso
//! `nexus-mcp-client`; questo modulo resta come facciata locale cosi' i call
//! site interni a plugin-service non devono cambiare il path d'import.

pub use nexus_mcp_client::{
    call_tool, call_tool_http, call_tool_http_with_client, call_tool_stdio, list_tools,
    list_tools_http, list_tools_http_with_client, list_tools_stdio, McpError, McpServerConfig,
    McpTool, McpToolResult, McpTransport,
};
