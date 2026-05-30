---
id: 89f38e7f-7e8d-4b2b-992f-ffc370cc69ea
kind: changelog
title: "fix(brain): elimina loop esplorazione allegati su scaffolding app"
slug: fixbrain-elimina-loop-esplorazione-allegati-su-scaffolding-app
tags:
  - changelog
source_commit: 8ce41156da2df56495aaed76d7c9cf53937f9e38
source_files:
  - brain/agents/nodes.py
  - brain/agents/state.py
  - brain/router/service.py
  - brain/tests/test_scaffolding_and_exploration_loop.py
  - db/migrations/0120_exploration_loop_threshold.sql
auto_generated: true
created_at: 2026-05-30T08:27:20Z
updated_at: 2026-05-30T08:27:19Z
nexus_meta_version: 1
---

# fix(brain): elimina loop esplorazione allegati su scaffolding app

**Commit**: `8ce41156da2df56495aaed76d7c9cf53937f9e38` (2026-05-30 08:27 UTC)

**Significance**: 0.73

## File toccati

- `brain/agents/nodes.py`
- `brain/agents/state.py`
- `brain/router/service.py`
- `brain/tests/test_scaffolding_and_exploration_loop.py`
- `db/migrations/0120_exploration_loop_threshold.sql`

## Cosa cambia

fix(brain): elimina loop esplorazione allegati su scaffolding app

## Riferimenti

- Vedi diff git: `git show 8ce41156da2df56495aaed76d7c9cf53937f9e38`

## Documenti correlati

- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[rest-endpoints]]
