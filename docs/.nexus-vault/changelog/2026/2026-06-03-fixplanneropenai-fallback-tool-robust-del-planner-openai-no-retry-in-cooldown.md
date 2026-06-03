---
id: f93dfb68-bd42-4991-8f7d-c3b7ab4346ae
kind: changelog
title: "fix(planner,openai): fallback tool-robust del planner + openai no retry in cooldown"
slug: fixplanneropenai-fallback-tool-robust-del-planner-openai-no-retry-in-cooldown
tags:
  - changelog
source_commit: b1e1276e0302971aab212d60ae95cb35b8ae5d91
source_files:
  - brain/agents/planner_node.py
  - brain/providers/openai_provider.py
  - db/migrations/0267_planner_fallback_purpose.sql
auto_generated: true
created_at: 2026-06-03T13:23:34Z
updated_at: 2026-06-03T13:23:33Z
nexus_meta_version: 1
---

# fix(planner,openai): fallback tool-robust del planner + openai no retry in cooldown

**Commit**: `b1e1276e0302971aab212d60ae95cb35b8ae5d91` (2026-06-03 13:23 UTC)

**Significance**: 0.69

## File toccati

- `brain/agents/planner_node.py`
- `brain/providers/openai_provider.py`
- `db/migrations/0267_planner_fallback_purpose.sql`

## Cosa cambia

fix(planner,openai): fallback tool-robust del planner + openai no retry in cooldown

## Riferimenti

- Vedi diff git: `git show b1e1276e0302971aab212d60ae95cb35b8ae5d91`

## Documenti correlati

- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[multi-provider-routing]]
- [[routing-matrix]]
