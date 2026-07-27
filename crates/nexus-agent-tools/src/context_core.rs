//! Contesto core di esecuzione tool (`ToolContextCore`).
//!
//! Split 7.4 (passo agent_tools-2): i campi di `AgentToolContext` senza
//! dipendenze da mcp-core. I tool che usano SOLO questi campi vivono in
//! questo crate; mcp-core avvolge `ToolContextCore` in `AgentToolContext`
//! (campo `core` + `Deref`) aggiungendo i 4 campi accoppiati al monolite
//! (playwright_channels, neural, dependency_status, port_registry).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use sqlx::PgPool;
use uuid::Uuid;

use crate::monitor::MonitorRegistry;

/// Contratto degli hook che circondano una MUTAZIONE FILE eseguita da un tool
/// agente. I tool estratti lo invocano senza conoscere l'implementazione: in
/// mcp-core vivono le funzioni concrete (`projects::reindex_single_file`,
/// `projects::maybe_auto_scan_file`, `security::resource_linter`,
/// `security::resource_governance`, `file_mutations`, `session_autocommit`),
/// che il monolite collega qui in un punto solo.
///
/// Nasce come `FileReindexer` (solo `reindex_file`, per il commit git) e viene
/// generalizzato all'INTERO ciclo di vita della mutazione perche' il ramo
/// post-scrittura era duplicato: `spawn_write_reindex` e `spawn_edit_reindex`
/// erano gemelli (stesse tre azioni, unica differenza l'hook documenti del
/// solo `write_file`). Qui la duplicazione si chiude in un metodo unico con
/// `content: Option<&str>` a distinguere i due casi (regola L).
///
/// I metodi `spawn_*` sono sincroni e fanno `tokio::spawn` al loro interno:
/// sono best-effort fire-and-forget, gli errori sono assorbiti/loggati
/// dall'implementazione.
pub trait FileMutationHooks: std::fmt::Debug + Send + Sync {
    /// Reindicizzazione vettoriale di un singolo file dopo una mutazione
    /// (usata dal commit git, che non passa dal ramo write/edit).
    fn reindex_file(
        &self,
        project_id: Uuid,
        root: PathBuf,
        file: PathBuf,
    ) -> BoxFuture<'static, ()>;

    /// Gate PRE-scrittura: governance risorse (placeholder di redazione, porte
    /// ADR 0010, URL interni hardcoded, quota disco). `Some(msg)` RIFIUTA la
    /// scrittura e il messaggio torna all'agente; `None` lascia passare.
    fn enforce_on_write<'a>(
        &'a self,
        core: &'a ToolContextCore,
        tool_name: &'a str,
        path: &'a str,
        content: &'a str,
    ) -> BoxFuture<'a, Option<String>>;

    /// Tracking ripristinabile della mutazione (mig 0349), registrato PRIMA
    /// della scrittura. Best-effort: non blocca il tool.
    fn record_mutation<'a>(
        &'a self,
        core: &'a ToolContextCore,
        path: &'a str,
        tool_name: &'a str,
        before: Option<&'a str>,
        after: Option<&'a str>,
    ) -> BoxFuture<'a, ()>;

    /// Snapshot di auto-commit per sessione su branch dedicato (rete di
    /// sicurezza sopra il tracking mutazioni).
    fn spawn_autocommit_snapshot(&self, core: &ToolContextCore, op: &str, path: &str);

    /// Hook POST-scrittura riuscita: reindicizzazione nel code index, eventuale
    /// auto-scan qualita' e ri-validazione delle violazioni di governance
    /// risolte dalla modifica (regola H: niente residui nel pannello Problemi).
    /// `content` valorizzato SOLO per `write_file`, dove abilita anche l'hook M2
    /// di registrazione documentazione; `None` per `edit_file`, che non ricrea
    /// il file da zero.
    fn spawn_post_write(
        &self,
        core: &ToolContextCore,
        target: &Path,
        path: &str,
        content: Option<&str>,
    );
}

/// Contratto di embedding testuale. Gemello di [`FileMutationHooks`] per i tool
/// vettoriali estratti (knowledge): evita di tirare l'orchestrator nel crate
/// basso per l'unica riga che serve davvero. In mcp-core lo implementa
/// `NeuralCoreClient` (zero-sized, delega a `NexusBridge::global()`).
pub trait TextEmbedder: std::fmt::Debug + Send + Sync {
    /// Vettore del testo. `model` vuoto = modello di default dell'embedder.
    /// L'errore e' gia' reso in stringa: i chiamanti lo formattano soltanto.
    fn embed_text<'a>(
        &'a self,
        model: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, Result<Vec<f32>, String>>;
}

/// Implementazione no-op per i test e per contesti senza indicizzazione.
#[derive(Debug, Clone, Copy)]
pub struct NoopMutationHooks;

impl FileMutationHooks for NoopMutationHooks {
    fn reindex_file(&self, _: Uuid, _: PathBuf, _: PathBuf) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }

    fn enforce_on_write<'a>(
        &'a self,
        _: &'a ToolContextCore,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn record_mutation<'a>(
        &'a self,
        _: &'a ToolContextCore,
        _: &'a str,
        _: &'a str,
        _: Option<&'a str>,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn spawn_autocommit_snapshot(&self, _: &ToolContextCore, _: &str, _: &str) {}

    fn spawn_post_write(&self, _: &ToolContextCore, _: &Path, _: &str, _: Option<&str>) {}
}

/// Embedder no-op per i test: nessun vettore disponibile.
#[derive(Debug, Clone, Copy)]
pub struct NoopEmbedder;

impl TextEmbedder for NoopEmbedder {
    fn embed_text<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Vec<f32>, String>> {
        Box::pin(async { Err("embedder non configurato".to_string()) })
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
    /// Pool del META-DB (settings, catalogo, template — config di piattaforma).
    pub db: Arc<PgPool>,
    /// Pool del DB dove vivono i dati per-progetto del run (agent_runs,
    /// nexus_agent_plans/todos, worklog). Risolto dal costruttore del contesto
    /// (separazione DB: a flag OFF coincide col meta). I tool che toccano il
    /// dominio run DEVONO usare questo, non `db`.
    pub run_db: Arc<PgPool>,
    /// ID del run padre (per agenti figlio lanciati da
    /// `dispatch_subagent`/`dispatch_subagents`).
    pub parent_run_id: Option<Uuid>,
    /// ID del run CORRENTE che sta eseguendo i tool (il run del grafo nativo che
    /// ha invocato il tool). Diverso da `parent_run_id`/`session_id`: e' il run
    /// che il motore SOSPENDE (`awaiting_subagents`) quando dispatcha figli in
    /// background e che il fan-in deve RIPRENDERE. Valorizzato SOLO dal path Real
    /// del grafo (`ToolRunnerExecutorAdapter::execute_real`, dalla narrazione del
    /// run invocante); `None` fuori dal grafo (server gRPC, dispatch legacy, test).
    pub run_id: Option<Uuid>,
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
    /// Hook attorno alle mutazioni file (vedi [`FileMutationHooks`]).
    pub hooks: Arc<dyn FileMutationHooks>,
    /// Embedding testuale per i tool vettoriali (vedi [`TextEmbedder`]).
    pub embedder: Arc<dyn TextEmbedder>,
    /// `true` se questo ctx appartiene a un SUB-RUN ISOLATO (scrive in un git
    /// worktree effimero proprio, non nella project_root condivisa). Leva della
    /// FASE 2 orchestrazione (isolamento fisico sub-agenti): quando `true` gli
    /// hook fire-and-forget keyed su session/project condivisi vanno SOPPRESSI —
    /// autocommit di sessione (index temp + branch ref condivisi) e reindex
    /// per-scrittura (indice del progetto + race col cleanup del worktree). Per i
    /// sub-run isolati l'UNICA fonte di verita' del commit e' l'apply atomico
    /// serializzato post-run (PR4), e il reindex avviene UNA volta sui soli file
    /// realmente promossi alla root. Default `false` -> comportamento invariato
    /// (ogni ctx non isolato mantiene autocommit + reindex come oggi).
    pub isolated_subrun: bool,
}
