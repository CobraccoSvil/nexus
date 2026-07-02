---
id: adr-0018-segnali-strutturali-vs-euristiche-testuali
kind: adr
title: "ADR 0018 - Segnali strutturali vs euristiche testuali nel loop agentico"
slug: 0018-segnali-strutturali-vs-euristiche-testuali
tags:
  - adr
  - agent-loop
  - routing
  - tool-choice
  - verifier
  - anti-pattern
  - structural-signals
  - capability-gate
auto_generated: false
created_at: 2026-06-04T00:00:00Z
updated_at: 2026-07-02T00:00:00Z
nexus_meta_version: 1
---

# ADR 0018 - Segnali strutturali vs euristiche testuali nel loop agentico

## Stato

Implementato (2026-07-02). Proposto il 2026-06-04, rafforzato lo stesso giorno
con il gate di capability (leva 0) dopo verifica sul catalog reale.

Leve 0/1/2 gia' in produzione da tempo: assorbite da
[[0024-capability-fonte-unica-classificazione]] per il gate capability e
`tool_choice_style`, da [[0033-routing-strict-pin-retry-e-anti-loop-onesto]]
per la classificazione errori e da
[[0034-esito-conversazione-strutturato-finish-task]] per l'esito dichiarato
`task_complete`. Completate il 2026-07-02:

- fase 3: `INTENT_NARRATION_PATTERNS` e `resigned_patterns` CANCELLATI;
  sostituti strutturali `structural_unfulfilled_signal` +
  `detect_pending_steps_report_with` + `task_complete`;
- leva 3: 3 criteri `action_requested` / `tool_capability` /
  `completion_confirmed` in `final_gate`/`criteria_runner`, kill-switch
  `agent.final_gate.structural_criteria_enabled` (mig 0503);
- A7: `WEAK_MODELS_HINT` sostituito da `performance_tier` dal catalog.

Fallback lessicali RESIDUI legittimi (strutturale-prima): `TOOL_ERROR_HINTS` e
`PENDING_STEPS_LABELS`. Residuo minore fase 2: `complexity_keyword_weights`
come fallback del classifier (documentato, non prioritario).

## Contesto

Il loop agentico di Nexus decide il routing e il controllo del run (re-iterare,
fare nudge, rilevare stop prematuro, completamento, complessita') basandosi su
**euristiche testuali**: blacklist di frasi e regex che cercano di indovinare il
comportamento del modello dal testo prodotto o ricevuto. Questo e' un
anti-pattern toppa: ogni nuovo verbo, lingua o modello sfugge alla lista e va
aggiunto a mano. La regola H del progetto (CLAUDE.md, "fix definitivi mai
toppe") vieta esattamente questo schema.

Caso scatenante reale: Beauty-Book chat 7, run `gemini-2.5-pro` su
`BookingPage.tsx`. Il modello ha annunciato "Estrarro/Scomporro... Inizio
creando la directory" e ha chiuso il turno **senza eseguire le edit**. Il
guardrail G1 (`route_after_executor`) non e' scattato perche' i verbi annunciati
non erano nella blacklist `_INTENT_NARRATION_PATTERNS`. Un fix morfologico
(commit recente) ha mitigato il caso, ma resta un'euristica testuale: domani un
altro verbo o un'altra lingua sfuggira' di nuovo.

Censimento dei 7 anti-pattern testuali, tutti sintomi dello stesso problema:

- **A1** - `_ACTION_PATTERNS` DUPLICATO: Python (`brain/agents/nodes/helpers.py:672`)
  + Rust (`crates/mcp-core/src/agent_types.rs:283`). Rileva se l'utente chiede
  un'azione. Le due liste sono desincronizzate (es. `dotnet watch` presente in
  Rust, assente in Python).
- **A2** - `_INTENT_NARRATION_PATTERNS`: 60+ stringhe (`helpers.py:721`). Rileva
  un intento annunciato ma non compiuto e guida il reroute G1. E' il piu'
  fragile e quello che ha causato l'incidente Beauty-Book.
- **A3** - `resigned_patterns`: 15 stringhe (`brain_agent_client.rs:1074`).
  Rileva la resa del modello.
- **A4** - `_TOOL_ERROR_HINTS`: 22 stringhe (`helpers.py:897`). Rileva errore
  nel tool result.
- **A5** - `_SCAFFOLD_VERBS` / `_SCAFFOLD_OBJECTS` (`helpers.py:1025`). Rileva
  richiesta di scaffolding applicazione.
- **A6** - `complexity_keyword_weights` (`helpers.py:49`). Stima la complessita'
  per il budget iterazioni.
- **A7** - `_WEAK_MODELS_HINT` (`helpers.py:61`). Rileva modelli deboli per il
  moltiplicatore di budget.

Sono tutte manifestazioni dello stesso errore di fondo: dedurre lo **stato** del
loop dal **testo** invece che dai segnali strutturali che il sistema gia'
possiede.

## Decisione

Sostituire le euristiche testuali con **segnali strutturali** gia' disponibili
nel sistema, eliminando le blacklist invece che relegarle a fallback. Quattro
leve complementari, in ordine di applicazione:

### 0. Gate di capability sul routing agentico

Il routing per i run agente deve selezionare SOLO modelli con
`ai_price_catalog.supports_tool_use = true`. I modelli non tool-capable (FIM,
chat-completion puri, alcuni reasoning) restano disponibili per usi
non-agentici (classify, vision, embedding, completion) ma non vengono mai
assegnati a un loop agentico. Il campo `supports_tool_use` (boolean NOT NULL) e
`consecutive_tool_failures` esistono GIA' in `ai_price_catalog`.

Questo gate e' la pre-condizione che rende applicabili le leve successive: se al
loop arrivano solo modelli capaci di usare tool, lo stop narrativo non e' piu'
un comportamento "lecito ma indesiderato" del modello, ma diventa rilevabile e
correggibile in modo strutturale.

> ### Evidenza dai dati (2026-06-04)
>
> Verifica sul catalog reale: nel routing agentico era presente
> `mistral | mistral-code-latest` con `supports_tool_use = false` — una
> misconfigurazione che permette al router di assegnare a un run agente un
> modello incapace di usare tool, producendo esattamente lo stop
> narrativo/fallimento oggetto di questo ADR. Altri modelli non-tool
> (`magistral-*` reasoning, `mistral-code-fim`, `gpt-5-chat-latest`,
> `gpt-5.1-chat-latest`) sono correttamente fuori dal routing agentico. Tutti
> gli altri modelli agentici (Claude 4.x, gemini-2.5, gpt-4o/5.x,
> deepseek-v4-pro, o1) sono tool-capable. Il gate di capability risolve la
> misconfigurazione in modo strutturale (regola H), non con un UPDATE manuale.

### 1. Segnali del protocollo (non testo)

Usare `tool_calls presence` + `stop_reason` (`end_turn` vs `tool_use`), gia'
normalizzati in `brain/agents/nodes/__init__.py:1948`. Questi segnali
sostituiscono A2 al 100%: la condizione

> `stop_reason = end_turn` AND nessun `tool_call` AND tool disponibili AND task
> non completo

identifica lo stop prematuro **senza leggere i verbi**. E' deterministica,
indipendente da lingua e modello.

### 2. tool_choice forcing

Forzare `tool_choice = "required"` nei turni d'azione, cosi' il modello **non
puo'** chiudere il turno con solo testo. Previene lo stop narrativo alla radice.
Esiste gia' una logica discovery-aware (commit `d5e5b1c`).

Caveat: non tutti i provider lo supportano (Gemini < 2.0). Va gestito
`MALFORMED_FUNCTION_CALL` per modelli non tool-capable, marcando
`ai_price_catalog.supports_tool_use = false` (gia' coperto dal gate della leva
0, quindi questi modelli non arrivano nemmeno al loop agentico).

Caveat capability fine: `supports_tool_use = true` non garantisce il supporto
di `tool_choice = required` (es. o1 e alcuni reasoning hanno restrizioni sul
forcing). Per distinguere, si puo' introdurre una capability
`tool_choice_forcing` nel jsonb `capabilities` di `ai_price_catalog`. I modelli
tool-capable ma senza forcing sono coperti dal livello 2 strutturale (segnale
`stop_reason` + `tool_calls presence`), che NON e' una blacklist testuale:
nessuna blacklist e' necessaria neppure per loro.

### 3. Motore di completamento task come fonte di verita' unica

Usare `verifier_node` + `final_gate` + `criteria_runner` (PR-2, gia' in
produzione) come unico giudice del completamento. I criteri sono gia'
deterministici: `http`, `run_command` exit-code, `file_exists`, `db_query`,
`regex_in_output`, `no_orphan_imported`. Estenderli con 3 nuovi criterion:
`action_requested`, `tool_capability`, `completion_confirmed`.

## Conseguenze

Positive:

- Niente piu' liste che crescono a ogni verbo o lingua nuova.
- Fine della duplicazione Python/Rust (A1).
- Decisioni del loop auditable e ripetibili.
- Robustezza a lingua e a modello nuovo per costruzione.

Conseguenza rafforzata sulle blacklist (rispetto alla stesura iniziale):

- Con il gate di capability (leva 0), il `tool_choice` forcing e' SEMPRE
  applicabile ai modelli agentici. Quindi le blacklist testuali
  (`_INTENT_NARRATION_PATTERNS`, `_ACTION_PATTERNS` come meccanismo di
  rilevamento intento, `resigned_patterns`) NON sono piu' necessarie nemmeno
  come fallback: vanno RIMOSSE, non solo deprecate.
- Il fallback per i (pochi) modelli che supportano `tool_use` ma non il
  `tool_choice` forcing e' il LIVELLO 2 STRUTTURALE (segnale `stop_reason` +
  `tool_calls presence`), che NON e' una blacklist testuale. Risultato: due
  livelli, entrambi strutturali, zero blacklist.

Negative e caveat:

- L'implementazione tocca il cuore del loop agentico e i provider adapter:
  rischio di regressione reale. Serve copertura test esplicita sulle
  ramificazioni critiche (regola F del progetto).
- La rimozione delle blacklist va eseguita solo dopo che gate + forcing +
  segnale strutturale sono in produzione e verificati (vedi Fase 3): rimuoverle
  prima lascerebbe scoperti i modelli senza forcing nella finestra di rollout.

## Piano a fasi

### Fase 1 - cuore (~90% del problema)

- (a) GATE di capability `supports_tool_use = true` sul routing agentico: il
  router non assegna mai un modello non tool-capable a un loop agente.
- (b) `tool_choice` forcing nei turni d'azione per i modelli che lo supportano,
  con normalizzazione cross-provider (Anthropic / Gemini / OpenAI / Mistral).
- (c) G1 reroute basato sul segnale strutturale `(stop_reason, tool_calls
  presence)`, in sostituzione del rilevamento unfulfilled-intent testuale.

### Fase 2 - unificazione e router

- Unificare A1 (`_ACTION_PATTERNS`) Python/Rust in un'unica fonte DB (tabella o
  `settings`), eliminando l'hardcode doppio (regola G); oppure rimuoverli del
  tutto se gia' coperti dal forcing e dall'intento del router.
- Migrare il complexity scoring (A6) da keyword a `user_intent` del router.

### Fase 3 - rimozione blacklist e verifier come giudice unico

- RIMOZIONE delle blacklist testuali (`_INTENT_NARRATION_PATTERNS` A2,
  `resigned_patterns` A3) ora che il gate (leva 0) + forcing (leva 2) + segnale
  strutturale (leva 1) le rendono superflue: niente piu' deprecazione, si
  cancellano.
- Eventuale capability `tool_choice_forcing` nel jsonb `capabilities` di
  `ai_price_catalog` per i modelli tool-capable senza forcing (coperti dal
  livello 2 strutturale).
- Aggiungere i 3 criterion type al verifier e al `criteria_runner`:
  `action_requested`, `tool_capability`, `completion_confirmed`.

## Rischi

- **Provider non tool-capable**: senza una mappa `supports_tool_use` accurata in
  `ai_price_catalog`, il forcing genera `MALFORMED_FUNCTION_CALL`. Mitigazione:
  il gate della leva 0 esclude questi modelli dal routing agentico prima ancora
  che arrivino al forcing; resta da mantenere accurato il campo nel catalog
  (l'incidente `mistral-code-latest` del 2026-06-04 mostra il costo di una sua
  misconfigurazione).
- **Regressione nel loop**: le tre leve toccano routing, adapter e gate.
  Mitigazione: rollout per fase, test di regressione sul caso Beauty-Book chat 7
  e su almeno un provider per ciascuna famiglia tool-choice.

## Riferimenti

- Regola H (CLAUDE.md) - fix definitivi, mai toppe.
- Regola G (CLAUDE.md) - niente valori hardcoded, DB unica fonte di verita'.
- Commit `d5e5b1c` - QW1 force tool_choice discovery-aware, QW2 diagnostica
  empty completion.
- ADR correlati: [[0014-context-size-management]], [[0017-knowledge-graph-parita]].
- PR-2 - `verifier_node` / `final_gate` / `criteria_runner`.
- Codice: `brain/agents/nodes/helpers.py` (A1, A2, A4, A5, A6, A7),
  `brain/agents/nodes/__init__.py:1948` (normalizzazione segnali),
  `crates/mcp-core/src/agent_types.rs:283` (A1 Rust),
  `crates/mcp-core/src/brain_agent_client.rs:1074` (A3).
</content>
</invoke>
