//! Contesto di esecuzione tool (`AgentToolContext`).
//!
//! Estratto da agent_tools.rs (refactor god-file). La classificazione dei
//! tool mutanti vive nel brain Python (`_MUTATING_FILE_TOOLS`).
//!
//! Split 7.4 (passo agent_tools-2): i campi senza dipendenze da mcp-core
//! vivono in `nexus_agent_tools::ToolContextCore` (campo `core`); il `Deref`
//! mantiene l'accesso diretto ai campi core (`ctx.project_id`, `ctx.db`, ...)
//! nei tool rimasti in questo crate. Qui restano solo i 4 campi accoppiati
//! al monolite.

use std::ops::Deref;

use nexus_agent_tools::ToolContextCore;

/// Contesto necessario all'esecuzione dei tool.
#[derive(Debug, Clone)]
pub struct AgentToolContext {
    /// Campi core condivisi con i tool estratti in nexus-agent-tools.
    pub core: ToolContextCore,
    /// Channel per stream live eventi Playwright (run live monitoring).
    pub playwright_channels: crate::playwright_live::PlaywrightChannels,
    /// Client Neural Core per i run figlio.
    pub neural: crate::orchestrator::NeuralCoreClient,
    /// Stato atomico dipendenze (Qdrant, embedder). Se down, i tool vettoriali
    /// ritornano subito un messaggio informativo invece di aspettare il timeout.
    pub dependency_status: crate::task_watchdog::DependencyStatusRef,
    /// Cache port_registry (PR hardening): usata da `tool_run_service` per
    /// auto-allocare PORT nel bucket del progetto via `find_or_allocate_port`.
    pub port_registry: crate::port_registry::PortRegistryCache,
}

impl Deref for AgentToolContext {
    type Target = ToolContextCore;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
