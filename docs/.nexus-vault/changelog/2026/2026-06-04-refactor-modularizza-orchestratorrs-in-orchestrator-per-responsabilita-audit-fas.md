---
id: 11963ec2-9533-4355-b901-23760b390211
kind: changelog
title: "refactor: modularizza orchestrator.rs in orchestrator/ per responsabilita (audit fase 1)"
slug: refactor-modularizza-orchestratorrs-in-orchestrator-per-responsabilita-audit-fas
tags:
  - changelog
source_commit: fc9feb995bf3907820f2e80a449308263438fc9d
source_files:
  - crates/mcp-core/src/orchestrator.rs
  - crates/mcp-core/src/orchestrator/core.rs
  - crates/mcp-core/src/orchestrator/intent.rs
  - crates/mcp-core/src/orchestrator/mod.rs
  - crates/mcp-core/src/orchestrator/model_routing.rs
  - crates/mcp-core/src/orchestrator/neural_client.rs
  - crates/mcp-core/src/orchestrator/tests.rs
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-refactor-estrai-le-route-di-mcp-core-da-mainrs-in-moduli-routes-audit-fase-1.md
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
created_at: 2026-06-04T06:38:13Z
updated_at: 2026-06-04T06:38:11Z
nexus_meta_version: 1
---

# refactor: modularizza orchestrator.rs in orchestrator/ per responsabilita (audit fase 1)

**Commit**: `fc9feb995bf3907820f2e80a449308263438fc9d` (2026-06-04 06:38 UTC)

**Significance**: 0.75

## File toccati

- `crates/mcp-core/src/orchestrator.rs`
- `crates/mcp-core/src/orchestrator/core.rs`
- `crates/mcp-core/src/orchestrator/intent.rs`
- `crates/mcp-core/src/orchestrator/mod.rs`
- `crates/mcp-core/src/orchestrator/model_routing.rs`
- `crates/mcp-core/src/orchestrator/neural_client.rs`
- `crates/mcp-core/src/orchestrator/tests.rs`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-refactor-estrai-le-route-di-mcp-core-da-mainrs-in-moduli-routes-audit-fase-1.md`
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

refactor: modularizza orchestrator.rs in orchestrator/ per responsabilita (audit fase 1)

## Riferimenti

- Vedi diff git: `git show fc9feb995bf3907820f2e80a449308263438fc9d`

## Documenti correlati

- [[crates-rust]]
- [[rest-endpoints]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
