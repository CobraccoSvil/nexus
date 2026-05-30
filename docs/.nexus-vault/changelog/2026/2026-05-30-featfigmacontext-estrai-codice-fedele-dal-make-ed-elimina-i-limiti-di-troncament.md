---
id: 3446cc70-2e5e-41b3-b21b-087cfb410003
kind: changelog
title: "feat(figma+context): estrai codice fedele dal .make ed elimina i limiti di troncamento"
slug: featfigmacontext-estrai-codice-fedele-dal-make-ed-elimina-i-limiti-di-troncament
tags:
  - changelog
source_commit: a046cc4fefc748578e7ff6aea827692831f5bd44
source_files:
  - brain/agents/context_offload.py
  - brain/agents/nodes.py
  - brain/grpc_server/main.py
  - brain/tests/test_context_offload.py
  - crates/mcp-core/src/agent_tools/archive_tools.rs
  - crates/mcp-core/src/agent_tools/attachment_inspector.rs
  - crates/mcp-core/src/agent_tools/attachment_settings.rs
  - crates/mcp-core/src/agent_tools/document_tools.rs
  - crates/mcp-core/src/agent_tools/figma_tools.rs
  - crates/mcp-core/src/agent_tools/files.rs
  - crates/mcp-core/src/agent_tools/mod.rs
  - crates/mcp-core/src/agent_tools/visual_compare.rs
  - crates/mcp-core/src/chat_messages.rs
  - crates/mcp-core/src/rag/indexer.rs
  - db/migrations/0210_figma_make_code_extraction.sql
  - db/migrations/0211_figma_make_strategy_directive.sql
  - db/migrations/0214_visual_compare_settings.sql
  - db/migrations/0215_visual_compare_directive.sql
  - db/migrations/0216_remove_attachment_extraction_limits.sql
  - db/migrations/0217_context_no_loss_rag.sql
auto_generated: true
created_at: 2026-05-30T11:29:10Z
updated_at: 2026-05-30T11:29:07Z
nexus_meta_version: 1
---

# feat(figma+context): estrai codice fedele dal .make ed elimina i limiti di troncamento

**Commit**: `a046cc4fefc748578e7ff6aea827692831f5bd44` (2026-05-30 11:29 UTC)

**Significance**: 0.95

## File toccati

- `brain/agents/context_offload.py`
- `brain/agents/nodes.py`
- `brain/grpc_server/main.py`
- `brain/tests/test_context_offload.py`
- `crates/mcp-core/src/agent_tools/archive_tools.rs`
- `crates/mcp-core/src/agent_tools/attachment_inspector.rs`
- `crates/mcp-core/src/agent_tools/attachment_settings.rs`
- `crates/mcp-core/src/agent_tools/document_tools.rs`
- `crates/mcp-core/src/agent_tools/figma_tools.rs`
- `crates/mcp-core/src/agent_tools/files.rs`
- `crates/mcp-core/src/agent_tools/mod.rs`
- `crates/mcp-core/src/agent_tools/visual_compare.rs`
- `crates/mcp-core/src/chat_messages.rs`
- `crates/mcp-core/src/rag/indexer.rs`
- `db/migrations/0210_figma_make_code_extraction.sql`
- `db/migrations/0211_figma_make_strategy_directive.sql`
- `db/migrations/0214_visual_compare_settings.sql`
- `db/migrations/0215_visual_compare_directive.sql`
- `db/migrations/0216_remove_attachment_extraction_limits.sql`
- `db/migrations/0217_context_no_loss_rag.sql`

## Cosa cambia

feat(figma+context): estrai codice fedele dal .make ed elimina i limiti di troncamento

## Riferimenti

- Vedi diff git: `git show a046cc4fefc748578e7ff6aea827692831f5bd44`

## Documenti correlati

- [[crates-rust]]
- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
