# Architecture Overview

AI-Orchestrator v2 is organized around a policy-driven orchestration core.

## Layers

- `Client layer`: Web IDE, VS Code extension, and CLI
- `Rust core`: orchestration, policy enforcement, indexing, quality engines, chat audit
- `Python neural core`: provider access, embeddings, structured outputs, routing
- `Persistence`: PostgreSQL, Redis, Qdrant, Shadow DB

## Principles

- Rust-first runtime for deterministic orchestration and heavy parsing
- DB-first prompt/provider/policy lifecycle
- MCP read-first access model for orchestration
- Structured outputs for routing, fixes, and pattern review
- Full auditability for every orchestrated action

