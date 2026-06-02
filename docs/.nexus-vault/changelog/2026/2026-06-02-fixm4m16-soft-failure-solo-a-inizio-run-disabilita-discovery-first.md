---
id: fdb9f71f-2083-46f0-bc90-62c48f914b0b
kind: changelog
title: "fix(M4+M16): soft-failure solo a inizio run + disabilita discovery-first"
slug: fixm4m16-soft-failure-solo-a-inizio-run-disabilita-discovery-first
tags:
  - changelog
source_commit: b2dc1a62b1b9c237419df23164a386b14b67e1d3
source_files:
  - brain/providers/adapter_base.py
  - brain/providers/registry.py
  - db/migrations/0246_discovery_first_default_off.sql
auto_generated: true
created_at: 2026-06-02T07:04:37Z
updated_at: 2026-06-02T07:04:36Z
nexus_meta_version: 1
---

# fix(M4+M16): soft-failure solo a inizio run + disabilita discovery-first

**Commit**: `b2dc1a62b1b9c237419df23164a386b14b67e1d3` (2026-06-02 07:04 UTC)

**Significance**: 0.69

## File toccati

- `brain/providers/adapter_base.py`
- `brain/providers/registry.py`
- `db/migrations/0246_discovery_first_default_off.sql`

## Cosa cambia

fix(M4+M16): soft-failure solo a inizio run + disabilita discovery-first

## Riferimenti

- Vedi diff git: `git show b2dc1a62b1b9c237419df23164a386b14b67e1d3`

## Documenti correlati

- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[multi-provider-routing]]
- [[routing-matrix]]
