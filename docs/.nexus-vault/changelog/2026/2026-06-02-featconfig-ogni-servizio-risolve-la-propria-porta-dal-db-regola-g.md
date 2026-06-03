---
id: a1069ea6-8069-4658-867a-5b25c0441124
kind: changelog
title: "feat(config): ogni servizio risolve la propria porta dal DB (regola G)"
slug: featconfig-ogni-servizio-risolve-la-propria-porta-dal-db-regola-g
tags:
  - changelog
source_commit: adf3445630d76828662dc8a76907be1578d247e9
source_files:
  - apps/nexus-gateway/src/server.ts
  - apps/web-ide/package.json
  - apps/web-ide/server.js
  - brain/grpc_server/main.py
  - brain/utils/settings_db.py
  - crates/admin-service/src/main.rs
  - crates/billing-service/src/main.rs
  - crates/browser-bridge-mcp/Cargo.toml
  - crates/browser-bridge-mcp/src/main.rs
  - crates/chat-service/src/main.rs
  - crates/doc-service/src/main.rs
  - crates/mcp-core/src/environment.rs
  - crates/mcp-core/src/main.rs
  - crates/nexus-auth/Cargo.toml
  - crates/nexus-auth/src/lib.rs
  - crates/plugin-service/src/main.rs
  - pnpm-lock.yaml
auto_generated: true
created_at: 2026-06-02T20:43:20Z
updated_at: 2026-06-02T20:43:19Z
nexus_meta_version: 1
---

# feat(config): ogni servizio risolve la propria porta dal DB (regola G)

**Commit**: `adf3445630d76828662dc8a76907be1578d247e9` (2026-06-02 20:43 UTC)

**Significance**: 0.71

## File toccati

- `apps/nexus-gateway/src/server.ts`
- `apps/web-ide/package.json`
- `apps/web-ide/server.js`
- `brain/grpc_server/main.py`
- `brain/utils/settings_db.py`
- `crates/admin-service/src/main.rs`
- `crates/billing-service/src/main.rs`
- `crates/browser-bridge-mcp/Cargo.toml`
- `crates/browser-bridge-mcp/src/main.rs`
- `crates/chat-service/src/main.rs`
- `crates/doc-service/src/main.rs`
- `crates/mcp-core/src/environment.rs`
- `crates/mcp-core/src/main.rs`
- `crates/nexus-auth/Cargo.toml`
- `crates/nexus-auth/src/lib.rs`
- `crates/plugin-service/src/main.rs`
- `pnpm-lock.yaml`

## Cosa cambia

feat(config): ogni servizio risolve la propria porta dal DB (regola G)

## Riferimenti

- Vedi diff git: `git show adf3445630d76828662dc8a76907be1578d247e9`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[frontend-nextjs]]
