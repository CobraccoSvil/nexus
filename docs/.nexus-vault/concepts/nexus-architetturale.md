---
id: 7b4074ac-7ab5-4233-87b2-dcd7e960cd8b
kind: other
title: Architettura di Nexus (vista architetturale)
slug: nexus-architetturale
tags:
  - concept
  - architettura
  - overview
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:08:59Z
updated_at: 2026-06-04T08:31:40Z
nexus_meta_version: 1
---

# Architettura di Nexus

Sistema tri-layer fortemente disaccoppiato. Vedi [[overview]] per il diagramma a blocchi.

## Layer

### 1. Frontend (Next.js)

- **web-ide** (`apps/web-ide`): UI principale (chat, file editor, terminal, knowledge panel).
- **admin** (in `apps/web-ide/app/admin`): pannello amministrativo (settings, billing, orchestrator, meta-docs).
- **landing** (`apps/landing`): sito vetrina pubblico.

Vedi [[frontend-nextjs]].

### 2. Backend orchestrazione (Rust)

- **mcp-core** (`crates/mcp-core`): cuore HTTP/SSE, agent loop, MCP tools (350+).
- **nexus-orchestrator** (`crates/nexus-orchestrator`): scheduler + 14 worker (Q-learning, anomaly, profiling, ecc.).
- **microservizi** (`crates/mcp-ast`, `mcp-quality`, `mcp-comments`, ecc.): tool dedicati gRPC.

Vedi [[crates-rust]].

### 3. Brain (Python + FastAPI)

- **LangGraph**: state machine per conversazioni agente.
- **Provider abstraction**: gateway unificato verso tutti i provider AI.
- **Embedding service**: `sentence-transformers/all-MiniLM-L6-v2` (384 dim).

Vedi [[brain-python]].

## Decisioni fondanti

- [[adr-0001-provider-abstraction-layer]] - Provider abstraction multi-LLM.
- [[adr-0005-meta-docs-vault]] - Meta-vault Obsidian-compatible.
- [[routing-matrix]] - Nessun modello AI hardcoded.
- [[isolamento-progetti]] - Ogni progetto e' un mondo a se'.

## Persistenza

- **PostgreSQL** (porta 5433 dev, 5432 prod): tutte le tabelle stato.
- **Qdrant** (porta 6333): collection vettoriali per RAG e semantica.
- **Redis** (porta 6379): cache + pub/sub.

Vedi [[postgres-tables]], [[qdrant-collections]].
