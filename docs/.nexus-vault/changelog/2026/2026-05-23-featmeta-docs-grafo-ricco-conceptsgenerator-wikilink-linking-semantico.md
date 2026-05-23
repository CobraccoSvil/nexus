---
id: 7af909da-e75a-4eea-8868-c08c51b6c928
kind: changelog
title: "feat(meta-docs): grafo ricco - ConceptsGenerator + wikilink + linking semantico"
slug: featmeta-docs-grafo-ricco-conceptsgenerator-wikilink-linking-semantico
tags:
  - changelog
source_commit: 187896c8107f5db09cf2077536733be75c98ddff
source_files:
  - apps/web-ide/app/admin/nexus-docs/page.tsx
  - apps/web-ide/lib/api-client.ts
  - crates/mcp-core/src/meta_docs/apply.rs
  - crates/mcp-core/src/meta_docs/generators/api.rs
  - crates/mcp-core/src/meta_docs/generators/architecture.rs
  - crates/mcp-core/src/meta_docs/generators/changelog.rs
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
  - crates/mcp-core/src/meta_docs/generators/mod.rs
  - crates/mcp-core/src/meta_docs/generators/schema.rs
  - crates/mcp-core/src/meta_docs/routes.rs
  - crates/mcp-core/src/meta_docs/vault.rs
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-05-23-featknowledge-recompute-links-note-funzionali-manuali-admin-polish.md
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
created_at: 2026-05-23T11:10:42Z
updated_at: 2026-05-23T11:10:41Z
nexus_meta_version: 1
---

# feat(meta-docs): grafo ricco - ConceptsGenerator + wikilink + linking semantico

**Commit**: `187896c8107f5db09cf2077536733be75c98ddff` (2026-05-23 11:10 UTC)

**Significance**: 0.75

## File toccati

- `apps/web-ide/app/admin/nexus-docs/page.tsx`
- `apps/web-ide/lib/api-client.ts`
- `crates/mcp-core/src/meta_docs/apply.rs`
- `crates/mcp-core/src/meta_docs/generators/api.rs`
- `crates/mcp-core/src/meta_docs/generators/architecture.rs`
- `crates/mcp-core/src/meta_docs/generators/changelog.rs`
- `crates/mcp-core/src/meta_docs/generators/concepts.rs`
- `crates/mcp-core/src/meta_docs/generators/mod.rs`
- `crates/mcp-core/src/meta_docs/generators/schema.rs`
- `crates/mcp-core/src/meta_docs/routes.rs`
- `crates/mcp-core/src/meta_docs/vault.rs`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-05-23-featknowledge-recompute-links-note-funzionali-manuali-admin-polish.md`
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

feat(meta-docs): grafo ricco - ConceptsGenerator + wikilink + linking semantico

## Riferimenti

- Vedi diff git: `git show 187896c8107f5db09cf2077536733be75c98ddff`

## Documenti correlati

- [[crates-rust]]
- [[frontend-nextjs]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
- [[meta-vault-architettura]]
- [[multi-provider-routing]]
- [[routing-matrix]]
