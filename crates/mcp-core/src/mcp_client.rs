//! Re-export del client MCP unico (regola L / ADR 0026, Wave C1).
//!
//! Prima questo file era una copia di ~510 righe duplicata identicamente in
//! `crates/plugin-service/src/mcp_client.rs`. Ora la logica vive nel crate
//! condiviso `nexus-mcp-client`; questo modulo resta come facciata locale
//! cosi' i call site interni a mcp-core non devono cambiare il path d'import.

pub use nexus_mcp_client::{
    call_tool, list_tools, resolve_stdio_timeout, McpServerConfig, McpTransport,
};
