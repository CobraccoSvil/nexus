---
id: adr-0005-meta-docs-vault
kind: adr
title: "ADR 0005 - Meta-docs vault Obsidian-compatible per il progetto Nexus"
status: accepted
tags: [documentation, obsidian, architecture, automation]
auto_generated: false
created_at: 2026-05-23T00:00:00Z
updated_at: 2026-05-23T00:00:00Z
---

# ADR 0005 - Meta-docs vault Obsidian-compatible per il progetto Nexus

## Stato

Accepted - 2026-05-23

## Contesto

Nexus aveva documentazione del proprio codice frammentata in:

- `README.md` (overview superficiale)
- `CLAUDE.md` (regole vincolanti per agenti)
- `docs/contributing.md`, `docs/runbook.md` (singoli runbook)
- `docs/architecture/overview.md` (un solo file minimalista)
- `docs/adr/0001-provider-abstraction-layer.md` (un solo ADR)

Le decisioni di design recenti (migrazione SQLite to PostgreSQL del learning storage, Knowledge Base per-progetto, fix routing matrix `conversation_summary`, ecc.) non erano catturate in alcun documento autoritativo. Quando Claude Code lavora su Nexus, deve ricostruire il contesto da zero leggendo codice sparso.

## Decisione

Costruiamo una **fonte di verita' unica** sotto `docs/.nexus-vault/`, organizzata come vault Obsidian-compatible con frontmatter YAML standard e wikilink `[[...]]`. Il vault contiene:

- `architecture/` (auto-generata da crate map)
- `adr/` (decisioni di design, curate a mano e/o da `ChangeDrafter`)
- `api/` (auto-generata da axum routers + .proto + AGENT_TOOLS_JSON)
- `schema/` (auto-generata da `information_schema` + Qdrant)
- `runbook/` (curato a mano, deploy/troubleshooting/monitoring)
- `changelog/` (auto-generato da commit con LLM significance >= soglia)
- `decisions/` (auto-estratto da `chat_messages` con pattern decisionali)

Il vault e' la **stessa cosa** su disco e in DB: tabella `nexus_meta_docs` indicizza tutto, collection Qdrant `nexus_meta_docs` permette ricerca semantica. Un file watcher bidirezionale sincronizza disco↔DB con loop detection via SHA-256.

## Conseguenze

### Positive

- **Singola fonte di verita'**: Claude Code carica il vault per orientarsi senza scansionare il codice.
- **Auto-aggiornamento**: ogni `git commit` triggera l'aggiornamento via hook lefthook `post-commit` (failsafe: worker periodico ogni 15 min).
- **Obsidian-native**: l'utente apre la cartella in Obsidian e vede backlinks + graph view senza setup.
- **Portabile**: il vault e' Markdown puro, esportabile/condivisibile come zip.
- **Ricerca semantica**: collection Qdrant permette `search_meta_docs(query)` riusato da `ChangeDrafter` e dai sub-agenti Claude Code.

### Negative

- **Costo LLM aggiuntivo**: `ChangelogGenerator` e `DecisionExtractor` chiamano LLM per ogni commit/giorno. Mitigato via routing matrix (modelli economici come `gpt-4.1-mini`).
- **Drift potenziale**: se l'utente edita file manualmente in Obsidian, il watcher li tratta come `auto_generated: false` per rispettare la modifica. Esiste un piccolo rischio di overwrite se il flag non viene cambiato.
- **Complessita' nuova**: 4 nuove tabelle DB, 5 generator, 1 worker, 1 watcher, 7 sub-agenti, ChangeDrafter UI/API.

### Alternative considerate

1. **mdbook** o **docusaurus**: scartate perche' non Obsidian-native (niente backlinks naturali, niente graph view).
2. **Solo README + docs/**: scartato perche' non scala (gia' frammentato oggi).
3. **Documentazione solo in DB (no filesystem)**: scartato perche' perde portabilita' e l'utente non puo' navigare offline.

## Riferimenti

- Migrazione `db/migrations/0177_nexus_meta_docs.sql`
- Modulo `crates/mcp-core/src/meta_docs/`
- File watcher `crates/mcp-core/src/meta_docs_watcher.rs`
- Worker periodico `crates/nexus-orchestrator/src/workers/meta_docs_refresh_worker.rs`
- Pattern clonato da Knowledge Base per-progetto (commit `59c7fc3`, mig `0175`)
- Sub-agenti Claude Code: `.claude/agents/*.md`
