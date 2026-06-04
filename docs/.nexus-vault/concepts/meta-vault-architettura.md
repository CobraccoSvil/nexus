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
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:08:59Z
updated_at: 2026-06-04T10:32:59Z
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
- **changelog/** - entry auto da commit significativi
- **decisions/** - decisioni estratte da chat utente
- **concepts/** - note concettuali (questa nota stessa)

## Pipeline auto-update

1. Sviluppatore fa `git commit`
2. Hook lefthook `post-commit` chiama `POST /api/meta-docs/ingest-commit`
3. mcp-core dispatcia 6 generator in parallelo:
   - SchemaGenerator
   - ArchitectureGenerator
   - ApiGenerator
   - ChangelogGenerator (LLM significance)
   - DecisionExtractor (LLM su chat_messages)
   - ConceptsGenerator (questo)
4. Ogni generator produce 1+ note `.md`
5. Hash-based loop detection: skip se il contenuto non e' cambiato
6. File watcher bidirezionale per modifiche manuali in Obsidian

Vedi [[adr-0005-meta-docs-vault]] per design rationale.

## Tabelle correlate

- `nexus_meta_docs` - le note del meta-vault
- `nexus_meta_doc_links` - relazioni (auto da wikilink + semantic via embedding)
- `nexus_meta_doc_changes` - commit processati

Vedi [[postgres-tables]].
