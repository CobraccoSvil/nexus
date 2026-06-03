---
id: f59b3381-6c0f-4213-a41b-af6196218e63
kind: changelog
title: "feat(watchdog): riavvio automatico dei microservizi Nexus caduti"
slug: featwatchdog-riavvio-automatico-dei-microservizi-nexus-caduti
tags:
  - changelog
source_commit: b0fdc9310bcd415e02a5d777d2bdca5b10685608
source_files:
  - crates/mcp-core/src/main.rs
  - crates/mcp-core/src/services_watchdog.rs
  - db/migrations/0272_services_watchdog.sql
auto_generated: true
created_at: 2026-06-03T15:15:40Z
updated_at: 2026-06-03T15:15:38Z
nexus_meta_version: 1
---

# feat(watchdog): riavvio automatico dei microservizi Nexus caduti

**Commit**: `b0fdc9310bcd415e02a5d777d2bdca5b10685608` (2026-06-03 15:15 UTC)

**Significance**: 0.69

## File toccati

- `crates/mcp-core/src/main.rs`
- `crates/mcp-core/src/services_watchdog.rs`
- `db/migrations/0272_services_watchdog.sql`

## Cosa cambia

feat(watchdog): riavvio automatico dei microservizi Nexus caduti

## Riferimenti

- Vedi diff git: `git show b0fdc9310bcd415e02a5d777d2bdca5b10685608`

## Documenti correlati

- [[crates-rust]]
- [[postgres-tables]]
- [[migrations-log]]
