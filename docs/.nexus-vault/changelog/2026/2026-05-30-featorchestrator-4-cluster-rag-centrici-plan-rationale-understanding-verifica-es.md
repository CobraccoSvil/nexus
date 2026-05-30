---
id: 7d81ea3b-e61d-48b3-801b-fb854fa7f198
kind: changelog
title: "feat(orchestrator): 4 cluster RAG-centrici (plan_rationale, understanding, verifica esplorativa, clarify tecnico/prodotto)"
slug: featorchestrator-4-cluster-rag-centrici-plan-rationale-understanding-verifica-es
tags:
  - changelog
source_commit: 45579225d859f612de2a1d38832a9e62dc069aa3
source_files:
  - brain/agents/clarify_or_expand_node.py
  - brain/agents/graph.py
  - brain/agents/nodes.py
  - brain/agents/orchestrator_config.py
  - brain/agents/planner_node.py
  - brain/agents/state.py
  - brain/agents/understanding_node.py
  - brain/agents/verifier_node.py
  - crates/mcp-core/src/agent_tools/todos.rs
  - crates/mcp-core/src/rag/config.rs
  - crates/mcp-core/src/rag/mod.rs
  - crates/mcp-core/src/rag/search.rs
  - db/migrations/0206_plan_rationale.sql
  - db/migrations/0207_understanding_node.sql
  - db/migrations/0208_exploratory_verify.sql
  - db/migrations/0209_clarify_decision_rag.sql
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-05-30-featorchestrator-modalita-orchestrator-worker-tier-based-default-off.md
  - docs/.nexus-vault/changelog/2026/2026-05-30-fixbrain-elimina-loop-esplorazione-allegati-su-scaffolding-app.md
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
created_at: 2026-05-30T08:57:00Z
updated_at: 2026-05-30T08:56:59Z
nexus_meta_version: 1
---

# feat(orchestrator): 4 cluster RAG-centrici (plan_rationale, understanding, verifica esplorativa, clarify tecnico/prodotto)

**Commit**: `45579225d859f612de2a1d38832a9e62dc069aa3` (2026-05-30 08:56 UTC)

**Significance**: 0.95

## File toccati

- `brain/agents/clarify_or_expand_node.py`
- `brain/agents/graph.py`
- `brain/agents/nodes.py`
- `brain/agents/orchestrator_config.py`
- `brain/agents/planner_node.py`
- `brain/agents/state.py`
- `brain/agents/understanding_node.py`
- `brain/agents/verifier_node.py`
- `crates/mcp-core/src/agent_tools/todos.rs`
- `crates/mcp-core/src/rag/config.rs`
- `crates/mcp-core/src/rag/mod.rs`
- `crates/mcp-core/src/rag/search.rs`
- `db/migrations/0206_plan_rationale.sql`
- `db/migrations/0207_understanding_node.sql`
- `db/migrations/0208_exploratory_verify.sql`
- `db/migrations/0209_clarify_decision_rag.sql`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-05-30-featorchestrator-modalita-orchestrator-worker-tier-based-default-off.md`
- `docs/.nexus-vault/changelog/2026/2026-05-30-fixbrain-elimina-loop-esplorazione-allegati-su-scaffolding-app.md`
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

feat(orchestrator): 4 cluster RAG-centrici (plan_rationale, understanding, verifica esplorativa, clarify tecnico/prodotto)

## Riferimenti

- Vedi diff git: `git show 45579225d859f612de2a1d38832a9e62dc069aa3`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
