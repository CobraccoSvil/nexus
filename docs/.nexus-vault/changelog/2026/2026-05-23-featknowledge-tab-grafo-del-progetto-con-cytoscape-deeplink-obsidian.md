---
id: 2c03dc97-f645-437f-9d6a-b7929f42076f
kind: changelog
title: "feat(knowledge): tab Grafo del progetto con Cytoscape + deeplink Obsidian"
slug: featknowledge-tab-grafo-del-progetto-con-cytoscape-deeplink-obsidian
tags:
  - changelog
source_commit: 3deaf53b5cafe303c52068cbfbac92125e4fd86e
source_files:
  - apps/web-ide/components/knowledge/graph-tab.tsx
  - apps/web-ide/components/knowledge/knowledge-graph.tsx
  - apps/web-ide/components/knowledge/knowledge-panel.tsx
  - apps/web-ide/components/knowledge/meta-tab.tsx
  - apps/web-ide/lib/api-client.ts
  - apps/web-ide/package.json
  - crates/mcp-core/src/knowledge/routes.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/meta_docs/routes.rs
  - db/migrations/0178_obsidian_vault_name.sql
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featmeta-docs-vault-obsidian-per-la-documentazione-di-nexus-agenti.md
  - docs/.nexus-vault/schema/migrations-log.md
  - docs/.nexus-vault/schema/postgres-tables.md
  - docs/.nexus-vault/schema/qdrant-collections.md
  - pnpm-lock.yaml
auto_generated: true
created_at: 2026-05-23T10:09:39Z
updated_at: 2026-05-23T10:09:38Z
nexus_meta_version: 1
---

# feat(knowledge): tab Grafo del progetto con Cytoscape + deeplink Obsidian

**Commit**: `3deaf53b5cafe303c52068cbfbac92125e4fd86e` (2026-05-23 10:09 UTC)

**Significance**: 0.95

## File toccati

- `apps/web-ide/components/knowledge/graph-tab.tsx`
- `apps/web-ide/components/knowledge/knowledge-graph.tsx`
- `apps/web-ide/components/knowledge/knowledge-panel.tsx`
- `apps/web-ide/components/knowledge/meta-tab.tsx`
- `apps/web-ide/lib/api-client.ts`
- `apps/web-ide/package.json`
- `crates/mcp-core/src/knowledge/routes.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/meta_docs/routes.rs`
- `db/migrations/0178_obsidian_vault_name.sql`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featmeta-docs-vault-obsidian-per-la-documentazione-di-nexus-agenti.md`
- `docs/.nexus-vault/schema/migrations-log.md`
- `docs/.nexus-vault/schema/postgres-tables.md`
- `docs/.nexus-vault/schema/qdrant-collections.md`
- `pnpm-lock.yaml`

## Cosa cambia

feat(knowledge): tab Grafo del progetto con Cytoscape + deeplink Obsidian

## Riferimenti

- Vedi diff git: `git show 3deaf53b5cafe303c52068cbfbac92125e4fd86e`
