---
id: 83c06f34-52b0-46e2-be58-0bc950390087
kind: other
title: Meta-vault Nexus (questa stessa doc)
slug: meta-vault-architettura
tags:
  - concept
  - meta-vault
  - obsidian
  - documentation
source_files:
  - crates/nexus-wiki/src/watcher.rs
  - crates/nexus-wiki/src/reingest.rs
auto_generated: false
created_at: 2026-05-23T11:08:59Z
updated_at: 2026-07-02T00:00:00Z
nexus_meta_version: 1
---

# Meta-vault Nexus

La documentazione del **progetto Nexus stesso** vive in `docs/.nexus-vault/` come vault Obsidian.

## Cosa contiene

- **architecture/** - mappa di crate Rust, moduli Python, app frontend
- **adr/** - decisioni di design (ADR)
- **api/** - endpoint REST, MCP tools, settings keys
- **schema/** - tabelle Postgres, migrazioni, Qdrant
- **runbook/** - deploy, troubleshooting, monitoring
- **changelog/** - entry storiche generate dai commit (pipeline ritirata, vedi sotto)
- **decisions/** - decisioni estratte da chat utente
- **concepts/** - note concettuali (questa nota stessa)

## Pipeline di ingestione (post ADR 0017 v2)

La pipeline commit-based originale (hook lefthook `post-commit` ->
`POST /api/meta-docs/ingest-commit` -> 6 generator automatici) e' stata
ritirata con [[0017-knowledge-graph-parita]]: il modulo
`crates/mcp-core/src/meta_docs/` e' stato rimosso e l'endpoint risponde
410 Gone (`migration_adr: 0017`). Le note changelog/decisions auto-generate
dai commit non hanno sostituto: i documenti si scrivono come file `.md`
nel vault (a mano o via agenti).

Il vault e' ingerito dal sistema wiki unificato:

1. **Watcher filesystem** (`crates/nexus-wiki/src/watcher.rs`, notify +
   debounce): ogni `.md` creato/modificato in `docs/.nexus-vault/` (o nei
   vault per progetto) viene reingerito in `wiki_docs` + Qdrant
   `wiki_content` senza passare dalla UI.
2. **Reingest one-shot** (`crates/nexus-wiki/src/reingest.rs`): bootstrap
   automatico se `wiki_docs` e' vuoto all'avvio, oppure on-demand via
   `POST /api/wiki/reingest`.
3. **Link**: wikilink resolver + linking semantico (cosine >= 0.60) nel
   `links_worker`.

Vedi [[adr-0005-meta-docs-vault]] per il design originale (storico) e
[[0017-knowledge-graph-parita]] per l'architettura corrente.

## Tabelle correlate

- `wiki_docs` - documenti unificati (scope `meta` per questo vault)
- `wiki_links` - relazioni (wikilink risolti + semantic via embedding)
- `wiki_concept_triples` - triple concettuali estratte via LLM
- `wiki_doc_revisions` - storico revisioni

Le tabelle legacy `nexus_meta_docs` / `nexus_meta_doc_links` /
`nexus_meta_doc_changes` sono state droppate (mig 0295-0298).

Vedi [[postgres-tables]].
