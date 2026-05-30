---
id: adr-0014
kind: adr
title: "ADR 0014 - Context size management nell'executor LangGraph"
status: accepted
tags: [adr, agent-tools, context-window, anti-loop, langgraph, executor]
created_at: 2026-05-29T00:00:00Z
updated_at: 2026-05-29T00:00:00Z
---

# ADR 0014 - Context size management nell'executor LangGraph

## Stato

Accettato. Migrazione `0199_context_management_settings.sql` applicata.
Estende ADR 0012 (pipeline allegati) attaccando il problema dal lato history,
non solo dal lato budget letture.

## Contesto

Caso reale: una sessione agente arrivata a **844K token** dopo 24 chiamate a
`nexus_read_archive_entry` sullo stesso `canvas.fig` (binario Figma) con offset
diversi. La pipeline ADR 0012 (cache, budget letture, pre-extract) NON ha
intercettato perche':

- Le chiamate avevano offset diversi -> non duplicati per cache.
- Il budget letture allegati (500 KB) non era ancora esaurito (le prime 5
  chiamate stavano sotto soglia, le successive sotto soglia per via di chunk
  piccoli).
- La compressione storica (`_compress_old_tool_results`) si attivava solo da
  iter 8 con keep_recent=6 e max_content_chars=500: troppo lasca per blob
  base64 da decine di KB ciascuno gia' accumulati nei primi 7 turni.
- Nessuna previsione del costo della prossima chiamata: il modello veniva
  invocato e produceva un nuovo blob base64 grande, poi a meta' iterazione il
  context era gia' compromesso.

Servono **quattro fix strutturali, complementari, tutti DB-driven**.

## Decisione

Implementati 4 fix in `brain/agents/nodes.py` (e settings in mig 0199):

### FIX A - Compressione anticipata

`_should_compress_now(iteration, settings)` calcola fase di compressione in
base ai boundary configurati (default `[5, 10, 20, 50]`). Da iter 5 in poi
applichiamo gia' una compressione "soft" (keep_recent=8, max_content_chars=2000)
invece di aspettare iter 8 come prima. Le fasi successive sono sempre piu'
aggressive fino a (2, 150) da iter 50.

Settings:

- `agent.context.compress_start_iter` (default `5`)
- `agent.context.compress_phase_boundaries` (default `5,10,20,50`)
- `agent.context.compress_phase_keep_recent` (default `8,5,3,2`)
- `agent.context.compress_phase_max_chars` (default `2000,1000,500,150`)

### FIX B - Dedup tool_result per signature

`_dedup_tool_results_history(messages)` calcola `signature = sha256(tool_name
+ json(args, sort_keys=True))[:16]` per ogni `tool_use` e tiene solo l'ULTIMO
`tool_result` con la stessa signature. Le occorrenze precedenti vengono
sostituite con un placeholder che cita l'indice del messaggio piu' recente.

Diverso dal preesistente `_dedup_tool_results` (BP11) che hashea il CONTENT:
qui hashiamo la CHIAMATA. Cosi' colpisce il caso "stesso file letto 24 volte
con stessi args" anche se i content differiscono per timestamp/metadata.

Setting: `agent.context.dedup_tool_results_enabled` (default `true`).

### FIX C - Drop body base64 non citati

`_drop_unused_base64_payloads(messages, max_age, keep_recent)` rileva
heuristicamente tool_result il cui content e' una grande stringa base64 e
verifica se nei `max_age` messaggi successivi i primi 16 char del blob
vengono citati testualmente (segnale che il modello sta lavorando su quel
contenuto). Se NO, il body viene sostituito con un placeholder che indica i
byte originali e ricorda all'agente di rileggere con il tool originale se
serve.

Setting: `agent.context.drop_unused_base64_age` (default `3`).

### FIX D - Predictive context cap

Prima di eseguire un tool (`tool_dispatch_node._predictive_cap_check`)
stimiamo:

- `current_tokens` = chars(system + history) / 3.5.
- `expected_tokens` = `_estimate_tool_result_size_bytes(tool, args) / 3.5`.
- `cap_tokens` = `ratio * context_window(model)` letto da `ai_price_catalog`.

Se `current + expected > cap`, la chiamata viene **intercettata**: viene
prodotto un `tool_result` sintetico con `is_error=true` che spiega al modello
quanto sta consumando e suggerisce alternative (estrattori strutturati,
length minore, fallback testuale all'utente).

Setting: `agent.context.predictive_cap_ratio` (default `0.5`, range
ammesso `0.3-0.9`).

Cache 120s su `_model_context_window`. Fallback safe 128_000 se il modello
non e' nel catalogo o il DB e' irraggiungibile.

## Conseguenze

- **Niente toppe** (regola H): tutti i parametri sono in `settings`, niente
  hardcoded fallback (eccetto i defaults safe usati solo se la cache DB e' down).
- **Niente hardcoded modelli** (regola G): il context_window e' letto da
  `ai_price_catalog`. Se Anthropic/Google ribilanciano il context_window, basta
  aggiornare la tabella.
- **Caching coerente**: tutte le cache sono 60s (o 120s per il context_window
  che cambia meno spesso) come `_load_adaptive_budget_config`.
- **Logging visibile**: ogni fix emette un `logger.info` con il delta
  caratteri risparmiati, cosi' in produzione si vede l'effetto.

## Pipeline executor finale

In `executor_node`, prima della chiamata al modello, in ordine:

1. Rolling summary (BP4) se context > 60% MAX_CONTEXT_CHARS.
2. **FIX B**: `_dedup_tool_results_history`.
3. **FIX C**: `_drop_unused_base64_payloads`.
4. **FIX A**: `_should_compress_now` + `_compress_old_tool_results`.
5. Safety net storica se ancora > 50% MAX_CONTEXT_CHARS.

In `tool_dispatch_node`, per ogni `tool_use` in pending:

1. **FIX D**: `_predictive_cap_check` (se trigger -> tool_result sintetico).
2. Budget letture allegati (ADR 0012).
3. Esecuzione tool reale via ToolRunner.

## Alternative considerate

- **Auto-bump context_window via fallback su modello con piu' window**:
  rinviato. Cascade fallback esistente gestisce gia' errori 4xx context,
  qui vogliamo prevenire prima ancora di buttare via il turno.
- **Rolling summary aggressivo da iter 1**: peggiora la qualita' (perdiamo
  decisioni passate strutturate). Meglio dedup + drop base64 mirati.
- **Hard limit per signature (es. blocco al 3o tool_use identico)**: gia'
  presente nel loop detector (`_detect_loop`), non sostituisce dedup history.

## Migrazione

`db/migrations/0199_context_management_settings.sql` (7 setting, idempotente
con `ON CONFLICT DO NOTHING`).

## Deploy

Riavvio del solo brain (`./deploy/deploy-local.sh --service brain`) o
`systemctl restart nexus-brain` (le funzioni e la cache 60s sono in-process).
Nessun ricompile Rust necessario.
