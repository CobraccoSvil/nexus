# Architecture Overview

AI-Orchestrator v2 is organized around a policy-driven orchestration core.

## Layers

- `Client layer`: Web IDE, VS Code extension, and CLI
- `Rust core`: orchestration, policy enforcement, indexing, quality engines, chat audit
- `Python neural core`: provider access, embeddings, structured outputs, routing
- `Persistence`: PostgreSQL, Redis, Qdrant, Shadow DB

## Principles

- Rust-first runtime for deterministic orchestration and heavy parsing
- DB-first prompt/provider/policy lifecycle — modelli AI mai hardcoded
  (vedi `CLAUDE.md` §G: registry DB unica fonte; `nexus_routing_matrix`,
  `nexus_purpose_model`, `nexus_provider_default_model`, `ai_price_catalog`
  con capability `thinking` per la categoria — mig 0170).
- MCP read-first access model for orchestration
- Structured outputs for routing, fixes, and pattern review
- Full auditability for every orchestrated action
- No magic fallback: errori esplicitamente propagati invece di degradare a
  comportamenti silenziosi (es. `routing_config` Rust solleva quando le
  chiavi `routing.classifier_*` mancano da `settings`; `brain/router/service`
  Python ritorna sentinelle `__router_unavailable__` / `__no_capable_provider__`).

## Pacchetti TypeScript hybrid LLM

I pacchetti `packages/{llm-gateway, embeddings, rag, audit}` implementano il
piano `docs/nexus-hybrid-llm-plan.md`. Stato attuale:

- `llm-gateway/`: provider abstraction completa (fasi 1-2 ADR 0001).
- `embeddings/`: scheletro ONNX runner + chunker + reranker (~330 LOC).
- `rag/`: scheletro ingestion + retrieval + hybrid-search (~320 LOC).
- `audit/`: scheletro Langfuse client + anomaly detector + DLP scanner +
  audit writer + logger (~510 LOC).

Le fasi 3-7 del piano (integrazione cross-layer, Presidio redaction, vLLM
portability) sono pianificate ma non eseguite — vedi
`docs/backlog-closure-2026-05-19.md`.

