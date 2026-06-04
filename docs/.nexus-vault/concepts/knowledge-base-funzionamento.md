---
id: 64b11844-a9f7-45a3-96f1-b037572ec083
kind: other
title: Knowledge Base per-progetto
slug: knowledge-base-funzionamento
tags:
  - concept
  - kb
  - knowledge
  - obsidian
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:08:59Z
updated_at: 2026-06-04T10:27:25Z
nexus_meta_version: 1
---

# Knowledge Base per-progetto

Ogni progetto registrato in Nexus ha una **Knowledge Base auto-aggiornata** che cattura:

- **Note funzionali** create manualmente (Feature, Requirement, Decisione, Dominio, User Story, Architettura)
- **Note auto** create da ogni messaggio chat dell'utente (intent classificato da LLM)
- **Tag** aggregati da contenuti
- **Link automatici** tra note simili (via embedding Qdrant + soglia di similarita')

## Sincronizzazione vault Obsidian

Ogni progetto ha una cartella `.nexus/knowledge/` sincronizzata bidirezionalmente:
- **DB -> filesystem**: ogni nota viene scritta come file `.md` Obsidian-compatible
- **filesystem -> DB**: un file watcher rileva modifiche manuali (es. da Obsidian) e aggiorna DB

Vedi [[adr-0003-knowledge-base-obsidian-compat]].

## Struttura tabelle

- `project_knowledge_notes` - le note
- `project_knowledge_links` - relazioni tra note
- `project_knowledge_tags` - tag aggregati

Vedi [[postgres-tables]] per dettagli schema.

## Differenza con meta-vault

Il **meta-vault Nexus** (`docs/.nexus-vault/`) documenta NEXUS STESSO (architettura, ADR, runbook).
La **KB per-progetto** documenta UN SINGOLO PROGETTO gestito da Nexus.

Vedi [[meta-vault-architettura]].
