//! Q-Learning Router per selezione intelligente di agenti
//!
//! Implementa un sistema di routing basato su reinforcement learning:
//! - HNSW similarity per trovare k agenti candidati
//! - Q-values per valutare performance storica di ogni (task_type, agent)
//! - Epsilon-greedy per bilanciare exploration/exploitation
//!
//! ## Persistence
//!
//! Se viene fornito un `PgPool` via `with_pool()`, il router:
//! - **Al primo avvio** carica tutti i Q-values da `nexus_q_values` in memoria
//! - **Ad ogni update** persiste asincronamente (fire-and-forget) il Q-value
//!   aggiornato su PostgreSQL senza bloccare il chiamante (< 0.1ms overhead)

use crate::embedder::Embedder;
use crate::types::*;
use dashmap::DashMap;
use crate::agent_types::AgentType;
use crate::task::Task;
use parking_lot::{Mutex, RwLock};
use rand::prelude::*;
use ruvector::{HnswConfig, HnswDb};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Q-Learning Router
pub struct QLearningRouter {
    /// Configurazione
    config: QLearningConfig,

    /// Q-table: mapping (task_type, agent_type) -> QValue
    /// Usiamo DashMap per concurrent access senza lock globale
    q_table: Arc<DashMap<QKey, QValue>>,

    /// HNSW database per similarity search tra agenti
    agent_hnsw: Arc<HnswDb>,

    /// Text embedder (task descriptions → vettori)
    embedder: Arc<dyn Embedder>,

    /// Registry degli agenti disponibili (agent_type → agent_id in HNSW)
    agent_registry: Arc<RwLock<Vec<AgentType>>>,

    /// RNG per epsilon-greedy exploration
    rng: Arc<Mutex<StdRng>>,

    /// Statistiche router
    stats: Arc<RwLock<RouterStats>>,

    /// Pool PostgreSQL per persistenza Q-values (opzionale)
    /// Se None, il router opera solo in memoria (utile per test)
    pool: Option<Arc<PgPool>>,
}

impl QLearningRouter {
    /// Crea un nuovo router (senza persistenza DB)
    pub fn new(config: QLearningConfig, embedder: Arc<dyn Embedder>) -> Self {
        let hnsw_config = HnswConfig::default();
        let agent_hnsw = Arc::new(HnswDb::new(hnsw_config));

        let mut stats = RouterStats::default();
        stats.current_epsilon = config.epsilon;

        Self {
            config,
            q_table: Arc::new(DashMap::new()),
            agent_hnsw,
            embedder,
            agent_registry: Arc::new(RwLock::new(Vec::new())),
            rng: Arc::new(Mutex::new(StdRng::seed_from_u64(42))),
            stats: Arc::new(RwLock::new(stats)),
            pool: None,
        }
    }

    /// Configura il pool PostgreSQL per persistenza Q-values.
    ///
    /// Da chiamare prima di `load_from_db()`. Può essere usato in builder chain:
    /// ```ignore
    /// let router = QLearningRouter::new(config, embedder).with_pool(pool);
    /// ```
    pub fn with_pool(mut self, pool: Arc<PgPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Carica tutti i Q-values da PostgreSQL in memoria.
    ///
    /// Da chiamare all'avvio del servizio, dopo `with_pool()`.
    /// È idempotente: carichi multipli sovrascrivono quelli precedenti.
    ///
    /// # Errori
    /// Propaga errori DB. Se il DB non è disponibile, il router parte
    /// con Q-table vuota (cold start) senza crashare.
    pub async fn load_from_db(&self) -> anyhow::Result<usize> {
        let pool = match &self.pool {
            Some(p) => p.clone(),
            None => return Ok(0),
        };

        // Usa query builder runtime (non macro) per evitare DATABASE_URL
        // al compile time (sqlx offline mode).
        let rows = sqlx::query(
            r#"
            SELECT task_type, agent_key, q_value, visit_count, last_reward
            FROM nexus_q_values
            "#
        )
        .fetch_all(pool.as_ref())
        .await?;

        let count = rows.len();
        for row in rows {
            use sqlx::Row;
            let key = QKey {
                task_type: row.try_get::<String, _>("task_type")?,
                agent_type: row.try_get::<String, _>("agent_key")?,
            };
            let value = QValue {
                value: row.try_get::<f32, _>("q_value")?,
                visit_count: row.try_get::<i64, _>("visit_count")? as u32,
                last_reward: row.try_get::<Option<f32>, _>("last_reward")?.unwrap_or(0.0),
                updated_at: chrono::Utc::now(),
            };
            self.q_table.insert(key, value);
        }

        info!("Q-Learning: caricati {} Q-values da PostgreSQL", count);
        Ok(count)
    }

    /// Persiste un singolo Q-value su PostgreSQL (UPSERT).
    /// Chiamato in modo fire-and-forget da `update_q_value`.
    /// Usa query builder runtime (non macro) per evitare DATABASE_URL al compile time.
    async fn persist_q_value(
        pool: Arc<PgPool>,
        task_type: String,
        agent_key: String,
        q_value: f32,
        visit_count: u32,
        success: bool,
        reward: f32,
    ) {
        let success_delta: i64 = if success { 1 } else { 0 };
        let failure_delta: i64 = if success { 0 } else { 1 };
        let result = sqlx::query(
            r#"
            INSERT INTO nexus_q_values
                (task_type, agent_key, q_value, visit_count,
                 success_count, failure_count, last_reward, avg_reward, updated_at)
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, $7, NOW())
            ON CONFLICT (task_type, agent_key) DO UPDATE
            SET
                q_value       = $3,
                visit_count   = nexus_q_values.visit_count + 1,
                success_count = nexus_q_values.success_count + $5,
                failure_count = nexus_q_values.failure_count + $6,
                last_reward   = $7,
                avg_reward    = (nexus_q_values.avg_reward * nexus_q_values.visit_count + $7)
                                / (nexus_q_values.visit_count + 1),
                updated_at    = NOW()
            "#
        )
        .bind(task_type)
        .bind(agent_key)
        .bind(q_value)
        .bind(visit_count as i64)
        .bind(success_delta)
        .bind(failure_delta)
        .bind(reward)
        .execute(pool.as_ref())
        .await;

        if let Err(e) = result {
            // Non-fatal: la Q-table in memoria è già aggiornata
            debug!("Q-value persist failed (non-fatal): {e}");
        }
    }

    /// Registra un agente nel router
    /// Crea un "profilo" dell'agente embeddando la sua descrizione
    pub fn register_agent(
        &self,
        agent_type: AgentType,
        description: &str,
    ) -> Result<(), String> {
        let embedding = self.embedder.embed(description);

        // Inserisci nell'HNSW per similarity search
        let agent_name = agent_type.name().to_string();
        self.agent_hnsw
            .insert(agent_name.clone(), embedding, None)
            .map_err(|e| format!("HNSW insert failed: {:?}", e))?;

        // Registra nel registry
        let mut registry = self.agent_registry.write();
        if !registry.contains(&agent_type) {
            registry.push(agent_type.clone());
            debug!("Registered agent: {} (total: {})", agent_name, registry.len());
        }

        Ok(())
    }

    /// Registra un batch di agenti
    pub fn register_agents(&self, agents: Vec<(AgentType, String)>) -> Result<(), String> {
        for (agent_type, description) in agents {
            self.register_agent(agent_type, &description)?;
        }
        info!(
            "Q-Learning router: {} agents registered",
            self.agent_registry.read().len()
        );
        Ok(())
    }

    /// Seleziona l'agente migliore per un task
    /// Questo è il metodo principale del router
    pub fn select_agent(&self, task: &Task) -> Result<RoutingDecision, String> {
        let start = Instant::now();

        let registry = self.agent_registry.read();
        if registry.is_empty() {
            return Err("No agents registered in router".to_string());
        }

        // 1. Embed task description
        let task_embedding = self.embedder.embed(&task.instructions);

        // 2. HNSW similarity search per trovare k candidati
        let k = self.config.k_candidates.min(registry.len());
        let similar = self
            .agent_hnsw
            .search(&task_embedding, k)
            .map_err(|e| format!("HNSW search failed: {:?}", e))?;

        if similar.is_empty() {
            // Cold start: non abbiamo ancora agenti registrati nell'HNSW
            return self.cold_start_selection(task, &registry, start);
        }

        // 3. Costruisci lista candidati con Q-values
        let mut candidates: Vec<CandidateAgent> = similar
            .iter()
            .filter_map(|result| {
                // Trova AgentType dal nome (stored come id nel HNSW)
                // Nota: la similitudine score è score (1/(1+dist))
                let agent_type = registry
                    .iter()
                    .find(|a| a.name() == result.id.as_str())
                    .cloned()?;

                let q_key = QKey::from_agent(&task.task_type, &agent_type);
                let q_value = self
                    .q_table
                    .get(&q_key)
                    .map(|v| v.value)
                    .unwrap_or(self.config.initial_q_value);

                Some(CandidateAgent {
                    agent_type,
                    similarity_score: result.score,
                    q_value,
                })
            })
            .collect();

        if candidates.is_empty() {
            return self.cold_start_selection(task, &registry, start);
        }

        // 4. Epsilon-greedy: decide exploration vs exploitation
        let should_explore = {
            let mut rng = self.rng.lock();
            rng.gen::<f32>() < self.stats.read().current_epsilon
        };

        let (selected, strategy) = if should_explore && candidates.len() > 1 {
            // Exploration: scegli random tra i candidati
            let mut rng = self.rng.lock();
            let idx = rng.gen_range(0..candidates.len());
            (candidates[idx].clone(), SelectionStrategy::Exploration)
        } else {
            // Exploitation: combina similarity + Q-value
            // Score combinato: 0.3 * similarity + 0.7 * q_value (normalizzato)
            candidates.sort_by(|a, b| {
                let score_a = 0.3 * a.similarity_score + 0.7 * a.q_value.max(0.0);
                let score_b = 0.3 * b.similarity_score + 0.7 * b.q_value.max(0.0);
                score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
            });
            (candidates[0].clone(), SelectionStrategy::Exploitation)
        };

        // 5. Calcola confidence (distanza dal secondo miglior candidato)
        let confidence = if candidates.len() >= 2 {
            let best_score = 0.3 * selected.similarity_score + 0.7 * selected.q_value.max(0.0);
            let second = &candidates[1];
            let second_score = 0.3 * second.similarity_score + 0.7 * second.q_value.max(0.0);
            ((best_score - second_score).max(0.0) + 0.5).clamp(0.0, 1.0)
        } else {
            0.5
        };

        let elapsed_us = start.elapsed().as_micros() as u64;

        // 6. Update stats
        self.update_stats(&strategy, elapsed_us);

        Ok(RoutingDecision {
            agent_type: selected.agent_type,
            q_value: selected.q_value,
            confidence,
            candidates,
            decision_time_us: elapsed_us,
            strategy,
        })
    }

    /// Cold start: seleziona primo agente disponibile
    fn cold_start_selection(
        &self,
        _task: &Task,
        registry: &[AgentType],
        start: Instant,
    ) -> Result<RoutingDecision, String> {
        let mut rng = self.rng.lock();
        let idx = rng.gen_range(0..registry.len());
        let agent_type = registry[idx].clone();
        drop(rng);

        let elapsed_us = start.elapsed().as_micros() as u64;

        self.update_stats(&SelectionStrategy::ColdStart, elapsed_us);

        Ok(RoutingDecision {
            agent_type: agent_type.clone(),
            q_value: self.config.initial_q_value,
            confidence: 0.1,
            candidates: vec![CandidateAgent {
                agent_type,
                similarity_score: 0.0,
                q_value: self.config.initial_q_value,
            }],
            decision_time_us: elapsed_us,
            strategy: SelectionStrategy::ColdStart,
        })
    }

    /// Update Q-value dopo esecuzione task
    /// Formula: Q(s,a) ← Q(s,a) + α·[r + γ·max(Q(s',a')) - Q(s,a)]
    ///
    /// Aggiorna la Q-table in memoria (sincrono, < 0.1ms) e, se disponibile
    /// un pool PostgreSQL, avvia una persist task asincrona (fire-and-forget).
    pub fn update_q_value(&self, outcome: &ExecutionOutcome) -> f32 {
        let q_key = QKey::from_agent(&outcome.task_type, &outcome.agent_type);
        let reward = outcome.compute_reward();

        // Get max Q-value per lo stesso task_type (per bootstrapping)
        let max_next_q = self.get_max_q_for_task(&outcome.task_type);

        // Update formula (single-step Q-Learning)
        let (new_q, visit_count) = {
            let mut entry = self.q_table.entry(q_key.clone()).or_default();
            let current_q = entry.value;

            let td_target = reward + self.config.discount_factor * max_next_q;
            let td_error = td_target - current_q;
            let updated = current_q + self.config.learning_rate * td_error;

            entry.value = updated;
            entry.visit_count += 1;
            entry.last_reward = reward;
            entry.updated_at = chrono::Utc::now();

            (updated, entry.visit_count)
        };

        // Aggiorna statistiche
        let mut stats = self.stats.write();
        stats.total_rewards += reward;

        // Epsilon decay (ogni update riduce epsilon)
        stats.current_epsilon = (stats.current_epsilon * self.config.epsilon_decay)
            .max(self.config.min_epsilon);

        debug!(
            "Q-update: task_type={}, agent={}, reward={:.3}, new_q={:.3}",
            outcome.task_type,
            outcome.agent_type.name(),
            reward,
            new_q
        );

        // Persistenza asincrona (fire-and-forget) — non blocca il chiamante
        if let Some(pool) = &self.pool {
            let pool = pool.clone();
            let task_type = outcome.task_type.clone();
            let agent_key = outcome.agent_type.name().to_string();
            let success = outcome.success;
            tokio::spawn(Self::persist_q_value(
                pool,
                task_type,
                agent_key,
                new_q,
                visit_count,
                success,
                reward,
            ));
        }

        new_q
    }

    /// Trova il Q-value massimo per un task_type tra tutti gli agenti
    fn get_max_q_for_task(&self, task_type: &str) -> f32 {
        let mut max_q = 0.0_f32;
        for entry in self.q_table.iter() {
            if entry.key().task_type == task_type {
                max_q = max_q.max(entry.value().value);
            }
        }
        max_q
    }

    /// Ottieni Q-value corrente
    pub fn get_q_value(&self, task_type: &str, agent_type: &AgentType) -> Option<QValue> {
        let key = QKey::from_agent(task_type, agent_type);
        self.q_table.get(&key).map(|v| v.value().clone())
    }

    /// Ottiene tutte le Q-values per un task_type (debug/introspection)
    pub fn get_q_values_for_task(&self, task_type: &str) -> Vec<(String, QValue)> {
        self.q_table
            .iter()
            .filter(|e| e.key().task_type == task_type)
            .map(|e| (e.key().agent_type.clone(), e.value().clone()))
            .collect()
    }

    /// Reset Q-table (utile per testing)
    pub fn reset(&self) {
        self.q_table.clear();
        let mut stats = self.stats.write();
        *stats = RouterStats::default();
        stats.current_epsilon = self.config.epsilon;
        warn!("Q-Learning router reset");
    }

    /// Update statistiche
    fn update_stats(&self, strategy: &SelectionStrategy, decision_time_us: u64) {
        let mut stats = self.stats.write();
        stats.total_decisions += 1;
        match strategy {
            SelectionStrategy::Exploitation => stats.exploitation_count += 1,
            SelectionStrategy::Exploration => stats.exploration_count += 1,
            SelectionStrategy::ColdStart => stats.cold_start_count += 1,
            SelectionStrategy::Forced => stats.forced_count += 1,
        }

        // Running average of decision time
        let n = stats.total_decisions as f64;
        stats.avg_decision_time_us =
            (stats.avg_decision_time_us * (n - 1.0) + decision_time_us as f64) / n;
    }

    /// Ottieni statistiche correnti
    pub fn stats(&self) -> RouterStats {
        self.stats.read().clone()
    }

    /// Numero di entries nella Q-table
    pub fn q_table_size(&self) -> usize {
        self.q_table.len()
    }

    /// Miglior Q-value per un agent_type specifico tra tutti i task type.
    /// Utile per dashboard e monitoring per ottenere una visione sintetica dell'agente.
    pub fn get_best_q_value_for_agent(&self, agent_type: &str) -> Option<f32> {
        self.q_table
            .iter()
            .filter(|e| e.key().agent_type == agent_type)
            .map(|e| e.value().value)
            .reduce(f32::max)
    }

    /// Numero di agenti registrati
    pub fn num_agents(&self) -> usize {
        self.agent_registry.read().len()
    }

    /// Persiste **tutti** i Q-values in memoria su PostgreSQL in modo sincrono.
    ///
    /// Chiamato durante graceful shutdown per garantire zero perdita dati:
    /// i `tokio::spawn` fire-and-forget precedenti potrebbero non essere completati
    /// prima del termine del processo. Questo metodo li completa tutti.
    ///
    /// Usa UPSERT (ON CONFLICT DO UPDATE) per ogni entry — idempotente,
    /// può essere chiamato più volte senza duplicati.
    ///
    /// Ritorna il numero di Q-values persistiti, o 0 se nessun pool configurato.
    pub async fn flush_all_to_db(&self) -> Result<usize, sqlx::Error> {
        let pool = match &self.pool {
            Some(p) => p.clone(),
            None => return Ok(0),
        };

        // Snapshot immediato della Q-table (non blocca altri thread)
        let entries: Vec<(QKey, QValue)> = self
            .q_table
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();

        let count = entries.len();
        for (key, val) in &entries {
            let success_delta: i64 = if val.last_reward > 0.0 { 1 } else { 0 };
            let failure_delta: i64 = if val.last_reward <= 0.0 { 1 } else { 0 };
            sqlx::query(
                r#"
                INSERT INTO nexus_q_values
                    (task_type, agent_key, q_value, visit_count,
                     success_count, failure_count, last_reward, avg_reward, updated_at)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7, $7, NOW())
                ON CONFLICT (task_type, agent_key) DO UPDATE
                SET
                    q_value       = EXCLUDED.q_value,
                    visit_count   = EXCLUDED.visit_count,
                    success_count = nexus_q_values.success_count + EXCLUDED.success_count,
                    failure_count = nexus_q_values.failure_count + EXCLUDED.failure_count,
                    last_reward   = EXCLUDED.last_reward,
                    avg_reward    = (nexus_q_values.avg_reward + EXCLUDED.last_reward) / 2.0,
                    updated_at    = NOW()
                "#,
            )
            .bind(&key.task_type)
            .bind(&key.agent_type)
            .bind(val.value)
            .bind(val.visit_count as i64)
            .bind(success_delta)
            .bind(failure_delta)
            .bind(val.last_reward)
            .execute(pool.as_ref())
            .await?;
        }

        info!("Q-Learning flush_all_to_db: {} Q-values persistiti su PostgreSQL", count);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedder::HashEmbedder;
    use crate::task::TaskBuilder;

    fn create_test_router() -> QLearningRouter {
        let embedder = Arc::new(HashEmbedder::new(128));
        let config = QLearningConfig {
            epsilon: 0.0, // Disable exploration per test deterministici
            ..Default::default()
        };
        let router = QLearningRouter::new(config, embedder);

        router
            .register_agents(vec![
                (
                    AgentType::Coder,
                    "Writes code, implements features, creates functions".to_string(),
                ),
                (
                    AgentType::Tester,
                    "Writes tests, test coverage, quality assurance".to_string(),
                ),
                (
                    AgentType::Reviewer,
                    "Reviews code, finds bugs, suggests improvements".to_string(),
                ),
                (
                    AgentType::Architect,
                    "Designs systems, architecture, technical decisions".to_string(),
                ),
            ])
            .expect("Failed to register agents");

        router
    }

    #[test]
    fn test_router_agent_registration() {
        let router = create_test_router();
        assert_eq!(router.num_agents(), 4);
    }

    #[test]
    fn test_router_selects_agent() {
        let router = create_test_router();

        let task = TaskBuilder::new(
            "code_review".to_string(),
            "review this code for potential bugs and issues".to_string(),
            "project1".to_string(),
        )
        .build();

        let decision = router.select_agent(&task).expect("Failed to select agent");

        // Should return a valid decision
        assert!(decision.decision_time_us > 0);
        assert!(!decision.candidates.is_empty());
    }

    #[test]
    fn test_q_value_update() {
        let router = create_test_router();

        let outcome = ExecutionOutcome {
            task_id: "task1".to_string(),
            task_type: "code_review".to_string(),
            agent_type: AgentType::Reviewer,
            success: true,
            quality_score: 0.9,
            execution_time_ms: 500,
            error: None,
        };

        let new_q = router.update_q_value(&outcome);
        assert!(new_q > 0.0, "Q-value dovrebbe essere positivo after success");

        // Verifica che la Q-table ora contiene l'entry
        let q = router.get_q_value("code_review", &AgentType::Reviewer);
        assert!(q.is_some());
        assert_eq!(q.unwrap().visit_count, 1);
    }

    #[test]
    fn test_reward_computation() {
        // Success case
        let outcome = ExecutionOutcome {
            task_id: "t1".to_string(),
            task_type: "test".to_string(),
            agent_type: AgentType::Coder,
            success: true,
            quality_score: 1.0,
            execution_time_ms: 100,
            error: None,
        };
        assert!(outcome.compute_reward() > 1.0);

        // Failure case
        let outcome_fail = ExecutionOutcome {
            success: false,
            quality_score: 0.0,
            ..outcome.clone()
        };
        assert!(outcome_fail.compute_reward() < 0.0);
    }

    #[test]
    fn test_learning_improves_selection() {
        let router = create_test_router();

        // Simula ripetute esecuzioni success di Reviewer su code_review
        for _ in 0..10 {
            let outcome = ExecutionOutcome {
                task_id: "t".to_string(),
                task_type: "code_review".to_string(),
                agent_type: AgentType::Reviewer,
                success: true,
                quality_score: 1.0,
                execution_time_ms: 200,
                error: None,
            };
            router.update_q_value(&outcome);
        }

        // Q-value di Reviewer per code_review dovrebbe essere alto
        let q = router.get_q_value("code_review", &AgentType::Reviewer).unwrap();
        assert!(
            q.value > 0.5,
            "Q-value dovrebbe essere alto dopo success ripetuti, got {}",
            q.value
        );
        assert_eq!(q.visit_count, 10);
    }

    #[test]
    fn test_decision_performance() {
        let router = create_test_router();
        let task = TaskBuilder::new(
            "generic_task".to_string(),
            "do something useful".to_string(),
            "p1".to_string(),
        )
        .build();

        // Warmup
        for _ in 0..10 {
            router.select_agent(&task).unwrap();
        }

        // Misura tempo medio su 100 decisioni
        let start = Instant::now();
        for _ in 0..100 {
            router.select_agent(&task).unwrap();
        }
        let elapsed = start.elapsed();
        let avg_us = elapsed.as_micros() / 100;

        println!("Tempo medio decisione: {}μs", avg_us);

        // Target: <1000μs = <1ms (rilassato per CI)
        assert!(
            avg_us < 5000,
            "Decision time troppo alto: {}μs (target: <5ms)",
            avg_us
        );
    }
}
