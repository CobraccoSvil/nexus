---
id: 7d9bc9a2-e893-4c45-bac2-662c1d5b1397
kind: changelog
title: "feat(routing): auto-manutenzione della matrix (cleanup stale + tool-failure)"
slug: featrouting-auto-manutenzione-della-matrix-cleanup-stale-tool-failure
tags:
  - changelog
source_commit: 55ddcd65a927f036359f76aa29ab157c8bd93960
source_files:
  - crates/mcp-core/src/agent_types.rs
  - crates/mcp-core/src/brain_agent_client.rs
  - crates/mcp-core/src/chat_messages.rs
  - crates/mcp-core/src/routing_matrix_auto_promoter.rs
  - db/migrations/0269_model_tool_failure_tracking.sql
auto_generated: true
created_at: 2026-06-03T14:11:00Z
updated_at: 2026-06-03T14:10:58Z
nexus_meta_version: 1
---

# feat(routing): auto-manutenzione della matrix (cleanup stale + tool-failure)

**Commit**: `55ddcd65a927f036359f76aa29ab157c8bd93960` (2026-06-03 14:10 UTC)

**Significance**: 0.73

## File toccati

- `crates/mcp-core/src/agent_types.rs`
- `crates/mcp-core/src/brain_agent_client.rs`
- `crates/mcp-core/src/chat_messages.rs`
- `crates/mcp-core/src/routing_matrix_auto_promoter.rs`
- `db/migrations/0269_model_tool_failure_tracking.sql`

## Cosa cambia

feat(routing): auto-manutenzione della matrix (cleanup stale + tool-failure)

## Riferimenti

- Vedi diff git: `git show 55ddcd65a927f036359f76aa29ab157c8bd93960`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
- [[multi-provider-routing]]
- [[routing-matrix]]
