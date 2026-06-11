//! Contesto core di esecuzione tool (`ToolContextCore`).
//!
//! Split 7.4 (passo agent_tools-2): i campi di `AgentToolContext` senza
//! dipendenze da mcp-core. I tool che usano SOLO questi campi vivono in
//! questo crate; mcp-core avvolge `ToolContextCore` in `AgentToolContext`
//! (campo `core` + `Deref`) aggiungendo i 4 campi accoppiati al monolite
//! (playwright_channels, neural, dependency_status, port_registry).

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use sqlx::PgPool;
use uuid::Uuid;

use crate::monitor::MonitorRegistry;

/// Contratto di reindicizzazione vettoriale di un singolo file dopo una
/// mutazione (commit, write). I tool estratti lo invocano senza conoscere
/// l'implementazione (in mcp-core: `reindex_single_file` via NeuralCoreClient).
/// Best-effort: gli errori sono assorbiti/loggati dall'implementazione.
pub trait FileReindexer: std::fmt::Debug + Send + Sync {
    fn reindex_file(
        &self,
        project_id: Uuid,
        root: PathBuf,
        file: PathBuf,
    ) -> BoxFuture<'static, ()>;
}

/// Implementazione no-op per i test e per contesti senza indicizzazione.
#[derive(Debug, Clone, Copy)]
pub struct NoopReindexer;

impl FileReindexer for NoopReindexer {
    fn reindex_file(&self, _: Uuid, _: PathBuf, _: PathBuf) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// Campi core del contesto tool, sufficienti per i tool estratti dal monolite.
#[derive(Debug, Clone)]
pub struct ToolContextCore {
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
    /// Pattern long-running caricati dal DB (pre-fetched all'inizio del run).
    pub long_running_patterns: Vec<String>,
    /// Ruolo utente corrente ("admin" | "editor" | "viewer") — usato dai tool nexus_builtin.
    pub user_role: String,
    /// Se true, l'agente opera come operatore Nexus con permessi completi
    /// sui file del progetto gestito. Bypassa `PROTECTED_PATTERNS` ma NON
    /// la protezione infrastruttura (path-traversal, container ideai-*).
    pub is_nexus_operator: bool,
    /// Dispatcher centrale di eventi cross-pannello.
    /// Tool che mutano risorse chiamano `nexus_events::dispatcher::emit` per
    /// notificare i pannelli frontend in tempo reale.
    pub project_channels: nexus_events::ProjectChannels,
    /// Registro monitor in-memory (per `dispatcher_update_monitor` tool).
    pub monitor_registry: MonitorRegistry,
    /// Reindicizzazione vettoriale post-mutazione (vedi `FileReindexer`).
    pub reindexer: Arc<dyn FileReindexer>,
}
