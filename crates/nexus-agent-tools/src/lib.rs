//! nexus-agent-tools — parti del toolkit agente estratte dal monolite
//! mcp-core (split 7.4). mcp-core::agent_tools re-esporta questo crate
//! per mantenere validi i path storici crate::agent_tools::*.
//!
//! Passo agent_tools-1: i moduli senza dipendenza dal contesto tool.
//! Passo agent_tools-2: `ToolContextCore` (i campi di AgentToolContext
//! senza dipendenze da mcp-core) + i tool che usano solo quei campi.
//! Candidati successivi: tool con 1-2 deps risolvibili (vision_tools,
//! git/figma_tools) e il pacchetto wiki (richiede de-axumizzazione).

pub mod archive_tools;
pub mod attachment_inspector;
pub mod attachment_settings;
pub mod attachments;
pub mod command_hints;
pub mod context_core;
pub mod dev_diagnostics;
pub mod dispatcher;
pub mod document_tools;
pub mod monitor;
pub mod profile_tools;
pub mod quality_tools;
pub mod read_cache;
pub mod safety;
pub mod scaffold_verifier;
pub mod shadcn_setup;
pub mod subagent;
pub mod todos;
pub mod tool_schema;
pub mod url_scanner;

pub use context_core::ToolContextCore;
