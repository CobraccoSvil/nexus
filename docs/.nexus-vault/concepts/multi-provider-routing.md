---
id: a792053a-e025-441d-9022-882058c60e1c
kind: other
title: Routing multi-provider AI
slug: multi-provider-routing
tags:
  - concept
  - routing
  - provider
  - ai
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:08:59Z
updated_at: 2026-05-28T12:24:17Z
nexus_meta_version: 1
---

# Routing multi-provider AI

Nessun nome modello AI e' hardcoded nel codice Nexus. La scelta di provider+modello viene fatta a runtime da un **routing layer** basato su tabelle DB.

## Tabelle chiave

- `nexus_routing_matrix` - mappa `(intent, behavior_mode) -> (provider, model_id)` per le richieste utente.
- `nexus_purpose_model` - mappa `purpose -> (provider, model_id)` per task interni (changelog_significance, decision_extractor, change_drafter, autofix_planner, embedding, ecc.).
- `nexus_provider_default_model` - fallback se non esiste mapping specifico.

## Cache Rust

`crates/mcp-core/src/routing_matrix.rs` mantiene una cache in memoria (TTL 60s) per evitare query DB ad ogni inferenza. Refresh automatico in background.

## Vantaggi

- **Switch provider on-the-fly**: cambi il mapping DB, niente redeploy.
- **A/B testing**: routing matrix supporta varianti per percentuale di traffico.
- **Cost optimization**: i Q-learning workers possono auto-promuovere modelli economici quando si dimostrano sufficienti.

Vedi [[adr-0001-provider-abstraction-layer]] e [[routing-matrix]].

## Behavior modes

- `bilanciata` (default)
- `veloce` (modelli economici/fast)
- `approfondita` (modelli top-tier)
- `economica` (cap di costo aggressivo)

L'utente sceglie via dropdown nel composer chat.
