---
id: 80e17d41-9ea0-47e9-9242-37330fd8747d
kind: changelog
title: "fix(ports): process-GC termina i dev-server duplicati per progetto"
slug: fixports-process-gc-termina-i-dev-server-duplicati-per-progetto
tags:
  - changelog
source_commit: 7e8b062a7a124a33492aedb05d0028a921788f64
source_files:
  - crates/mcp-core/src/port_registry.rs
  - crates/mcp-core/src/project_workspace/port_recovery.rs
  - db/migrations/0263_dev_server_dedupe_setting.sql
auto_generated: true
created_at: 2026-06-03T10:34:45Z
updated_at: 2026-06-03T10:34:44Z
nexus_meta_version: 1
---

# fix(ports): process-GC termina i dev-server duplicati per progetto

**Commit**: `7e8b062a7a124a33492aedb05d0028a921788f64` (2026-06-03 10:34 UTC)

**Significance**: 0.69

## File toccati

- `crates/mcp-core/src/port_registry.rs`
- `crates/mcp-core/src/project_workspace/port_recovery.rs`
- `db/migrations/0263_dev_server_dedupe_setting.sql`

## Cosa cambia

fix(ports): process-GC termina i dev-server duplicati per progetto

## Riferimenti

- Vedi diff git: `git show 7e8b062a7a124a33492aedb05d0028a921788f64`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
