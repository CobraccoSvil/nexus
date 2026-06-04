---
id: 487f21ed-b95f-4c48-b3b5-aaebf513f462
kind: other
title: Sub-agenti Claude Code (.claude/agents/)
slug: sub-agents-claude-code
tags:
  - concept
  - claude-code
  - agent
  - ai
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:00Z
updated_at: 2026-06-04T09:10:13Z
nexus_meta_version: 1
---

# Sub-agenti Claude Code

Set di 7 sub-agenti specializzati registrati in `.claude/agents/*.md`. Vengono spawnati automaticamente da Claude Code quando la richiesta tocca un ambito specifico.

## Catalogo

- **nexus-rust-implementer** - backend Rust (crates/)
- **nexus-python-implementer** - brain Python
- **nexus-frontend-implementer** - apps/web-ide
- **nexus-db-architect** - migrazioni Postgres, Qdrant
- **nexus-doc-writer** - vault meta (docs/.nexus-vault/)
- **nexus-test-author** - test (Playwright, Rust, Python)
- **_nexus-orchestrator** - meta-agent per task multi-ambito

## Pattern

Ogni sub-agent:
1. Ha `description` con trigger semantici
2. Ha `tools` whitelist (subset MCP)
3. Carica il meta-vault prima di proporre modifiche
4. Restituisce un diff + razionale al main agent

Combinato con [[change-drafter]] forma il workflow di modifica codice supervisionata.
