---
id: 88979ba2-c01e-4d1c-86d3-cce2d0b621c6
kind: changelog
title: "chore: escludi modello ONNX dal repo, download via scripts/fetch-models.sh"
slug: chore-escludi-modello-onnx-dal-repo-download-via-scriptsfetch-modelssh
tags:
  - changelog
source_commit: 109bfafad79cbe4c32779f771e0982e92635cf47
source_files:
  - .gitignore
  - crates/mcp-core/src/docs_core/mod.rs
  - crates/mcp-core/src/docs_core/vault.rs
  - crates/mcp-core/src/knowledge/vault.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/meta_docs/vault.rs
  - deploy/deploy-local.sh
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-03-fix-routing-thinking-gestione-contesto-tool-search-semantico-e-db-provisioning-d.md
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
  - docs/.nexus-vault/schema/migrations-log.md
  - docs/.nexus-vault/schema/postgres-tables.md
  - docs/.nexus-vault/schema/qdrant-collections.md
  - models/minilm/model.onnx
  - models/minilm/tokenizer.json
  - scripts/fetch-models.sh
auto_generated: true
created_at: 2026-06-03T20:57:35Z
updated_at: 2026-06-03T20:57:33Z
nexus_meta_version: 1
---

# chore: escludi modello ONNX dal repo, download via scripts/fetch-models.sh

**Commit**: `109bfafad79cbe4c32779f771e0982e92635cf47` (2026-06-03 20:57 UTC)

**Significance**: 0.45

## File toccati

- `.gitignore`
- `crates/mcp-core/src/docs_core/mod.rs`
- `crates/mcp-core/src/docs_core/vault.rs`
- `crates/mcp-core/src/knowledge/vault.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/meta_docs/vault.rs`
- `deploy/deploy-local.sh`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-03-fix-routing-thinking-gestione-contesto-tool-search-semantico-e-db-provisioning-d.md`
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
- `docs/.nexus-vault/schema/migrations-log.md`
- `docs/.nexus-vault/schema/postgres-tables.md`
- `docs/.nexus-vault/schema/qdrant-collections.md`
- `models/minilm/model.onnx`
- `models/minilm/tokenizer.json`
- `scripts/fetch-models.sh`

## Cosa cambia

chore: escludi modello ONNX dal repo, download via scripts/fetch-models.sh

## Riferimenti

- Vedi diff git: `git show 109bfafad79cbe4c32779f771e0982e92635cf47`

## Documenti correlati

- [[crates-rust]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[meta-vault-architettura]]
- [[multi-provider-routing]]
- [[routing-matrix]]
