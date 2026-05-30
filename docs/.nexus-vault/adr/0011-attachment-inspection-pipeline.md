---
id: adr-0011
kind: adr
title: "ADR 0011 - Pipeline di ispezione e estrazione allegati"
status: accepted
tags: [adr, agent-tools, attachments, prompt, ingestion]
created_at: 2026-05-28T00:00:00Z
updated_at: 2026-05-28T00:00:00Z
---

# ADR 0011 - Pipeline di ispezione e estrazione allegati

## Stato

Accettato. Migrazione `0193_attachment_inspection_directive.sql` applicata.
Estende ADR 0010.

## Contesto

ADR 0010 ha introdotto `nexus_list_attachments` e `nexus_read_attachment` per
consentire all'agente di leggere allegati grandi a richiesta. Restava aperto
il caso degli allegati con MIME ambiguo o estensione non standard.

Caso reale: l'utente carica `PL.make` (2.2 MB, MIME
`application/octet-stream`). In realta' e' uno ZIP che contiene
`canvas.fig` (formato Figma). Il modello (in questo caso Mistral) si arrende
con "non posso leggere file binari" invece di:

1. Ispezionare i magic bytes -> e' uno ZIP.
2. Esplorare il contenuto -> dentro c'e' `canvas.fig`.
3. Estrarre il payload Figma -> stringhe leggibili + hint per usare Figma API.

`nexus_read_attachment(encoding="base64")` ritorna i byte grezzi: utili solo
se l'agente sa gia' come decodificarli. Niente nel prompt diceva "fai magic
byte detection prima di rinunciare".

## Decisione

Si introduce una pipeline a due livelli con sette nuovi tool MCP, tutti
isolati per `project_id`, tutti con limiti configurabili da DB.

### Livello 1 - magic byte detection

`nexus_inspect_attachment(attachment_id)` legge i primi 32 KB del file,
applica `infer` (crate magic byte) + euristica testuale e ritorna:

```json
{
  "id": "...",
  "name": "PL.make",
  "size_bytes": 2269841,
  "mime_dichiarato": "application/octet-stream",
  "mime_reale": "application/zip",
  "kind": "figma",
  "extension_reale": "zip",
  "is_text": false,
  "extraction_tools": ["nexus_extract_figma_structure", "nexus_list_archive_entries"],
  "hint": "File Figma rilevato..."
}
```

Il `kind` e' un'etichetta logica (`zip`, `tar`, `gzip`, `pdf`, `docx`, `xlsx`,
`pptx`, `figma`, `png`, `jpeg`, `gif`, `webp`, `svg`, `mp3`, `mp4`, `wav`,
`json`, `xml`, `markdown`, `html`, `css`, `javascript`, `typescript`,
`python`, `rust`, `go`, `java`, `c`, `cpp`, `sql`, `toml`, `yaml`, `csv`,
`text`, `binary`). Lo ZIP esterno viene ispezionato per discriminare
`docx`/`xlsx`/`pptx`/`figma`/`zip` cercando entries note (`word/document.xml`,
`xl/workbook.xml`, `canvas.fig`, ecc.).

### Livello 2 - tool di estrazione specializzati

| Tool | Formati | Output |
|---|---|---|
| `nexus_list_archive_entries` | zip, tar, tar.gz | array `{name,size,is_dir,...}`, max 1000 entries |
| `nexus_read_archive_entry` | zip, tar, tar.gz | contenuto entry, max 200 KB |
| `nexus_extract_pdf_text` | pdf | testo + total_pages + is_scanned_pdf hint |
| `nexus_extract_docx_text` | docx | testo paragrafi |
| `nexus_extract_xlsx_data` | xlsx | array righe/celle, max 1000 righe |
| `nexus_extract_figma_structure` | figma | payload canvas.fig + stringhe ASCII + hint |

Tutte le operazioni di parsing (zip, tar, pdf-extract, quick-xml) sono CPU-bound
ed eseguite in `tokio::task::spawn_blocking` per non bloccare la runtime
async.

### Livello 3 - direttiva di prompt

Il system prompt `system.nexus_base` e `agent.coder.base` ricevono un blocco
`<attachment_investigation>` (mig 0193) che istruisce esplicitamente:

> Se vedi un allegato con MIME `application/octet-stream` o estensione
> sospetta (`.make`, `.dat`, `.bin`, `.pkg`, `.fig`), NON dichiarare subito
> "non posso leggerlo". Chiama prima `nexus_inspect_attachment` e usa il
> tool di estrazione corrispondente al `kind` rilevato.

In parallelo il blocco `<allegati>` del prompt iniziale ora stampa il campo
`ID: <uuid>` per ogni allegato + un suggerimento `nexus_inspect_attachment`
inline, evitando un round-trip via `nexus_list_attachments` quando l'agente
ha gia' l'id sotto agli occhi.

## Contratto pubblico

### Tool MCP

- `nexus_inspect_attachment(attachment_id: uuid) -> { kind, mime_reale, extraction_tools[], hint, ... }`
- `nexus_list_archive_entries(attachment_id: uuid) -> { format, total_entries, entries[] }`
- `nexus_read_archive_entry(attachment_id: uuid, entry_path: string, encoding?: "auto"|"text"|"base64") -> { content, total_size, truncated }`
- `nexus_extract_pdf_text(attachment_id: uuid, page_start?: int, page_end?: int) -> { total_pages, pages_extracted, text, is_scanned_pdf? }`
- `nexus_extract_docx_text(attachment_id: uuid) -> { paragraphs_count, text }`
- `nexus_extract_xlsx_data(attachment_id: uuid, sheet_name?: string) -> { sheet, rows_count, rows[][] }`
- `nexus_extract_figma_structure(attachment_id: uuid) -> { format, ... }` (vedi sezione "Figma Make handling" sotto)

### Settings (tabella `settings`, cache 60s, mig 0193 + 0196)

| Key | Default | Descrizione |
|---|---|---|
| `agent.attachment.archive_entry_max_bytes` | `204800` | Max byte letti per una entry archivio. |
| `agent.attachment.archive_max_entries` | `1000` | Max entries elencate per archivio. |
| `agent.attachment.pdf_max_text_bytes` | `102400` | Max byte testo PDF estratto. |
| `agent.attachment.xlsx_max_rows` | `1000` | Max righe XLSX estratte. |
| `agent.attachment.figma_max_bytes` | `51200` | Max byte payload `canvas.fig` (fallback legacy). |
| `agent.attachment.figma_make_ai_chat_max_load_bytes` | `5242880` | Max byte caricati da `ai_chat.json` prima del parsing. |
| `agent.attachment.figma_make_chat_messages_max_chars` | `51200` | Max caratteri cumulativi dei messaggi chat AI Figma Make. |
| `agent.attachment.figma_make_chat_messages_max_count` | `20` | Max numero messaggi user+assistant restituiti. |
| `agent.attachment.figma_make_assistant_message_max_chars` | `2000` | Truncatura per singolo messaggio assistant. |

### Figma Make handling (mig 0196)

I file `.make` Figma Make sono archivi ZIP con la seguente struttura tipica:

```
canvas.fig          binario proprietario opaco (magic `fig-makej`)
thumbnail.png       preview del design
meta.json           { client_meta.render_coordinates, file_name, exported_at }
ai_chat.json        thread chat AI con il PROMPT ORIGINALE dell'utente
images/*            asset PNG/JPG
blob_store/*        contenuti chat
make_binary_files/* binari ausiliari
```

Il contenuto **autoritativo** e' `ai_chat.json`. Il parser proprietario per
`canvas.fig` non esiste pubblicamente, ma il thread chat contiene la
specifica originale dell'app (il prompt che l'utente ha dato a Figma Make).
Il tool `nexus_extract_figma_structure` ora applica questa pipeline:

1. **Apre ZIP**. Se non e' ZIP, fallback a `figma_binary_legacy` su payload raw.
2. **Indicizza** entry note: `ai_chat.json`, `meta.json`, `thumbnail.png`,
   `canvas.fig`, conteggio file in `images/*`.
3. **Caso Figma Make** (presenza `ai_chat.json`):
   - Legge `meta.json` (fino a 64 KB) e ne estrae `file_name`, `exported_at`,
     `client_meta.render_coordinates`.
   - Legge `ai_chat.json` (fino a `figma_make_ai_chat_max_load_bytes`).
   - Per ogni `threads[].messages[]` con `role in {user, assistant}` e ogni
     `parts[]` con `partType == "text"`, parsa `contentJson` come stringa JSON
     ed estrae il campo interno `.text`.
   - Applica i tre cap: `chat_messages_max_count`, `chat_messages_max_chars`
     cumulativi, `assistant_message_max_chars` per-messaggio. I messaggi
     `user` non sono mai troncati singolarmente (prompt autoritativo).
   - Ritorna JSON con `format=figma_make`, `meta`, `chat_messages[]`,
     `primary_content="chat_messages"`, flag di truncatura, `thumbnail_hint`.
4. **Caso Figma binario legacy** (no `ai_chat.json`, solo `canvas.fig`):
   ritorna `format=figma_binary_legacy` + `extracted_strings[]` + flag
   `extracted_strings_fallback=true`.
5. **Caso archivio non riconosciuto** (no `ai_chat.json` ne' `canvas.fig`):
   errore esplicito (mai inghiottito in silenzio).

Esempio di output (`figma_make`):

```json
{
  "format": "figma_make",
  "meta": {
    "file_name": "PL",
    "exported_at": "2026-05-27T10:00:00Z",
    "dimensions": { "w": 1024, "h": 768 }
  },
  "chat_messages": [
    { "role": "user", "text": "Il Prompt per lo Sviluppatore: ..." },
    { "role": "assistant", "text": "Ti propongo questa architettura: ..." }
  ],
  "chat_messages_count": 2,
  "chat_messages_truncated": false,
  "ai_chat_truncated_at_load": false,
  "thumbnail_available": true,
  "thumbnail_hint": "ZIP Figma Make contiene thumbnail.png ma non e' direttamente ispezionabile dai tool standard. Per analisi visiva chiedi all'utente di esportare il design come PNG separato e ricaricarlo.",
  "canvas_available": true,
  "images_count": 4,
  "primary_content": "chat_messages",
  "hint": "Contenuto primario in 'chat_messages': ..."
}
```

Il helper `extract_figma_strings_inline(file_path, max_chars)` (FIX 3 ADR
0012) usa la stessa pipeline ma rende testo markdown-style per inclusione
diretta nel blocco `<allegati>` del prompt iniziale.

### Modifica struct `ChatAttachment`

Aggiunto campo `id: Option<Uuid>`. Popolato in `chat_messages.rs` tramite
`enrich_attachments_with_ids(...)` dopo `persist_message_attachments`. Il
blocco `<allegati>` del prompt iniziale stampa quindi l'UUID accanto al nome.

## Conseguenze

### Positive

- Il modello non si arrende piu' su MIME ambiguo: ha sempre un percorso da
  seguire (`nexus_inspect_attachment` -> tool specializzato).
- Niente nomi modello / provider hardcoded: tutti i limiti sono in DB.
- Niente env var: lo schema settings e' coerente con la regola G di
  `CLAUDE.md`.
- Tutte le operazioni CPU-bound sono isolate in `spawn_blocking` -> la
  runtime axum resta responsive.
- I parser (zip, tar, pdf-extract, quick-xml) sono crate maturi e
  audit-ed, niente FFI a tool esterni.

### Negative

- `pdf-extract` carica l'intero PDF in memoria (limite di fatto ~50MB per
  l'upload chat, accettabile).
- Figma MVP: ritorna solo stringhe + hint. Per una vera ricostruzione di
  frame/componenti serve un parser dedicato del formato proprietario, fuori
  scope ADR 0011.
- Tre crate aggiuntivi a build (`pdf-extract`, `zip`, `infer`, `quick-xml`,
  `tar`, `flate2`) -> aumento di 2-3 secondi al tempo di compilazione
  incrementale di `mcp-core`.

## Riferimenti file

- `crates/mcp-core/src/agent_tools/attachment_inspector.rs`
- `crates/mcp-core/src/agent_tools/archive_tools.rs`
- `crates/mcp-core/src/agent_tools/document_tools.rs`
- `crates/mcp-core/src/agent_tools/figma_tools.rs`
- `crates/mcp-core/src/agent_tools/attachment_settings.rs`
- `crates/mcp-core/src/agent_tools/mod.rs` (registro + dispatch)
- `crates/mcp-core/src/chat_messages.rs` (`enrich_attachments_with_ids`,
  blocco `<allegati>` con `ID:`)
- `crates/mcp-core/src/orchestrator.rs` (`ChatAttachment.id`)
- `db/migrations/0193_attachment_inspection_directive.sql`
- `db/migrations/0196_figma_make_pipeline_settings.sql`
