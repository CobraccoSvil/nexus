---
id: ff20a838-e453-41ff-a359-e2d252d5961d
kind: changelog
title: "refactor: spezza source-control-panel.tsx in sotto-componenti (audit fase 1 frontend)"
slug: refactor-spezza-source-control-paneltsx-in-sotto-componenti-audit-fase-1-fronten
tags:
  - changelog
source_commit: 57c95686cfb271293a808dfa51b56c03c31cf7b6
source_files:
  - apps/web-ide/components/git/source-control-panel.tsx
  - apps/web-ide/components/git/source-control/analysis-insights-card.tsx
  - apps/web-ide/components/git/source-control/github-account-card.tsx
  - apps/web-ide/components/git/source-control/github-import-section.tsx
  - apps/web-ide/components/git/source-control/labels.ts
  - apps/web-ide/components/git/source-control/nexus-status-card.tsx
  - apps/web-ide/components/git/source-control/remote-card.tsx
  - apps/web-ide/components/git/source-control/repository-actions-card.tsx
  - apps/web-ide/components/git/source-control/styles.ts
  - brain/providers/_schema_utils.py
  - crates/mcp-core/src/agent_types.rs
  - crates/mcp-core/src/brain_agent_client.rs
  - db/migrations/0291_provider_empty_responses.sql
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-refactor-spezza-plugin-managertsx-in-sotto-componenti-audit-fase-1-frontend.md
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
created_at: 2026-06-04T10:20:49Z
updated_at: 2026-06-04T10:20:48Z
nexus_meta_version: 1
---

# refactor: spezza source-control-panel.tsx in sotto-componenti (audit fase 1 frontend)

**Commit**: `57c95686cfb271293a808dfa51b56c03c31cf7b6` (2026-06-04 10:20 UTC)

**Significance**: 0.95

## File toccati

- `apps/web-ide/components/git/source-control-panel.tsx`
- `apps/web-ide/components/git/source-control/analysis-insights-card.tsx`
- `apps/web-ide/components/git/source-control/github-account-card.tsx`
- `apps/web-ide/components/git/source-control/github-import-section.tsx`
- `apps/web-ide/components/git/source-control/labels.ts`
- `apps/web-ide/components/git/source-control/nexus-status-card.tsx`
- `apps/web-ide/components/git/source-control/remote-card.tsx`
- `apps/web-ide/components/git/source-control/repository-actions-card.tsx`
- `apps/web-ide/components/git/source-control/styles.ts`
- `brain/providers/_schema_utils.py`
- `crates/mcp-core/src/agent_types.rs`
- `crates/mcp-core/src/brain_agent_client.rs`
- `db/migrations/0291_provider_empty_responses.sql`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-refactor-spezza-plugin-managertsx-in-sotto-componenti-audit-fase-1-frontend.md`
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

refactor: spezza source-control-panel.tsx in sotto-componenti (audit fase 1 frontend)

## Riferimenti

- Vedi diff git: `git show 57c95686cfb271293a808dfa51b56c03c31cf7b6`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[frontend-nextjs]]
- [[postgres-tables]]
- [[migrations-log]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
