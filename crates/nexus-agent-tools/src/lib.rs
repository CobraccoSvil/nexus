//! nexus-agent-tools — parti del toolkit agente estratte dal monolite
//! mcp-core (split 7.4). mcp-core::agent_tools re-esporta questo crate
//! per mantenere validi i path storici crate::agent_tools::*.
//!
//! Passo agent_tools-1: i moduli senza dipendenza dal contesto tool.
//! Passo agent_tools-2: `ToolContextCore` (i campi di AgentToolContext
//! senza dipendenze da mcp-core) + i tool che usano solo quei campi.
//! Passo agent_tools-3: vision_tools (settings via punto unico nexus-auth)
//! e figma_tools (path-safety via nexus_types::workspace_paths).
//! Passo agent_tools-4: git (esecutore in nexus_types::git_exec; reindex
//! post-commit de-accoppiato via trait `FileReindexer` nel context core).
//! Candidato successivo: il pacchetto wiki (richiede de-axumizzazione).

pub mod archive_tools;
pub mod attachment_inspector;
pub mod attachment_settings;
pub mod attachments;
pub mod command_hints;
pub mod context_core;
pub mod dev_diagnostics;
pub mod dispatcher;
pub mod document_tools;
pub mod figma_tools;
pub mod gateway_client;
pub mod git;
pub mod image_tools;
pub mod monitor;
pub mod profile_tools;
pub mod quality_tools;
pub mod read_cache;
pub mod safety;
pub mod scaffold_verifier;
pub mod shadcn_setup;
// NB: l'orchestrazione dei sub-agenti vive in `mcp-core::agent_tools::subagent_native`
// (richiede `native_engine`, non accessibile da qui per la gerarchia crate). Il
// vecchio modulo `subagent` che chiamava il brain /agent/subagent-run e' stato
// rimosso nel porting a grafo nativo (zero-Python).
pub mod todos;
pub mod tool_schema;
pub mod url_scanner;
pub mod vision_tools;

pub use context_core::ToolContextCore;
