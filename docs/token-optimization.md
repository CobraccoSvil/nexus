# Ottimizzazione token in ingresso e uscita dai provider

Valutazione del 2026-08-12. Censimento verificato sul codice (file:riga), best practice
esterne con fonti, gap e piano di interventi. Questo documento e' il riferimento
permanente del filone: la sezione "Misure di riferimento" in fondo va aggiornata a
ogni fase chiusa.

## Il modello di spesa

- **L'INPUT domina i COSTI** quando non e' servito dalla cache. La voce piu' grossa
  e ricorrente sono gli schemi dei tool: `AGENT_TOOLS_JSON` = 95 tool, 84.445 char
  ~ 24.100 token inviati a OGNI turno (`crates/nexus-agent-tools/src/tool_schema.rs`).
  Su 10-30 turni/run sono 240K-720K token/run di soli schemi. Il peso reale dipende
  dal hit-rate cache per provider (misure 29/07/2026): deepseek 67%, mistral 5,2%,
  openrouter 9,2% — la potatura rende di piu' proprio sul tier economico dove il
  routing manda i task facili.
- **L'OUTPUT domina i TEMPI**: -50% di token di output ~ -50% di latenza; -50% di
  input ~ -1-5% (fonte: guida OpenAI). Le tre emorragie di output: thinking non
  governato per-modello (caso misurato su gemini-2.5-flash: 3 token visibili contro
  157 di reasoning = 98% dell'output fatturabile), verbosita' imposta dal template
  (mig 0363: riepilogo di 3-6 punti obbligatorio a ogni turno), spreco non
  contabilizzato (completion degeneri/retry/escalation invisibili al ledger).
- **Prima di ottimizzare, misurare**: il ledger non vede i tentativi falliti, la
  vista analitica (mig 0644) non ha consumatori applicativi, `usageBreakdown` non
  espone la cache, nessuna telemetria scompone il prompt nelle sue componenti.

## Censimento — token in INGRESSO

### Cosa funziona gia'
- Disciplina cache: `parte_stabile`/`CONFINE_DI_TURNO` (`nexus-types/src/system_prompt.rs`),
  breakpoint `cache_control` Anthropic sulla sola parte stabile (`anthropic.rs:518`) +
  breakpoint history sul terzultimo messaggio user (`anthropic.rs:565`),
  `prompt_cache_key` = sha256(tenant‖user‖parte_stabile‖nomi tool) per i provider
  `RequiresKey` (`openai_compat.rs:878`), pinning upstream per OpenRouter (mig 0657).
- Compressione history attiva e stratificata: fasi 3/7/15/30, keep_recent 5/3/2/1,
  max_chars 1200/600/300/100, token brake 0.55, hard cap 0.95, dedup per firma e
  contenuto, drop base64, auto-compact sessione 0.60
  (`nexus-agent-graph/src/decisions/context_reduction.rs`, settings `agent.context.*`).
- Cap sui tool result: 6000 char (`v_model_capabilities.tool_result_max_chars`),
  context budget 400K char, predictive cap 0.40, cache risultati tool TTL 1800s,
  budget allegati 500KB/sessione.
- Recall con limiti: memorie top-5 score>=0.78, mandato figure <=4000 char top-5,
  KB top-5, allegati top-8 chunk<=8000 char.

### Gap (input)
| # | Gap | Riferimento | Rimedio |
|---|---|---|---|
| I1 | Stima contesto passa `system_text` vuoto: i 4 freni (brake 0.55, hard cap 0.95, forced-RAG 0.30, smart-upscale) sbagliano di 7-24K token | `executor.rs:1099`, `:10125` | A2 |
| I2 | 95 schemi tool a ogni turno, nessuna potatura fuori o-series; whitelist esistenti ma gate a valle cablato `false` (motivato: i sub-run non passano da `build_tools_json_for_agent`) | `native_engine.rs:2629`, `agent_turn_setup.rs` | B2, B4 |
| I3 | Settings/colonne morti: `history_keep_recent_messages` (12), `history_max_old_tool_result_chars` (2000), `rolling_window_turns`, `system_prompt_offload_*`, `dedup_tool_results_enabled`; `drop_unused_base64_age` DB=3 ma codice cabla 8 | `vocabolario.rs:354,360`, `executor.rs:10141` | A5 |
| I4 | `supports_prompt_cache` falso su 9 coppie con cache reale nel ledger (mistral/mistral-small-latest: 2.461.120 letture su 152 chiamate) | `vocabolario.rs:308`, `capability_census.rs:376-390` | A5 |
| I5 | Cache hit-rate invisibile all'operatore: `usageBreakdown` non espone cache_read/cache_creation; la vista 0644 non ha consumatori | `chat_agent.rs:488-521` | A1 |
| I6 | Leve 0521 spente (continuity_trim, compress_offload, rolling_summary_offload) | mig 0521 | pilota post-B1 |

## Censimento — token in USCITA

### Gap (output)
| # | Gap | Riferimento | Rimedio |
|---|---|---|---|
| O1 | `max_tokens` del loop hardcoded `clamp(token_budget*4, 8192, 16384)` con `token_budget` derivato dalla LUNGHEZZA dell'ultimo messaggio (non dal task); Anthropic default 4096 hardcoded | `executor.rs:3124`, `router.rs:103`, `anthropic.rs:75` | A3 |
| O2 | Colonne `default_max_output_tokens`/`max_output_tokens_hard` esistono in `provider_capabilities` ma NessunLettore; setting `default_max_tokens` orfano | `vocabolario.rs:248-261`, mig 0240/0067 | A3 |
| O3 | Thinking budget globali, non per-modello: `anthropic_thinking_budget`, `orchestrator.gemini_thinking_budget` (8192), `providers.openai.reasoning_effort`; Kimi sempre acceso non spegnibile; guardia Anthropic `budget>=max_tokens` spegne il thinking IN SILENZIO | `anthropic.rs:395`, `capability.rs:153-159`, `openai.rs:36` | B3 |
| O4 | Google: il thinking ALZA `maxOutputTokens` (mt+budget, fino a 40960 richiesti) | `google.rs:1922-1926` | B3 |
| O5 | Completion scartate MAI nel ledger: `record_and_declare` solo su successo; 9 punti di ri-generazione censiti (retry stesso modello max 3, retry sanificato, fallback chain, retry-senza-forcing, auto-escalation signature-loop max 3, retry Google senza thinkingConfig, timeout per-tentativo, vision retry) | `routes.rs:586`, `types.rs:644` | A1 |
| O6 | Verbosita' imposta: mig 0363 obbliga riepilogo 3-6 punti a OGNI turno; nessuna direttiva di concisione nel system base; summary sub-run pagato intero e POI tagliato a 4000 char | `subagent_native.rs:5713` | A4 |
| O7 | Timeout non dimensionati sull'output: 75s/tentativo contro 16.384+24.576 token possibili su Google (8-11 min a 60-80 tok/s); i "n/d" della mig 0581 sono strutturali | `llm_timeouts.rs` | B6 |
| O8 | Batch API Anthropic (-50%) completa nel gateway ma usata da UN solo tool; nessun percorso interno differibile la usa | `batch.rs`, `quality_tools.rs` | B5 |
| O9 | Nessun report input_cost vs output_cost per modello (la vista 0644 non li seleziona); quota preventiva stima output con `req.max_tokens` (None -> 0) e ignora gli schemi | `billing.rs:34,100` | A1, A2, A3 |
| O10 | `strict` mai attivato (0 produttori); Anthropic ignora `response_format`; `run_cost_budget_usd` = 0.0 disabilitato | grep workspace, mig 0533 | fuori scope / B3+ |

## Best practice esterne (fonti)

- [OpenAI — Latency optimization](https://developers.openai.com/api/docs/guides/latency-optimization):
  -50% output ~ -50% latenza; -50% input ~ -1-5%. Nomi campi corti negli output
  strutturati (case study: un campo rinominato = -19 token/chiamata). Combinare passi
  sequenziali, parallelizzare, modelli piccoli dove bastano, evitare l'LLM dove basta codice.
- [Anthropic — Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents):
  "il piu' piccolo insieme di token ad alto segnale che massimizza l'esito";
  compaction, tool clearing, note-taking fuori contesto.
- [Anthropic — Writing effective tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents):
  tool result token-efficient (pagination, filtering, truncation con default sensati);
  schemi compatti ~ -40% token/tool tenendo solo i campi essenziali.
- [Redis — LLM token optimization](https://redis.io/blog/llm-token-optimization-speed-up-apps/),
  [NeuralTrust — AI token optimization](https://neuraltrust.ai/blog/ai-token-optimization-guide),
  [DigitalApplied — Prompt caching 2026](https://www.digitalapplied.com/blog/prompt-caching-2026-cut-llm-costs-engineering-guide):
  coppia `max_tokens` esplicito + vincolo di lunghezza NEL prompt; static-first/dynamic-last;
  response caching solo su workload con query ripetute; batch API -50% sui differibili;
  routing per difficolta' (gia' presente in Nexus).
- Ricerca (arXiv): adaptive reasoning budget (TALE-EP e affini) — budget di thinking
  stimato per task invece che fisso.

## Piano

Dettaglio operativo nel piano di sessione; qui la mappa e le dipendenze.

**Gruppo A (quick win)** — A1 contabilita' completa (status `discarded` +
`discard_reason` tipizzato, cache e costi in `usageBreakdown`, vista con
input/output/cache cost; quota: degeneri si', timeout no) · A2 stima contesto vera
(system+schemi) · A3 tetto d'uscita per-modello dalle colonne DB esistenti (punto
unico `output_budget` nel gateway) · A4 concisione nel template + riepilogo SOLO al
fine-run (deciso 12/08) · A5 igiene config.

**Gruppo B (strutturali)** — B1 telemetria `ai_prompt_breakdown`
(system|schemi|history|tool_result per chiamata) · B2 compaction schemi (~-40%,
nomi intoccati) · B3 thinking budget per-modello + fix inflazione Google + esito
dichiarato della guardia Anthropic · B4 sblocco gate M16 con catalogo REALE del run
· B5 batch API sui percorsi interni differibili · B6 timeout dimensionati
sull'output (velocita' osservata per modello).

```
Fase 0 (doc) -> A1 --+-> A3 --> B3 --> B6
                     +-> A4
                     +-> B1 --> B2 --> B4
                     +-> B5
A2, A5: indipendenti, in parallelo ad A1
```

## Cosa NON si fa (con motivo)

- **Response cache semantica/esatta**: il traffico e' agentico, prompt sempre diversi
  per costruzione (history crescente); il 50-90% delle fonti vale per workload FAQ.
  L'exact-match sui tool result esiste gia' (TTL 1800s). Riesaminare se B1 mostra
  ripetizioni reali.
- **Streaming nel loop agentico**: la scelta non-streaming e' deliberata (eventi step
  via SSE, `event_sink.rs:223`); lo streaming riduce latenza percepita, non token.
  Concern separato da questo filone.
- **Ritocco soglie/fasi della compressione history**: solo DOPO A2 (stima corretta) e
  B1 (scomposizione), altrimenti si tara su un numero sbagliato (regola H).
- **Accensione in blocco delle leve 0521**: sono leve di qualita' (recuperabilita'
  semantica), non di risparmio; pilota per singola leva dopo B1 (flag DB, zero codice).
- **Rinominare tool/campi con nomi corti**: assorbito da B2; rompere il contratto
  parser per un guadagno marginale rispetto alla compaction delle descrizioni.
- **Google Batch API** (oggi 501): effort alto, nessun percorso interno la richiede
  finche' B5 non gira su Anthropic.

## Distribuzione delle chiamate per provider (misurata 12/08/2026, 7 giorni)

Domanda: perche' alcuni provider vengono chiamati piu' di altri?

| provider | chiamate | quota | causa |
|---|---|---|---|
| mistral | 1.290 (di cui 1.074 `mistral-small-latest`) | 65% | selezione per TIER delle figure/sub-run (`Rank::CostFirst`): i purpose con `tier` valorizzato ignorano il modello statico e prendono il piu' economico del tier — `mistral-small-latest` e' il tier medium piu' economico eleggibile ($0.06/M input) |
| openrouter | 259 | 13% | primario di matrix per meta' degli intent (priority 80) |
| deepseek | 210 | 11% | primario di matrix per l'altra meta' (priority 80) |
| google | 100 | 5% | terzo primario (priority 80) + purpose light/high (explorer, reviewer) |
| kimi | 54 | 3% | 4 righe di matrix + purpose dedicati |
| groq | 49 | 2% | 14 righe di matrix TUTTE inattive: riceve solo il purpose `agent` |
| anthropic | 0 | — | cooldown billing ATTIVO (`credit_balance_too_low`, misurato alle 08:09 del 12/08) |
| openai | 0 | — | HTTP 429 quota ricorrente (ultimo alle 08:10 del 12/08); avrebbe il modello medium piu' economico (gpt-5-nano $0.05/M) |
| perplexity | 0 | — | in matrix solo per 4 righe (search) |

Osservazione collegata al piano: la selezione base (`Rank::CostFirst`) ordina sul
prezzo NOMINALE di listino, mentre la catena di escalation ordina gia' sul costo
ATTESO con `observed_cache_hit_rates`. Col hit-rate misurato, `deepseek-v4-flash`
($0.14/M, cache 67%) costa per token effettivo ~quanto `mistral-small-latest`
($0.06/M, cache 5,2% storica) ed e' tier heavy: il "piu' economico nominale" non
e' il piu' economico effettivo. Possibile intervento futuro (dopo B1): estendere
il criterio cache-aware dalla catena di escalation alla selezione per tier.

## Misure di riferimento

Baseline da rilevare PRIMA di ogni fase e confrontare dopo, dal ledger /
`ai_usage_analytics_view` su scenario eval fisso eseguito attraverso il gateway
reale (regola O):

| Metrica | Fonte | Baseline (data) | Dopo fase |
|---|---|---|---|
| token/costo per (provider, model) | vista analitica | da rilevare | |
| cache hit-rate per (provider, model) | `observed_cache_hit_rates` / vista | deepseek 67%, mistral 5,2%, openrouter 9,2% (29/07) | |
| p50/p95 `completion_tokens` per modello | vista analitica | da rilevare | |
| spesa nascosta (righe `discarded`) | ledger post-A1 | non misurabile prima di A1 | |
| componenti prompt (system/schemi/history/tool_result) | `ai_prompt_breakdown` post-B1 | non misurabile prima di B1 | |
| rapporto reasoning/(visibile+reasoning) Google | ledger (Separate) | 98% caso misurato (gemini-2.5-flash) | |
| tasso timeout per modello | `discarded(timeout)` post-A1 | "n/d" della mig 0581 | |

Storia degli aggiornamenti:
- 12/08/2026 — A1 chiuso: status `discarded` + causa nel ledger (mig 0701),
  vista con costi per direzione e scarti (mig 0702), cache e scarti in
  `usageBreakdown`. Baseline della spesa nascosta: da rilevare dopo il deploy.
- 12/08/2026 — A2 chiuso: i freni dell'executor (brake, hard cap, forced-RAG,
  smart-upscale) e il predictive cap contano system + schemi
  (`stima_overhead_turno` + punto unico `tools_schema_token_estimate`); la
  quota preventiva del gateway conta gli schemi. Le soglie DB (0.55/0.95/0.30)
  NON sono state ritoccate: prima si osserva la frequenza dei trigger col
  numero vero.
- 12/08/2026 — A5 chiuso: mig 0703 (DELETE dei 4 setting offload/rolling senza
  lettore; `supports_prompt_cache` allineata ai fatti del ledger) + wiring di
  `dedup_tool_results_enabled` e `drop_unused_base64_age` (vince il DB: 3, non
  il cablato 8).
- 16/08/2026 — Fase 5b (A/B lingua) infrastruttura pronta: varianti EN dei 4
  template machine-only come righe `<chiave>.en` + selettore CSV
  `prompt.english_variants` nel punto unico di lettura (mig 0725). Il flip per
  blocchi e' un UPDATE del setting con cutover secco (mai per-chiamata: cache
  fredda in entrambi i bracci), rollback = svuotare il CSV. Misure attese dal
  ledger a esercizio ripreso: `prompt_tokens` per purpose prima/dopo il flip
  (delta live misurato -13/-20% sui template tradotti, atteso ~5-10%
  dell'input di piattaforma).
