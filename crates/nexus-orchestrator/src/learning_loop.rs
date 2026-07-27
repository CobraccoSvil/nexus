//! Learning Loop — framework per worker di background che eseguono
//! continuous improvement dopo esecuzione di task.
//!
//! Architettura:
//! - `LearningWorker` trait: ogni worker implementa `run(context)` async
//! - `LearningContext`: contiene i dati rilevanti post-esecuzione
//!   (task result, swarm result, namespace, router reference)
//! - `LearningScheduler`: accetta worker registrati e li esegue
//!   in risposta a eventi (task_completed) oppure periodicamente (cron-like)
//!
//! I worker possono essere:
//! - **Reactive**: triggerati da `on_task_complete(result)`
//! - **Periodic**: eseguiti a intervalli fissi via `tick()`
//!
//! Design pragmatico: non usiamo un vero cron — un timer `tokio::time::interval`
//! è sufficiente per le esigenze iniziali di Nexus.

use crate::namespace::MemoryNamespace;
use crate::q_learning::QLearningRouter;
use crate::swarm_types::{SwarmExecutionResult, SwarmTaskOutcome};
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, warn};

/// Contesto passato ai worker durante l'esecuzione
#[derive(Clone)]
pub struct LearningContext {
    /// Risultato dello swarm che ha appena completato
    pub swarm_result: Option<Arc<SwarmExecutionResult>>,
    /// Namespace dello swarm (shared memory)
    pub namespace: Option<Arc<MemoryNamespace>>,
    /// Router per update Q-values
    pub router: Option<Arc<QLearningRouter>>,
    /// Timestamp corrente
    pub now: Instant,
}

impl LearningContext {
    pub fn new() -> Self {
        Self {
            swarm_result: None,
            namespace: None,
            router: None,
            now: Instant::now(),
        }
    }

    pub fn with_swarm(mut self, result: Arc<SwarmExecutionResult>) -> Self {
        self.swarm_result = Some(result);
        self
    }

    pub fn with_namespace(mut self, ns: Arc<MemoryNamespace>) -> Self {
        self.namespace = Some(ns);
        self
    }

    pub fn with_router(mut self, router: Arc<QLearningRouter>) -> Self {
        self.router = Some(router);
        self
    }

    /// Helper: iterazione sui task outcomes
    pub fn task_outcomes(&self) -> &[SwarmTaskOutcome] {
        match &self.swarm_result {
            Some(r) => &r.task_results,
            None => &[],
        }
    }
}

impl Default for LearningContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Quando un worker viene eseguito
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerTrigger {
    /// Dopo ogni task completato (reactive)
    OnTaskComplete,
    /// Periodicamente ogni N secondi
    Periodic,
    /// Entrambi
    Both,
}

/// Outcome dell'esecuzione di un worker
#[derive(Clone, Debug)]
pub struct WorkerOutcome {
    pub worker_name: String,
    pub success: bool,
    pub duration_ms: u64,
    pub message: Option<String>,
    pub metrics: HashMap<String, f32>,
}

impl WorkerOutcome {
    pub fn ok(name: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            worker_name: name.into(),
            success: true,
            duration_ms,
            message: None,
            metrics: HashMap::new(),
        }
    }

    pub fn fail(name: impl Into<String>, msg: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            worker_name: name.into(),
            success: false,
            duration_ms,
            message: Some(msg.into()),
            metrics: HashMap::new(),
        }
    }

    pub fn with_metric(mut self, key: impl Into<String>, value: f32) -> Self {
        self.metrics.insert(key.into(), value);
        self
    }
}

/// Trait principale dei learning workers
#[async_trait]
pub trait LearningWorker: Send + Sync {
    /// Nome identificativo
    fn name(&self) -> &str;

    /// Quando viene triggerato
    fn trigger(&self) -> WorkerTrigger;

    /// Intervallo periodico (usato se `trigger()` è `Periodic` o `Both`)
    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    /// Esecuzione del worker
    async fn run(&self, context: &LearningContext) -> WorkerOutcome;

    /// Abilitato di default
    fn enabled(&self) -> bool {
        true
    }
}

/// E' ora di eseguire questo worker? PUNTO UNICO (regola L) del criterio di
/// cadenza: `tick()` lo applica a ogni worker periodico.
///
/// Perche' esiste: [`LearningWorker::interval`] era dichiarato dal trait e
/// implementato dai worker, ma NON veniva letto da nessuno —
/// `start_periodic_loop` aveva un solo intervallo globale ed eseguiva TUTTI i
/// worker periodici a ogni giro. Le cadenze dichiarate erano quindi
/// configurazione morta, con danni reali: lo snapshot di sessione aveva un TTL
/// di 600s ma veniva riscritto ogni 1800s (scaduto per due terzi del tempo,
/// quindi il ripristino dopo un crash quasi mai disponibile), e il cleanup
/// dichiarato "essenziale per evitare memory leak" a 60s girava 30 volte meno
/// spesso del previsto.
///
/// `None` (mai eseguito) vale sempre "e' ora": ogni worker gira una volta al
/// primo tick utile. Un `last_run` nel futuro (orologio spostato indietro)
/// vale "e' ora" invece di bloccare il worker fino a quando il tempo lo
/// raggiunge.
pub(crate) fn is_worker_due(
    last_run: Option<chrono::DateTime<chrono::Utc>>,
    interval: Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(last) = last_run else { return true };
    match now.signed_duration_since(last).to_std() {
        Ok(elapsed) => elapsed >= interval,
        Err(_) => true,
    }
}

/// Statistiche cumulate dello scheduler
#[derive(Clone, Debug, Default)]
pub struct SchedulerStats {
    pub total_runs: u64,
    pub total_failures: u64,
    pub per_worker: HashMap<String, WorkerStats>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerStats {
    pub runs: u64,
    pub failures: u64,
    pub total_duration_ms: u64,
    pub last_run: Option<chrono::DateTime<chrono::Utc>>,
}

/// Scheduler centralizzato dei learning workers
pub struct LearningScheduler {
    workers: RwLock<Vec<Arc<dyn LearningWorker>>>,
    stats: RwLock<SchedulerStats>,
}

impl Default for LearningScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl LearningScheduler {
    pub fn new() -> Self {
        Self {
            workers: RwLock::new(Vec::new()),
            stats: RwLock::new(SchedulerStats::default()),
        }
    }

    /// Registra un worker
    pub fn register(&self, worker: Arc<dyn LearningWorker>) {
        self.workers.write().push(worker);
    }

    /// Esegue tutti i worker reactive con il contesto dato
    pub async fn on_task_complete(&self, context: LearningContext) -> Vec<WorkerOutcome> {
        let workers: Vec<Arc<dyn LearningWorker>> = {
            self.workers
                .read()
                .iter()
                .filter(|w| {
                    w.enabled()
                        && matches!(
                            w.trigger(),
                            WorkerTrigger::OnTaskComplete | WorkerTrigger::Both
                        )
                })
                .cloned()
                .collect()
        };

        self.run_workers(&workers, &context).await
    }

    /// Esegue tutti i worker periodici (chiamato dallo user loop / tokio interval)
    pub async fn tick(&self, context: LearningContext) -> Vec<WorkerOutcome> {
        let now = chrono::Utc::now();
        // Istantanea degli ultimi avvii: si copia sotto lock e lo si rilascia
        // subito, perche' il filtro sotto chiama `w.interval()` sui worker e
        // tenere due lock insieme e' un invito al deadlock.
        let last_runs: HashMap<String, chrono::DateTime<chrono::Utc>> = {
            let stats = self.stats.read();
            stats
                .per_worker
                .iter()
                .filter_map(|(name, s)| s.last_run.map(|t| (name.clone(), t)))
                .collect()
        };
        let workers: Vec<Arc<dyn LearningWorker>> = {
            self.workers
                .read()
                .iter()
                .filter(|w| {
                    w.enabled()
                        && matches!(
                            w.trigger(),
                            WorkerTrigger::Periodic | WorkerTrigger::Both
                        )
                        // Ogni worker alla SUA cadenza (vedi [`is_worker_due`]).
                        && is_worker_due(last_runs.get(w.name()).copied(), w.interval(), now)
                })
                .cloned()
                .collect()
        };

        self.run_workers(&workers, &context).await
    }

    async fn run_workers(
        &self,
        workers: &[Arc<dyn LearningWorker>],
        context: &LearningContext,
    ) -> Vec<WorkerOutcome> {
        let mut outcomes = Vec::with_capacity(workers.len());
        for worker in workers {
            let start = Instant::now();
            let name = worker.name().to_string();
            debug!("Running worker: {}", name);

            // Catch panics via std::panic::AssertUnwindSafe + catch_unwind is complex in async.
            // Un worker che panica causerà un errore gestito da tokio.
            // Per ora: chiamata diretta. Un wrapper di safety può essere aggiunto.
            // Notifica inizio worker (system-wide, broadcast a tutti i client)
            nexus_events::dispatcher::broadcast_all_global(
                nexus_events::ProjectEvent::SubagentRunChanged {
                    run_id: name.clone(),
                    status: "started".to_string(),
                    parent_run_id: None,
                },
            );

            let outcome = worker.run(context).await;
            let duration = start.elapsed().as_millis() as u64;

            // Notifica completamento/fallimento worker
            nexus_events::dispatcher::broadcast_all_global(
                nexus_events::ProjectEvent::SubagentRunChanged {
                    run_id: name.clone(),
                    status: if outcome.success { "completed".to_string() } else { "failed".to_string() },
                    parent_run_id: None,
                },
            );

            if !outcome.success {
                warn!(
                    "Worker {} failed: {:?}",
                    name,
                    outcome.message.as_deref().unwrap_or("(no message)")
                );
            }

            self.record_stats(&name, &outcome, duration);
            outcomes.push(outcome);
        }
        outcomes
    }

    fn record_stats(&self, name: &str, outcome: &WorkerOutcome, duration_ms: u64) {
        let mut stats = self.stats.write();
        stats.total_runs += 1;
        if !outcome.success {
            stats.total_failures += 1;
        }
        let entry = stats
            .per_worker
            .entry(name.to_string())
            .or_default();
        entry.runs += 1;
        if !outcome.success {
            entry.failures += 1;
        }
        entry.total_duration_ms += duration_ms;
        entry.last_run = Some(chrono::Utc::now());
    }

    /// Numero di worker registrati
    pub fn len(&self) -> usize {
        self.workers.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.workers.read().is_empty()
    }

    /// Nomi dei worker registrati
    pub fn worker_names(&self) -> Vec<String> {
        self.workers
            .read()
            .iter()
            .map(|w| w.name().to_string())
            .collect()
    }

    /// Statistiche correnti
    pub fn stats(&self) -> SchedulerStats {
        self.stats.read().clone()
    }

    /// Avvia il loop in background che chiama `tick()`.
    /// Ritorna un handle tokio che può essere abortito dal chiamante.
    ///
    /// `granularity` NON e' la cadenza dei worker: e' ogni quanto lo scheduler
    /// si sveglia per CHIEDERSI chi e' in scadenza. La cadenza vera di ciascun
    /// worker e' la sua [`LearningWorker::interval`], applicata da `tick()` via
    /// [`is_worker_due`]. Va scelta piu' fine dell'intervallo del worker piu'
    /// frequente, altrimenti quel worker slitta alla granularita'.
    ///
    /// Nota: il contesto passato non include swarm_result perché il tick
    /// è indipendente dall'esecuzione. I periodic worker lavorano su stato
    /// globale (namespace cleanup, metrics aggregation, ecc.).
    pub fn start_periodic_loop(
        self: Arc<Self>,
        granularity: Duration,
        context_factory: Arc<dyn Fn() -> LearningContext + Send + Sync>,
    ) -> tokio::task::JoinHandle<()> {
        let interval = granularity;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let ctx = context_factory();
                let outcomes = self.tick(ctx).await;
                for o in &outcomes {
                    if !o.success {
                        error!("periodic worker {} failed", o.worker_name);
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cadenza per-worker ──────────────────────────
    //
    // Il difetto che questi test presidiano: `interval()` era dichiarato dal
    // trait e implementato dai worker, ma nessuno lo leggeva. Se `is_worker_due`
    // tornasse a rispondere `true` sempre (com'era di fatto prima), i primi due
    // test falliscono.

    /// Cadenza dichiarata da `CleanupWorker`.
    const CLEANUP_SECS: u64 = 60;
    /// Cadenza dichiarata da `SessionPersistenceWorker`.
    const SESSION_SECS: u64 = 300;
    /// TTL dello snapshot scritto da `SessionPersistenceWorker`.
    const SESSION_TTL_SECS: u64 = 600;
    /// Cadenza dichiarata da `PromptOptimizerWorker`.
    const OPTIMIZER_SECS: u64 = 1800;

    /// Un worker appena registrato gira al primo tick utile.
    #[test]
    fn mai_eseguito_e_sempre_in_scadenza() {
        let now = chrono::Utc::now();
        assert!(is_worker_due(None, Duration::from_secs(OPTIMIZER_SECS), now));
    }

    /// Prima che l'intervallo sia trascorso il worker non viene eseguito.
    #[test]
    fn dentro_il_proprio_intervallo_non_si_esegue() {
        let now = chrono::Utc::now();
        let interval = Duration::from_secs(CLEANUP_SECS);
        // A meta' dell'intervallo non tocca a lui...
        let last = now - chrono::Duration::seconds(CLEANUP_SECS as i64 / 2);
        assert!(!is_worker_due(Some(last), interval, now));
        // ...e passato l'intervallo si'.
        let last = now - chrono::Duration::seconds(CLEANUP_SECS as i64 + 1);
        assert!(is_worker_due(Some(last), interval, now));
    }

    /// La cadenza del persistence worker deve restare sotto il TTL che scrive.
    #[test]
    fn snapshot_di_sessione_riscritto_prima_di_scadere() {
        // Il danno concreto del difetto: SessionPersistenceWorker dichiara la sua
        // cadenza e scrive uno snapshot con un TTL doppio, ma girava ogni 1800s —
        // quindi la chiave era scaduta per due terzi del tempo e il "ripristino
        // sessione dopo crash" quasi mai disponibile.
        let interval = Duration::from_secs(SESSION_SECS);
        let ttl = Duration::from_secs(SESSION_TTL_SECS);
        assert!(
            interval < ttl,
            "la cadenza ({interval:?}) deve stare sotto il TTL ({ttl:?})"
        );
        let now = chrono::Utc::now();
        let last = now - chrono::Duration::seconds(SESSION_SECS as i64 + 1);
        assert!(is_worker_due(Some(last), interval, now));
    }

    /// Un `last_run` nel futuro non congela il worker.
    #[test]
    fn orologio_spostato_indietro_non_blocca_il_worker() {
        // Senza questo ramo il worker resterebbe fermo finche' il tempo non
        // raggiunge il valore registrato.
        let now = chrono::Utc::now();
        let last = now + chrono::Duration::seconds(CLEANUP_SECS as i64);
        assert!(is_worker_due(
            Some(last),
            Duration::from_secs(CLEANUP_SECS),
            now
        ));
    }

    struct DummyWorker {
        name: String,
        trigger: WorkerTrigger,
        should_fail: bool,
    }

    #[async_trait]
    impl LearningWorker for DummyWorker {
        fn name(&self) -> &str {
            &self.name
        }
        fn trigger(&self) -> WorkerTrigger {
            self.trigger
        }
        async fn run(&self, _ctx: &LearningContext) -> WorkerOutcome {
            if self.should_fail {
                WorkerOutcome::fail(&self.name, "deliberate failure", 10)
            } else {
                WorkerOutcome::ok(&self.name, 10)
            }
        }
    }

    #[tokio::test]
    async fn test_scheduler_runs_reactive_workers() {
        let scheduler = LearningScheduler::new();
        scheduler.register(Arc::new(DummyWorker {
            name: "w1".to_string(),
            trigger: WorkerTrigger::OnTaskComplete,
            should_fail: false,
        }));
        scheduler.register(Arc::new(DummyWorker {
            name: "w2".to_string(),
            trigger: WorkerTrigger::Periodic,
            should_fail: false,
        }));

        let outcomes = scheduler.on_task_complete(LearningContext::new()).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].worker_name, "w1");
    }

    #[tokio::test]
    async fn test_scheduler_runs_periodic_workers() {
        let scheduler = LearningScheduler::new();
        scheduler.register(Arc::new(DummyWorker {
            name: "w1".to_string(),
            trigger: WorkerTrigger::OnTaskComplete,
            should_fail: false,
        }));
        scheduler.register(Arc::new(DummyWorker {
            name: "w2".to_string(),
            trigger: WorkerTrigger::Periodic,
            should_fail: false,
        }));
        scheduler.register(Arc::new(DummyWorker {
            name: "w3".to_string(),
            trigger: WorkerTrigger::Both,
            should_fail: false,
        }));

        let outcomes = scheduler.tick(LearningContext::new()).await;
        assert_eq!(outcomes.len(), 2); // w2 + w3
    }

    #[tokio::test]
    async fn test_stats_tracking() {
        let scheduler = LearningScheduler::new();
        scheduler.register(Arc::new(DummyWorker {
            name: "ok_w".to_string(),
            trigger: WorkerTrigger::OnTaskComplete,
            should_fail: false,
        }));
        scheduler.register(Arc::new(DummyWorker {
            name: "fail_w".to_string(),
            trigger: WorkerTrigger::OnTaskComplete,
            should_fail: true,
        }));

        scheduler.on_task_complete(LearningContext::new()).await;
        scheduler.on_task_complete(LearningContext::new()).await;

        let stats = scheduler.stats();
        assert_eq!(stats.total_runs, 4);
        assert_eq!(stats.total_failures, 2);
        assert_eq!(stats.per_worker.get("ok_w").unwrap().runs, 2);
        assert_eq!(stats.per_worker.get("fail_w").unwrap().failures, 2);
    }
}
