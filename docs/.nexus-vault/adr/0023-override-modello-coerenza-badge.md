---
id: 0023-override-modello-coerenza-badge
kind: adr
title: "Override modello rispettato + coerenza badge provider tra header e step"
slug: 0023-override-modello-coerenza-badge
tags:
  - architecture
  - routing
  - agent
  - frontend
  - provider-badge
  - consistency
auto_generated: false
created_at: 2026-06-04T21:00:00Z
updated_at: 2026-06-04T21:00:00Z
nexus_meta_version: 1
---

# ADR 0023 — Override modello rispettato + coerenza badge provider

> **Status**: proposto
> **Decisori**: team Nexus + utente (decisione 04/06/2026: "entrambi i casi gestiti")
> **Correlato**: task #27/#28 (emit provider+model nei meta-step, badge colorato)
> **Trigger**: screenshot 04/06/2026. Header "Agente in esecuzione" mostra `mistral/mistral-large-2411`, ma i badge dei singoli meta-step e il thinking mostrano `deepseek/deepseek-v4-pro`. I provider non sono allineati.

## Contesto

L'utente segnala che, in un run agentico sul progetto Beauty-Book:
- Header "Agente in esecuzione" e indicatore composer "run: ...": `mistral/mistral-large-2411`
- Badge dei singoli meta-step (RoutingIntent, list_files, read_file) e thinking: `deepseek/deepseek-v4-pro`

L'esecuzione **reale** e' su deepseek: il thinking del brain emette "consulto il modello deepseek/deepseek-v4-pro" da `executor_node`, dopo la risoluzione effettiva del modello (`brain/agents/nodes/__init__.py:1654`).

Decisione utente sul comportamento atteso: **entrambi i casi gestiti**
1. Se l'utente forza un modello PRIMA dello start → l'override vince su tutto il run.
2. Se Auto → il routing sceglie, e header + step mostrano lo STESSO modello reale (niente mix).

## Diagnosi: due bug concorrenti

### Bug 1 — Override modello non propagato quando provider = "auto"

`apps/web-ide/components/chat-panel.tsx:547-557`:

```js
const shouldForce = selectedProvider !== "auto";
const effectiveModel = shouldForce
  ? (selectedModel !== "auto" ? selectedModel : undefined)
  : (selectedProvider === "auto" ? hint?.model : undefined);
```

`effectiveModel` (→ `modelOverride` nella richiesta) viene inviato **solo se `shouldForce`**, cioe' solo se il provider selezionato non e' "auto". Se l'utente lascia il provider su "Auto" ma sceglie un modello specifico, il modello **non** viene mandato come override.

### Bug 2 — Backend richiede provider E model per attivare l'override

`crates/mcp-core/src/orchestrator/core.rs:575-576`:

```rust
if let Some(p) = provider_override.filter(|v| !v.trim().is_empty()) {
    if let Some(m) = model_override.filter(|v| !v.trim().is_empty()) {
        // usa override
```

L'override scatta solo se **entrambi** `provider_override` e `model_override` sono presenti. Un `model_override` da solo (senza provider) viene ignorato → si applica il routing per intent. Questo e' incoerente: un modello identifica univocamente il suo provider (via `ai_price_catalog`), quindi `model_override` da solo dovrebbe bastare.

### Bug 3 — Header mostra il modello registrato a spawn, non quello effettivo

`apps/web-ide/components/chat/agent-steps-panel.tsx:837`:

```jsx
{agentRun.provider}/{agentRun.model}
```

L'header legge `agentRun.provider/model` (valore registrato alla creazione del run). I badge dei meta-step leggono il provider/model **effettivo** di ogni step (emesso dal brain). Se la risoluzione a spawn-time diverge dall'esecuzione reale (per fallback cascade, smart upscale, context-aware re-routing in `agent_run.rs:1111-1134`), header e step divergono. L'header dovrebbe riflettere il modello con cui gli step girano davvero.

### Conferma flusso

`executor_node` del brain (`__init__.py:1597-1610`) RISPETTA l'override se arriva:

```python
provider = sticky_provider or state.get("provider_override")
model = sticky_model or state.get("model_override")
if not provider or not model:
    decision = _router.route_model(intent, ...)   # routing per intent
    provider = provider or decision.provider
    model = model or decision.model
```

Quindi la catena di rispetto esiste; il problema e' a monte (l'override non arriva valorizzato dal frontend/mcp-core).

## Decisione

Tre fix, uno per bug, per realizzare "entrambi i casi".

### Fix 1 (frontend) — Inviare model_override anche con provider auto

In `chat-panel.tsx`, separare la logica provider da quella model:

```js
// Provider: forzato solo se selezionato esplicitamente (non "auto")
const effectiveProvider = selectedProvider !== "auto"
  ? selectedProvider
  : hint?.provider;
// Model: forzato se selezionato esplicitamente, INDIPENDENTEMENTE dal provider.
// Un modello identifica il suo provider; "auto" sul provider non deve azzerare
// la scelta esplicita del modello.
const effectiveModel = selectedModel !== "auto"
  ? selectedModel
  : (selectedProvider === "auto" ? hint?.model : undefined);
```

Cosi' "provider Auto + modello mistral-large-2411" invia `modelOverride=mistral-large-2411`, `providerOverride=undefined`.

### Fix 2 (backend) — model_override da solo e' sufficiente

In `orchestrator/core.rs` (e nei rami gemelli `resolve_agent_provider*` / `model_routing.rs`), quando arriva un `model_override` senza `provider_override`, derivare il provider dal catalogo:

```rust
// Se ho il modello ma non il provider, ricavo il provider dal catalogo.
let resolved_provider = match (provider_override, model_override) {
    (Some(p), Some(m)) => Some((p.to_string(), m.to_string())),
    (None, Some(m)) => {
        // SELECT provider FROM ai_price_catalog WHERE model = $1 LIMIT 1
        provider_for_model(db, m).await.map(|p| (p, m.to_string()))
    }
    _ => None,
};
```

Se il modello non e' nel catalogo, fallback al routing normale + log warn (regola G: niente provider hardcoded).

### Fix 3 (display) — Header e composer mostrano il modello effettivo

L'header del run e l'indicatore "run: ..." devono riflettere il modello con cui gli step girano davvero. Due opzioni:

- **3a (preferita)**: mcp-core, quando riceve dal brain il provider/model effettivo del primo step di esecuzione, AGGIORNA `agent_runs.provider/model` (e l'evento SSE del run) col valore reale. L'header, leggendo `agentRun.provider/model`, mostra automaticamente il valore corretto. Cosi' sia header sia step convergono sul modello reale.
- **3b (fallback display-only)**: nel frontend, se i meta-step hanno un provider/model che diverge da `agentRun.provider/model`, l'header usa quello degli step (l'esecuzione e' la fonte di verita').

Preferenza: 3a (la fonte di verita' e' il DB/SSE, non una pezza frontend). Implementare 3a; usare 3b solo se 3a richiede troppa invasivita' sullo schema SSE.

## Effetto sui due casi

| Caso | Prima | Dopo |
|---|---|---|
| Provider Auto + modello mistral-large-2411 scelto | override perso → esegue deepseek, header mistral (incoerente) | override propagato → esegue mistral, header+step mistral |
| Provider e modello entrambi Auto | routing deepseek, header puo' divergere se cascade/upscale | routing sceglie, header+step sempre allineati al modello reale |
| Cascade fallback a metà run (mistral → openai) | header resta sul primario, step sul fallback | header aggiornato al modello reale del fallback |

## Sequenza implementativa

| Fase | File | Effort |
|---|---|---|
| F1 frontend override | `chat-panel.tsx` (logica effectiveModel) | 0.2 gg |
| F2 backend model-only override | `orchestrator/core.rs`, `model_routing.rs`, helper `provider_for_model` | 0.5 gg |
| F3 display coerente | `agent_run.rs` (update provider/model reale su primo step) + evento SSE + `agent-steps-panel.tsx` se serve 3b | 0.5 gg |
| F4 test | E2E: forza modello con provider auto → run usa quel modello; Auto puro → header=step | 0.3 gg |

## Metriche di Done

- ✅ Provider "Auto" + modello X scelto → richiesta contiene `modelOverride=X`
- ✅ Backend con solo `model_override` → risolve provider dal catalogo, usa X
- ✅ `executor_node` brain riceve `model_override=X` → thinking e step mostrano X
- ✅ Header "Agente in esecuzione" === badge dei meta-step (stesso provider/model) in tutti i casi
- ✅ Cascade fallback: header aggiornato al modello reale del fallback
- ✅ `cargo check --workspace` + `pnpm verify` verdi
- ✅ Nessun provider/modello hardcoded (regola G)

## Rischi

| Rischio | Mitigazione |
|---|---|
| `provider_for_model` ambiguo (stesso model su piu' provider) | `ORDER BY` deterministico (priorita' provider, poi costo); log se multi-match |
| Aggiornare `agent_runs.provider/model` a runtime confonde l'audit | Tracciare sia `requested_provider/model` (a spawn) sia `effective_provider/model` (esecuzione); l'header mostra effective, l'audit conserva entrambi |
| Override perso in sticky/cascade | Il fix non tocca sticky (M61): l'override resta il primario, il cascade subentra solo su fallimento reale, ed e' giusto che l'header lo segua (Fix 3) |

## Riferimenti

- `apps/web-ide/components/chat-panel.tsx:538-570` (Bug 1)
- `crates/mcp-core/src/orchestrator/core.rs:575-580` (Bug 2)
- `apps/web-ide/components/chat/agent-steps-panel.tsx:837` (Bug 3)
- `brain/agents/nodes/__init__.py:1597-1659` (executor rispetta override, emette thinking)
- `crates/mcp-core/src/chat_messages/agent_run.rs:392-457` (risoluzione provider con override)
- Screenshot incident 04/06/2026
