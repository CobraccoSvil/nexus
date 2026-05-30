---
id: 9f9ff568-e42b-4d8c-98a3-318cd01c7358
kind: other
title: Pattern LearningWorker (scheduler async)
slug: pattern-learning-worker
tags:
  - concept
  - pattern
  - rust
  - async
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:00Z
updated_at: 2026-05-30T06:47:36Z
nexus_meta_version: 1
---

# Pattern LearningWorker

Worker scheduler pattern usato in `crates/nexus-orchestrator/src/learning_loop.rs`.

## Trait

```rust
#[async_trait]
trait LearningWorker {
    fn name(&self) -> &'static str;
    fn trigger(&self) -> WorkerTrigger; // OnTaskComplete | Periodic | Both
    fn interval(&self) -> Duration;
    async fn run(&self, ctx: &LearningContext) -> Result<()>;
    fn enabled(&self) -> bool { true }
}
```

## Worker registrati

- **Reactive** (OnTaskComplete): `UltralearnWorker`, `AuditWorker`, `MetricsAggregationWorker`, `VersioningWorker`
- **Periodic**: `ProfilingWorker`, `AnomalyDetectionWorker`, `MemoryConsolidationWorker`, `CleanupWorker`, `SessionPersistenceWorker`, `QLearningReplayWorker`, `ReplicationWorker`, `ClusteringWorker`
- **Meta-vault**: `MetaDocsRefreshWorker`, `NexusAutoFixWorker`

## Aggiungere un nuovo worker

1. File `crates/nexus-orchestrator/src/workers/my_worker.rs`
2. Impl `LearningWorker`
3. Aggiungere a `workers/mod.rs`
4. Registrare in `nexus_bridge.rs` (`scheduler.register(...)`)

Vedi [[crates-rust]].
