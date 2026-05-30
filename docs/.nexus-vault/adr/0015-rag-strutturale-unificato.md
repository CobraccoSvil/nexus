# ADR 0015 — RAG strutturale unificato

Data: 2026-05-29
Stato: accettato (Fase 1: allegati + KB + tool agente)

## Contesto

La pipeline pre-extract degli allegati (ADR 0010 / 0011 / 0012) include
in chiaro nel primo turno della chat il testo estratto dai file allegati,
con limiti hardcoded:

- 50 KB cumulativi (`agent.attachment.preextract_max_chars`)
- 30 KB per singolo allegato
- 20 KB per estrazione (PDF/DOCX/Figma) entro la funzione `pre_extract_attachments_for_chat`

Quando l'utente carica un asset Figma da 11 MB (`PL.make`, contenente
`ai_chat.json` da ~6 MB di testo crudo), l'estrazione produce un blocco
che eccede il budget: il prompt finisce in *metadata-only* e il modello
risponde "allucinato" perche' non vede il contenuto.

Aumentare i limiti hardcoded e' una toppa (regola H di `CLAUDE.md`).
La causa radice e' che mettiamo *l'intero file* nel prompt invece di
ricavarne i frammenti semanticamente rilevanti rispetto al messaggio
utente.

## Decisione

Introduciamo un sistema RAG (Retrieval-Augmented Generation) unificato
che indicizza ogni allegato, ogni nota KB, ogni messaggio chat passato
e ogni tool result voluminoso in Qdrant come chunk vettorializzati, e
recupera al momento della costruzione del prompt solo i chunk piu'
rilevanti (top-K configurabile) per la query corrente.

### Architettura

- **Collection Qdrant nuove** (auto-create al primo upsert):
  - `attachment_chunks` — chunks di allegati
  - `kb_chunks` — chunks delle note Knowledge Base
  - `chat_history_chunks` — chunks dei messaggi chat passati
  - `tool_results_chunks` — chunks di tool result oversized
  - (esistente `code_embeddings` rimane per `search_codebase_semantic`)

- **Schema payload comune**:
  ```json
  {
    "source_kind": "attachment|kb|chat_history|tool_result|code",
    "source_id": "<uuid>",
    "project_id": "<uuid>",
    "session_id": "<uuid?>",
    "chunk_index": 0,
    "chunk_text": "...",
    "metadata": { ... }
  }
  ```

- **Modulo Rust unificato** `crates/mcp-core/src/rag/`:
  - `chunker.rs` — split testo in chunk con overlap (boundary su whitespace)
  - `qdrant_client.rs` — wrapper HTTP REST minimo (ensure/upsert/search/delete)
  - `indexer.rs` — `index_text`, `index_attachment`, `delete_source`
  - `search.rs` — `search_semantic(query, kinds, project, session, top_k, extras)`
  - `config.rs` — caricamento `settings.agent.rag.*` con cache 60s
  - Niente fallback hardcoded silenziosi (regola G): se il DB non ha i
    settings la pipeline ritorna errore esplicito.

- **Auto-indicizzazione allegati**: `persist_message_attachments` lancia
  `tokio::spawn` fire-and-forget che chiama `rag::index_attachment` dopo
  l'INSERT su `chat_message_attachments`. Aggiorna `indexed_at` e
  `chunk_count` (migrazione 0200).

- **Sostituzione `pre_extract_attachments_for_chat`**: nuova
  `rag_or_legacy_extract_for_chat` che per ogni allegato gia' indicizzato
  costruisce il blocco `### Pre-extracted content` con i top-K chunk piu'
  vicini semanticamente al `params.content` del messaggio. Per gli allegati
  non ancora indicizzati (race con `tokio::spawn` appena lanciato) fallback
  al vecchio comportamento.

- **Auto-indicizzazione KB**: `knowledge_create_note` lancia
  `tokio::spawn` -> `rag::index_text(SourceKind::Kb, ...)` per popolare
  `kb_chunks` (parallelamente all'indicizzazione legacy
  `qdrant_point_id` su KB notes).

- **Indicizzazione chat history**: nuovo modulo
  `brain/workers/chat_indexer.py` con `run_once(conn)` da schedulare
  ogni 5 minuti (registrazione APScheduler lasciata al main.py
  successivo, non in scope di questo ADR).

- **Tool agente unificato** `nexus_search_semantic`:
  - Input: `query`, `source_kinds[]`, `top_k`, `filter_attachment_id`,
    `filter_session_id`.
  - Output: JSON `{query, count, hits: [{source_kind, source_id,
    chunk_index, chunk_text, score, metadata}]}`.
  - Permette agli agent di "ricordare" contenuti senza ri-leggere file.

### Settings DB (migrazione 0200)

Tutti tunabili a runtime senza redeploy (regola G):

| key | default | scopo |
|-----|---------|-------|
| `agent.rag.enabled` | `true` | master switch |
| `agent.rag.chunk_size` | `1000` | caratteri per chunk |
| `agent.rag.chunk_overlap` | `200` | overlap caratteri |
| `agent.rag.top_k_default` | `8` | hit di default |
| `agent.rag.embedding_endpoint` | `/embed` | path REST brain |
| `agent.rag.qdrant_url` | `http://localhost:6333` | endpoint Qdrant |
| `agent.rag.embedding_dim` | `384` | dimensione vettori |
| `agent.rag.collection_attachments` | `attachment_chunks` | nome collection |
| `agent.rag.collection_kb` | `kb_chunks` | nome collection |
| `agent.rag.collection_chat_history` | `chat_history_chunks` | nome collection |
| `agent.rag.collection_tool_results` | `tool_results_chunks` | nome collection |

### Endpoint embedding

Riusiamo l'endpoint esistente `POST /embed` (vedi
`brain/grpc_server/main.py:542`) con payload `{texts: [...]}` -> 
`{vectors: [[...]], count, model}`. Niente endpoint duplicato.

## Conseguenze

### Positive

- Riduzione context drastica per allegati grandi: invece di 50 KB hard
  cap per *file*, ora top-K=8 chunk x 1000 char = ~8 KB *del file
  rilevante* per il messaggio utente.
- "Memoria" trasversale: l'agente puo' ritrovare quello che ha gia'
  visto in messaggi precedenti / note KB / risultati tool senza ri-leggere.
- I 4 limiti hardcoded del pre-extract diventano irrilevanti per i casi
  in cui l'allegato e' gia' indicizzato.

### Negative / Trade-off

- Latenza chat: aggiunta di una embed query + una qdrant search al
  build del prompt (~50-150 ms in locale). Mitigato da Qdrant indicizzato
  su `source_id` e `project_id`.
- Race condition: i primi 1-2 turni dopo l'upload usano il fallback legacy
  perche' il `tokio::spawn` di indicizzazione non ha finito. Accettabile.
- Dipendenza Qdrant: se Qdrant e' down, RAG ritorna errore -> fallback
  legacy. Quindi degradazione graceful.

### Migrazione

- Migrazione 0200 idempotente: `ADD COLUMN IF NOT EXISTS`,
  `INSERT ON CONFLICT DO NOTHING`.
- Re-indicizzazione retroattiva degli allegati esistenti: opzionale,
  via job manuale (out of scope di questo ADR).

## Riferimenti

- Migrazione: `db/migrations/0200_rag_unified.sql`
- Modulo Rust: `crates/mcp-core/src/rag/`
- Tool agente: `crates/mcp-core/src/agent_tools/rag_search.rs`
- Worker chat history: `brain/workers/chat_indexer.py`
- ADR collegati: 0010, 0011, 0012, 0014
