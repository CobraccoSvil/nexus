---
id: 23e9c6d8-df90-4c86-9209-692d88a4eae0
kind: changelog
title: "fix(knowledge): bug INSERT links - rimuovi colonna project_id inesistente + 3 generator KB ricca"
slug: fixknowledge-bug-insert-links-rimuovi-colonna-project-id-inesistente-3-generator
tags:
  - changelog
source_commit: ba9fc87e7ec6685d8fc38dbcf6f88c6202ebe9f1
source_files:
  - crates/mcp-core/src/knowledge/generators.rs
  - crates/mcp-core/src/knowledge/mod.rs
  - crates/mcp-core/src/knowledge/routes.rs
  - crates/mcp-core/src/knowledge_workers.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/vector_memory.rs
  - db/migrations/0180_project_kb_kind.sql
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featknowledge-chiude-loop-ai-kb-iniettata-nel-system-context-mcp-tools-per-agent.md
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
  - tests/debug_dupes.py
  - tests/debug_scores.py
  - tests/debug_search.py
  - tests/debug_search.sh
auto_generated: true
created_at: 2026-05-23T15:32:20Z
updated_at: 2026-05-23T15:32:18Z
nexus_meta_version: 1
---

# fix(knowledge): bug INSERT links - rimuovi colonna project_id inesistente + 3 generator KB ricca

**Commit**: `ba9fc87e7ec6685d8fc38dbcf6f88c6202ebe9f1` (2026-05-23 15:32 UTC)

**Significance**: 0.95

## File toccati

- `crates/mcp-core/src/knowledge/generators.rs`
- `crates/mcp-core/src/knowledge/mod.rs`
- `crates/mcp-core/src/knowledge/routes.rs`
- `crates/mcp-core/src/knowledge_workers.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/vector_memory.rs`
- `db/migrations/0180_project_kb_kind.sql`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featknowledge-chiude-loop-ai-kb-iniettata-nel-system-context-mcp-tools-per-agent.md`
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
- `tests/debug_dupes.py`
- `tests/debug_scores.py`
- `tests/debug_search.py`
- `tests/debug_search.sh`

## Cosa cambia

fix(knowledge): bug INSERT links - rimuovi colonna project_id inesistente + 3 generator KB ricca

## Riferimenti

- Vedi diff git: `git show ba9fc87e7ec6685d8fc38dbcf6f88c6202ebe9f1`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
- [[rest-endpoints]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
