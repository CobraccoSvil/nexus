---
id: f73d34e9-0634-4262-97c0-8338e9f73374
kind: changelog
title: "feat(routing): fix architetturali #77 #78 #79 + safety net shutdown"
slug: featrouting-fix-architetturali-77-78-79-safety-net-shutdown
tags:
  - changelog
source_commit: 73c57b761a39d3489cef7f23ff7df54866360875
source_files:
  - brain/grpc_server/main.py
  - crates/mcp-core/src/internal_routing.rs
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/model_catalog_sync.rs
  - crates/mcp-core/src/provider_cooldown.rs
  - db/migrations/0186_chat_message_attachments.sql
  - db/migrations/0187_catalog_chat_only_filter.sql
auto_generated: true
created_at: 2026-05-28T11:39:04Z
updated_at: 2026-05-28T11:39:02Z
nexus_meta_version: 1
---

# feat(routing): fix architetturali #77 #78 #79 + safety net shutdown

**Commit**: `73c57b761a39d3489cef7f23ff7df54866360875` (2026-05-28 11:39 UTC)

**Significance**: 0.75

## File toccati

- `brain/grpc_server/main.py`
- `crates/mcp-core/src/internal_routing.rs`
- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/model_catalog_sync.rs`
- `crates/mcp-core/src/provider_cooldown.rs`
- `db/migrations/0186_chat_message_attachments.sql`
- `db/migrations/0187_catalog_chat_only_filter.sql`

## Cosa cambia

feat(routing): fix architetturali #77 #78 #79 + safety net shutdown

## Riferimenti

- Vedi diff git: `git show 73c57b761a39d3489cef7f23ff7df54866360875`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[multi-provider-routing]]
- [[routing-matrix]]
