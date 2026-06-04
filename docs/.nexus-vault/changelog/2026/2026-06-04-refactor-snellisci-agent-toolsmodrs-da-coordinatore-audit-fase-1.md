---
id: 1b865f5f-7276-4339-b434-ead8bee6a7c3
kind: changelog
title: "refactor: snellisci agent_tools/mod.rs da coordinatore (audit fase 1)"
slug: refactor-snellisci-agent-toolsmodrs-da-coordinatore-audit-fase-1
tags:
  - changelog
source_commit: 9589dff4464951c4c27a821246c0fdce1d5f7aa3
source_files:
  - crates/mcp-core/src/agent_tools/context.rs
  - crates/mcp-core/src/agent_tools/dispatch.rs
  - crates/mcp-core/src/agent_tools/helpers.rs
  - crates/mcp-core/src/agent_tools/knowledge.rs
  - crates/mcp-core/src/agent_tools/mod.rs
  - crates/mcp-core/src/agent_tools/profile_tools.rs
  - crates/mcp-core/src/agent_tools/quality_tools.rs
  - crates/mcp-core/src/agent_tools/semantic_tools.rs
  - crates/mcp-core/src/agent_tools/testing.rs
  - crates/mcp-core/src/agent_tools/tool_schema.rs
  - db/migrations/0288_playwright_preflight_setting.sql
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-refactor-spezza-ide-shelltsx-in-sotto-componenti-audit-fase-1-frontend.md
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
  - docs/.nexus-vault/concepts/sub-agents-claude-code.md
auto_generated: true
created_at: 2026-06-04T09:08:54Z
updated_at: 2026-06-04T09:08:52Z
nexus_meta_version: 1
---

# refactor: snellisci agent_tools/mod.rs da coordinatore (audit fase 1)

**Commit**: `9589dff4464951c4c27a821246c0fdce1d5f7aa3` (2026-06-04 09:08 UTC)

**Significance**: 0.95

## File toccati

- `crates/mcp-core/src/agent_tools/context.rs`
- `crates/mcp-core/src/agent_tools/dispatch.rs`
- `crates/mcp-core/src/agent_tools/helpers.rs`
- `crates/mcp-core/src/agent_tools/knowledge.rs`
- `crates/mcp-core/src/agent_tools/mod.rs`
- `crates/mcp-core/src/agent_tools/profile_tools.rs`
- `crates/mcp-core/src/agent_tools/quality_tools.rs`
- `crates/mcp-core/src/agent_tools/semantic_tools.rs`
- `crates/mcp-core/src/agent_tools/testing.rs`
- `crates/mcp-core/src/agent_tools/tool_schema.rs`
- `db/migrations/0288_playwright_preflight_setting.sql`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-refactor-spezza-ide-shelltsx-in-sotto-componenti-audit-fase-1-frontend.md`
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
- `docs/.nexus-vault/concepts/sub-agents-claude-code.md`

## Cosa cambia

refactor: snellisci agent_tools/mod.rs da coordinatore (audit fase 1)

## Riferimenti

- Vedi diff git: `git show 9589dff4464951c4c27a821246c0fdce1d5f7aa3`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
