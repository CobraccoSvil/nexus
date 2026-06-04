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
auto_generated: false
created_at: 2026-06-04T00:00:00Z
updated_at: 2026-06-04T00:00:00Z
nexus_meta_version: 1
---

# ADR 0018 - Segnali strutturali vs euristiche testuali nel loop agentico

## Stato

Proposto (2026-06-04). Implementazione a fasi.

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
nel sistema, relegando le blacklist a fallback di ultima istanza. Tre leve
complementari:

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
`ai_price_catalog.supports_tool_use = false` e facendo fallback alle euristiche
solo per quei modelli.

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

Negative e caveat:

- Le blacklist restano come **fallback** per i provider senza tool_choice
  forcing: non si eliminano del tutto, ma cessano di essere il meccanismo
  decisionale primario.
- L'implementazione tocca il cuore del loop agentico e i provider adapter:
  rischio di regressione reale. Serve copertura test esplicita sulle
  ramificazioni critiche (regola F del progetto).

## Piano a fasi

### Fase 1 - cuore (~90% del problema)

- tool_choice forcing nei provider adapter, con normalizzazione cross-provider
  (Anthropic / Gemini / OpenAI / Mistral).
- Sostituzione del rilevamento G1 unfulfilled-intent con il segnale strutturale
  (`stop_reason` + `tool_calls presence`).
- `_INTENT_NARRATION_PATTERNS` (A2) diventa fallback solo quando tool_choice non
  e' supportato dal provider.

### Fase 2 - unificazione e router

- Unificare A1 (`_ACTION_PATTERNS`) Python/Rust in un'unica fonte DB (tabella o
  `settings`), eliminando l'hardcode doppio (regola G).
- Migrare il complexity scoring (A6) da keyword a `user_intent` del router.

### Fase 3 - verifier come giudice unico

- Aggiungere i 3 criterion type al verifier e al `criteria_runner`:
  `action_requested`, `tool_capability`, `completion_confirmed`.
- Deprecare A2 e A3 come logica primaria.

## Rischi

- **Provider non tool-capable**: senza una mappa `supports_tool_use` accurata in
  `ai_price_catalog`, il forcing genera `MALFORMED_FUNCTION_CALL`. Mitigazione:
  popolare e mantenere il campo, fallback automatico alle euristiche per quei
  modelli.
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
