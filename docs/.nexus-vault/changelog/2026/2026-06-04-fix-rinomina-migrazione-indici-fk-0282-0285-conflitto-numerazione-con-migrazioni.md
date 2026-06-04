---
id: 652d5493-cb60-4e0f-9821-ddc1394cd405
kind: changelog
title: "fix: rinomina migrazione indici FK 0282 -> 0285 (conflitto numerazione con migrazioni wiki preesistenti)"
slug: fix-rinomina-migrazione-indici-fk-0282-0285-conflitto-numerazione-con-migrazioni
tags:
  - changelog
source_commit: a1dd3a174318e8a92a0808d9e39a37ac06afdf8a
source_files:
  - db/migrations/0282_fk_indexes.sql
  - db/migrations/0285_fk_indexes.sql
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-perfrefactor-indici-fk-mancanti-modularizzazione-api-client-audit-fase-1.md
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
created_at: 2026-06-04T04:27:55Z
updated_at: 2026-06-04T04:27:55Z
nexus_meta_version: 1
---

# fix: rinomina migrazione indici FK 0282 -> 0285 (conflitto numerazione con migrazioni wiki preesistenti)

**Commit**: `a1dd3a174318e8a92a0808d9e39a37ac06afdf8a` (2026-06-04 04:27 UTC)

**Significance**: 0.95

## File toccati

- `db/migrations/0282_fk_indexes.sql`
- `db/migrations/0285_fk_indexes.sql`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-perfrefactor-indici-fk-mancanti-modularizzazione-api-client-audit-fase-1.md`
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

fix: rinomina migrazione indici FK 0282 -> 0285 (conflitto numerazione con migrazioni wiki preesistenti)

## Riferimenti

- Vedi diff git: `git show a1dd3a174318e8a92a0808d9e39a37ac06afdf8a`

## Documenti correlati

- [[postgres-tables]]
- [[migrations-log]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
