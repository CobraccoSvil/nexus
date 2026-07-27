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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use nexus_agent_tools::context_core::{FileMutationHooks, TextEmbedder};
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

/// Implementazione mcp-core del contratto [`FileMutationHooks`]: e' il punto in
/// cui i tool file estratti in nexus-agent-tools ritrovano le funzioni che sono
/// rimaste nel monolite (`projects::reindex_single_file`,
/// `projects::maybe_auto_scan_file`, `security::resource_linter`,
/// `security::resource_governance`, `file_mutations`, `session_autocommit`).
#[derive(Debug, Clone)]
pub struct NeuralFileReindexer {
    pub db: Arc<PgPool>,
    pub neural: crate::orchestrator::NeuralCoreClient,
}

impl FileMutationHooks for NeuralFileReindexer {
    fn reindex_file(
        &self,
        project_id: Uuid,
        root: PathBuf,
        file: PathBuf,
    ) -> BoxFuture<'static, ()> {
        let db = self.db.clone();
        let neural = self.neural.clone();
        Box::pin(async move {
            let _ =
                crate::projects::reindex_single_file(&db, &neural, project_id, &root, &file).await;
        })
    }

    fn enforce_on_write<'a>(
        &'a self,
        core: &'a ToolContextCore,
        tool_name: &'a str,
        path: &'a str,
        content: &'a str,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(crate::security::resource_governance::enforce_on_write(
            core, tool_name, path, content,
        ))
    }

    fn record_mutation<'a>(
        &'a self,
        core: &'a ToolContextCore,
        path: &'a str,
        tool_name: &'a str,
        before: Option<&'a str>,
        after: Option<&'a str>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(e) = crate::file_mutations::record_mutation(
                &core.db,
                core.project_id,
                core.session_id,
                Some(core.user_id),
                path,
                tool_name,
                before,
                after,
            )
            .await
            {
                tracing::warn!(
                    project_id = %core.project_id, path = %path,
                    "file_mutations::record_mutation fallita ({tool_name}): {e}"
                );
            }
        })
    }

    fn spawn_autocommit_snapshot(&self, core: &ToolContextCore, op: &str, path_str: &str) {
        let ac_db = core.db.clone();
        let ac_root = core.root_path.clone();
        let ac_is_git = core.is_git_repo;
        let ac_sid = core.session_id;
        // Soppressione FASE 2 (buco B2): per un sub-run isolato l'autocommit e' no-op
        // (il flag e' passato alla funzione, che early-return). L'unica fonte del
        // commit e' l'apply atomico post-run (PR4).
        let ac_isolated = core.isolated_subrun;
        let ac_path = path_str.to_string();
        let ac_op = op.to_string();
        tokio::spawn(async move {
            crate::session_autocommit::snapshot_after_mutation(
                &ac_db,
                &ac_root,
                ac_is_git,
                ac_sid,
                ac_isolated,
                &ac_op,
                &ac_path,
            )
            .await;
        });
    }

    fn spawn_post_write(
        &self,
        core: &ToolContextCore,
        target: &Path,
        path_str: &str,
        content: Option<&str>,
    ) {
        // Soppressione FASE 2 (buco B2): per un sub-run ISOLATO il reindex
        // fire-and-forget e' un no-op. Indicizzerebbe path del worktree effimero
        // nell'indice neurale del PROGETTO (contenuti mai promossi alla root) e,
        // non atteso, correrebbe col cleanup del worktree (lettura di file gia'
        // rimossi, lock su Windows). Il reindex avviene UNA volta post-apply sui
        // soli file realmente promossi alla project_root (PR4).
        if core.isolated_subrun {
            return;
        }
        let db_bg = core.db.clone();
        let neural_bg = self.neural.clone();
        let project_id_bg = core.project_id;
        let root_bg = core.root_path.clone();
        let target_bg = target.to_path_buf();
        let path_str_bg = path_str.to_string();
        let content_bg = content.map(str::to_string);
        tokio::spawn(async move {
            let _ = crate::projects::reindex_single_file(
                &db_bg,
                &neural_bg,
                project_id_bg,
                &root_bg,
                &target_bg,
            )
            .await;
            crate::projects::maybe_auto_scan_file(&db_bg, project_id_bg, &root_bg, &target_bg)
                .await;
            // Ri-valuta le violazioni di governance risorse sul file appena scritto:
            // se l'edit ha rimosso la porta/URL hardcoded, la diagnosi policy_violation
            // viene chiusa e sparisce dal pannello Problemi (regola H: niente residui).
            crate::security::resource_linter::revalidate_file_violations(
                &db_bg,
                project_id_bg,
                &root_bg.to_string_lossy(),
                &target_bg,
            )
            .await;
            // Hook M2: se il file e' un .md di documentazione, registra in
            // project_documents. Solo per `write_file`, che porta il contenuto
            // integrale: `edit_file` non ricrea il .md da zero.
            if let Some(c) = content_bg {
                let _ = nexus_agent_tools::files::upsert_project_document_if_doc(
                    &db_bg,
                    project_id_bg,
                    &path_str_bg,
                    &c,
                )
                .await;
            }
        });
    }
}

impl TextEmbedder for crate::orchestrator::NeuralCoreClient {
    fn embed_text<'a>(
        &'a self,
        model: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<Vec<f32>, String>> {
        Box::pin(async move {
            crate::orchestrator::NeuralCoreClient::embed_text(self, model, text)
                .await
                .map_err(|e| e.to_string())
        })
    }
}
