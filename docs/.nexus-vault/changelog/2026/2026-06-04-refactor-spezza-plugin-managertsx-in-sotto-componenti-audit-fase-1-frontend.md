---
id: 35263c8e-6fee-4545-b3cc-02b666e19815
kind: changelog
title: "refactor: spezza plugin-manager.tsx in sotto-componenti (audit fase 1 frontend)"
slug: refactor-spezza-plugin-managertsx-in-sotto-componenti-audit-fase-1-frontend
tags:
  - changelog
source_commit: 1e81b75818ff73e7bc8d1320f54f8e2b93fcc5c1
source_files:
  - apps/web-ide/components/settings/plugin-manager.tsx
  - apps/web-ide/components/settings/plugin-manager/catalog-tab.tsx
  - apps/web-ide/components/settings/plugin-manager/figma-oauth-card.tsx
  - apps/web-ide/components/settings/plugin-manager/installed-plugins-list.tsx
  - apps/web-ide/components/settings/plugin-manager/legacy-mcp-list.tsx
  - apps/web-ide/components/settings/plugin-manager/plugin-helpers.ts
  - apps/web-ide/components/settings/plugin-manager/plugin-styles.ts
  - apps/web-ide/components/settings/plugin-manager/policy-tab.tsx
  - apps/web-ide/components/settings/plugin-manager/types.ts
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-docs-rigenera-meta-vault-dopo-adr-0017-sudo-manager-livello-1.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-fixsudo-manager-correggi-command-template-playwright-install-deps-per-ubuntu-240.md
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
created_at: 2026-06-04T10:14:04Z
updated_at: 2026-06-04T10:14:04Z
nexus_meta_version: 1
---

# refactor: spezza plugin-manager.tsx in sotto-componenti (audit fase 1 frontend)

**Commit**: `1e81b75818ff73e7bc8d1320f54f8e2b93fcc5c1` (2026-06-04 10:14 UTC)

**Significance**: 0.75

## File toccati

- `apps/web-ide/components/settings/plugin-manager.tsx`
- `apps/web-ide/components/settings/plugin-manager/catalog-tab.tsx`
- `apps/web-ide/components/settings/plugin-manager/figma-oauth-card.tsx`
- `apps/web-ide/components/settings/plugin-manager/installed-plugins-list.tsx`
- `apps/web-ide/components/settings/plugin-manager/legacy-mcp-list.tsx`
- `apps/web-ide/components/settings/plugin-manager/plugin-helpers.ts`
- `apps/web-ide/components/settings/plugin-manager/plugin-styles.ts`
- `apps/web-ide/components/settings/plugin-manager/policy-tab.tsx`
- `apps/web-ide/components/settings/plugin-manager/types.ts`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-docs-rigenera-meta-vault-dopo-adr-0017-sudo-manager-livello-1.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-fixsudo-manager-correggi-command-template-playwright-install-deps-per-ubuntu-240.md`
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

refactor: spezza plugin-manager.tsx in sotto-componenti (audit fase 1 frontend)

## Riferimenti

- Vedi diff git: `git show 1e81b75818ff73e7bc8d1320f54f8e2b93fcc5c1`

## Documenti correlati

- [[frontend-nextjs]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
