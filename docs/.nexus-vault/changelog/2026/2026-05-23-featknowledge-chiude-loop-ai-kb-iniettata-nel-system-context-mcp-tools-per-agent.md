---
id: cbfc4a13-468e-4aea-b899-dc2520c03367
kind: changelog
title: "feat(knowledge): chiude loop AI - KB iniettata nel system_context, MCP tools per agenti, RAG brain Python"
slug: featknowledge-chiude-loop-ai-kb-iniettata-nel-system-context-mcp-tools-per-agent
tags:
  - changelog
source_commit: 3d8bab3a5bf856c05d0f7967dd6ccee6ced0c5b0
source_files:
  - brain/agents/nodes.py
  - crates/mcp-core/src/agent_tools/knowledge.rs
  - crates/mcp-core/src/agent_tools/mod.rs
  - crates/mcp-core/src/chat_messages.rs
  - crates/mcp-core/src/knowledge/routes.rs
  - crates/mcp-core/src/main.rs
  - db/migrations/0179_kb_context_injection.sql
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-fixknowledge-usa-dialog-nexus-useglobaldialog-invece-di-windowalertconfirmprompt.md
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
auto_generated: true
created_at: 2026-05-23T13:27:08Z
updated_at: 2026-05-23T13:27:05Z
nexus_meta_version: 1
---

# feat(knowledge): chiude loop AI - KB iniettata nel system_context, MCP tools per agenti, RAG brain Python

**Commit**: `3d8bab3a5bf856c05d0f7967dd6ccee6ced0c5b0` (2026-05-23 13:27 UTC)

**Significance**: 0.95

## File toccati

- `brain/agents/nodes.py`
- `crates/mcp-core/src/agent_tools/knowledge.rs`
- `crates/mcp-core/src/agent_tools/mod.rs`
- `crates/mcp-core/src/chat_messages.rs`
- `crates/mcp-core/src/knowledge/routes.rs`
- `crates/mcp-core/src/main.rs`
- `db/migrations/0179_kb_context_injection.sql`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-fixknowledge-usa-dialog-nexus-useglobaldialog-invece-di-windowalertconfirmprompt.md`
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

## Cosa cambia

feat(knowledge): chiude loop AI - KB iniettata nel system_context, MCP tools per agenti, RAG brain Python

## Riferimenti

- Vedi diff git: `git show 3d8bab3a5bf856c05d0f7967dd6ccee6ced0c5b0`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
