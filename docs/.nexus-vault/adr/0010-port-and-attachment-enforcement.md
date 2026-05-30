---
id: adr-0010
kind: adr
title: "ADR 0010 - Enforcement porte hardcoded e accesso allegati via tool"
status: accepted
tags: [adr, agent-tools, ports, attachments, prompt]
created_at: 2026-05-28T00:00:00Z
updated_at: 2026-05-28T00:00:00Z
---

# ADR 0010 - Enforcement porte hardcoded e accesso allegati via tool

## Stato

Accettato. Migrazione `0192_agent_attachment_tool_directive.sql` applicata.

## Contesto

### Problema A - porte hardcoded

Il bucket porte Nexus (20000-39999) e il tool `request_port(label=...)`
esistono dalla migrazione 0141, e dalla 0191 sono documentati come direttiva
`<port_allocation>` nei system prompt. Nonostante questo gli agenti continuano
a generare sorgenti con `app.listen(3000)`, `s.bind("0.0.0.0:8080")`,
`PORT = 5173`. Il prompt da solo non basta: serve un meccanismo che intercetti
la scrittura e blocchi i casi violanti.

### Problema B - allegati troppo grandi

Da `chat_messages.rs` il prompt iniziale include un blocco `<allegati>` con
metadata + testo inline (fino a 30KB per file). Quando l'utente carica file
grandi (PRD da 80KB, CSV da 200KB, log) il prompt esplode rapidamente, oppure
viene troncato silenziosamente. Non c'e' modo per l'agente di "tornare a
leggere" l'allegato dopo che il messaggio iniziale e' stato emesso, perche'
il tool `read_file` cerca sul filesystem del progetto utente e gli allegati
vivono in `chat_message_attachments` (tabella + path interno mcp-core).

## Decisione

### Fix 1 - port_scanner come hook in write_file/edit_file

Nuovo modulo `crates/mcp-core/src/agent_tools/port_scanner.rs` con:

- `scan_content(path, content) -> PortScanOutcome` che applica una piccola
  batteria di regex (`.listen(NNNN)`, `.bind("...:NNNN")`, `listen=NNNN`,
  `PORT=NNNN`) e ritorna findings per ogni porta hardcoded fuori range
  20000-39999.
- `is_enforcement_enabled(db)` legge il setting `agent.enforce_port_allocation`
  (default `true`) con cache 60s. Niente env var, niente fallback hardcoded
  silenzioso (regola G di `CLAUDE.md`).
- `format_reject_message(...)` produce un messaggio in italiano che istruisce
  l'agente a chiamare `request_port(label=...)`.

Il hook viene chiamato in `tool_write_file` (su `content`) e `tool_edit_file`
(su `new_string`). I file `.env*`, `docker-compose*.yml`, `Dockerfile*` sono
esclusi: in quei file le porte sono attese e gestite a livello infra.

Le righe che leggono `PORT` da env (`process.env.PORT`, `os.environ.get("PORT")`,
`env::var("PORT")`, `getenv("PORT")`, `PORT=$`, `PORT=${`) sono whitelistate:
indicano gia' un comportamento corretto.

### Fix 2 - tool nexus_list_attachments + nexus_read_attachment

Nuovo modulo `crates/mcp-core/src/agent_tools/attachments.rs` espone:

- `nexus_list_attachments(session_id?)`: ritorna `[{ id, file_name, mime_type,
  size_bytes, kind, created_at }, ...]` leggendo da `chat_message_attachments`
  filtrata per session_id (default = sessione corrente) e project_id corrente.
- `nexus_read_attachment(attachment_id, encoding?="auto", offset?=0,
  length?=102400)`: apre il file su disco (campo `file_path`), seek + read di
  max 100KB, ritorna JSON con `content`, `encoding`, `offset`, `length`,
  `total_size`, `truncated`. Encoding `auto` decide testo vs base64 in base
  al MIME (`text/*`, `application/json|xml|...`).

In parallelo `chat_messages.rs` evolve il blocco `<allegati>`: se la somma dei
contenuti supera 50KB, mostra solo i metadata e istruisce l'agente a usare
`nexus_read_attachment`. Sotto soglia continua il comportamento inline (30KB
per file) ma il messaggio di truncation rimanda comunque ai tool per il resto.

La direttiva `<attachment_access>` viene aggiunta (mig 0192) ai system prompt
`system.nexus_base` e `agent.coder.base` per spiegare il flusso.

## Contratto pubblico

### Setting

- `settings.key = 'agent.enforce_port_allocation'`, value `'true'|'false'`,
  default `'true'`. Cache 60s lato Rust.

### Tool MCP

- `nexus_list_attachments(session_id?: uuid) -> { count, attachments[] }`
- `nexus_read_attachment(attachment_id: uuid, encoding?, offset?, length?) ->
  { id, name, mime_type, encoding, offset, length, total_size, truncated, content }`

### Comportamento dell'agente atteso

- Prima di `write_file`/`edit_file` di codice server: assicurarsi che la porta
  venga letta da env oppure dal valore ritornato da `request_port`.
- Se il blocco `<allegati>` mostra "non incluso" o un file viene troncato:
  chiamare `nexus_list_attachments` -> `nexus_read_attachment`.

## Conseguenze

### Positive

- Le porte hardcoded vengono bloccate alla scrittura, non a posteriori. Niente
  conflitti tra progetti che girano sulla stessa macchina.
- Gli allegati grandi non saturano piu' il context window: solo cio' che
  serve viene letto.
- Tutta la configurazione passa per il DB, niente env var (regola G).

### Negative

- Falsi positivi possibili (es. costanti `MAX_PORT = 65535` o documentazione
  inline). Mitigazione: esclusione dei file `.env*` / `docker-compose*` /
  `Dockerfile*`, esclusione porte < 1024 (riservate, tipicamente documentali),
  whitelist righe che leggono da env. Il setting puo' essere disabilitato per
  debug locale.
- Latenza aggiuntiva su ogni `write_file`/`edit_file`: trascurabile (regex su
  contenuto in memoria, lookup setting cacheato).

## Riferimenti file

- `crates/mcp-core/src/agent_tools/port_scanner.rs`
- `crates/mcp-core/src/agent_tools/attachments.rs`
- `crates/mcp-core/src/agent_tools/files.rs` (hook in `tool_write_file` /
  `tool_edit_file`)
- `crates/mcp-core/src/agent_tools/mod.rs` (registro JSON + dispatch)
- `crates/mcp-core/src/chat_messages.rs` (blocco `<allegati>` evoluto)
- `db/migrations/0192_agent_attachment_tool_directive.sql`
- `db/migrations/0191_agent_port_registry_directive.sql` (direttiva
  `<port_allocation>` precedente, complementare)
- `crates/mcp-core/src/agent_tools/ports.rs` (tool `request_port`)
