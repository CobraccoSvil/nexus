//! ClusteringWorker — raggruppa pattern simili per tipo di agente
//!
//! Worker periodico che analizza le entry `pattern:*` nel namespace e le
//! raggruppa per `agent_type`. Per ogni cluster produce:
//! - Numero di pattern
//! - Tasso di successo medio
//! - Tempo di esecuzione medio
//! - Score di qualità medio
//!
//! Il risultato viene pubblicato nel namespace come `cluster:{agent_type}`
//! e può essere usato da:
//! - Q-Learning router per bias iniziale delle selezioni
//! - Dashboard di monitoring per visualizzare performance per agente
//! - UltralearnWorker per prioritizzare pattern ad alta qualità
//!
//! ## Approccio
//!
//! Clustering semplice (groupby agent_type) senza ML — aggiunge significato
//! statistico senza overhead computazionale. Un futuro upgrade potrebbe
//! usare K-Means su embedding per clustering semantico.

use crate::learning_loop::{LearningContext, LearningWorker, WorkerOutcome, WorkerTrigger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Schema delle entry pattern:* (compatibile con UltralearnWorker)
#[derive(Debug, Deserialize)]
struct PatternEntry {
    pub agent_type: String,
    pub success: bool,
    pub quality_score: f32,
    pub execution_time_ms: u64,
}

/// Risultato del clustering per un singolo agente
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCluster {
    pub agent_type: String,
    pub pattern_count: usize,
    pub success_rate: f32,
    pub avg_quality: f32,
    pub avg_execution_ms: f32,
    pub computed_at: String,
}

pub struct ClusteringWorker {
    interval: Duration,
    /// Soglia minima di pattern per creare un cluster (evita noise)
    min_patterns_for_cluster: usize,
}

impl Default for ClusteringWorker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(240), // ogni 4 minuti
            min_patterns_for_cluster: 2,
        }
    }
}

impl ClusteringWorker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_min_patterns(mut self, min: usize) -> Self {
        self.min_patterns_for_cluster = min;
        self
    }
}

#[async_trait]
impl LearningWorker for ClusteringWorker {
    fn name(&self) -> &str {
        "clustering"
    }

    fn trigger(&self) -> WorkerTrigger {
        WorkerTrigger::Periodic
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn run(&self, context: &LearningContext) -> WorkerOutcome {
        let start = Instant::now();

        let ns = match &context.namespace {
            Some(ns) => ns,
            None => {
                return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                    .with_metric("clusters_found", 0.0);
            }
        };

        // Raccoglie tutte le entry pattern:* dal namespace
        let pattern_keys: Vec<String> = ns
            .keys()
            .into_iter()
            .filter(|k| k.starts_with("pattern:"))
            .collect();

        if pattern_keys.is_empty() {
            return WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
                .with_metric("clusters_found", 0.0)
                .with_metric("patterns_analyzed", 0.0);
        }

        // Raggruppa per agent_type
        #[derive(Default)]
        struct ClusterAccum {
            count: usize,
            successes: usize,
            quality_sum: f32,
            exec_time_sum: f64,
        }

        let mut groups: HashMap<String, ClusterAccum> = HashMap::new();
        let mut patterns_analyzed = 0usize;

        for key in &pattern_keys {
            let entry = match ns.get(key) {
                Some(e) => e,
                None => continue,
            };
            let pattern: PatternEntry = match serde_json::from_value(entry.value) {
                Ok(p) => p,
                Err(_) => continue,
            };

            let accum = groups
                .entry(pattern.agent_type.clone())
                .or_default();
            accum.count += 1;
            if pattern.success {
                accum.successes += 1;
            }
            accum.quality_sum += pattern.quality_score;
            accum.exec_time_sum += pattern.execution_time_ms as f64;
            patterns_analyzed += 1;
        }

        // Costruisce e pubblica i cluster
        let now_str = chrono::Utc::now().to_rfc3339();
        let mut clusters_found = 0usize;

        for (agent_type, accum) in &groups {
            if accum.count < self.min_patterns_for_cluster {
                continue;
            }

            let cluster = AgentCluster {
                agent_type: agent_type.clone(),
                pattern_count: accum.count,
                success_rate: accum.successes as f32 / accum.count as f32,
                avg_quality: accum.quality_sum / accum.count as f32,
                avg_execution_ms: accum.exec_time_sum as f32 / accum.count as f32,
                computed_at: now_str.clone(),
            };

            let key = format!("cluster:{}", agent_type.to_lowercase());
            let value = serde_json::to_value(&cluster).unwrap_or(serde_json::Value::Null);
            // TTL: 1 ora — viene sovrascritto ad ogni tick
            ns.set_with_ttl(&key, value, self.name(), Duration::from_secs(3600));
            clusters_found += 1;
        }

        WorkerOutcome::ok(self.name(), start.elapsed().as_millis() as u64)
            .with_metric("clusters_found", clusters_found as f32)
            .with_metric("patterns_analyzed", patterns_analyzed as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::MemoryNamespace;
    use std::sync::Arc;

    fn pattern(agent: &str, success: bool, quality: f32, time_ms: u64) -> serde_json::Value {
        serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "agent_type": agent,
            "task_id": "t1",
            "success": success,
            "quality_score": quality,
            "execution_time_ms": time_ms
        })
    }

    #[tokio::test]
    async fn test_clustering_groups_by_agent() {
        let ns = Arc::new(MemoryNamespace::new("cluster-test"));

        // 3 pattern Coder + 2 pattern Tester
        ns.set("pattern:1", pattern("Coder", true, 0.9, 100), "ul");
        ns.set("pattern:2", pattern("Coder", true, 0.8, 120), "ul");
        ns.set("pattern:3", pattern("Coder", false, 0.3, 200), "ul");
        ns.set("pattern:4", pattern("Tester", true, 0.7, 150), "ul");
        ns.set("pattern:5", pattern("Tester", true, 0.9, 130), "ul");

        let worker = ClusteringWorker::new().with_min_patterns(2);
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("clusters_found"), Some(&2.0));
        assert_eq!(outcome.metrics.get("patterns_analyzed"), Some(&5.0));

        // Verifica cluster Coder
        let coder_entry = ns.get("cluster:coder").expect("cluster:coder should exist");
        let coder: AgentCluster = serde_json::from_value(coder_entry.value).unwrap();
        assert_eq!(coder.pattern_count, 3);
        assert!((coder.success_rate - 2.0 / 3.0).abs() < 0.01);

        // Verifica cluster Tester
        let tester_entry = ns.get("cluster:tester").expect("cluster:tester should exist");
        let tester: AgentCluster = serde_json::from_value(tester_entry.value).unwrap();
        assert_eq!(tester.pattern_count, 2);
        assert!((tester.success_rate - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_clustering_respects_min_patterns() {
        let ns = Arc::new(MemoryNamespace::new("min-test"));
        // Solo 1 pattern per Architect — sotto soglia
        ns.set("pattern:solo", pattern("Architect", true, 0.8, 200), "ul");

        let worker = ClusteringWorker::new().with_min_patterns(2);
        let ctx = LearningContext::new().with_namespace(ns.clone());
        let outcome = worker.run(&ctx).await;

        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("clusters_found"), Some(&0.0));
        assert!(ns.get("cluster:architect").is_none());
    }

    #[tokio::test]
    async fn test_clustering_no_patterns() {
        let ns = Arc::new(MemoryNamespace::new("empty-cluster"));
        let worker = ClusteringWorker::new();
        let ctx = LearningContext::new().with_namespace(ns);
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
        assert_eq!(outcome.metrics.get("clusters_found"), Some(&0.0));
    }

    #[tokio::test]
    async fn test_clustering_no_namespace() {
        let worker = ClusteringWorker::new();
        let ctx = LearningContext::new();
        let outcome = worker.run(&ctx).await;
        assert!(outcome.success);
    }
}
