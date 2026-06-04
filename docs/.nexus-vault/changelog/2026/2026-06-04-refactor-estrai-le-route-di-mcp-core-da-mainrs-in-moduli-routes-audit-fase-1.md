---
id: efc8e555-cfc4-435d-965e-daf1ba88a227
kind: changelog
title: "refactor: estrai le route di mcp-core da main.rs in moduli routes/ (audit fase 1)"
slug: refactor-estrai-le-route-di-mcp-core-da-mainrs-in-moduli-routes-audit-fase-1
tags:
  - changelog
source_commit: a367c2c2fde4d601201d834d03e32807da2adbac
source_files:
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/nexus_builtin/mcp_runtime.rs
  - crates/mcp-core/src/plugins/install.rs
  - crates/mcp-core/src/routes/admin.rs
  - crates/mcp-core/src/routes/change_drafts.rs
  - crates/mcp-core/src/routes/chat_commands.rs
  - crates/mcp-core/src/routes/dispatcher.rs
  - crates/mcp-core/src/routes/documents.rs
  - crates/mcp-core/src/routes/knowledge.rs
  - crates/mcp-core/src/routes/meta_docs.rs
  - crates/mcp-core/src/routes/mod.rs
  - crates/mcp-core/src/routes/project_db.rs
  - crates/mcp-core/src/routes/prompt_templates.rs
  - crates/mcp-core/src/routes/protected.rs
  - crates/mcp-core/src/routes/public.rs
  - crates/mcp-core/src/routes/security_quota.rs
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-refactorstyle-modularizza-nexus-tool-catalog-formattazione-uniforme-audit-fase-1.md
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
created_at: 2026-06-04T05:07:03Z
updated_at: 2026-06-04T05:07:02Z
nexus_meta_version: 1
---

# refactor: estrai le route di mcp-core da main.rs in moduli routes/ (audit fase 1)

**Commit**: `a367c2c2fde4d601201d834d03e32807da2adbac` (2026-06-04 05:07 UTC)

**Significance**: 0.75

## File toccati

- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/nexus_builtin/mcp_runtime.rs`
- `crates/mcp-core/src/plugins/install.rs`
- `crates/mcp-core/src/routes/admin.rs`
- `crates/mcp-core/src/routes/change_drafts.rs`
- `crates/mcp-core/src/routes/chat_commands.rs`
- `crates/mcp-core/src/routes/dispatcher.rs`
- `crates/mcp-core/src/routes/documents.rs`
- `crates/mcp-core/src/routes/knowledge.rs`
- `crates/mcp-core/src/routes/meta_docs.rs`
- `crates/mcp-core/src/routes/mod.rs`
- `crates/mcp-core/src/routes/project_db.rs`
- `crates/mcp-core/src/routes/prompt_templates.rs`
- `crates/mcp-core/src/routes/protected.rs`
- `crates/mcp-core/src/routes/public.rs`
- `crates/mcp-core/src/routes/security_quota.rs`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-refactorstyle-modularizza-nexus-tool-catalog-formattazione-uniforme-audit-fase-1.md`
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

refactor: estrai le route di mcp-core da main.rs in moduli routes/ (audit fase 1)

## Riferimenti

- Vedi diff git: `git show a367c2c2fde4d601201d834d03e32807da2adbac`

## Documenti correlati

- [[crates-rust]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
- [[meta-vault-architettura]]
- [[multi-provider-routing]]
- [[routing-matrix]]
