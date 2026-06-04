---
id: adr-0020-gate-unico-disponibilita-provider
kind: adr
title: "ADR 0019 - Gate unico di disponibilita provider (cooldown enforcement centralizzato)"
slug: 0020-gate-unico-disponibilita-provider
tags:
  - adr
  - routing
  - provider
  - cooldown
  - enforcement
  - billing
  - capability-gate
auto_generated: false
created_at: 2026-06-04T20:00:00Z
updated_at: 2026-06-04T20:00:00Z
nexus_meta_version: 1
---

# ADR 0019 - Gate unico di disponibilita provider (cooldown enforcement centralizzato)

## Stato

Proposto (2026-06-04). Implementazione in 2 fasi (A: mcp-core, B: brain).

> Nota di numerazione: esiste gia' una nota con basename `0019` per il file
> picking robusto ([[0019-file-picking-robusto-verify-chain]]). Questo ADR copre
> un tema indipendente (gate di disponibilita' provider) e va riconciliato in
> fase di consolidamento del vault assegnandogli un numero progressivo libero.

## Contesto

La selezione di provider/modello AI in Nexus e' **frammentata** su circa 15
punti: 9 in mcp-core (Rust) e 6+ nel brain (Python). Il cooldown billing/quota -
gestito da `provider_cooldown.rs` con chiave Redis
`nexus:billing_cooldown:<provider>` (6h per billing) e auto-disable via
`provider_health_probe` - **non e' rispettato in modo uniforme**.

Lo rispettano:

- `best_model_for_tier`
- `resolve_agent_provider` / `resolve_agent_provider_detailed`
- `route_by_slots`
- `route_model_from_catalog`

Lo ignorano:

- `routing_matrix.lookup` / `lookup_with_budget` / `purpose_model`
- soprattutto il **brain** nel fallback/escalation: `_pick_escalation_model`
  (`helpers.py:977`), executor `loop_fallback` (`__init__.py:2196`), e i
  `purpose_model` dei nodi planner / verifier / clarify / understanding /
  subagent.

Incidente reale: durante un run agente, dopo che `deepseek-v4-pro` fallisce il
`tool_choice` forcing, il brain fa cascade fallback su gemini, poi RITENTA
anthropic (`credit_balance_too_low`) e openai (`insufficient_quota`) - entrambi
in cooldown billing gia' noto - sprecando tentativi e **bloccando il
completamento del task**, invece di saltarli e usare subito google.

E' lo stesso anti-pattern della regola H (CLAUDE.md): un cooldown gia' esistente
applicato solo in alcuni codepath equivale a un enforcement parziale, che
produce bug subdoli (provider morti ritentati all'infinito).

## Decisione

Creare **un gate unico** di risoluzione provider, single source of truth, che
TUTTI i punti di selezione devono usare. Candidato:
`resolve_agent_provider_detailed()` (`crates/mcp-core/src/orchestrator/core.rs:252`),
che gia' filtra il cooldown e ritorna `no_capable_provider`, esposto via
endpoint `/api/internal/routing/resolve` (oggi `/decide`).

Regola di enforcement:

- **Modalita AUTO** (intent/purpose/slots, fallback, escalation): il cooldown e'
  VINCOLANTE. I provider in cooldown billing/quota sono SALTATI. Se tutti i
  provider capable sono in cooldown: errore CHIARO (`no_capable_provider`), non
  blocco silenzioso.
- **Forzatura ESPLICITA utente** (`provider_override` / `model_override` dal
  dropdown chat, non Auto): il provider scelto e' usato ANCHE se in cooldown
  (l'utente decide consapevolmente), con flag `user_override` nella risposta.

Il gate centralizza routing intent-based, purpose-based e slot-based in un unico
flusso che applica sempre il filtro cooldown + capability gate
(`supports_tool_use`, vedi ADR 0018) + override. Il brain **smette di scegliere
provider autonomamente** nel fallback/escalation: consulta sempre il gate.

## Conseguenze

Positive:

- Comportamento uniforme e prevedibile su tutti i punti di selezione.
- Niente piu' run bloccati da provider morti ritentati.
- Un solo punto da mantenere (riduzione della superficie di bug).
- Il cooldown billing, gia' esistente, finalmente rispettato ovunque.
- Coerente con ADR 0018 (capability gate) e con la regola H (un solo punto di
  enforcement, come per la WikiAcl).

Negative e rischi:

- Tocca il cuore del routing (mcp-core `core.rs` + `internal_routing`) e il loop
  del brain: serve copertura test esplicita (regola F).
- La rimozione della logica di fallback duplicata nel brain va fatta con cura
  per non regredire (i codepath `purpose_model`, `_pick_escalation_model`,
  `loop_fallback`, cascade fallback sono storicamente indipendenti).

## Piano a fasi

### Fase A - mcp-core

- Estendere il gate `/api/internal/routing/resolve` per gestire **purpose** e
  **slots** DENTRO `resolve_agent_provider_detailed` (oggi `resolve_purpose` e
  `route_by_slots` sono codepath paralleli), sempre con filtro cooldown.
- Aggiungere il flag `user_override` nella risposta.
- Errore esplicito `no_capable_provider` quando tutti i provider capable sono in
  cooldown.

### Fase B - brain

- Far convergere TUTTI i punti del brain sul gate via endpoint: `purpose_model`
  dei nodi, `_pick_escalation_model`, `loop_fallback`, cascade fallback.
- Rimuovere/deprecare la logica di selezione provider duplicata (registry
  `_is_in_billing_cooldown` reattivo, `cooldown_bridge`), affidandosi al gate
  proattivo.

## Riferimenti

- Regola H/G (CLAUDE.md) - un punto di enforcement, niente provider hardcoded,
  fix definitivo mai toppa. Esempio CLAUDE.md: "Anthropic `billing_error` ->
  auto-disable sulla routing matrix; ripristino automatico al primo 200".
- ADR 0018 (capability gate `supports_tool_use`) - il gate provider integra
  anche quel filtro: [[0018-segnali-strutturali-vs-euristiche-testuali]].
- File chiave:
  - `crates/mcp-core/src/orchestrator/core.rs:252` -
    `resolve_agent_provider_detailed`
  - `crates/mcp-core/src/internal_routing.rs` - endpoint
  - `crates/mcp-core/src/provider_cooldown.rs` - cooldown Redis
  - `brain/agents/nodes/helpers.py:977` - `_pick_escalation_model`
  - `brain/agents/nodes/__init__.py:2196` - `loop_fallback`
</content>
</invoke>
