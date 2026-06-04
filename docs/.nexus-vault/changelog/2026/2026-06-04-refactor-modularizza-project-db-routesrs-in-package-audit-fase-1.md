---
id: 4f2a7c95-7234-47c7-861c-44c8f7647652
kind: changelog
title: "refactor: modularizza project_db_routes.rs in package (audit fase 1)"
slug: refactor-modularizza-project-db-routesrs-in-package-audit-fase-1
tags:
  - changelog
source_commit: 171c2da1220f4e3ea87358c54516a91feafa8061
source_files:
  - crates/mcp-core/src/project_db_routes.rs
  - crates/mcp-core/src/project_db_routes/config.rs
  - crates/mcp-core/src/project_db_routes/connection.rs
  - crates/mcp-core/src/project_db_routes/migrations.rs
  - crates/mcp-core/src/project_db_routes/mod.rs
  - crates/mcp-core/src/project_db_routes/provision.rs
  - crates/mcp-core/src/project_db_routes/query.rs
  - crates/mcp-core/src/project_db_routes/shared.rs
  - db/migrations/0289_sudo_manager.sql
  - docs/.nexus-vault/changelog/2026/2026-06-04-docs-rigenera-meta-vault-dopo-playwright-preflight-refactor-agent-tools.md
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
created_at: 2026-06-04T09:20:43Z
updated_at: 2026-06-04T09:20:41Z
nexus_meta_version: 1
---

# refactor: modularizza project_db_routes.rs in package (audit fase 1)

**Commit**: `171c2da1220f4e3ea87358c54516a91feafa8061` (2026-06-04 09:20 UTC)

**Significance**: 0.95

## File toccati

- `crates/mcp-core/src/project_db_routes.rs`
- `crates/mcp-core/src/project_db_routes/config.rs`
- `crates/mcp-core/src/project_db_routes/connection.rs`
- `crates/mcp-core/src/project_db_routes/migrations.rs`
- `crates/mcp-core/src/project_db_routes/mod.rs`
- `crates/mcp-core/src/project_db_routes/provision.rs`
- `crates/mcp-core/src/project_db_routes/query.rs`
- `crates/mcp-core/src/project_db_routes/shared.rs`
- `db/migrations/0289_sudo_manager.sql`
- `docs/.nexus-vault/changelog/2026/2026-06-04-docs-rigenera-meta-vault-dopo-playwright-preflight-refactor-agent-tools.md`
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

refactor: modularizza project_db_routes.rs in package (audit fase 1)

## Riferimenti

- Vedi diff git: `git show 171c2da1220f4e3ea87358c54516a91feafa8061`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
