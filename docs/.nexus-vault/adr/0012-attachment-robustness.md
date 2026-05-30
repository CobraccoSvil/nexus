---
id: adr-0012
kind: adr
title: "ADR 0012 - Robustezza pipeline allegati (next_action, cache, pre-extract, budget)"
status: accepted
tags: [adr, agent-tools, attachments, context-window, anti-loop, ingestion]
created_at: 2026-05-28T00:00:00Z
updated_at: 2026-05-28T00:00:00Z
---

# ADR 0012 - Robustezza pipeline allegati

## Stato

Accettato. Migrazione `0195_attachment_robustness_settings.sql` applicata.
Estende ADR 0010 (lista/lettura allegati) e ADR 0011 (magic byte detection +
estrattori strutturati).

## Contesto

Test E2E con `PL.make` (2.2 MB ZIP contenente `canvas.fig` Figma binario)
ha mostrato un fallimento sistemico:

1. Gemini 2.5 Pro chiama `nexus_read_archive_entry` per `canvas.fig` con
   offset progressivi 4+ volte (legge il binario raw in base64).
2. Loop detection scatta su deepseek-reasoner (3+ chiamate stesso tool con
   stessa signature).
3. Context window esplode a 216% (280K token).
4. Fallback OpenAI fallisce (quota 429).
5. Risposta finale: `[Error: Provider openai non raggiungibile]`.

### Cause radice (vedi sezione H CLAUDE.md, niente toppe)

- **C1**: `nexus_inspect_attachment` ritorna un array `extraction_tools`
  ma NON indica quale chiamare per primo. Il modello ha scelto la lettura
  raw invece del piu' adeguato `nexus_extract_figma_structure`.
- **C2**: `nexus_read_archive_entry` e `nexus_read_attachment` non
  deduplicano. Stessa richiesta serve byte identici N volte.
- **C3**: Per i kind noti (PDF/DOCX/Figma) la prima azione utile e' SEMPRE
  l'estrazione strutturata. Il sistema lasciava decidere al modello.
- **C4**: Manca un cap di budget letture per sessione. Il modello poteva
  leggere 10+ MB di base64 cumulativi prima di andare in context overflow.

## Decisione

Si introducono **4 fix strutturali**, tutti configurabili via DB.

### FIX 1 — Inspector con `next_action_recommended`

`crates/mcp-core/src/agent_tools/attachment_inspector.rs` ora ritorna,
oltre all'array `extraction_tools`, un campo `next_action_recommended`
con:

```json
{
  "tool": "nexus_extract_figma_structure",
  "input": { "attachment_id": "..." },
  "rationale": "Archivio contiene canvas.fig. Letture raw del binario sono inutili...",
  "expected_tokens_output": 5000
}
```

Mapping per kind:

| kind | tool consigliato | rationale |
|---|---|---|
| figma | `nexus_extract_figma_structure` | binario opaco: estrai stringhe + meta |
| zip | `nexus_list_archive_entries` | esplora prima, decidi dopo |
| pdf | `nexus_extract_pdf_text` (pagine 1-10) | testo strutturato |
| docx | `nexus_extract_docx_text` | paragrafi puliti |
| xlsx | `nexus_extract_xlsx_data` | righe + sharedStrings |
| pptx | `nexus_list_archive_entries` | esplora slide |
| png/jpeg/svg/... | `nexus_describe_image_attachment` | vision |
| testo/codice | `nexus_read_attachment` encoding=text | lettura diretta |
| binary opaco | null | chiedi all'utente |

La description del tool nello schema MCP e' aggiornata: "dopo aver chiamato
nexus_inspect_attachment, chiama OBBLIGATORIAMENTE il tool indicato in
`next_action_recommended.tool`".

### FIX 2 — Cache deduplica letture

Nuovo modulo `crates/mcp-core/src/agent_tools/read_cache.rs`:

- LRU 256 entry, TTL 5 min (configurabile via
  `agent.attachment.read_cache_ttl_seconds`).
- Chiave: `(attachment_id, kind, entry_path, offset, length, encoding)`.
- Quando `served_count >= 2` il payload ritornato include
  `from_cache: true`, `served_count: N`, `hint: "questa richiesta e' stata
  servita N volte: usa un tool di estrazione strutturata"`.
- Wrapping applicato a `tool_nexus_read_archive_entry` e
  `tool_nexus_read_attachment`.

Dipendenza: `lru = "0.12"` in `crates/mcp-core/Cargo.toml`.

### FIX 3 — Pre-extraction automatica per kind noti

In `crates/mcp-core/src/chat_messages.rs`, nel costruire il blocco
`<allegati>` del messaggio iniziale del turno, si chiama
`pre_extract_attachments_for_chat` che:

- Per ogni allegato PDF/DOCX/ZIP-con-canvas.fig esegue l'estrattore inline
  dedicato (helper `extract_pdf_text_inline`, `extract_docx_text_inline`,
  `extract_figma_strings_inline`).
- Inserisce un sub-blocco `### Pre-extracted content (auto)` dentro
  `<allegati>`.
- Rispetta budget totale 50_000 char (`agent.attachment.preextract_max_chars`).
- Cache 60s del flag `agent.attachment.preextract_enabled` (default true).
- Immagini: NON pre-extract (vision deve essere chiamato esplicitamente).

Cosi' il modello vede subito il contenuto strutturato e NON deve fare un
giro inspector -> tool aggiuntivo per ottenerlo.

### FIX 4 — Cap budget letture allegati per sessione

In `brain/agents/state.py`: nuovo campo `attachment_read_bytes: int`
sull'`AgentState`.

In `brain/agents/nodes.py` `tool_dispatch_node`: prima di invocare il
ToolRunner, per ogni `tool_use` di `nexus_read_attachment` o
`nexus_read_archive_entry`, se la chiamata farebbe superare
`agent.attachment.session_read_budget_bytes` (default 500_000), il brain
sostituisce il tool_use con un tool_result sintetico:

```json
{
  "error": "budget letture allegati esaurito (XXX byte gia' letti su YYY budget). Usa un tool di estrazione strutturata (nexus_extract_pdf_text, nexus_extract_figma_structure, ...) oppure chiedi all'utente una versione testuale del file.",
  "budget_bytes": 500000,
  "already_read": XXX
}
```

Dopo ogni tool_result di successo, `attachment_read_bytes` viene
aggiornato sommando il campo `length` ritornato dal tool.

## Conseguenze

### Positive

- Il modello e' guidato verso il tool corretto al primo colpo (FIX 1).
- Letture identiche ripetute vengono servite dalla cache + hint chiaro per
  cambiare strategia (FIX 2).
- Per kind noti, zero tool call necessarie nella prima iterazione: il
  contenuto e' gia' nel system message (FIX 3).
- Il context window non puo' essere saturato da letture binarie a chunk
  crescenti (FIX 4).

### Negative

- Pre-extraction aggiunge latenza al primo messaggio (PDF/DOCX vengono
  parsati sincrono): disattivabile via setting.
- Cache LRU consuma ~256 \* avg_size memoria (al massimo decine di MB).
- I limit chiari portano in alcuni casi a un tool_result che rifiuta letture
  legittime di file molto grandi: il modello deve fallback a estrattori
  strutturati o chiedere all'utente.

## Settings configurabili (mig 0195)

| Key | Default | Descrizione |
|---|---|---|
| `agent.attachment.preextract_enabled` | `true` | Pre-extraction auto on/off |
| `agent.attachment.preextract_max_chars` | `50000` | Budget totale pre-extract |
| `agent.attachment.session_read_budget_bytes` | `500000` | Budget letture/sessione |
| `agent.attachment.read_cache_ttl_seconds` | `300` | TTL cache deduplica |

## File modificati

- `crates/mcp-core/Cargo.toml` (+lru)
- `crates/mcp-core/src/agent_tools/mod.rs` (mod read_cache, description tool)
- `crates/mcp-core/src/agent_tools/read_cache.rs` (nuovo)
- `crates/mcp-core/src/agent_tools/attachment_settings.rs` (read_cache_ttl_seconds)
- `crates/mcp-core/src/agent_tools/attachment_inspector.rs` (next_action_recommended)
- `crates/mcp-core/src/agent_tools/archive_tools.rs` (wrap cache)
- `crates/mcp-core/src/agent_tools/attachments.rs` (wrap cache + raw split)
- `crates/mcp-core/src/agent_tools/document_tools.rs` (helper *_inline)
- `crates/mcp-core/src/agent_tools/figma_tools.rs` (helper *_inline)
- `crates/mcp-core/src/chat_messages.rs` (pre-extract automatica)
- `brain/agents/state.py` (attachment_read_bytes)
- `brain/agents/nodes.py` (budget check + tracking)
- `db/migrations/0195_attachment_robustness_settings.sql` (nuova)
