---
id: 1b087880-69af-4bd5-be2a-1242a3093986
kind: changelog
title: "fix(providers): cooldown billing persistente (auto-disable) + chiusura client async Google"
slug: fixproviders-cooldown-billing-persistente-auto-disable-chiusura-client-async-goo
tags:
  - changelog
source_commit: c78c6f9d83d404d73daa10ae3adca183cb54690f
source_files:
  - brain/providers/google_provider.py
  - brain/providers/registry.py
  - db/migrations/0255_provider_health_cooldown.sql
auto_generated: true
created_at: 2026-06-02T16:17:19Z
updated_at: 2026-06-02T16:17:17Z
nexus_meta_version: 1
---

# fix(providers): cooldown billing persistente (auto-disable) + chiusura client async Google

**Commit**: `c78c6f9d83d404d73daa10ae3adca183cb54690f` (2026-06-02 16:17 UTC)

**Significance**: 0.69

## File toccati

- `brain/providers/google_provider.py`
- `brain/providers/registry.py`
- `db/migrations/0255_provider_health_cooldown.sql`

## Cosa cambia

fix(providers): cooldown billing persistente (auto-disable) + chiusura client async Google

## Riferimenti

- Vedi diff git: `git show c78c6f9d83d404d73daa10ae3adca183cb54690f`

## Documenti correlati

- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[multi-provider-routing]]
- [[routing-matrix]]
