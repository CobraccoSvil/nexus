---
id: 07d2f6a5-2342-4ef6-bdd0-4107e9ba4c00
kind: changelog
title: "perf+refactor: indici FK mancanti + modularizzazione api-client (audit fase 1)"
slug: perfrefactor-indici-fk-mancanti-modularizzazione-api-client-audit-fase-1
tags:
  - changelog
source_commit: 8903d1f21d0ce7368e9eef518b9a2a248dbb2287
source_files:
  - apps/web-ide/app/admin/nexus-docs/page.tsx
  - apps/web-ide/components/chat/markdown-renderer.tsx
  - apps/web-ide/components/project-db/project-db-panel.tsx
  - apps/web-ide/components/wiki/markdown-wiki-extras.ts
  - apps/web-ide/components/wiki/wiki-editor.tsx
  - apps/web-ide/components/wiki/wiki-history.tsx
  - apps/web-ide/components/wiki/wiki-scope.ts
  - apps/web-ide/components/wiki/wiki-shell.tsx
  - apps/web-ide/components/wiki/wiki-side-panel.tsx
  - apps/web-ide/components/wiki/wiki-tree.tsx
  - apps/web-ide/lib/api-client.ts
  - apps/web-ide/lib/api-client.ts.backup.step8
  - apps/web-ide/lib/api/_shared.ts
  - apps/web-ide/lib/api/admin-settings.ts
  - apps/web-ide/lib/api/admin-users.ts
  - apps/web-ide/lib/api/agent.ts
  - apps/web-ide/lib/api/billing.ts
  - apps/web-ide/lib/api/change-drafts.ts
  - apps/web-ide/lib/api/chat.ts
  - apps/web-ide/lib/api/git.ts
  - apps/web-ide/lib/api/knowledge.ts
  - apps/web-ide/lib/api/mcp-plugins.ts
  - apps/web-ide/lib/api/meta-docs.ts
  - apps/web-ide/lib/api/models.ts
  - apps/web-ide/lib/api/profiles.ts
  - apps/web-ide/lib/api/project-db.ts
  - apps/web-ide/lib/api/projects.ts
  - apps/web-ide/lib/api/prompts.ts
  - apps/web-ide/lib/api/quality.ts
  - apps/web-ide/lib/api/runtime.ts
  - apps/web-ide/lib/api/system.ts
  - apps/web-ide/lib/api/workspace.ts
  - apps/web-ide/next.config.ts
  - crates/mcp-core/src/docs_core/mod.rs
  - crates/mcp-core/src/docs_core/revisions.rs
  - crates/mcp-core/src/docs_core/routes.rs
  - crates/mcp-core/src/docs_core/storage.rs
  - crates/mcp-core/src/knowledge/routes.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/meta_docs/apply.rs
  - db/migrations/0282_fk_indexes.sql
  - db/migrations/0282_wiki_unification_expand.sql
  - db/migrations/0283_wiki_backfill_revisions.sql
  - db/migrations/0284_wiki_unified_view.sql
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-03-chore-escludi-modello-onnx-dal-repo-download-via-scriptsfetch-modelssh.md
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
created_at: 2026-06-04T04:26:49Z
updated_at: 2026-06-04T04:26:47Z
nexus_meta_version: 1
---

# perf+refactor: indici FK mancanti + modularizzazione api-client (audit fase 1)

**Commit**: `8903d1f21d0ce7368e9eef518b9a2a248dbb2287` (2026-06-04 04:26 UTC)

**Significance**: 0.80

## File toccati

- `apps/web-ide/app/admin/nexus-docs/page.tsx`
- `apps/web-ide/components/chat/markdown-renderer.tsx`
- `apps/web-ide/components/project-db/project-db-panel.tsx`
- `apps/web-ide/components/wiki/markdown-wiki-extras.ts`
- `apps/web-ide/components/wiki/wiki-editor.tsx`
- `apps/web-ide/components/wiki/wiki-history.tsx`
- `apps/web-ide/components/wiki/wiki-scope.ts`
- `apps/web-ide/components/wiki/wiki-shell.tsx`
- `apps/web-ide/components/wiki/wiki-side-panel.tsx`
- `apps/web-ide/components/wiki/wiki-tree.tsx`
- `apps/web-ide/lib/api-client.ts`
- `apps/web-ide/lib/api-client.ts.backup.step8`
- `apps/web-ide/lib/api/_shared.ts`
- `apps/web-ide/lib/api/admin-settings.ts`
- `apps/web-ide/lib/api/admin-users.ts`
- `apps/web-ide/lib/api/agent.ts`
- `apps/web-ide/lib/api/billing.ts`
- `apps/web-ide/lib/api/change-drafts.ts`
- `apps/web-ide/lib/api/chat.ts`
- `apps/web-ide/lib/api/git.ts`
- `apps/web-ide/lib/api/knowledge.ts`
- `apps/web-ide/lib/api/mcp-plugins.ts`
- `apps/web-ide/lib/api/meta-docs.ts`
- `apps/web-ide/lib/api/models.ts`
- `apps/web-ide/lib/api/profiles.ts`
- `apps/web-ide/lib/api/project-db.ts`
- `apps/web-ide/lib/api/projects.ts`
- `apps/web-ide/lib/api/prompts.ts`
- `apps/web-ide/lib/api/quality.ts`
- `apps/web-ide/lib/api/runtime.ts`
- `apps/web-ide/lib/api/system.ts`
- `apps/web-ide/lib/api/workspace.ts`
- `apps/web-ide/next.config.ts`
- `crates/mcp-core/src/docs_core/mod.rs`
- `crates/mcp-core/src/docs_core/revisions.rs`
- `crates/mcp-core/src/docs_core/routes.rs`
- `crates/mcp-core/src/docs_core/storage.rs`
- `crates/mcp-core/src/knowledge/routes.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/meta_docs/apply.rs`
- `db/migrations/0282_fk_indexes.sql`
- `db/migrations/0282_wiki_unification_expand.sql`
- `db/migrations/0283_wiki_backfill_revisions.sql`
- `db/migrations/0284_wiki_unified_view.sql`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-03-chore-escludi-modello-onnx-dal-repo-download-via-scriptsfetch-modelssh.md`
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

perf+refactor: indici FK mancanti + modularizzazione api-client (audit fase 1)

## Riferimenti

- Vedi diff git: `git show 8903d1f21d0ce7368e9eef518b9a2a248dbb2287`

## Documenti correlati

- [[crates-rust]]
- [[frontend-nextjs]]
- [[postgres-tables]]
- [[migrations-log]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
- [[meta-vault-architettura]]
- [[multi-provider-routing]]
- [[routing-matrix]]
