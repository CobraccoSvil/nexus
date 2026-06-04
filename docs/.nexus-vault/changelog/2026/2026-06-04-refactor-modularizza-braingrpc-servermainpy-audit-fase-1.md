---
id: 9703e253-28f4-450e-b425-b6f4cf194505
kind: changelog
title: "refactor: modularizza brain/grpc_server/main.py (audit fase 1)"
slug: refactor-modularizza-braingrpc-servermainpy-audit-fase-1
tags:
  - changelog
source_commit: dc1ca99ab4cf4646b7bd228a2db10e639a9eb095
source_files:
  - Cargo.lock
  - Cargo.toml
  - apps/web-ide/app/admin/sudo-manager/page.tsx
  - apps/web-ide/components/admin-sidebar.tsx
  - apps/web-ide/lib/api-client.ts
  - apps/web-ide/lib/api/admin-sudo.ts
  - apps/web-ide/next.config.ts
  - brain/grpc_server/app.py
  - brain/grpc_server/main.py
  - brain/grpc_server/routes/__init__.py
  - brain/grpc_server/routes/agent.py
  - brain/grpc_server/routes/core.py
  - brain/grpc_server/routes/terminal.py
  - brain/grpc_server/routes/vision.py
  - brain/grpc_server/runtime.py
  - brain/tests/test_terminal_token_auth.py
  - crates/mcp-core/src/agent_tools/testing.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/routes/admin.rs
  - crates/mcp-core/src/sudo_manager.rs
  - crates/mcp-core/src/sudo_routes.rs
  - crates/nexus-sudo-runner/Cargo.toml
  - crates/nexus-sudo-runner/src/main.rs
  - deploy/install-sudo-manager.sh
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-04-refactor-modularizza-project-db-routesrs-in-package-audit-fase-1.md
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
created_at: 2026-06-04T09:35:30Z
updated_at: 2026-06-04T09:35:30Z
nexus_meta_version: 1
---

# refactor: modularizza brain/grpc_server/main.py (audit fase 1)

**Commit**: `dc1ca99ab4cf4646b7bd228a2db10e639a9eb095` (2026-06-04 09:35 UTC)

**Significance**: 0.75

## File toccati

- `Cargo.lock`
- `Cargo.toml`
- `apps/web-ide/app/admin/sudo-manager/page.tsx`
- `apps/web-ide/components/admin-sidebar.tsx`
- `apps/web-ide/lib/api-client.ts`
- `apps/web-ide/lib/api/admin-sudo.ts`
- `apps/web-ide/next.config.ts`
- `brain/grpc_server/app.py`
- `brain/grpc_server/main.py`
- `brain/grpc_server/routes/__init__.py`
- `brain/grpc_server/routes/agent.py`
- `brain/grpc_server/routes/core.py`
- `brain/grpc_server/routes/terminal.py`
- `brain/grpc_server/routes/vision.py`
- `brain/grpc_server/runtime.py`
- `brain/tests/test_terminal_token_auth.py`
- `crates/mcp-core/src/agent_tools/testing.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/routes/admin.rs`
- `crates/mcp-core/src/sudo_manager.rs`
- `crates/mcp-core/src/sudo_routes.rs`
- `crates/nexus-sudo-runner/Cargo.toml`
- `crates/nexus-sudo-runner/src/main.rs`
- `deploy/install-sudo-manager.sh`
- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-04-refactor-modularizza-project-db-routesrs-in-package-audit-fase-1.md`
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

refactor: modularizza brain/grpc_server/main.py (audit fase 1)

## Riferimenti

- Vedi diff git: `git show dc1ca99ab4cf4646b7bd228a2db10e639a9eb095`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[frontend-nextjs]]
- [[rest-endpoints]]
- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
