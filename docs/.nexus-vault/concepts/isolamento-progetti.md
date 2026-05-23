---
id: 0a5a4575-7440-474a-b8d3-9b03c5cbfd6d
kind: other
title: Isolamento tra progetti
slug: isolamento-progetti
tags:
  - concept
  - security
  - isolation
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:08:59Z
updated_at: 2026-05-23T11:38:17Z
nexus_meta_version: 1
---

# Isolamento tra progetti

Ogni progetto registrato in Nexus e' un **mondo a se'**: codice, chat, knowledge, credenziali, container Docker, services systemd.

## Regole assolute (vedi CLAUDE.md sezione E)

- **Scope al progetto attivo**: ogni operazione MCP/agent vive dentro `project_root` del run corrente.
- **Cleanup Docker filtrato**: vietato `docker stop $(docker ps -q)` o `docker system prune` globali. Permesso solo con `-f <compose-progetto>` o `--filter "label=com.docker.compose.project=<slug>"`.
- **Container `ideai-*` intoccabili**: `ideai-postgres-nexus-1`, `ideai-qdrant-1`, `ideai-redis-1`, `ideai-grafana-1`. Mai fermarli/rimuoverli.
- **Letture massive ricorsive vietate** fuori dalla root progetto.

## Implementazione

- Sandbox Docker per processi agente (`nexus-sandbox:latest`).
- `ensure_project_access(db, user_id, project_id)` su ogni endpoint sensibile.
- File watcher per-progetto separati (uno per `.nexus/knowledge/` di ogni progetto).
- Port allocator `nexus_port_allocations` per evitare conflitti tra progetti.

Vedi [[postgres-tables]], [[runbook]].
