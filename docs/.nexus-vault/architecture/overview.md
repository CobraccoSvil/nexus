---
id: arch-overview
kind: architecture
title: "Overview architettura Nexus"
tags: [architecture, overview, entry-point]
auto_generated: false
created_at: 2026-05-23T00:00:00Z
updated_at: 2026-05-23T00:00:00Z
---

# Overview architettura Nexus

> Entry-point del meta-vault. Per dettagli specifici naviga le sezioni linkate.

## Struttura ad alto livello

Nexus e' una piattaforma AI orchestrator multi-progetto composta da tre layer:

```
+--------------------------------------------------+
|  apps/web-ide (Next.js)                          |  3000
|  apps/admin (Next.js)                            |  3001
|  apps/nexus-gateway (Node, gateway LLM)          |  4001
+--------------------------------------------------+
        |  HTTP/SSE  |  WebSocket
        v            v
+--------------------------------------------------+
|  crates/mcp-core (Rust, axum)                    |  4000
|  - chat sessions / chat messages                 |
|  - knowledge base per-progetto                   |
|  - meta-docs vault (questo modulo)               |
|  - agent loop + MCP tools (354+ tool)            |
|  - vector_memory (Qdrant client)                 |
|  - routing matrix (cache 60s, DB source)         |
+--------------------------------------------------+
        |  gRPC :50051        |  HTTP :8001
        v                     v
+--------------------------------------------------+
|  brain/ (Python, FastAPI + LangGraph)            |
|  - agents/ (LangGraph nodes)                     |
|  - memory/ (PostgresLearningStorage)             |
|  - providers/ (OpenAI/Anthropic/Google/...)      |
|  - router/ (semantic + agentic_classifier)       |
+--------------------------------------------------+
        |
        v
+--------------------------------------------------+
|  PostgreSQL (5432 prod / 5433 dev docker)        |
|  Qdrant (6333)                                   |
|  Redis (6379)                                    |
+--------------------------------------------------+
```

## Componenti chiave

- **mcp-core** (`crates/mcp-core/`): cuore HTTP/SSE. Gestisce sessioni chat, progetti, agenti, knowledge base, meta-docs, routing matrix. Vedi [[crates-rust]] per dettaglio per crate.
- **nexus-orchestrator** (`crates/nexus-orchestrator/`): worker scheduler + learning loop (Q-learning, anomaly detection, profiling). Vedi [[crates-rust#nexus-orchestrator]].
- **brain** (`brain/`): inferenza AI dietro gRPC. LangGraph per stato conversazione. Vedi [[brain-python]].
- **web-ide** (`apps/web-ide/`): UI principale Next.js. Vedi [[frontend-nextjs]].
- **PostgreSQL**: tutte le tabelle (`projects`, `chat_messages`, `agent_runs`, `settings`, `nexus_routing_matrix`, `nexus_meta_docs`, ...). Vedi [[schema/postgres-tables|Schema Postgres]].
- **Qdrant**: collection `project_code_index`, `project_context`, `prompt_corrections`, `knowledge_notes`, `nexus_meta_docs`. Vedi [[schema/qdrant-collections|Schema Qdrant]].

## Decisioni di design fondanti

- [[adr/0001-provider-abstraction-layer|ADR 0001 - Provider abstraction layer]]
- [[adr/0005-meta-docs-vault|ADR 0005 - Meta-docs vault Obsidian-compatible]]
- Regola "no modelli AI hardcoded": vedi `CLAUDE.md` sezione G.
- Regola "isolamento progetti": vedi `CLAUDE.md` sezione E.

## Flussi principali

Vedi [[data-flow]] per diagrammi sequence dei flussi:

- Chat user message → orchestrator → brain → response
- Agent run → MCP tools → file edit → git commit
- Learning loop → embedding → Q-table update
- Knowledge note creation → Qdrant + filesystem vault

## Entry-point per Claude Code

Quando un agente Claude Code lavora su Nexus, deve:

1. Leggere `CLAUDE.md` (regole vincolanti)
2. Leggere questo file e seguire i link
3. Per ambito specifico, caricare il sub-agent dedicato (vedi `.claude/agents/`)

## Stato di aggiornamento

Questo file e' **curato a mano** (`auto_generated: false`). I file per-crate ([[crates-rust]], [[brain-python]], [[frontend-nextjs]]) sono auto-generati ad ogni commit.
