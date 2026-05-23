# Nexus Meta-Vault

Questa cartella e' un **vault Obsidian-compatible** che documenta il meta-progetto Nexus (la piattaforma stessa, non i progetti utente registrati al suo interno).

## Cos'e'

Una **fonte di verita' unica** sullo stato del codice Nexus:

- **architecture/** — come e' fatto il sistema (crates Rust, brain Python, frontend Next.js, flussi dati)
- **adr/** — decisioni di design (Architecture Decision Records)
- **api/** — endpoint REST/gRPC, MCP tools, settings keys
- **schema/** — tabelle Postgres, migrazioni, collection Qdrant
- **runbook/** — deploy, troubleshooting, monitoring
- **changelog/** — entry auto-generate per commit significativi
- **decisions/** — decisioni estratte automaticamente da conversazioni chat

## Come si aggiorna

Il vault si aggiorna **da solo** ad ogni `git commit` (via hook lefthook `post-commit`). I generator backend (in `crates/mcp-core/src/meta_docs/generators/`) ri-leggono il codice sorgente e aggiornano le note pertinenti. Un file watcher bidirezionale mantiene allineato disco e DB.

Se modifichi un file `.md` qui dentro a mano (anche da Obsidian), il watcher rileva la modifica entro 500ms e aggiorna il DB.

## Come aprire in Obsidian

1. Apri Obsidian.
2. `File -> Open vault -> Open folder as vault`.
3. Seleziona questa cartella: `/home/administrator/ideai/docs/.nexus-vault/`.
4. Lascia abilitati i plugin `Backlinks` e `Graph view` (gia' configurati).

## Convenzioni

- Ogni nota ha frontmatter YAML con `id`, `kind`, `tags`, `auto_generated`, `created_at`, `updated_at`.
- I wikilink `[[2026-05-23-knowledge-meta-progetto]]` referenziano altre note del vault.
- I file `auto_generated: true` vengono sovrascritti dai generator. Per editare a mano una nota auto-generata, cambia `auto_generated: false` nel frontmatter; il watcher rispettera' la modifica.
- Nuovi ADR seguono la numerazione `NNNN-titolo-kebab-case.md` in `adr/`.

## Configurazione

Le soglie sono in tabella `settings` (chiavi `meta_docs.*`). Esempi:

- `meta_docs.changelog_min_significance` (default `0.4`): soglia LLM sotto cui un commit non genera entry changelog.
- `meta_docs.refresh_worker_interval_secs` (default `900`): failsafe ogni 15 minuti.
- `meta_docs.autofix_enabled` (default `true`): abilita le PR automatiche di NexusAutoFixAgent.

Modificale via UI admin (`/admin/settings`) o direttamente in DB. Le modifiche sono lette dalla cache (TTL 60s).
