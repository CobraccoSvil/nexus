# ADR 0037 - Chat come flusso attivita' multi-provider (activity stream)

Stato: Accettato (2026-07-03) - implementazione in corso
Data: 2026-07-03

## Contesto

Verifica del flusso di visualizzazione della chat (`apps/web-ide`) e inventario
esaustivo dei segnali (workflow `analisi-flusso-chat-nexus`: 4 lettori paralleli
frontend/event-stream/backend/multi-provider + sintesi). Esito: l'infrastruttura
per una UI "stile agente" ESISTE gia' quasi tutta, ma e' resa come card ISOLATE e
collassate, con gerarchia visiva debole.

Cosa c'e' gia' e funziona:
- Il motore Rust emette meta-step semantici ricchi da un punto unico
  (`nexus-agent-graph::nodes::emit_phase_meta`): `routing`, `plan`,
  `executor_call` (provider/model/iteration/tools_count PRIMA della LLM call),
  `escalation` (from_provider/to_provider/to_model/reason), `fallback`,
  `context_overflow`, `final_gate`, `reflection`.
- `SseEventSinkAdapter` ricostruisce un `AITraceEvent` per iterazione
  (provider/model/token in-out/tool_calls/stop_reason), emesso live e persistito
  su `nexus_agent_traces` (mig 0485).
- `ProviderBadge` colora per provider (brand) con opacita' per costo; convergenza
  live SSE / refresh DB reale (`metaStepsMap` riletto al bootstrap).
- Il `ThinkingPanel` legge `message.reasoning` dal DB (sopravvive al reload).

Dove e' debole (motivazione della decisione):
- La sequenza strategica di un turno (`routing -> executor_call -> tool ->
  fallback/escalation -> final_gate`) non ha filo narrativo: sono card separate.
- Il carattere multi-provider e' percepibile SOLO aprendo la card giusta:
  l'header del messaggio mostra `[provider/model]` come testo grigio senza badge;
  la barra attivita' live non mostra il provider; il `routing` non espone il
  provider scelto; la trace usa testo grigio senza colore brand.
- Disallineamento provider iniziale-vs-effettivo: la UI mostra il provider del
  routing HTTP finche' non arriva la prima trace.
- `lib/model-catalog.ts` ha prezzi/context-window HARDCODED (viola regola G).

## Decisione

### 1. Punto unico di composizione (regola L)

Un modulo frontend autoritativo (`apps/web-ide/lib/use-chat/activity-stream.ts`)
piega la timeline per-run gia' disponibile (`metaStepsMap` + `agentStepsMap` +
`traces`) in un modello ordinato `ActivityStream` di eventi raggruppati in
SEGMENTI-PER-PROVIDER. TUTTI i renderer (live e storico) consumano quel modello;
nessuna re-implementazione della sequenza altrove. Un nuovo kind di evento si
aggiunge in quel solo punto.

### 2. Contratto dei segnali multi-provider (regola M: strutturati, mai testo)

- Il provider di ogni segmento e' letto dal SEGNALE STRUTTURATO
  (`executor_call.provider`, `escalation.to_provider`, `AITraceEvent.provider`),
  mai dedotto dal testo umano del titolo.
- Invarianti visive garantite a QUALUNQUE densita':
  1. nodo colorato per provider su ogni evento del nastro;
  2. banda "Cambio provider" a colore pieno (da -> a + motivo) su ogni switch;
  3. esito tool strutturato (`ok`/`errore` + exit code da `is_error`, mai parsing
     dell'output).
- Costo-per-provider: aggregazione delle `AITraceEvent` per provider (token
  in/out) prezzata dal catalogo `/api/models` (`ai_price_catalog`), NON un nuovo
  calcolo con prezzi hardcoded (regola G). Questo sostituisce l'uso di
  `model-catalog.ts` come fonte prezzi.

  **SUPERATO il 10/08/2026.** Il footer non aggrega piu' le trace e non prezza
  piu' nulla: voci, token e costo vengono dal LEDGER, dal perimetro del run e
  dalla stessa lettura che porta il totale mostrato accanto
  (`GET /api/billing/session-usage?run_id=...`, campo `current_run.breakdown`).
  La decisione di allora non era sbagliata sui prezzi — il listino resta fuori
  dal codice — ma sulla FONTE: il totale veniva gia' dal ledger e l'elenco no,
  e i due non tornavano. MISURATO su un footer reale: `openai $0.0000` in
  elenco, e nel ledger delle stesse 12 ore per openai nessuna riga, mentre kimi
  (15 chiamate) e groq (10) non comparivano. Le trace sono un'ottima fonte per
  il NASTRO (chi ha eseguito cosa, e quando) e una fonte sbagliata per la
  contabilita', che ha gia' la sua. Vedi CLAUDE.md, riga «PERIMETRO contabile
  del contatore di chat».

### 3. Densita' adattiva

Guidata da `@container` query sul CONTENITORE della lista messaggi (il pannello
chat e' ridimensionabile, indipendente dal viewport). Soglie: `<=380px` compatto,
`>=600px` esteso, in mezzo medio. Gerarchia di sacrificio quando lo spazio cala:
cedono per prime le etichette ridondanti col glifo, il nome-modello nel badge (
resta la sigla provider), il testo dei turni storici, i nomi provider nei costi;
il ragionamento si clampa con "espandi". NON cedono mai le tre invarianti (2).

### 4. Arricchimenti alla fonte (additivi, retro-compatibili)

Il frontend li consuma se presenti, degrada pulito se assenti:
- **A** - il nodo di routing include il provider/model scelto nel payload del
  meta-step `routing` (oggi emette solo intent/profilo/modalita'/budget).
- **B** - la traccia usa il provider EFFETTIVO (`LlmResponse.provider_used` /
  `RunControlStore.set_effective_model`) quando differisce dal richiesto; il
  `cooldown` (`EscalationInputs.provider_in_cooldown`) e' incluso come causa nel
  meta-step `escalation`.
- **C** - `Usage` porta provider/model per il breakdown costo senza dipendere
  dall'aggregazione delle trace.
- **D** - i meta-step `subagent_*` (ponte narrazione, mig 0535) portano
  provider/model del FIGLIO nel payload (`provider`/`model`). Lato frontend il
  compositore del nastro attribuisce ai blocchi SUBAGENTE la provenienza del
  FIGLIO letta dal payload (mai quella del segmento PADRE, che sarebbe
  un'attribuzione falsa: il figlio ha il proprio routing), con propagazione
  retroattiva allo `started` per `subagent_run_id` quando il provider arriva su
  un progress successivo; provider davvero ignoto -> icona '?'. L'arricchimento
  del payload alla fonte e' additivo e retro-compatibile: finche' i campi non
  sono presenti, ogni blocco subagente degrada pulito a '?'.

### 5. Flag e sicurezza

Rendering gated da settings `chat.activity_stream_enabled` (default OFF, cache
60s lato client, regola G: nel DB, niente env var). OFF = rendering odierno,
bit-identico. I componenti attuali restano finche' il flag non e' stabilizzato ON.

## Conseguenze

- Multi-provider percepibile scorrendo la chat senza aprire card, a ogni
  larghezza del pannello.
- Un solo punto di composizione: la sequenza e le regole di collasso vivono li'.
- Nessun modello/prezzo hardcoded: costo derivato dal catalogo DB (chiude il
  debito di `model-catalog.ts`).
- Backend additivo: nessuna rottura del path esistente; A/B/C incrementali e
  indipendenti dal frontend.

## Riferimenti

- Regole L (punto unico), M (segnali strutturati), G (registry DB) - CLAUDE.md.
- ADR 0033 (classificazione errori deterministica), 0034 (esito strutturato),
  0023 (coerenza badge modello), 0026 (catalogo punti unici).
- File chiave: `components/chat/{message-list,agent-meta-step-card,provider-badge}.tsx`,
  `lib/use-chat*`, `lib/api/{agent,chat}.ts`; backend
  `crates/nexus-agent-graph/src/nodes/{mod,executor,router}.rs`,
  `crates/mcp-core/src/agent_graph_adapter/event_sink.rs`.
