---
id: bb6cace8-4260-4f38-af20-3f2f2bb1c361
kind: other
title: Routing matrix DB
slug: routing-matrix
tags:
  - concept
  - routing
  - matrix
  - ai
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:01Z
updated_at: 2026-05-23T11:19:44Z
nexus_meta_version: 1
---

# Routing matrix

Tabella `nexus_routing_matrix`: unica fonte di verita' per scegliere quale modello AI usare per ogni richiesta utente.

## Schema

```sql
nexus_routing_matrix (
  intent           TEXT,    -- es. 'fix', 'feature', 'refactor', 'chat', 'docs', ...
  behavior_mode    TEXT,    -- 'bilanciata' | 'veloce' | 'approfondita' | 'economica'
  provider         TEXT,    -- 'openai' | 'anthropic' | 'google' | 'mistral' | 'deepseek'
  model_id         TEXT,    -- nome esatto del modello vendor
  PRIMARY KEY (intent, behavior_mode)
)
```

## API Rust

```rust
let matrix = state.orchestrator.routing_matrix.current_async().await?;
let (provider, model) = matrix.lookup(intent, behavior_mode)
    .unwrap_or_else(|| matrix.default_model("openai"));
```

## Auto-promote / Q-learning

Il worker `routing_matrix_auto_promoter` aggiorna le righe in base a:
- Reward medio (success rate, latenza, costo)
- Cap di costo per intent
- Black-list provider down

Vedi [[multi-provider-routing]], [[postgres-tables]].
