---
id: adr-0033-stato-provider-segnali-strutturati
kind: adr
title: "ADR 0033 - Stato tecnico del provider da segnali strutturati (billing/transient nel gateway)"
slug: 0033-stato-provider-segnali-strutturati
tags:
  - adr
  - gateway
  - routing
  - cooldown
  - provider-error
  - structural-signals
  - anti-pattern
  - single-source
auto_generated: false
created_at: 2026-07-01T00:00:00Z
updated_at: 2026-07-01T00:00:00Z
nexus_meta_version: 1
---

# ADR 0033 - Stato tecnico del provider da segnali strutturati (billing/transient nel gateway)

## Stato

Accettato e implementato (2026-07-01). Estende al `nexus-gateway` il principio
gia' sancito dall'[[0018-segnali-strutturali-vs-euristiche-testuali]] per il loop
agentico: lo **stato tecnico si deduce dai segnali strutturati, mai dal testo**.

## Contesto

Il gateway LLM (`crates/nexus-gateway`) deve, quando un provider fallisce,
decidere il tipo di cooldown da applicare:

- **billing** (crediti/quota esauriti): cooldown lungo, il provider non tornera'
  utile in pochi minuti; il fallback deve scalare a un altro provider;
- **transient** (5xx, rate-limit di finestra, errore di rete): cooldown breve.

Questa decisione avveniva sul **testo** dell'errore. Sul percorso di produzione
`server::routes::run_fallback` (e nel gemello per lo streaming SSE) l'errore del
provider risaliva come `anyhow::Error` stringa e veniva ri-classificato con:

```rust
let msg = err.to_string();            // "openai HTTP 429: {body}"
if is_billing_error(&msg) { mark_billing } else { mark_transient }
```

`is_billing_error` (`providers/openai_compat.rs`) e' un substring matching
case-insensitive sul messaggio gia' renderizzato. E' lo stesso anti-pattern
censito dall'ADR 0018: dedurre lo **stato** dal **testo**. Difetti concreti:

- **Confonde noise e segnale**: classifica su una stringa che contiene nome
  provider e status HTTP mescolati al body.
- **Non distingue i 429 ambigui**: un HTTP 429 puo' essere quota esaurita
  (billing, cooldown lungo) oppure rate-limit di finestra (transient). Un boolean
  su testo non ha modo di separarli in modo affidabile.
- **Fragile a lingua/provider**: ogni nuovo messaggio provider che non contiene
  i marker previsti sfugge alla lista.

Il segnale strutturato esisteva gia' nella risposta HTTP ma veniva **buttato**:
i provider (`providers/openai_compat.rs`, `anthropic.rs`, `google.rs`) leggevano
lo `status` e il body JSON e poi facevano `anyhow::bail!("{provider} HTTP
{status}: {text}")`, collassando status + `error.type` in una stringa opaca.

Esisteva inoltre una **duplicazione** dello stesso smell in un modulo morto:
`crates/nexus-gateway/src/fallback.rs` (`FallbackChain`), dichiarato in `lib.rs`
ma mai istanziato in produzione (il path vivo e' `run_fallback` in `routes.rs`),
che ri-classificava anch'esso con `is_billing_error(&msg)`.

Il punto canonico deterministico esisteva gia' altrove, ma **non riusabile** dal
gateway: `mcp-core/src/provider_error_classifier.rs::classify_text` (punto unico
guard-protetto, Wave 8b / ADR 0026) e il gemello orientato al cooldown
`mcp_core::brain_agent_client::classify_provider_error(error_class, msg)`
classificano sulla mappa HTTP status strutturata col testo solo come fallback
(regex `billing_re`). `mcp-core` pero' **dipende da** `nexus-gateway` (edge
unidirezionale: il gateway e' anche libreria riusata da mcp-core), quindi il
gateway non puo' importare mcp-core senza creare un ciclo. Il gateway non aveva
quindi un equivalente strutturato proprio.

## Decisione

Introdurre un **errore tipizzato** dei provider e un **punto unico** di
classificazione basato sui segnali strutturati, sul modello del punto canonico
`mcp-core` (`provider_error_classifier.rs`), rispecchiato — non importato — per il
vincolo di dipendenza sopra.

### 1. Errore tipizzato `ProviderError`

Nuovo modulo `crates/nexus-gateway/src/provider_error.rs`:

```rust
pub struct ProviderError {
    pub provider: String,
    pub status: u16,                 // status HTTP (segnale strutturato)
    pub error_class: Option<String>, // error.type/code/status dal body JSON
    pub message: String,             // body grezzo: SOLO Display/diagnostica
}
```

`ProviderError::from_http(provider, status, body)` estrae l'`error_class`
leggendo i **campi** JSON canonici dei dialetti provider (`error.type` per
OpenAI/Anthropic, `error.status` per Google, `error.code` se stringa) — non fa
substring sul messaggio. Il `Display` riproduce il formato storico
`"{provider} HTTP {status}: {body}"`, cosi' i consumatori a valle (il body del
500 che il brain legge) restano invariati.

### 2. Punto unico di classificazione (regola L)

`ProviderError::cooldown_reason()` decide **solo** su status + `error_class`:

1. `status == 402` -> Billing.
2. `error_class` nel set billing (`insufficient_quota`, `credit_balance_too_low`,
   `quota_exceeded`, `billing_error`, ...) -> Billing. Copre il 429 ambiguo:
   `type: "insufficient_quota"` -> billing, `rate_limit_error` -> transient.
3. Fallback **residuo** sul body via il punto unico dei marker testuali
   `providers::is_billing_error`, necessario solo per i provider senza
   `error_class` dedicato (Anthropic segnala il credito esaurito con
   `type: "invalid_request_error"` generico e il dettaglio solo nel messaggio).
   E' l'identico compromesso del classificatore canonico di mcp-core, documentato
   come tale.

Il free helper `cooldown_reason_for(&anyhow::Error)` fa il downcast al tipizzato o,
se l'errore e' di trasporto/parsing (rete, timeout, JSON), ritorna Transient.
`run_fallback` e lo stream path delegano **entrambi** a questo helper: nessun
call site tocca il testo. La mappatura motivo -> durata e' centralizzata in
`CooldownManager::mark(reason)` (regola L).

### 3. Provider emettono il tipizzato

I 6 siti d'errore del path chat (`openai_compat` complete/stream — che copre
openai/mistral/deepseek/vllm — piu' `anthropic` e `google` complete/stream)
ritornano `ProviderError::from_http(...)` invece del `bail!` stringa.

### 4. Rimozione del modulo morto `fallback.rs`

`FallbackChain` viene eliminato: era codice morto che duplicava la logica di
`run_fallback` con lo stesso match testuale. La sua rimozione elimina la
duplicazione (regola L) e non lascia in giro un secondo esempio dell'anti-pattern.

## Conseguenze

Positive:

- La classificazione billing/transient e' **deterministica** su status +
  `error_class`, robusta a lingua e a nuovi messaggi provider.
- Distingue i 429 ambigui (quota vs rate-limit) che il boolean su testo non
  sapeva separare.
- **Un solo** punto di classificazione nel gateway; i due call site delegano.
- Nessuna duplicazione: il modulo morto e' rimosso.
- Il body del 500 e la lista `failures` restano invariati (Display preservato):
  nessuna regressione per il brain che legge il body.

Negative e caveat:

- Resta un fallback **residuo** su marker testuali per Anthropic, che espone il
  billing solo nel messaggio (`type` generico). E' un vincolo dell'API Anthropic,
  non una scelta: e' confinato dietro i segnali strutturati (primari) e
  centralizzato in `is_billing_error`. Se in futuro Anthropic esporra' un
  `error.type` dedicato, il fallback diventera' inutile e andra' rimosso.
- La classificazione del gateway e quella di `mcp-core`
  (`provider_error_classifier.rs`) restano due implementazioni con lo **stesso
  vocabolario** ma senza codice condiviso: `mcp-core` dipende da `nexus-gateway`
  (edge unidirezionale), quindi il gateway non puo' importare il classificatore
  canonico senza ciclo, e l'unico home comune ai due (`nexus-auth`/`nexus-cache`)
  sarebbe semanticamente improprio. Consolidamento in un crate condiviso a monte di
  entrambi possibile in futuro se il vocabolario dovesse divergere.

## Regola M (CLAUDE.md)

Questo ADR e' il razionale della regola M: **lo stato tecnico si deduce dai
segnali strutturati (status HTTP, `error_class`, `stop_reason`, exit-code, ...),
mai dal testo libero del messaggio.** Generalizza l'ADR 0018 (loop agentico) e la
regola H (fix definitivi, mai toppe: una classificazione su testo e' una toppa che
sfugge a ogni nuovo messaggio). Il testo resta ammesso solo come fallback residuo
documentato, dietro i segnali strutturati, quando il provider non espone un
segnale dedicato.

## Riferimenti

- Regola M (CLAUDE.md) - stato tecnico dai segnali strutturati, mai dal testo.
- Regola L (CLAUDE.md) - punto unico di controllo per ogni concern.
- Regola H (CLAUDE.md) - fix definitivi, mai toppe.
- ADR correlati: [[0018-segnali-strutturali-vs-euristiche-testuali]] (stesso
  principio nel loop agentico), [[0026-punto-unico-de-duplicazione]] (catalogo
  punti unici), [[0020-gate-unico-disponibilita-provider]] (gate provider),
  [[0024-capability-fonte-unica-classificazione]] (fonte unica capability).
- Punto canonico gemello (non riusabile per il vincolo di dipendenza):
  `crates/mcp-core/src/provider_error_classifier.rs` (`classify_text`, guard-protetto
  in `scripts/check-single-source.sh`, Wave 8b) e
  `crates/mcp-core/src/brain_agent_client.rs` (`classify_provider_error`).
- Codice: `crates/nexus-gateway/src/provider_error.rs` (nuovo punto unico),
  `crates/nexus-gateway/src/server/routes.rs` (`run_fallback` + stream),
  `crates/nexus-gateway/src/cooldown.rs` (`CooldownManager::mark`),
  `crates/nexus-gateway/src/providers/{openai_compat,anthropic,google}.rs`.
- Rimosso: `crates/nexus-gateway/src/fallback.rs` (`FallbackChain`, modulo morto).
</content>
