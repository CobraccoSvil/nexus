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
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use nexus_agent_tools::context_core::FileReindexer;
use nexus_agent_tools::ToolContextCore;
use sqlx::PgPool;
use uuid::Uuid;

/// Canale di NARRAZIONE del run invocante: run_id + sender broadcast SSE del
/// run del grafo nativo che sta eseguendo il tool corrente. Permette ai tool a
/// lunga durata (`dispatch_subagent`/`dispatch_subagents`) di emettere meta-step
/// sul run PADRE mentre lavorano (avvio/progresso/chiusura del sub-run), invece
/// di lasciare la chat muta per minuti. Valorizzato SOLO dal path Real del
/// grafo nativo (`ToolRunnerExecutorAdapter`): fuori dal grafo (server gRPC,
/// dispatch legacy) resta `None` -> nessuna narrazione, comportamento invariato.
#[derive(Debug, Clone)]
pub struct ParentNarration {
    /// Run del grafo che ha invocato il tool (destinatario dei meta-step).
    pub run_id: Uuid,
    /// Sessione del run invocante (colonne trace/persistenze correlate).
    pub session_id: Uuid,
    /// Canale broadcast SSE del run invocante (lo stesso di `agent_channels`).
    pub step_tx: tokio::sync::broadcast::Sender<crate::agent_types::AgentStepEvent>,
}

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
    /// Narrazione verso il run invocante (vedi [`ParentNarration`]). `None`
    /// fuori dal grafo nativo.
    pub parent_narration: Option<ParentNarration>,
}

impl Deref for AgentToolContext {
    type Target = ToolContextCore;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

/// Implementazione mcp-core del contratto `FileReindexer`: delega al punto
/// unico `crate::projects::reindex_single_file` (NeuralCoreClient + Qdrant).
#[derive(Debug, Clone)]
pub struct NeuralFileReindexer {
    pub db: Arc<PgPool>,
    pub neural: crate::orchestrator::NeuralCoreClient,
}

impl FileReindexer for NeuralFileReindexer {
    fn reindex_file(
        &self,
        project_id: Uuid,
        root: PathBuf,
        file: PathBuf,
    ) -> BoxFuture<'static, ()> {
        let db = self.db.clone();
        let neural = self.neural.clone();
        Box::pin(async move {
            let _ = crate::projects::reindex_single_file(&db, &neural, project_id, &root, &file)
                .await;
        })
    }
}
