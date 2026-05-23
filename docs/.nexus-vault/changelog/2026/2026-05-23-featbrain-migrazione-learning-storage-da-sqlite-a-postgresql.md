---
id: 412c7521-1e23-420b-889a-1a3ffc3b8fa3
kind: changelog
title: "feat(brain): migrazione learning storage da SQLite a PostgreSQL"
slug: featbrain-migrazione-learning-storage-da-sqlite-a-postgresql
tags:
  - changelog
source_commit: 49cf252ce9dbffe3961b6fb09fcbc01609a43e37
source_files:
  - brain/agents/checkpointer.py
  - brain/agents/graph.py
  - brain/agents/nodes.py
  - brain/grpc_server/main.py
  - brain/memory/__init__.py
  - brain/memory/storage.py
  - db/migrations/0176_brain_learning_to_postgres.sql
  - tests/test_langgraph_integration.py
auto_generated: true
created_at: 2026-05-23T07:20:01Z
updated_at: 2026-05-23T07:20:00Z
nexus_meta_version: 1
---

# feat(brain): migrazione learning storage da SQLite a PostgreSQL

**Commit**: `49cf252ce9dbffe3961b6fb09fcbc01609a43e37` (2026-05-23 07:20 UTC)

**Significance**: 0.77

## File toccati

- `brain/agents/checkpointer.py`
- `brain/agents/graph.py`
- `brain/agents/nodes.py`
- `brain/grpc_server/main.py`
- `brain/memory/__init__.py`
- `brain/memory/storage.py`
- `db/migrations/0176_brain_learning_to_postgres.sql`
- `tests/test_langgraph_integration.py`

## Cosa cambia

feat(brain): migrazione learning storage da SQLite a PostgreSQL

## Riferimenti

- Vedi diff git: `git show 49cf252ce9dbffe3961b6fb09fcbc01609a43e37`
