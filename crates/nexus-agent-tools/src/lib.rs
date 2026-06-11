//! nexus-agent-tools — parti del toolkit agente estratte dal monolite
//! mcp-core (split 7.4, passo agent_tools-1): i moduli senza dipendenza
//! da AgentToolContext. mcp-core::agent_tools re-esporta questo crate
//! per mantenere validi i path storici crate::agent_tools::*.
//! Prossimo passo: ToolContextCore + i tool che usano solo i campi core.

pub mod attachment_settings;
pub mod command_hints;
pub mod monitor;
pub mod read_cache;
pub mod safety;
pub mod tool_schema;
pub mod url_scanner;
