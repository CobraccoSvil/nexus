---
id: 8f951e3e-fffe-49c2-8703-bcd3107ffdd8
kind: changelog
title: "fix(ports): kill del process group + GC periodico delle porte orfane"
slug: fixports-kill-del-process-group-gc-periodico-delle-porte-orfane
tags:
  - changelog
source_commit: d93095947b254b882aec3f0d040374544b230193
source_files:
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/port_registry.rs
  - crates/mcp-core/src/project_workspace/port_recovery.rs
  - db/migrations/0262_port_gc_settings.sql
auto_generated: true
created_at: 2026-06-03T09:52:04Z
updated_at: 2026-06-03T09:52:04Z
nexus_meta_version: 1
---

# fix(ports): kill del process group + GC periodico delle porte orfane

**Commit**: `d93095947b254b882aec3f0d040374544b230193` (2026-06-03 09:52 UTC)

**Significance**: 0.71

## File toccati

- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/port_registry.rs`
- `crates/mcp-core/src/project_workspace/port_recovery.rs`
- `db/migrations/0262_port_gc_settings.sql`

## Cosa cambia

fix(ports): kill del process group + GC periodico delle porte orfane

## Riferimenti

- Vedi diff git: `git show d93095947b254b882aec3f0d040374544b230193`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
