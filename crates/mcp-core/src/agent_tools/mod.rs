//! Definizioni dei tool disponibili all'agente e funzioni di esecuzione.
//!
//! I tool sono sicuri: nessuna esecuzione di shell arbitraria.
//! Tutte le operazioni file sono vincolate alla root del progetto.
//!
//! Coordinatore del package `agent_tools`. La logica e' splittata per dominio
//! nei sottomoduli; questo file dichiara i moduli e re-esporta i simboli che il
//! resto del crate (e i sottomoduli che fanno `use super::*`) si aspettano.
//!
//! Splitting interno (refactor god-file):
//! - `tool_schema`    — costante `AGENT_TOOLS_JSON` (schema tool, dato puro)
//! - `context`        — `AgentToolContext` + `is_mutating_tool`
//! - `helpers`        — costanti lettura file, pattern protetti, helper condivisi
//! - `dispatch`       — `execute_agent_tool` (routing nome-tool -> handler)
//! - `profile_tools`  — tool profili utente + stub dispatch_subtask
//! - `quality_tools`  — analisi qualita codice (scan, batch)
//! - `semantic_tools` — ricerca semantica (codebase, recall, in-file)
//!
//! Sottomoduli per dominio operativo:
//! - `files`   — operazioni su filesystem (read/write/edit/delete/list/search)
//! - `git`     — comandi Git
//! - `service` — gestione processi long-running e build immagine progetto
//! - `sandbox` — configurazione sandbox del progetto
//! - `command` — esecuzione comandi shell e test runner

pub(crate) mod archive_tools;
pub(crate) mod attachment_inspector;
pub(crate) mod attachment_settings;
pub(crate) mod attachments;
pub(crate) mod command;
pub(crate) mod command_hints;
pub(crate) mod context;
pub(crate) mod dev_diagnostics;
pub(crate) mod dispatch;
pub(crate) mod dispatcher;
pub(crate) mod document_tools;
pub(crate) mod figma_tools;
pub(crate) mod files;
pub(crate) mod git;
pub(crate) mod helpers;
pub(crate) mod knowledge;
pub(crate) mod monitor;
pub(crate) mod port_scanner;
pub(crate) mod ports;
pub(crate) mod profile_tools;
pub(crate) mod project_db_query;
pub(crate) mod quality_tools;
pub(crate) mod rag_search;
pub(crate) mod read_cache;
pub(crate) mod safety;
pub(crate) mod sandbox;
pub(crate) mod scaffold_verifier;
pub(crate) mod semantic_tools;
pub(crate) mod service;
pub(crate) mod shadcn_setup;
pub(crate) mod subagent;
pub(crate) mod testing;
pub(crate) mod todos;
pub(crate) mod tool_schema;
pub(crate) mod vision_tools;
pub(crate) mod visual_compare;

// ── API pubblica del package (call site esterni: invariata) ─────────────────
pub use context::{is_mutating_tool, AgentToolContext};
pub use dispatch::execute_agent_tool;
pub use tool_schema::AGENT_TOOLS_JSON;

// Re-export per uso interno crate (tool_run_tests è chiamato da agent_loop, in teoria).
pub(crate) use command::tool_run_tests;

// ── Re-export per i sottomoduli che usano `use super::*` ────────────────────
// Mantengono risolvibili i simboli che prima vivevano in questo file: tipi base,
// helper condivisi e path di crate referenziati via `super::`.
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::Arc;

pub(crate) use serde_json::Value;
pub(crate) use sqlx::{PgPool, Row};
pub(crate) use tokio::process::Command;
pub(crate) use uuid::Uuid;

pub(crate) use crate::projects::{resolve_relative_path, run_git_command};
pub(crate) use crate::vector_memory;
pub(crate) use ::mcp_db;
pub(crate) use ::mcp_quality;

pub(crate) use helpers::{
    classify_command_error, extract_file_structure, format_process_output, is_protected_path,
    looks_like_long_running_command, PROTECTED_PATTERNS, READ_FILE_LINES_MAX,
    READ_FILE_STRUCTURE_HINT_LINES,
};
