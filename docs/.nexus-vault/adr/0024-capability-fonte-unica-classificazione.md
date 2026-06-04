---
id: adr-0024-capability-fonte-unica-classificazione
kind: adr
title: "ADR 0024 - Capability modello: fonte unica + classificazione automatica"
slug: 0024-capability-fonte-unica-classificazione
tags:
  - adr
  - routing
  - capability
  - catalog
  - thinking
  - tool-use
  - single-source-of-truth
auto_generated: false
nexus_meta_version: 1
---

# ADR 0024 - Capability modello: fonte unica + classificazione automatica

## Stato

Implementato. Migrazione `0318_capability_single_source_view.sql`.

## Contesto

I flag semantici di capability di un modello erano duplicati FISICAMENTE su due
tabelle, senza vincolo che li legasse:

| Capacita' | `ai_price_catalog` (routing, Rust) | `nexus_provider_capabilities` (adapter, brain) |
|---|---|---|
| tool use | `supports_tool_use` | `tool_use` |
| vision | `capabilities->>'vision'` (jsonb, di fatto VUOTO) | `vision` |
| thinking | `is_thinking` (colonna) | `thinking` |

Due colonne fisiche scrivibili per lo stesso fatto => il drift e' inevitabile.
Incidente reale (run eec9bffe): `deepseek-v4-pro` aveva `is_thinking=false` nel
catalog (routing lo sceglieva per task agentici) ma e' un modello thinking che
non regge il loop a tool forzati -> chat bloccata. Le migrazioni 0256 e 0317
correggevano tabelle diverse senza allinearle.

Inoltre la classificazione di un modello NON si aggiornava insieme al modello:
`catalog_sync` (LiteLLM + discovery provider) non popolava i flag canonici, quindi
un modello nuovo nasceva con default sbagliati.

## Decisione

### 1. Due concetti "thinking" distinti (non fondere)

- **A - "escludi dal routing agentico"** = `ai_price_catalog.is_thinking`
  (solo Rust). Reasoning-only che non reggono il tool-forcing.
- **B - "gira in thinking mode -> non forzare tool_choice + budget"** =
  `ai_price_catalog.uses_thinking_mode` (letto dal brain via vista).

Claude ha legittimamente A=false (ottimo agentico) e B=true (extended thinking):
fonderli avrebbe rotto Claude.

### 2. Fonte unica + derivazione (drift impossibile)

- `ai_price_catalog` e' l'UNICA casa fisica dei flag semantici: colonne reali
  `supports_tool_use`, `supports_vision`, `is_thinking` (A), `uses_thinking_mode` (B).
- `nexus_provider_capabilities` tiene SOLO le meccaniche di chiamata
  (`tool_choice_style`, clamp token, timeout, dialetti). Le colonne
  `thinking`/`tool_use`/`vision` sono DROPPATE.
- Vista `v_model_capabilities` = meccaniche JOIN catalog, espone i flag derivati
  dal catalog. Il brain (`capability_loader`, `anthropic_provider`,
  vision-routing) legge da li'.

Non esiste piu' un secondo posto dove scrivere quei flag: una
`UPDATE nexus_provider_capabilities SET thinking=...` fallirebbe (colonna
inesistente). Due valori non possono divergere perche' c'e' un valore solo.

### 3. Classificatore unico auto-allineante

`model_catalog_sync::classify_capabilities(provider, model, meta_tool_use,
meta_vision, meta_reasoning) -> ClassifiedCaps`: unica logica di
classificazione, invocata da OGNI path di aggiornamento del catalog
(`models::run_catalog_sync` con metadata LiteLLM; `sync_provider` con euristica
sul nome). Aggiornare i modelli aggiorna la classificazione.

### 4. Protezione curature manuali

Colonna `ai_price_catalog.capability_source` (`auto`|`manual`). Il classificatore
(via UPSERT con guard SQL) aggiorna SOLO le righe `auto`; le `manual` (curate da
admin/migrazioni) sono protette. La 0318 marca `manual` tutte le righe con una
decisione thinking deliberata (A o B).

## Conseguenze

Positive:
- Drift strutturalmente impossibile (fonte unica + vista derivata).
- Modelli futuri auto-classificati e coerenti su entrambi i lati.
- Vision-routing del brain di nuovo funzionante (leggeva un jsonb vuoto).
- `anthropic_provider` legge il thinking dalla colonna canonica (prima jsonb vuoto).

Limiti onesti:
- La CORRETTEZZA del flag per un modello mai visto dipende dall'euristica del
  classificatore / dai metadata LiteLLM; `capability_source='manual'` permette
  all'admin di correggere casi limite senza che il sync li ribalti.

## Riferimenti

- Migrazione `db/migrations/0318_capability_single_source_view.sql`.
- ADR [[0020-gate-unico-disponibilita-provider]] (gate cooldown), mig 0317
  (is_thinking deepseek-v4), 0256 (capabilities thinking), 0274 (thinking su
  intent agentici).
- Regola G/H (CLAUDE.md): fonte unica nel DB, niente duplicazione, fix definitivo.
