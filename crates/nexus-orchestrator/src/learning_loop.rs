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
            .or_insert_with(WorkerStats::default);
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

    /// Avvia un loop periodico in background che chiama `tick()` ogni N secondi.
    /// Ritorna un handle tokio che può essere abortito dal chiamante.
    ///
    /// Nota: il contesto passato non include swarm_result perché il tick
    /// è indipendente dall'esecuzione. I periodic worker lavorano su stato
    /// globale (namespace cleanup, metrics aggregation, ecc.).
    pub fn start_periodic_loop(
        self: Arc<Self>,
        interval: Duration,
        context_factory: Arc<dyn Fn() -> LearningContext + Send + Sync>,
    ) -> tokio::task::JoinHandle<()> {
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
