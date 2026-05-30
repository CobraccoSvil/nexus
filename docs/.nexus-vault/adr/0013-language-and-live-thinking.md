---
id: adr-0013
kind: adr
title: "ADR 0013 - Enforcement lingua italiana e streaming live thinking"
status: accepted
tags: [adr, agent-prompt, language, streaming, langgraph]
created_at: 2026-05-29T00:00:00Z
updated_at: 2026-05-29T00:00:00Z
---

# ADR 0013 - Enforcement lingua italiana e streaming live thinking

## Stato

Accettato. Migrazione `0197_language_directive.sql` applicata. Modifiche
codice:

- `crates/mcp-core/src/agent_tools/port_scanner.rs` (FIX 2, vedi ADR 0010)
- `brain/agents/nodes.py` + `brain/grpc_server/main.py` (FIX 3 streaming live)

## Contesto

### Problema A - lingua agente

I task reali (#88 e affini) hanno osservato il modello rispondere in
cinese, arabo o lingue diverse dall italiano. Causa: quando il contesto
contiene stringhe in altre lingue (UTF-8 estratto da Figma binario,
snippet di documenti caricati, output di tool su sorgenti multilingua) il
modello tende a fare "matching" col contesto e cambia lingua. I system
prompt `system.nexus_base` e `agent.coder.base` non avevano una direttiva
di lingua sufficientemente forte e visibile (era solo accennata e non in
posizione end-of-prompt).

### Problema B - thinking visibile solo a nodo finito

In `brain/agents/nodes.py` la funzione `_emit_thinking(updates, *lines)`
accoda le righe in `updates["nexus_thinking"]`. LangGraph emette gli
updates SOLO al return del nodo: l utente vede le righe di thinking solo
alla FINE del nodo, anche dopo 30-60 secondi di latenza, perdendo il
beneficio percepito di "vedere il ragionamento". Per i nodi piu' lenti
(executor con tool chiamati in serie, planner che decompone un task
complesso) l esperienza era "schermo fermo finche' non finisce".

## Decisione

### Lingua italiana hard-enforced

Aggiungere un blocco `<language_directive>` al fondo dei system prompt
`system.nexus_base` e `agent.coder.base`. La posizione end-of-prompt
massimizza la salienza per i modelli attuali (recency bias). Il blocco
specifica:

- italiano sempre, anche se l utente scrive in altre lingue;
- identificatori di codice (nomi variabili, file) restano in lingua
  originale;
- testo non-italiano nel contesto va tradotto, mai copiato come output;
- self-check: se ti accorgi di scrivere in altra lingua, fermati e
  ricomincia in italiano.

La migrazione `0197` e' idempotente: applica l UPDATE solo dove
`<language_directive>` non e' gia' presente nel content.

### Streaming live thinking via LangGraph custom events

LangGraph 0.2+ (verificato 1.1.9 installato) espone
`langgraph.config.get_stream_writer()` che permette ad un nodo di
emettere eventi custom durante l esecuzione. Il consumer riceve gli
eventi via `astream(..., stream_mode=["updates","custom"])` come tuple
`(mode, payload)`.

Implementazione:

1. `brain/agents/nodes.py::_stream_thinking_live(line)` ottiene il
   writer e pusha `{"kind":"nexus_thinking","text": line}`. Best-effort:
   try/except per gestire ambienti senza writer (test, riuso fuori
   grafo).
2. `_emit_thinking(updates, *lines)` chiama `_stream_thinking_live` per
   ogni riga (visibilita' live) E continua ad appendere in
   `updates["nexus_thinking"]` (backward-compat per final state).
3. `brain/grpc_server/main.py` cambia `astream(stream_mode="updates")`
   in `astream(stream_mode=["updates","custom"])`. Gli eventi sono
   `(mode, payload)`: se `mode == "custom"` emettiamo
   `thinking_delta` SSE immediatamente, altrimenti applichiamo il
   processing storico sui delta degli updates.

Fallback difensivo: se `raw_event` non e' tupla (versioni vecchie),
trattalo come `mode="updates"`.

## Conseguenze

### Positive

- L utente vede il thinking riga per riga, senza attendere fine nodo.
- Lingua non torna piu' a cinese/arabo perche' la direttiva e'
  in end-of-prompt e impone self-check esplicito.
- Niente toppe lato runtime: nessun retry "se vedi cinese rigenera".

### Negative / rischi

- Doppia emissione thinking (live + final delta). Il frontend gia'
  deduplica per `text` lato accumulator. Costo extra: marginale.
- `stream_mode=["..."]` cambia firma; gli unit test che mock-ano
  `astream` vanno aggiornati se assumevano dict puro. Mitigato dal
  fallback difensivo.

### Verifiche

- Migrazione 0197 applicata: 2 righe aggiornate, riapplicazione
  no-op (lunghezza content invariata).
- Python AST: `import brain.agents.nodes; import brain.grpc_server.main` OK.
- Rust cargo check + clippy + 19/19 test port_scanner: OK.

## Riferimenti

- CLAUDE.md sezione A (lingua), G (registry DB), H (fix definitivi)
- ADR 0010 (port enforcement) - esteso da FIX 2
- LangGraph docs: stream_mode "custom", get_stream_writer
