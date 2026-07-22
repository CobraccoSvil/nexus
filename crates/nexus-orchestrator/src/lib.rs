//! Nexus Orchestrator - Q-Learning router, learning loop, worker reattivi
//!
//! Implementa l'orchestrazione multi-agente per Nexus:
//! - Q-Learning router per selezione intelligente di agenti
//! - HNSW-based similarity search (via RuVector)
//! - Epsilon-greedy exploration/exploitation
//! - Reward-based continuous learning
//! - Memory namespace per shared context
//! - Consensus engine quorum-based
//! - Background learning workers per continuous improvement
//!
//! Nota: i tipi `AgentType`, `Task*`, `prompt_registry` sono stati assorbiti
//! dal crate `nexus-agents` durante la fase 5e del refactor (opzione B).
//! Il trait `Agent` e tutta l'infrastruttura di esecuzione Rust sono stati
//! rimossi: l'esecuzione vive nel brain LangGraph.
//! `SwarmCoordinator` rimosso nella fase 5g: vedi `swarm_types` per i tipi
//! di dato mantenuti (`SwarmExecutionResult`, `SwarmTaskOutcome`).

pub mod agent_types;
pub mod consensus;
pub mod embedder;
pub mod learning_loop;
pub mod namespace;
pub mod prompt_registry;
pub mod q_learning;
pub mod swarm_types;
pub mod task;
pub mod types;
pub mod workers;

// Re-exports principali
pub use agent_types::AgentType;
pub use consensus::{
    AggregatedResult, ConsensusEngine, ConsensusResult, ConsensusStrategy, Vote,
};
pub use embedder::{
    CachedEmbedder, Embedder, HashEmbedder,
    OnnxMiniLmEmbedder, MINILM_DIM,
    DEFAULT_MODEL_PATH, DEFAULT_TOKENIZER_PATH,
};
pub use learning_loop::{
    LearningContext, LearningScheduler, LearningWorker, SchedulerStats, WorkerOutcome,
    WorkerStats, WorkerTrigger,
};
pub use namespace::{MemoryNamespace, NamespaceEntry, NamespaceEvent};
pub use q_learning::QLearningRouter;
pub use swarm_types::{SwarmExecutionResult, SwarmId, SwarmTaskOutcome};
pub use task::{
    AgentMetadata, Feedback, Task, TaskBuilder, TaskConstraints, TaskContext, TaskResult,
};
pub use types::{
    CandidateAgent, ExecutionOutcome, QKey, QLearningConfig, QValue, RouterStats,
    RoutingDecision, SelectionStrategy,
};
pub use workers::{
    AnomalyDetectionWorker, CleanupWorker, ClusteringWorker,
    GuidelineAlignmentWorker, MemoryConsolidationWorker, MetricsAggregationWorker,
    ProfilingWorker, PromptOptimizerWorker, QLearningReplayWorker, ReplicationBatch,
    ReplicationEntry, ReplicationWorker, SessionPersistenceWorker,
    UltralearnWorker, VersioningWorker,
};
