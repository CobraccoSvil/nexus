---
id: cce427a7-d930-4840-b5c8-9e0308899b4a
kind: other
title: Cosa fa Nexus (funzionale)
slug: nexus-funzionale
tags:
  - concept
  - funzionale
  - overview
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:08:58Z
updated_at: 2026-05-23T12:20:34Z
nexus_meta_version: 1
---

# Cosa fa Nexus (vista funzionale)

Nexus e' una **piattaforma AI orchestrator multi-progetto** che aiuta sviluppatori e team a:

## Capacita' principali

- **Gestire molteplici progetti software** in un unico hub, con isolamento totale (codice, sessioni chat, credenziali, knowledge base).
- **Chattare con AI multi-provider** (OpenAI, Anthropic, Google, DeepSeek, Mistral) usando una matrice di routing che sceglie il modello migliore per ogni intento.
- **Eseguire agenti autonomi** (Coder, Tester, Reviewer, Architect, SecurityAuditor, ecc.) che leggono/modificano il codice del progetto via MCP tools.
- **Memorizzare la conoscenza del progetto** in una Knowledge Base auto-aggiornata, navigabile come vault Obsidian (vedi [[knowledge-base-funzionamento]]).
- **Documentare automaticamente il proprio codice** (meta-vault Nexus) con architettura, ADR, API, schema DB, changelog, decisioni estratte da chat.
- **Apprendere dagli outcomes** via Q-learning + feedback workers che migliorano routing e prompt nel tempo.

## Stakeholder

- **Utenti finali**: sviluppatori che vogliono un IDE web con AI integrata multi-progetto.
- **Team leader**: vogliono telemetria/billing/governance su uso AI.
- **Admin/DevOps**: gestiscono provider, policy, deploy.

## Casi d'uso tipici

1. **Onboarding rapido**: importa un repo Git, Nexus indicizza il codice e prepara KB.
2. **Implementazione feature**: chat porta avanti task multi-step con agenti (vedi [[change-drafter]]).
3. **Code review automatica**: SecurityAuditor + Reviewer analizzano PR.
4. **Doc auto-generata**: meta-vault (vedi [[meta-vault-architettura]]) si aggiorna ad ogni commit.

Vedi anche: [[nexus-architetturale]], [[knowledge-base-funzionamento]], [[multi-provider-routing]].
