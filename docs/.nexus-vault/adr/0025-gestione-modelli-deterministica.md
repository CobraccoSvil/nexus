---
id: adr-0025-gestione-modelli-deterministica
kind: adr
title: "ADR 0025 - Gestione modelli deterministica + uso corretto per-provider"
slug: 0025-gestione-modelli-deterministica
tags:
  - adr
  - routing
  - catalog
  - provider
  - thinking
  - model-selection
  - single-source-of-truth
nexus_meta_version: 1
---

# ADR 0025 - Gestione modelli deterministica + uso corretto per-provider

## Stato

Implementato. Migrazioni 0319 (agentic_thinking_policy) e 0320 (model_selection_policy).

## Contesto

Due problemi accumulati come stratificazione di toppe:

1. **Disponibilita' non deterministica.** Il catalog era popolato dal dump storico
   LiteLLM (mai pruned) + discovery `/v1/models` (espone deprecati); il probe-on-insert
   abilitava "tutto cio' che risponde a ping". Stato: OpenAI 62/140 enabled con
   gpt-3.5-turbo, gpt-4-0613, gpt-4-turbo, ecc.
2. **Uso non conforme ai contratti ufficiali** dei modelli thinking/reasoning:
   `deepseek-v4-pro` in un loop a tool -> HTTP 400 `reasoning_content must be passed
   back`. Tampone precedente (escludere i thinking dagli agentici) metteva in
   panchina modelli capaci.

## Decisione

### Parte 1 - Disponibilita' deterministica (allowlist famiglie + prune)

- `nexus_model_selection_policy(provider, allowed_patterns[], denied_patterns[])`:
  famiglie correnti vs legacy come regex, DB-driven (niente nomi hardcoded, regola G).
- PUNTO UNICO di ammissione `model_passes_selection_policy(provider, model)`: matcha
  allowed e nessun denied (default allow se provider non configurato).
- Discovery come VALIDAZIONE: ogni path che ABILITA un modello (probe-on-insert,
  re-enable su ricomparsa, auto-upgrade famiglia) consulta la policy; un pass
  self-healing disabilita ogni modello enabled fuori policy a ogni sync. LiteLLM
  resta solo per i prezzi. Auto-allineante: una famiglia nuova rientra, una legacy no.

### Parte 2 - Uso corretto per-provider (thinking = modalita' per-chiamata)

- `ai_price_catalog.agentic_thinking_policy`: `none | disable_for_tools | native |
  exclude` (esposto da `v_model_capabilities`).
- Eleggibilita' agentica via policy (`<> 'exclude'`), non piu' il flag cieco
  `is_thinking`. I dual-mode (deepseek-v4, claude, gemini-2.5) tornano eleggibili.
- Adapter verticali: nei tool-loop forzano il NON-THINKING nel dialetto ufficiale
  (DeepSeek `extra_body.thinking=disabled`; Anthropic niente extended thinking;
  Google `ThinkingConfig` off). La chat non-agentica mantiene il thinking.

### Regola L (CLAUDE.md) - punto unico di controllo

- `select_agentic_model()` e' l'UNICO selettore di modello per i run a tool: tutti
  i call site (route_model_from_catalog, best_model_for_tier, cooldown-fallback in
  core.rs, re-route/cascade in agent_run.rs) delegano ad esso. Eleggibilita'
  definita una sola volta; niente query SQL duplicate.

## Conseguenze

Positive: catalog pulito e deterministico; modelli usati secondo l'API ufficiale;
deepseek-v4 agentico senza 400; eleggibilita' agentica coerente in un solo punto.

Supersede: il flag cieco `is_thinking` come input del gate (mig 0317), e i filtri
di selezione sparsi.

## Riferimenti

- Migrazioni 0319, 0320; classify_capabilities, select_agentic_model,
  model_passes_selection_policy (`crates/mcp-core/src/model_catalog_sync.rs`,
  `orchestrator/model_routing.rs`).
- Doc ufficiali: DeepSeek thinking_mode, Anthropic extended thinking, OpenAI o-series.
- ADR [[0024-capability-fonte-unica-classificazione]], [[0020-gate-unico-disponibilita-provider]].
