//! Contesto di esecuzione tool () e classificazione mutating.
//!
//! Estratto da  (refactor god-file).

use std::path::PathBuf;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

/// Contesto necessario all'esecuzione dei tool.
#[derive(Debug, Clone)]
pub struct AgentToolContext {
    /// Root assoluta del progetto (path-traversal-safe).
    pub root_path: PathBuf,
    pub user_id: Uuid,
    pub is_git_repo: bool,
    pub can_write: bool,
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub db: Arc<PgPool>,
    /// ID del run padre (per agenti figlio lanciati da dispatch_subtask).
    pub parent_run_id: Option<Uuid>,
    /// Canali agente (DashMap) per registrare i run figlio.
    pub agent_channels: crate::AgentChannels,
    /// Channel per stream live eventi Playwright (run live monitoring).
    pub playwright_channels: crate::playwright_live::PlaywrightChannels,
    /// Client Neural Core per i run figlio.
    pub neural: crate::orchestrator::NeuralCoreClient,
    /// Automation mode corrente (ereditata dai run figlio).
    pub automation_mode: crate::orchestrator::AutomationMode,
    /// Terminali IDE connessi per utente/progetto.
    pub terminal_consumers: crate::TerminalConsumers,
    /// Pattern long-running caricati dal DB (pre-fetched all'inizio del run).
    pub long_running_patterns: Vec<String>,
    /// Cache template prompt (condivisa con AppState).
    pub template_cache: crate::prompt_templates::TemplateCache,
    /// Ruolo utente corrente ("admin" | "editor" | "viewer") — usato dai tool nexus_builtin.
    pub user_role: String,
    /// Se true, l'agente opera come operatore Nexus con permessi completi
    /// sui file del progetto gestito. Bypassa `PROTECTED_PATTERNS` ma NON
    /// la protezione infrastruttura (path-traversal, container ideai-*).
    pub is_nexus_operator: bool,
    /// Stato atomico dipendenze (Qdrant, embedder). Se down, i tool vettoriali
    /// ritornano subito un messaggio informativo invece di aspettare il timeout.
    pub dependency_status: crate::task_watchdog::DependencyStatusRef,
    /// Dispatcher centrale di eventi cross-pannello.
    /// Tool che mutano risorse chiamano `nexus_events::dispatcher::emit` per
    /// notificare i pannelli frontend in tempo reale.
    pub project_channels: nexus_events::ProjectChannels,
    /// Registro monitor in-memory (per `dispatcher_update_monitor` tool).
    pub monitor_registry: std::sync::Arc<
        parking_lot::RwLock<
            std::collections::HashMap<Uuid, std::collections::HashMap<String, serde_json::Value>>,
        >,
    >,
    /// Cache port_registry (PR hardening): usata da `tool_run_service` per
    /// auto-allocare PORT nel bucket del progetto via `find_or_allocate_port`.
    pub port_registry: crate::port_registry::PortRegistryCache,
}

/// Ritorna true se il tool modifica lo stato del filesystem o del repository.
pub fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "edit_file"
            | "delete_file"
            | "rename_file"
            | "git_stage"
            | "git_commit"
            | "git_push"
            | "git_pull"
            | "git_remote_add"
    )
    // run_in_terminal è intenzionalmente NON mutante: il comando appare nel terminale
    // ma l'agente non ha visibilità dell'output, quindi non blocca la conferma.
}
