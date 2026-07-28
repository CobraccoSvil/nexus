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
updated_at: 2026-07-27T00:00:00Z
nexus_meta_version: 1
---

# ADR 0023 — Override modello rispettato + coerenza badge provider

> **Status**: implementato (verificato 2026-07-02) con l'opzione 3a
> **Aggiornamento 2026-07-02 (as-built)**: provider/model effettivi emessi dal backend nel payload dei meta_step `executor_call` via SSE, consumati da `ProviderBadge`; nessuna logica di deduzione lato frontend.
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

## Aggiornamento 2026-07-27 — il pin del provider viaggia come PIN (chat non agentica)

> **Trigger**: forzato deepseek dalla UI (dropdown provider + pulsante "Forza",
> tooltip "Override attivo: il provider selezionato viene forzato"), ha risposto
> **google**. Misurato E2E il 27/07/2026.

### Cos'era rotto

Nel turno di chat non agentico (`orchestrator::core::execute_via_gateway`) la
forzatura non arrivava mai al gateway come tale:

1. il provider forzato veniva concatenato al **nome del modello**
   (`format!("{provider}/{alias}")`), e `GwRequest::pin_provider` — il campo che
   esiste esattamente per questo — restava `None`. Senza pin il gateway esegue
   `policy.decide` e sceglie da solo: la forzatura non aveva alcun effetto sul
   routing;
2. il prefisso distruggeva la risoluzione dell'alias lato gateway. Il fornitore
   riceveva il nome letterale e rifiutava ("you passed coder-large") con un 400,
   da cui la cascata di policy che finiva su un altro provider;
3. la riga di prenotazione del ledger portava una coppia impossibile — provider
   forzato + modello suggerito da un ALTRO provider — perche' il modello
   suggerito era calcolato prima, e all'oscuro della forzatura.

### Cosa vale ora

- il provider forzato finisce in `GwRequest::pin_provider`: il gateway esegue
  ESATTAMENTE quel provider (`resolve_pinned_provider`, chain di un elemento),
  senza `policy.decide` ne' fallback cross-provider;
- il modello **non** viene piu' prefissato col provider. Col pin il gateway non
  risolve alias, quindi mcp-core manda un modello concreto; senza pin manda
  l'alias logico e lo risolve il gateway, come sempre;
- quando il provider e' forzato e il modello no, il modello si riallinea a quel
  provider delegando al punto unico `RoutingConfig::resolve_model` (il modello
  suggerito vale solo se e' di quel provider, altrimenti si scende al default
  del provider forzato — `nexus_provider_default_model`, mig 0101);
- la prenotazione a ledger porta la stessa coppia che si sta per chiedere.

Punto unico: `build_chat_gateway_call` in
`crates/mcp-core/src/orchestrator/model_routing.rs` (catalogo in ADR 0026).

### Conseguenze da conoscere

| Conseguenza | Perche' e' accettata |
|---|---|
| Col pin NON c'e' fallback cross-provider: se il provider forzato fallisce, la chiamata fallisce | E' cio' che l'utente ha chiesto. L'errore lo dice: la resa aggiunge "Provider forzato dall'utente (X): con la forzatura attiva non viene tentato nessun altro fornitore" (`RenderedError::con_provider_forzato`, punto unico `nexus-types::error_presentation`) |
| Non e' il vecchio "pin senza fallback" che mandava i run in abort | Quello era il pin sui TRANSITORI, chiuso in ADR 0033: col pin il gateway va in `strict` e ritenta lo STESSO modello con backoff, attendendo i cooldown brevi entro la deadline (`run_fallback`, `complete_with_retry`). Qui il fallimento residuo e' quello vero |
| Provider forzato non configurato nel gateway -> errore esplicito invece di ripiego silenzioso | Regola G. Prima l'utente vedeva "funzionare" una forzatura che il gateway ignorava |
| Contenuto a sensitivity elevata + provider cloud forzato -> `POLICY_TIER_EXCLUDED` invece del re-route privacy | Il pin bypassa il ROUTING, mai la sicurezza (`pin_tier_gate`). Coi flag DLP di default (profilo cloud) il gate non scatta |
| L'errore non e' piu' appiattito in una stringa | `execute_via_gateway` chiudeva con `bail!("Nexus Gateway failed ...: {e}")`, che rompeva la catena `anyhow`: `chat_messages::run` cercava `GatewayHttpError` per rendere l'errore e trovava solo testo. Ora la resa nasce dove i fatti sono vivi e viaggia tipizzata |

Test (regola O — attraversano la funzione di produzione, non una richiesta
fabbricata): `pin_duro_dal_wire_viaggia_come_pin_e_il_modello_non_e_prefissato`,
`provider_pinnato_e_modello_libero_danno_una_coppia_dello_stesso_provider`,
`provider_pinnato_e_modello_entrambi_scelti_viaggiano_intatti`,
`senza_scelta_dell_utente_resta_l_alias_e_nessun_pin`,
`il_fallimento_col_provider_forzato_lo_dice_all_utente` in
`crates/mcp-core/src/orchestrator/tests.rs`.

## Aggiornamento 2026-07-27 (2) — preferenza e pin sono due cose diverse

Il pin funzionante ha reso VISIBILE un difetto che prima era innocuo: la
forzatura era dedotta dal solo dropdown.

### Cos'era rotto

1. **Il pulsante "Forza" non arrivava al backend.** `doSend` costruiva
   `providerOverride` da `selectedProvider !== "auto"`; lo stato `forceProvider`
   serviva solo al colore del bordo e a mostrare il dropdown dei modelli. Finche'
   l'override non aveva effetto la cosa era innocua — anzi, i tooltip del
   pulsante spento ("il routing puo' scegliere un provider diverso") erano
   accidentalmente VERI, perche' rispondere con un altro provider era il difetto
   stesso. Col pin funzionante quelle stesse frasi sarebbero diventate false: la
   sola selezione dal dropdown avrebbe prodotto un vincolo duro. Lo stesso vizio
   di partenza — la UI che dichiara il contrario di cio' che il backend fa — col
   segno invertito.
2. **Il vincolo era per-SESSIONE e persistente.** Cambiare il dropdown scrive
   `chat_sessions.preferred_provider` sul server, e l'handler leggeva
   `body.provider_override.or(session_preferred_provider)`: ogni messaggio
   successivo, anche da superfici che non mandano alcun override, sarebbe nato
   pinnato. Se quel provider entrava in cooldown, la sessione restava bloccata
   senza che l'utente sapesse perche'.

### Cosa vale ora

- **preferenza e pin sono distinti e distinguibili sul wire**: `providerOverride`
  (quale provider) + `providerOverrideMode` (quanto vincola), identificatori
  canonici inglesi `preferred|pinned` come `automationMode`/`supervisorMode`
  (regola N). Non un booleano dedotto da altro;
- **il pin duro scatta solo col pulsante "Forza" attivo su una scelta esplicita
  del dropdown**. Il predicato vive nel punto unico frontend
  (`isProviderPinned`): `forceProvider` e' uno stato locale che sopravvive al
  ritorno del dropdown su "Auto", e senza la congiunzione un invio guidato da un
  hint esterno erediterebbe un "Forza" premuto prima per un altro provider;
- **`pin_provider` si valorizza solo col pin duro.** Con la sola preferenza il
  provider entra come suggerimento (decide da dove parte il routing e su cosa
  prenota il ledger), la richiesta porta l'ALIAS logico e il gateway conserva il
  fallback cross-provider — esattamente cio' che i tooltip promettono;
- **la preferenza persiste sulla sessione, il pin no.** `ProviderChoice::resolve`
  conia un pin solo dal provider della richiesta IN CORSO; il provider ricordato
  (preferenza di sessione, metadata del messaggio in un resend) vale sempre come
  preferenza. Un vincolo duro e' un ordine: vale per la richiesta in cui lo si
  da', non per tutte quelle che seguono da superfici che non lo sanno;
- **i tooltip dichiarano la conseguenza** (il fallback c'e' o non c'e') invece di
  ripetere il nome del pulsante, e nascono nello stesso modulo che decide cosa
  viaggia sul wire: la frase e il fatto non possono piu' divergere;
- il badge `⚠ pin non rispettato` (ex `override -> fallback`) compare solo col
  pin: con la preferenza un provider diverso e' il comportamento promesso, non
  un'anomalia.

Punti unici: `crates/mcp-core/src/orchestrator/provider_choice.rs`
(`ProviderOverrideMode`, `ProviderChoice::resolve`) e
`apps/web-ide/components/chat/provider-choice-logic.ts`
(`providerChoiceForSend`, `isProviderPinned`, tooltip). Guard
`vocabolario forza-vincolo provider` e `nascita del pin duro` in
`scripts/check-single-source.sh`.

Test — partono dal WIRE, non dall'enum: un test che costruisse la scelta a mano
resterebbe verde anche se il campo sparisse dal corpo della richiesta.
Rust (`crates/mcp-core/src/orchestrator/tests.rs`):
`identificatori_canonici_di_provider_override_mode`,
`la_sola_preferenza_non_pinna_e_conserva_il_fallback`,
`la_preferenza_di_sessione_da_sola_non_produce_un_pin`,
`il_modo_pinned_senza_provider_non_pinna_il_ricordo_della_sessione`.
Frontend (`apps/web-ide/components/chat/provider-choice-logic.test.ts`): il wire
nei quattro stati del composer e i tooltip verificati INSIEME alla richiesta che
parte in quello stato.

## Aggiornamento 2026-07-27 (3) — il vincolo vale anche nel percorso agentico

> **Limite chiuso**: i due aggiornamenti precedenti lasciavano il pin efficace
> solo sul turno singolo (`study`). In `confirm` — il DEFAULT della UI — e in
> `automatic` la richiesta devia su `spawn_agent_run`, e `SpawnAgentParams`
> portava il solo NOME del provider: il vincolo moriva al confine dell'handler.

### Perche' era invisibile

Nel percorso agentico OGNI chiamata al gateway e' gia' pinnata al provider
risolto (`agent_graph_adapter/llm_gateway.rs`), quindi il pin dell'utente
sembrava rispettato. A cambiare fornitore non e' il gateway: e' l'ESECUTORE, fra
una chiamata e l'altra — escalation, ripiego su provider caduto, upscale di
finestra, cambio di tier. Un vincolo che non copre quei punti e' un vincolo che
vale fino al primo intoppo.

### Cosa vale ora

- `SpawnAgentParams` porta `ProviderChoice` INTERA (non il nome): la regressione
  "passo il nome e perdo la forza" non e' piu' scrivibile, la impedisce il tipo;
- il vincolo prosegue in `NativeRunInput::provider_pin` (`ProviderPin`, il tipo
  che porta anche il PUNTO UNICO del confronto, `ammette`) e da li' nelle DUE
  porte che possono cambiare fornitore in corsa, costruite PER il run:
  `PgEscalationPort` e `CatalogModelUpscalePort`. Gli undici rami che chiedono un
  candidato non sanno del pin: ricevono gia' solo candidati leciti;
- **l'escalation intra-provider resta viva**: il vincolo e' sul FORNITORE, non
  sul modello. Il run vincolato puo' ancora salire di modello dentro il fornitore
  scelto — e' cio' che tiene in piedi i run lunghi;
- **il ripiego cross-provider non viene cercato**, e la chiusura lo DICE
  nominando il vincolo e il pulsante da cui viene. Le due chiusure — rete di
  riserva esaurita contro vincolo esplicito — hanno lo stesso `StopReason` e
  portano l'utente in direzioni opposte: la differenza vive nel testo.

### Il prezzo, dichiarato

Un run vincolato su un fornitore che cade si FERMA invece di continuare altrove.
E' l'opposto di quanto deciso in ADR 0033 (dove il pin era imposto a tutti dal
gateway e un cooldown transitorio di 21s diventava un hard fail), e non lo
contraddice: li' nessuno aveva chiesto il vincolo, qui l'utente lo ha chiesto in
quella richiesta. Restano attive entrambe le protezioni di allora — il gateway
ritenta lo stesso modello con backoff e attende i cooldown transitori brevi
(≤45s, `RetryPolicy`) — quindi all'esecutore arrivano solo i fallimenti su cui
insistere non serve. Senza "Forza" non cambia nulla: il default resta la
preferenza col fallback.

### Il pin NON scende ai sub-agenti (misurato)

Figure del consiglio, revisori e worker non ereditano il vincolo. La misura, dal
catalogo vivo (`ai_price_catalog` + `nexus_purpose_model` +
`nexus_subagent_definitions`): i 19 kind chiedono 4 fasce distinte (medium 12,
light 3, heavy 3, high 1), mentre un fornitore ne copre da 1 a 5 — deepseek una
sola. Un vincolo ereditato lascerebbe senza modello 16 kind su 19 col fornitore
peggiore, e un solo cooldown fermerebbe l'intero panel. Per il kind `review`
sarebbe anche contrario al vincolo piu' forte che gia' vige (giudice != worker).
Resta la preferenza-forte tier-aware di `resolve_worker_model`, che DEGRADA. La
scelta e' dichiarata nei tooltip, non nascosta nel codice.

## Riferimenti

- `apps/web-ide/components/chat-panel.tsx:538-570` (Bug 1)
- `crates/mcp-core/src/orchestrator/core.rs:575-580` (Bug 2)
- `apps/web-ide/components/chat/agent-steps-panel.tsx:837` (Bug 3)
- `brain/agents/nodes/__init__.py:1597-1659` (executor rispetta override, emette thinking)
- `crates/mcp-core/src/chat_messages/agent_run.rs:392-457` (risoluzione provider con override)
- Screenshot incident 04/06/2026
