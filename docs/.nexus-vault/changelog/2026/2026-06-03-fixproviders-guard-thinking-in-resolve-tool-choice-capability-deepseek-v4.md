---
id: b69e659b-5fc3-48d3-9ea7-61eb4cc988cc
kind: changelog
title: "fix(providers): guard thinking in resolve_tool_choice + capability DeepSeek V4"
slug: fixproviders-guard-thinking-in-resolve-tool-choice-capability-deepseek-v4
tags:
  - changelog
source_commit: 702ada25972e903597a582778d3d6b2f54d82472
source_files:
  - brain/providers/adapter_base.py
  - brain/tests/test_adapter_tool_choice.py
  - db/migrations/0256_deepseek_v4_thinking_capability.sql
auto_generated: true
created_at: 2026-06-03T08:39:39Z
updated_at: 2026-06-03T08:39:38Z
nexus_meta_version: 1
---

# fix(providers): guard thinking in resolve_tool_choice + capability DeepSeek V4

**Commit**: `702ada25972e903597a582778d3d6b2f54d82472` (2026-06-03 08:39 UTC)

**Significance**: 0.69

## File toccati

- `brain/providers/adapter_base.py`
- `brain/tests/test_adapter_tool_choice.py`
- `db/migrations/0256_deepseek_v4_thinking_capability.sql`

## Cosa cambia

fix(providers): guard thinking in resolve_tool_choice + capability DeepSeek V4

## Riferimenti

- Vedi diff git: `git show 702ada25972e903597a582778d3d6b2f54d82472`

## Documenti correlati

- [[brain-python]]
- [[postgres-tables]]
- [[migrations-log]]
- [[multi-provider-routing]]
- [[routing-matrix]]
