---
id: d7c2ad7a-8750-4e7c-a71d-89ee574369a9
kind: changelog
title: "@ feat(orchestrator): modalita orchestrator-worker tier-based (default OFF)"
slug: featorchestrator-modalita-orchestrator-worker-tier-based-default-off
tags:
  - changelog
source_commit: e3fa507f68bb76de32cc2e2867ae6143cad85c08
source_files:
  - brain/agents/graph.py
  - brain/agents/nodes.py
  - brain/agents/orchestrator_config.py
  - brain/agents/state.py
  - brain/agents/subagent_dispatch_node.py
  - brain/grpc_server/main.py
  - crates/mcp-core/src/internal_routing.rs
  - crates/mcp-core/src/nexus_routing.rs
  - crates/mcp-core/src/orchestrator.rs
  - crates/mcp-core/src/routing_matrix.rs
  - db/migrations/0203_purpose_model_tier.sql
  - db/migrations/0204_orchestrator_worker_prompts.sql
  - db/migrations/0205_orchestrator_adaptive_settings.sql
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-05-30-feat-pipeline-allegati-robusta-rag-routingdlp-fix-definitivi.md
  - docs/.nexus-vault/concepts/auto-fix-workflow.md
  - docs/.nexus-vault/concepts/change-drafter.md
  - docs/.nexus-vault/concepts/glossario.md
  - docs/.nexus-vault/concepts/isolamento-progetti.md
  - docs/.nexus-vault/concepts/knowledge-base-funzionamento.md
  - docs/.nexus-vault/concepts/meta-vault-architettura.md
  - docs/.nexus-vault/concepts/multi-provider-routing.md
  - docs/.nexus-vault/concepts/nexus-architetturale.md
  - docs/.nexus-vault/concepts/nexus-funzionale.md
  - docs/.nexus-vault/concepts/pattern-learning-worker.md
  - docs/.nexus-vault/concepts/pattern-mcp-tool.md
  - docs/.nexus-vault/concepts/routing-matrix.md
  - docs/.nexus-vault/concepts/sub-agents-claude-code.md
  - docs/.nexus-vault/schema/migrations-log.md
  - docs/.nexus-vault/schema/postgres-tables.md
  - docs/.nexus-vault/schema/qdrant-collections.md
auto_generated: true
created_at: 2026-05-30T07:40:09Z
updated_at: 2026-05-30T07:40:07Z
nexus_meta_version: 1
---

# @ feat(orchestrator): modalita orchestrator-worker tier-based (default OFF)

**Commit**: `e3fa507f68bb76de32cc2e2867ae6143cad85c08` (2026-05-30 07:40 UTC)

**Significance**: 0.80

## File toccati

- `brain/agents/graph.py`
- `brain/agents/nodes.py`
- `brain/agents/orchestrator_config.py`
- `brain/agents/state.py`
- `brain/agents/subagent_dispatch_node.py`
- `brain/grpc_server/main.py`
- `crates/mcp-core/src/internal_routing.rs`
- `crates/mcp-core/src/nexus_routing.rs`
- `crates/mcp-core/src/orchestrator.rs`
- `crates/mcp-core/src/routing_matrix.rs`
- `db/migrations/0203_purpose_model_tier.sql`
- `db/migrations/0204_orchestrator_worker_prompts.sql`
- `db/migrations/0205_orchestrator_adaptive_settings.sql`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-05-30-feat-pipeline-allegati-robusta-rag-routingdlp-fix-definitivi.md`
- `docs/.nexus-vault/concepts/auto-fix-workflow.md`
- `docs/.nexus-vault/concepts/change-drafter.md`
- `docs/.nexus-vault/concepts/glossario.md`
- `docs/.nexus-vault/concepts/isolamento-progetti.md`
- `docs/.nexus-vault/concepts/knowledge-base-funzionamento.md`
- `docs/.nexus-vault/concepts/meta-vault-architettura.md`
- `docs/.nexus-vault/concepts/multi-provider-routing.md`
- `docs/.nexus-vault/concepts/nexus-architetturale.md`
- `docs/.nexus-vault/concepts/nexus-funzionale.md`
- `docs/.nexus-vault/concepts/pattern-learning-worker.md`
- `docs/.nexus-vault/concepts/pattern-mcp-tool.md`
- `docs/.nexus-vault/concepts/routing-matrix.md`
- `docs/.nexus-vault/concepts/sub-agents-claude-code.md`
- `docs/.nexus-vault/schema/migrations-log.md`
- `docs/.nexus-vault/schema/postgres-tables.md`
- `docs/.nexus-vault/schema/qdrant-collections.md`

## Cosa cambia

@ feat(orchestrator): modalita orchestrator-worker tier-based (default OFF)

## Riferimenti

- Vedi diff git: `git show e3fa507f68bb76de32cc2e2867ae6143cad85c08`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
