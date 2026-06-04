---
id: 86b9bfa4-006f-4e87-9818-84b31630d95d
kind: changelog
title: "refactor: spezza project-db-panel.tsx in sotto-componenti (audit fase 1 frontend)"
slug: refactor-spezza-project-db-paneltsx-in-sotto-componenti-audit-fase-1-frontend
tags:
  - changelog
source_commit: 9aca34f6306316aa454934514575e09174bebb74
source_files:
  - apps/web-ide/components/project-db/connection-form.tsx
  - apps/web-ide/components/project-db/connection-list.tsx
  - apps/web-ide/components/project-db/create-db-wizard.tsx
  - apps/web-ide/components/project-db/db-helpers.ts
  - apps/web-ide/components/project-db/detect-config.tsx
  - apps/web-ide/components/project-db/migrations-section.tsx
  - apps/web-ide/components/project-db/project-db-panel.tsx
  - apps/web-ide/components/project-db/recent-queries-section.tsx
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-featrag-pipeline-rag-strutturale-completa-adr-0016-7-sprint.md
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
  - docs/.nexus-vault/schema/migrations-log.md
  - docs/.nexus-vault/schema/postgres-tables.md
  - docs/.nexus-vault/schema/qdrant-collections.md
auto_generated: true
created_at: 2026-06-04T08:31:40Z
updated_at: 2026-06-04T08:31:40Z
nexus_meta_version: 1
---

# refactor: spezza project-db-panel.tsx in sotto-componenti (audit fase 1 frontend)

**Commit**: `9aca34f6306316aa454934514575e09174bebb74` (2026-06-04 08:31 UTC)

**Significance**: 0.75

## File toccati

- `apps/web-ide/components/project-db/connection-form.tsx`
- `apps/web-ide/components/project-db/connection-list.tsx`
- `apps/web-ide/components/project-db/create-db-wizard.tsx`
- `apps/web-ide/components/project-db/db-helpers.ts`
- `apps/web-ide/components/project-db/detect-config.tsx`
- `apps/web-ide/components/project-db/migrations-section.tsx`
- `apps/web-ide/components/project-db/project-db-panel.tsx`
- `apps/web-ide/components/project-db/recent-queries-section.tsx`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-featrag-pipeline-rag-strutturale-completa-adr-0016-7-sprint.md`
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
- `docs/.nexus-vault/schema/migrations-log.md`
- `docs/.nexus-vault/schema/postgres-tables.md`
- `docs/.nexus-vault/schema/qdrant-collections.md`

## Cosa cambia

refactor: spezza project-db-panel.tsx in sotto-componenti (audit fase 1 frontend)

## Riferimenti

- Vedi diff git: `git show 9aca34f6306316aa454934514575e09174bebb74`

## Documenti correlati

- [[frontend-nextjs]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
