# Studio emendato: provider (Perplexity + OpenRouter/Groq/Cohere), admin, robustezza provider, orchestratore

Revisione 2026-07-12 (HEAD `1409a080`, migrazioni fino a `0563`, repo `D:\IDEAI`).
Sostituisce la rev. 2026-07-06 (commit `6d23f9a0`, mig 0532): da allora 138 commit
hanno chiuso una parte dei problemi censiti e cambiato l'ambiente di esecuzione.
Ogni riferimento file:riga e' stato ri-verificato sul tree attuale; gli stati
(CHIUSO/PARZIALE/APERTO) riflettono il codice, non le intenzioni.

## 0. Premesse verificate e delta

### 0.1 Cosa e' cambiato (storico verificato)

Rispetto allo studio originale (pre 2026-07-06):

| Premessa originale | Stato reale |
|---|---|
| brain Python (LangGraph) e' il path di produzione; Rust in shadow | Falso. `Engine::Rust` primario instradato globalmente; cutover versionato (mig `0532`). La dir `brain/` e' stata ELIMINATA dal repo (commit `75a6d62`) |
| intent `ricerca_web` richiede lavoro sul classifier brain Python | Falso. Classifier Rust (`intent_classifier.rs`), cutover versionato (mig `0458`+`0460`) |
| Routing primario = `nexus_routing_matrix` | La matrice e' solo fallback: `behavior_mode='dinamico'` cost-first dal catalog (header mig `0530`) |
| Parte D: "creare trait EmbeddingProvider" | Il trait `Embedder` esiste (`nexus-orchestrator/src/embedder.rs:18-43`), ora anche con `signature()`/`name()` per il reindex-on-model-change (`:37,:42`) |

Rispetto alla rev. 2026-07-06 (nuove condizioni):

| Cosa | Delta al 2026-07-12 |
|---|---|
| Ambiente di esecuzione | **Windows nativo canonico**: PowerShell, toolchain MSVC, servizi WinSW via `deploy/deploy-local.ps1`, dev via `deploy/dev-start\|stop\|build\|service.ps1`. `scripts/dev-wsl.sh` RIMOSSO; `pnpm verify` orchestrato da `scripts/verify.mjs`. Niente WSL |
| Regole CLAUDE.md nuove | **M** (stato tecnico SOLO da segnali strutturati: vieta `contains()` sul testo — impatta A2/F3/G) e **N** (identificatori canonici inglesi, mig `0558`) |
| Scala tier | Vocabolario a 5 livelli COMPLETATO ovunque (mig `0547`), punto unico `nexus-agent-graph/src/decisions/tiers.rs:22` (`PERFORMANCE_TIERS`) |
| Catene tier | Consolidate sul punto unico `agentic_tier_chain` (`orchestrator/model_routing.rs:311`; commit `993fbb79`) |
| Failover | Whitelist 4xx recuperabili DB-driven (mig `0560`, `ProviderUnavailableInfo::allows_cross_provider_failover`); failover su risposta degenere (`ec805f1c`); causa strutturata fino alla chat (`a6a21e34`, `POLICY_TIER_EXCLUDED` deployato); last_error provider persistito in DB (mig `0536`) |
| Feature nuove fuori piano | Consiglio delle Competenze (advisory panel multi-provider a monte, mig `0546-0554`) e ServiceManager multipiattaforma (ADR 0038, mig `0541/0552/0557/0561-0563`) — consumano purpose/catalog e vanno considerate consumer del routing |
| ADR nuove rilevanti | 0030 (selettore modello unico), 0033 (pin/retry/anti-loop onesto), 0034/0035 (esito strutturato, progresso misurato), 0037 (chat multiprovider), 0038 (servizi multipiattaforma) |
| Numerazione migrazioni | Le nuove partono da **0564** |

### 0.2 Landmine regola H: cutover engine + feature flag orchestratore (entrambe chiuse)

- **CHIUSA (motore)**: il cutover (`'*'->rust` in `nexus_orchestrator_engine`) e'
  versionato dalla mig `0532` (Parte 0, committata in `6d23f9a0`).
- **CHIUSA (feature flag orchestratore) — mig `0564`, eseguita 2026-07-12**: quello
  che sembrava un singolo flag (`plan_phase_enabled`) era una landmine di CLASSE.
  Interrogando il DB vivo: dei 39 booleani `orchestrator.*` sono TUTTI `true`, ma
  solo `dag_topological_enabled` (mig `0466`) e `subagent_isolation_enabled`
  (mig `0517`) erano versionati. Su wipe + re-migrate ~15 flag tornavano `false`
  → planner, understanding, sub-agenti, verifier, worker mode, DAG parallelo spenti.
  La mig `0564` versiona (pattern idempotente della `0517`, `value <> 'true'`) i
  **10 flag divergenti E vivi nel codice Rust**: `plan_phase_enabled`,
  `verifier_enabled`, `plan_rationale_enabled`, `subagents_enabled`,
  `worker_mode_enabled`, `dag_parallel_enabled`, `exploratory_verify_enabled`,
  `understanding_{enabled,fanout_enabled,synthesize_enabled}`. Dry-run sul vivo:
  0 righe toccate (no-op), corregge solo il DB fresco.
- **Scoperta collaterale**: 5 flag che la `0439` citava come "prerequisiti gia' on"
  (`adaptive_gating_enabled`, `adaptive_classifier_enabled`,
  `plan_rationale_persist_as_note`, `clarify.require_llm_classifier`,
  `orchestrator.meta_steps.*`) sono **MORTI nel codice Rust** (residui del brain
  Python: `is_eligible_adaptive` era Python; il codice legge `reflection_enabled`
  top-level, non `meta_steps.*`). Esclusi dalla `0564` — vanno rimossi, non accesi
  (audit separato, come la `0463`).
- **Follow-up aperto**: namespace `agent.*` ha 72 booleani `true` / 1 `false`: stessa
  classe potenziale, ma valori NON uniformi (include flag di sicurezza/policy tipo
  `dlp_allow_cloud_*`) → audit dedicato caso-per-caso, NON un UPDATE di massa.

### 0.3 Problemi di routing per tier (T1-T6) — stato al 2026-07-12

| # | Problema | Stato | Evidenza attuale | Indirizzato in |
|---|---|---|---|---|
| T1 | Routing matrix senza FK/validazione verso `ai_price_catalog`; config stantia ricorrente | **PARZIALE (nucleo APERTO)** | unica mitigazione runtime: `heal_orphan_pinned_models` (`routing_matrix_auto_promoter.rs:126,407`), copre solo i pin a modelli inesistenti. Nessun FK/validator/endpoint; i workaround una-tantum continuano: mig `0530` E mig `0556:31` (seconda della stessa classe) | Parte E (validator promosso a MVP) |
| T2 | Fallback silenzioso intent → `("light","chat")` | **PARZIALE** | il path dinamico primario ora logga WARN e usa `("medium","reasoning")` (`core.rs:886-905`, "niente magic fallback light"); l'helper `intent_tier_capability` resta silenzioso (`core.rs:260-262`) ed e' usato dai gate tool/vision/cooldown (`core.rs:603,708,1049`) | Parte A (seed obbligatorio) + G g6 (residuo) |
| T3 | Mismatch scala tier 5 vs 3 | **CHIUSO** | mig `0547:41-44` allarga i CHECK residui (`nexus_intent_capability.base_tier`, `nexus_purpose_model.tier`, `nexus_routing_slots_matrix.preferred_tier`, `nexus_intent_routing_requirements`); vocabolario unico `decisions/tiers.rs:22` (commit `76cd1732`) | — |
| T4 | `KNOWN_PROVIDERS` hardcoded a 5 (dashboard admin) | **CHIUSO per la dashboard** (2026-07-12) | Dashboard FATTA: `provider_names_for_status` (`environment.rs`) deriva la lista da catalog∪api_key (no-op sul vivo, +test); `KNOWN_PROVIDERS` resta solo fallback DB-down. Verificato che gli usi in `model_routing.rs:897,986` NON sono bug: sono fallback (nessun setting) e garanzia-dei-core (un provider nuovo entra via i settings di gerarchia). RESTA solo: registrazione bootstrap | Parte F1 (registry) |
| T5 | `provider_map` sync LiteLLM hardcoded a 5 provider | **APERTO — non micro-fix** | `models.rs:192-202`: mappa prefisso->provider per decidere quali modelli LiteLLM importare. Data-driven pulito richiede una tabella prefissi nuova + ha rischio correttezza (prefisso troppo largo importa modelli sbagliati). NON urgente: i provider nuovi si seedano con la loro migrazione catalog; il sync serve solo agli aggiornamenti prezzi automatici. Da fare con l'onboarding del 1o provider o con F3 | Onboarding / F3 |
| T6 | Conflitto `ricerca_web` x routing dinamico (pavimento agentico + `require_tool_use` escludono sonar) | **APERTO** | pavimento `floor_tier_for_agentic` (`model_routing.rs:194-208`) + `agentic_min_tier` (`:171`) intatti; `require_tool_use:true` sui path agentici (`model_routing.rs:353,590,656`; `core.rs:712`). Zero `web_search` nel tree. Esiste pero' la primitiva `best_non_agentic_model` (`model_routing.rs:480`, `require_tool_use:false`) come base del flusso dedicato | Parte A (A3/A4) |

Mitigato by-design (invariato): il claim `sensitivity_tier` — enforcement nel gateway
(`effective_tier = max(claim, classificazione)` + `validate_tier_claim`); dal 6d23f9a
il chiamante invia comunque il claim onesto (`classify_sensitivity`).

## 1. Contesto e obiettivi (invariati)

Quattro bisogni non coperti dai 5 provider attuali (OpenAI, Anthropic,
Google/Gemini+Vertex, Mistral, DeepSeek):

1. Ricerca web nativa con citazioni → Perplexity (Sonar).
2. Copertura orizzontale a basso costo di integrazione → OpenRouter (300+ modelli).
3. Inferenza ultra-veloce per task interni ad alta frequenza → Groq.
4. Rerank/embedding di qualita' per il RAG/Qdrant → Cohere.

Scelte confermate dall'utente: tutti e 4 i provider; Perplexity come motore
search/grounding dedicato con citazioni fino alla UI (tool calling disabilitato:
`sonar` rifiuta le tool definitions con HTTP 400). In piu': rivisitazione admin
(Parte E), robustezza/generalizzazione provider (Parte F), potenziamento
orchestratore (Parte G). Regole di progetto: G (config nel DB), H (fix definitivi),
L (punto unico), M (segnali strutturati), N (identificatori canonici),
F (niente segreti nei log).

Nessuna delle Parti A-F risulta iniziata al 2026-07-12: nessun provider nuovo nel
gateway, nessun wizard admin, nessuna interfaccia reranker. L'unica anticipazione:
`capability.rs:65` mappa gia' i dialetti `tool_choice` per
`"openrouter" | "groq" | "xai" | "azure_openai"` (non cablata end-to-end).

## 2. Architettura accertata (stato reale al 2026-07-12)

Catena di produzione, 100% Rust:

```
mcp-core (intent: orchestrator/intent.rs + intent_classifier.rs;
          selezione: orchestrator/model_routing.rs 'dinamico' cost-first dal catalog,
          nexus_routing_matrix come fallback; EligibilityFilter in model_selection.rs)
  -> nexus-agent-graph (grafo agentico di produzione, nodes/ + decisions/)
  -> nexus-gateway (policy sensitivity/DLP + fallback + cooldown + causa strutturata)
  -> provider Rust (OpenAI-compat -> OpenAiCompatClient; Anthropic/Google -> adapter dedicati)
```

- Punto unico OpenAI-compat: `crates/nexus-gateway/src/providers/openai_compat.rs`
  (endpoint `{base_url}/chat/completions`, Bearer, SSE, tool calling, usage).
  Struct client a 4 campi (`:78-83`): NIENTE `extra_headers`, NIENTE `citations`.
- Registrazione provider: `crates/nexus-gateway/src/server/bootstrap.rs`
  (`ProviderKeys` a campi fissi `:86-97` + blocchi `if let` `:154-205`). Nessun registry.
- Health probe: `provider_health_probe.rs:43` (`PROBED_PROVIDERS` hardcoded a 5,
  commento che la dichiara "allineata con KNOWN_PROVIDERS").
- Capability/costi: `ai_price_catalog` con `performance_tier` a 5 livelli, inferito a
  ogni sync da `infer_tier_from_name` (`model_catalog_sync.rs:412`); capability via
  vista `v_model_capabilities` (`capability.rs`).
- **Tier — punti unici nuovi**: vocabolario `decisions/tiers.rs:22`
  (`PERFORMANCE_TIERS`, 5 livelli); catena di degrado `agentic_tier_chain`
  (`model_routing.rs:311`), delegata da `core.rs:1052-1057,1153-1157`; pavimento
  agentico `floor_tier_for_agentic` (`model_routing.rs:194-208`).
- **Selettore**: `EligibilityFilter` (`model_selection.rs:126-158`) con
  `require_tool_use`, `require_thinking_non_exclude`, `capability: Option<&str>`
  (`:143`), `min_context_window`, `exclude_providers`, `apply_cooldown`,
  `only_provider` (`:157`, propagazione pin ai subagenti).
- **Failover/robustezza**: `classify_provider_error` + `ProviderHttpError`
  (punto unico regola M); whitelist 4xx recuperabili da
  `routing.client_error_failover_codes` (mig `0560`); failover su risposta degenere;
  `POLICY_TIER_EXCLUDED` emesso dal gateway (`server/routes.rs:94`,
  `policy_engine.rs:105`); last_error persistito (mig `0536`,
  `nexus_provider_health/_history`). Escalation con `max_escalations`
  (`runtime/ports.rs:1282-1283`, enforced `progress_controller.rs:554`) e
  `failover_downgrade_penalty` cablata (`governance.rs:107,120`,
  `escalation.rs:223-224`).
- Embedding: trait `Embedder` (`embedder.rs:18-43`) con `signature()`/`name()`
  (`:37,:42`, reindex-on-model-change). Rerank: `packages/embeddings/src/reranker.ts`
  classe concreta, nessuna interfaccia.
- Riferimenti architetturali: ADR 0030 (selettore unico), 0033, 0034, 0035, 0037, 0038.

## 3. Parte 0 — Bonifica cutover — ESEGUITA (commit `6d23f9a0`), follow-up aperti

Eseguito e committato: mig `0532` (cutover `'*'->rust` versionato + entry `brain`
rimossa dal watchdog), default difensivi → Rust (`select_engine`,
`select_classifier_engine`), task_watchdog senza restart brain, claim sensitivity
onesto, doc/env bonificati. La dir `brain/` e' stata poi eliminata dal repo
(`75a6d62`) con relative pulizie script (`ad881193`, `fec5ab6d`).

**Follow-up ancora aperti** (invariati + uno nuovo):
1. Rimozione fisica di `brain_agent_client.rs` (esiste, in QUARANTENA documentata;
   solo refactor `9bf756c3`) + variante `Engine::Python` (`agent_run.rs:4457-4474`)
   + valore `'python'` dal CHECK di `nexus_orchestrator_engine`.
2. NUOVO: doc-modulo di `provider_error_classifier.rs:3-9` cita ancora
   `brain/providers/error_handler.py` come riferimento di parita' — stale, da bonificare.
3. NUOVO (0.2): **mig `0564`** — versionare `orchestrator.plan_phase_enabled='true'`
   (landmine gemella del cutover engine).

## 4. Parte A — Perplexity (search/grounding + citazioni UI) — PROVIDER onboarded (2026-07-12)

Ruolo invariato: provider dedicato alla ricerca web citata, NON nodo agentico.

Stato: **provider FATTO** via registry F2 (mig `0568_perplexity_provider.sql`),
**citazioni + intent ANCORA DA FARE** (il valore distintivo).
- **Mig 0568**: settings `perplexity_api_key`/`_enabled`; registry (`perplexity`,
  `openai_compat`, base_url `https://api.perplexity.ai`, **`supports_tools=false`**);
  3 modelli Sonar `is_enabled=false` (`sonar` 1/1, `sonar-pro` 3/15,
  `sonar-reasoning-pro` 3/15) con **`supports_tool_use=false`** (garanzia A4: il
  selettore agentico li esclude) e capability `web_search` (per il futuro flusso).
  `default.yaml`: `perplexity` aggiunto. Additivo/opt-in (verificato).
- **Prezzi DA VERIFICARE** prima di abilitare: Perplexity ha un request fee per
  "search context size" NON modellato in `ai_price_catalog`.
- **PROSSIMO PEZZO (grosso) — citazioni end-to-end + intent** (vedi A2/A3 sotto):
  tocca `openai_compat.rs` (punto unico critico: campo `citations` nella struct
  wire) e il **frontend web-ide** (`message-list.tsx`, oggi CONTESO da un'altra
  sessione attiva -> da fare quando la concorrenza si calma). Il provider da solo
  non ha ancora il valore search-citato: e' il prerequisito.

### A1. Provider Rust (invariato)
- Nuovo `crates/nexus-gateway/src/providers/perplexity.rs` su template
  `mistral.rs` (wrapper sottile su `OpenAiCompatClient`, regola L):
  `DEFAULT_BASE_URL = "https://api.perplexity.ai"`, `name() = "perplexity"`,
  `supports_streaming() = true`, `supports_tools() = false`,
  `tier_compatibility() = [0,1,2]`.
- Export in `providers/mod.rs`; registrazione in `bootstrap.rs`; `"perplexity"` in
  `PROBED_PROVIDERS` (`provider_health_probe.rs:43`). Se F1 (registry) e' gia' fatto,
  questi punti collassano in righe DB.

### A2. Propagazione citazioni (invariata; rafforzata dalla regola M)
`openai_compat.rs` non ha alcun campo `citations` (riverificato). La regola M impone
esattamente questo approccio: le citazioni viaggiano come CAMPO STRUTTURATO wire→UI,
mai estratte dal testo. Sei hop, tutti `Option`/`#[serde(default)]`, canale
`metadata` JSONB esistente su `chat_messages` (nessun ALTER):
1. `nexus-gateway/src/types.rs` — `LlmResponse.citations: Option<Vec<String>>`.
2. `openai_compat.rs` — struct wire `ChatCompletion` + mapping in
   `from_chat_completion` (una sola volta nel punto unico).
3. `server/routes.rs` — nessuna modifica (serde).
4. `mcp-core/src/nexus_gateway.rs` — `GwResponse.citations`.
5. `mcp-core/src/chat_messages/agent_run.rs` — propagare in `SpawnAgentResult`
   e scrivere `"citations"` nel blocco `metadata`.
6. Lettura: `persistence.rs` (`to_message_view`) → frontend
   `apps/web-ide/lib/api/chat.ts` + `components/chat/message-list.tsx`
   (pannello "Fonti consultate").
Streaming: citazioni nel risultato finale; estensione SSE futura, non MVP.

### A3. Instradamento (vincoli T2/T6, agganci verificati)

Il percorso primario e' il dinamico cost-first; la matrice e' fallback. La
convenzione `intent != "chat"` attiva pavimento tier (`floor_tier_for_agentic`) +
`require_tool_use`, che escluderebbe sonar (`tools=false`). Quindi:

**Fase 1 (obbligata, non alternativa): pin esplicito + pulsante UI.**
- Pulsante "Cerca sul web" in chat che invia con `pin_provider:"perplexity"` (+
  modello dal catalog). Il pin bypassa il selettore dinamico agentico in modo pulito
  (stesso canale del pin gia' propagato ai subagenti via
  `EligibilityFilter.only_provider`). Alias `web-search -> perplexity/sonar-pro`
  in `config/model-aliases.yaml` per uso via API.

**Fase 2: intent end-to-end come flusso NON-agentico.**
- Capability `web_search: true` nel JSONB `capabilities` di `ai_price_catalog` per i
  modelli sonar; il selettore la richiede via il campo GIA' ESISTENTE
  `EligibilityFilter.capability` (`model_selection.rs:143`) — nessuna modifica
  strutturale al filtro.
- Flusso non-agentico modellato sulla primitiva esistente `best_non_agentic_model`
  (`model_routing.rs:480`, gia' `require_tool_use:false`): variante/parametro con
  `capability="web_search"`, fuori dal pavimento agentico.
- Intent `ricerca_web` in `intent_classifier.rs` (`ALLOWED_INTENTS`, oggi 11 voci,
  `:75-87`) — lavoro interamente Rust.
- Righe in `nexus_routing_matrix` per `ricerca_web` → (`perplexity`,`sonar-pro`),
  fallback `sonar`: coprono il solo percorso di fallback.

**Migrazione unica (numerazione da `0564+`), per T2 e regola G:**
- `settings`: `perplexity_api_key` (is_secret), `perplexity_enabled`.
- `ai_price_catalog`: `sonar`, `sonar-pro`, `sonar-reasoning-pro` con prezzi
  (1/1, 3/15, 2/8 $/Mtok + request fee da annotare), `context_window`
  (~127k / ~200k), capabilities `{tools:false, vision:true, web_search:true}`,
  `performance_tier` esplicito (non affidarsi all'inferenza per nomi nuovi).
- `nexus_provider_capabilities`: `tool_choice_style='none'`.
- **`nexus_intent_capability`: seed di `ricerca_web`** — resta OBBLIGATORIO: T2 e'
  mitigato solo sul path dinamico (WARN + medium/reasoning, `core.rs:886-905`);
  l'helper dei gate e' ancora silenzioso (`core.rs:260-262`).
- `config/policies/default.yaml` (+ hybrid): aggiungere `perplexity` a `providers:`.

### A4. Tool-off garantito (invariata)
- Il selettore esclude gia' i modelli `tools=false` dai path agentici
  (`require_tool_use:true` su `model_routing.rs:353,590,656`, `core.rs:712`).
  Garanzia per l'agentico, ostacolo per il percorso dedicato — da cui Fase 1/Fase 2.
- Verificare che `resolve_tool_choice` non forzi `tool_choice` su `perplexity`.

## 5. Parte B — OpenRouter (gateway orizzontale) — ONBOARDED opt-in (2026-07-12)

FATTO via il registry F2 (mig 0565), come Groq: ZERO nuovo codice Rust.
- **Mig `0567_openrouter_provider.sql`**: settings `openrouter_api_key`/`_enabled`;
  registry (`openrouter`, `openai_compat`, base_url `https://openrouter.ai/api/v1`);
  2 modelli ESEMPIO `is_enabled=false` (`x-ai/grok-4.5` frontier 2/6 ctx 500k,
  `z-ai/glm-5.2` high 0.42/1.32 ctx 1M), prezzi da openrouter.ai (lug 2026).
  `config/policies/default.yaml`: `openrouter` aggiunto (YAML validato).
- **Additivo/opt-in, zero impatto** (verificato: 0 modelli enabled). OpenRouter e'
  transport: l'admin aggiunge i model id `vendor/model` che gli servono (qui solo
  2 esempi, per non inondare il catalog).
- **Header (attrito 1) NON implementato di proposito**: `HTTP-Referer`/`X-Title`
  sono raccomandati ma NON obbligatori (l'API funziona col solo Bearer). Restano un
  miglioramento opzionale (campo `extra_headers` in `OpenAiCompatClient` + registry)
  da fare se/quando serve l'attribuzione, senza toccare ora il punto unico critico.

Premesse riverificate (per l'eventuale completamento):
- Provider Rust `providers/openrouter.rs` (template mistral), export, bootstrap,
  probe, policy yaml (o registry F1 se gia' disponibile). Nota: `capability.rs:65`
  anticipa gia' il dialetto `tool_choice` per `openrouter` (e `groq`/`xai`).
- Header custom: `OpenAiCompatClient` NON ha `extra_headers` (riverificato,
  `openai_compat.rs:78-83`) — aggiungere campo opzionale
  `extra_headers: HashMap<String,String>` nel punto unico (`HTTP-Referer`,
  `X-Title`). Non bloccante, consigliato.
- Model id `vendor/model`: `model_alias_resolver.rs:191` (`.split('/').nth(1)`)
  riverificato — instradare OpenRouter con model id diretti dalla routing matrix;
  se servono alias logici, correggere il parser (regola H).
- catalog_sync: filtro/whitelist per OpenRouter su `/models`.
- T5: prefissi OpenRouter in `provider_map` (`models.rs:192-202`) oppure registry F1.
- DB: settings + catalog/capabilities per i soli modelli usati, `performance_tier`
  esplicito.

## 6. Parte C — Groq (velocita' per task interni) — ONBOARDED opt-in (2026-07-12)

FATTO via il registry F2 (mig 0565), ZERO nuovo codice Rust — dimostrazione che il
registry funziona: un provider OpenAI-compat = dati.
- **Mig `0566_groq_provider.sql`**: settings `groq_api_key` (segreta, vuota) +
  `groq_enabled`; riga `nexus_provider_registry` (`groq`, `openai_compat`,
  base_url `https://api.groq.com/openai/v1`); 4 modelli catalog **`is_enabled=false`**
  (`llama-3.1-8b-instant` light, `llama-3.3-70b-versatile` medium,
  `openai/gpt-oss-20b` light, `openai/gpt-oss-120b` high), prezzi $/Mtok verificati
  (groq.com/pricing, lug 2026), `capability_source='manual'`.
- **`config/policies/default.yaml`**: `groq` aggiunto al blocco `providers:` (YAML
  validato).
- **Additivo e OPT-IN, zero impatto**: key vuota -> gateway non costruisce il
  provider; modelli disabled -> routing non li seleziona, probe non li sonda
  (verificato in transazione: 0 modelli groq enabled). Attivazione admin in 3 step:
  inserire `groq_api_key`, abilitare i modelli, opz. instradare purpose interni.
- **Nota**: `openai/gpt-oss-*` hanno lo slash (come vendor/model OpenRouter):
  instradare con model_id diretto. T5 (prezzi via sync) resta manuale finche' non
  data-driven.
- **Da fare pre-uso (serve la API key)**: smoke test `POST /v1/complete` con
  `pin_provider:"groq"` su un modello abilitato -> esercita il provider generico F2
  end-to-end.

## 7. Parte D — Cohere (rerank + embedding RAG) — RIDIMENSIONATA

- **Embedding (Rust): il trait esiste ed e' migliorato** — `Embedder`
  (`embedder.rs:18-43`) ora espone `signature()`/`name()` (`:37,:42`) con meccanismo
  reindex-on-model-change GIA' esistente: il rischio "cambio embedder → migrazione
  vettori" e' in parte gia' gestito dal sistema. Lavoro reale:
  `CohereEmbeddingProvider` (reqwest verso `api.cohere.com/v1/embed`), setting DB
  `agent.rag.embedding_provider` (assente: nessuna selezione provider nelle mig),
  selezione runtime in `nexus_bridge.rs`.
- **Attenzione a non confondere**: `EmbeddingStore` (mig `0521`,
  `agent_graph_adapter/embedding_store.rs`) e' la compressione semantica del
  CONTESTO AGENTICO (continuity-trim), NON la pipeline embedding RAG: non fa
  avanzare questa parte.
- **Rerank (TS): confermato senza interfaccia** (`reranker.ts:18`, classe concreta) —
  creare `RerankerProvider` in `packages/embeddings`, refactor ONNX come impl,
  `CohereRerankerProvider` (`/v1/rerank`, `rerank-3.5`), factory + DI in
  `retrieval.ts`, provider da setting DB.
- Chiave `cohere_api_key` (is_secret). Prefissi Cohere in `provider_map` se si
  vogliono i prezzi embed nel catalog.
- Reindicizzazione Qdrant: resta da pianificare come step esplicito, ma con
  `signature()` gia' disponibile come trigger.
- Stima: ~2.5-3 giorni.

## 8. Parte E — Admin provider/modelli — RAFFORZATA (di nuovo)

### E0. Pain point riverificati al 2026-07-12
- Frammentazione su 5+ pannelli: confermata (`provider-settings`, `provider-budget`,
  `gateway-config`, `catalog-maintenance`, `routing-config/*`, piu'
  `infrastructure-settings`, `security-settings`). Nessun wizard, nessun
  test-connessione (`POST /api/admin/providers/:name/test` assente).
- Endpoint esistenti e riusabili (righe riverificate): `sync-model-catalog`
  (`routes/admin.rs:295`), `probe-models` (`:302`), `routing/purpose-models`
  (`:418`), `gateway/providers` (`:432,:448`), `models/routing-preview` (`:512`).
- **Doppia fonte routing UI: ancora presente** — `NEXUS_ROUTING_MATRIX` hardcoded a
  `routing-config/shared.ts:62-107` con modelli stale (`gpt-4.1-mini`,
  `mistral-small-4`, `open-mistral-nemo`, `claude-opus-4-6`); `ProviderName` fisso
  a 5 (`:15,:48`). Nel frattempo il file ha ricevuto `PURPOSE_TIER_OPTIONS` a 5 tier
  (mig `0547`): la parte tier e' aggiornata, la matrice no.
- **T4**: la dashboard provider itera `KNOWN_PROVIDERS` (`environment.rs:748,819,1287`).

### E1-E2. Design e endpoint mancanti (invariati)
Pagina unica "AI - Provider e Modelli" (tabella provider, wizard "Aggiungi
provider", catalog editabile, routing editabile + preview). Endpoint da aggiungere:
`POST /api/admin/providers/:name/test`, `GET/PUT /api/admin/catalog*`, CRUD
`nexus_routing_matrix`, `GET/PUT provider-capabilities`,
`POST /api/admin/validate/routing-matrix`. Tutti delegano ai punti unici esistenti
(regola L; per la selezione modello: ADR 0030).

### E4. Ambito (confermato, motivazione piu' forte)
- **Precondizione bloccante del MVP**: eliminare `NEXUS_ROUTING_MATRIX` da
  `shared.ts` e leggere dal DB (regola G/L).
- **Validator coerenza catalog↔routing nel MVP** (T1): la classe di incidenti e'
  ricorrente e documentata — mig `0530` (36/49 righe economica) E mig `0556`
  (alias deprecati/invalid_model) sono due workaround una-tantum della stessa
  classe; l'unica mitigazione runtime (`heal_orphan_pinned_models`) copre solo i
  pin orfani.
- **Dashboard provider data-driven** (T4) nel MVP.
- MVP: pagina unificata + wizard + test connessione + catalog editabile + preview
  routing + validazione coerenza + provider list data-driven.
- Completo: CRUD routing matrix, capabilities editor, catalog-sync config.

## 9. Parte F — Robustezza e generalizzazione

### F0. Inventario riverificato (~10 punti hardcoded, invariati)
1. `providers/<x>.rs`; 2. `providers/mod.rs`; 3-4. `ProviderKeys` + `load` +
`build_providers` (`bootstrap.rs:86-97,154-205`); 5. import in bootstrap;
6. `PROBED_PROVIDERS` (`provider_health_probe.rs:43`); 7. classificatore sdoppiato
(`provider_error_classifier.rs` vs `is_billing_error` in `openai_compat.rs:1026`);
8. policy yaml + settings/catalog DB; 9. `KNOWN_PROVIDERS` (`environment.rs:748` —
T4); 10. `provider_map` (`models.rs:192-202` — T5).

Confermati inoltre: quirk per-nome (`is_o_series`, XML DeepSeek, prompt cache
Anthropic); `reasoning_dialect` assente in `nexus_provider_capabilities`; `base_url`
non letto da settings; doppio cooldown (gateway reattivo vs mcp-core statico) —
ANCORA due moduli, divergenza in parte intenzionale.

### Cosa F3 ha GIA' assorbito dal lavoro recente (registrare, non rifare)
- Stato errori provider in DB: mig `0536` (`nexus_provider_health/_history`,
  last_error scritto dal gateway a ogni errore).
- Failover su 4xx provider-specifici: mig `0560`
  (`routing.client_error_failover_codes`, CSV DB-driven, consumato da
  `ProviderUnavailableInfo::allows_cross_provider_failover` in `ports.rs`).
- Failover su risposta degenere (200 senza output utile): `ec805f1c`.
- Causa strutturata del failover fino alla UI: `a6a21e34` (`POLICY_TIER_EXCLUDED`
  incluso, deployato).

### Stato di esecuzione F step 1 (2026-07-12)
- **(a) health probe data-driven: FATTO.** `provider_health_probe.rs` non usa piu'
  la costante `PROBED_PROVIDERS`: nuova `probed_providers(db)` deriva la lista da
  `SELECT DISTINCT provider FROM ai_price_catalog WHERE is_enabled` (regola G/L),
  con fallback ai 5 noti (`FALLBACK_PROBED_PROVIDERS`) se query vuota/fallita
  (fail-safe). Logica di fallback estratta pura (`resolve_probed_providers`) +
  2 unit test. No-op sul vivo (la query ritorna oggi esattamente i 5); un provider
  nuovo con modelli abilitati nel catalog viene sondato senza toccare il codice.
- **T4 dashboard data-driven: FATTO** (stesso pattern). `environment.rs`:
  `provider_names_for_status` deriva la lista provider della dashboard da
  catalog∪api_key configurata (punto unico `merge_provider_names`, +2 test);
  `providers_status_internal` e `build_providers_fallback` non iterano piu'
  `KNOWN_PROVIDERS` (resta solo come fallback DB-down). No-op sul vivo (unione = i
  5 attuali). RESTA aperto: `KNOWN_PROVIDERS` in `orchestrator/mod.rs` (candidati
  fallback routing) + registrazione bootstrap -> Parte F1.
- **(b) base_url da DB: RIMANDATO** — valore immediato basso (nessun provider nuovo
  ancora a usarlo) e rischio provider-specifico: gli adapter dedicati Google
  (Vertex/region) e Anthropic risolvono l'endpoint con logica propria, un override
  cieco e' pericoloso. Farlo insieme a OpenRouter/Groq (Parti B/C), dove il
  `<provider>_base_url` ha un consumatore reale e i provider sono OpenAI-compat.
- **(c) convergenza classificatore: PARZIALE** — fatta la bonifica dei riferimenti
  stale al brain in `provider_error_classifier.rs`. La convergenza vera di
  `is_billing_error` sul punto strutturato tocca il path cooldown (rischio
  comportamentale): da fare con test di regressione dedicati sui provider esistenti.

### F2 — Registry provider: IMPLEMENTATO (2026-07-12)

Stato: implementato nel working tree, build+test verdi, regression-zero verificata.
- **Mig `0565_provider_registry.sql`**: tabella `nexus_provider_registry` + seed dei
  6 provider (registrazione identica a bootstrap). Validata sul DB vivo in
  transazione ROLLBACK (crea, 6 righe, query loader combacia).
- **`GenericOpenAiProvider`** (`providers/generic.rs`): gemello parametrico di
  `MistralProvider`, capacita' dal registry. +1 test.
- **`bootstrap.rs`**: `ProviderKeys`+if-let sostituiti da `load_provider_descriptors`
  (con fallback ai 6 se la tabella non c'e' -> nessuna regressione se la mig non e'
  ancora applicata) + `build_providers` async con factory `construct_provider`
  (adapter dedicati per nome, generico per gli `openai_compat` nuovi) +
  `provider_is_active` puro. +4 test (fallback = 6 provider; attivazione api_key /
  base_url / **google-vertex-bypassa-enabled**).
- **Bug di regressione intercettato e corretto in fase di implementazione**: Google
  con Vertex configurato dev'essere attivo anche con `google_enabled=false` (Vertex
  bypassa il flag). La prima stesura `enabled && (key||vertex)` era errata; corretta
  in `(enabled && key) || vertex`, con test dedicato.
- **base_url da DB (F1 parte b) assorbito gratis**: il loader risolve
  `<provider>_base_url` -> `base_url_default` -> costante; per i 6 attuali e' None
  (nessun setting) -> costante, identico.
- **Effetto raggiunto**: aggiungere un provider OpenAI-compat = 1 riga registry +
  righe catalog, zero nuovo codice/file/if-let. I quirk (openai/anthropic/deepseek/
  google) restano nei costruttori dedicati selezionati per nome (F3 li spostera' nel
  DB). mistral/vllm restano sui wrapper dedicati (regression-zero); il generico e'
  esercitato dal primo provider nuovo.
- **Da fare pre-deploy (non fattibile qui)**: smoke test e2e — riavviare il gateway,
  verificare che `/providers` mostri i 6 e un `pin_provider` su ciascuno funzioni.
  Il path generico va esercitato col primo onboarding (Perplexity/Groq/OpenRouter).
- **Nota Windows**: i test girano con `cargo test -p nexus-gateway --lib` (il bin
  `.exe` e' lockato dal servizio WinSW in esecuzione).

Design (ancorato al codice reale). I costruttori provider sono gia' uniformi:
`new(http, api_key: impl Into<String>, base_url: Option<String>)` (openai/anthropic/
mistral/deepseek/google, + `with_db` per quelli DB-driven; vllm `new(http, base_url, ...)`).

- **Migrazione `nexus_provider_registry`** (nuova tabella, regola G): colonne
  `name` PK, `api_format` CHECK IN (`openai|anthropic|google|deepseek|openai_compat`),
  `key_setting`, `enabled_setting`, `base_url_setting` (NULL), `base_url_default`
  (NULL), `requires_key` BOOL, `is_active` BOOL, `sort_order` INT. Seed dei 6
  provider attuali con i loro setting correnti (nessun cambio comportamentale).
- **`ProviderKeys::load` -> `load_registry`**: legge le righe attive, risolve per
  ciascuna key/enabled/base_url dai setting nominati (riusa `keyed`/`get_setting`).
- **`build_providers` -> loop + factory per `api_format`**: un solo `match` mappa
  il formato al costruttore. **I quirk restano nei costruttori dedicati**
  (openai `is_o_series`, deepseek XML/thinking, anthropic cache, google
  vertex/region): la factory li SELEZIONA per formato, non li parametrizza — la
  parametrizzazione dei quirk nel DB e' F3, non un prerequisito. `openai_compat`
  copre mistral/vllm + i nuovi (perplexity/openrouter/groq) con un provider
  generico `GenericOpenAiCompatProvider{name, base_url, tier}`.
- **Caso Vertex**: `google` ha `requires_key=false` + logica `vertex_configured`
  gestita dentro il costruttore google (invariata).
- **`/admin/reload`**: `build_runtime` richiama load+build -> rilegge il registry
  automaticamente. Nessun cambio al reload.
- **Effetto**: aggiungere un provider OpenAI-compat = 1 riga nel registry + righe
  catalog. I ~10 punti hardcoded (T4 registrazione, ProviderKeys, if-let,
  eventualmente PROBED/KNOWN/provider_map che possono derivare dal registry come
  punto unico cross-crate) collassano.
- **Verifica**: unit test del loader (descrittori seed -> lista provider attesa);
  build verde; i 6 provider costruiti identici (regression). `GenericOpenAiCompatProvider`
  deve replicare esattamente `MistralProvider` (name/tier/streaming) per non
  regredire mistral/vllm.
- **Rischio/coordinamento**: tocca il bootstrap del gateway (path critico). Non e'
  verificabile e2e senza chiavi provider; farlo in un branch dedicato con smoke
  test provider. `nexus-gateway` non e' oggi in WIP (basso rischio conflitto), ma
  il concetto "registry provider" e' cross-crate: coordinare con eventuale lavoro
  su `model_selection.rs`/`model_routing.rs`.

### F1-F5 (ricalibrate)
- F1 registry data-driven + factory per `api_format`: include T4 e T5 (lista
  provider dashboard e provider_map derivate dal registry). Invariata, resta il
  moltiplicatore di valore.
- F2 quirk/capability dal DB (aggiungere `reasoning_dialect`; leggere
  `tool_call_format`, prompt cache dal DB; eliminare doppie fonti del trait).
  Invariata.
- **F3 CORRETTA nel merito (regola M)**: la proposta originale "tabella
  `provider_error_patterns` con regex" CONFLIGGE con la regola M (vieta la
  classificazione dal testo). Riformulazione: consolidare sul punto unico
  ESISTENTE `classify_provider_error` + `ProviderHttpError` (codici strutturati
  status+error.code alla fonte, quirk per-provider isolati negli adapter — ADR
  0033); l'eventuale estensione DB-driven segue il pattern della mig `0560`
  (whitelist di CODICI, non regex). `is_billing_error` (`openai_compat.rs:1026`)
  converge li'. Health probe data-driven e retry con `Retry-After` restano.
  Consolidamento cooldown: solo lo STATO (gia' avviato con mig `0536`),
  preservando i due comportamenti.
- F4/F5 invariati. Bonifiche brain residue (Parte 0 follow-up) incluse qui.

## 10. Parte G — Orchestratore — perimetro residuo al 2026-07-12

Gia' implementato e verificato (rimosso dal perimetro):
- **g1 CHIUSO**: soglie loop DB-driven — `LoopThresholds`
  (`loop_signatures.rs:26-56`, settings `agent.loop.signature_threshold`/`
  recent_signatures_cap`, cablate in `executor.rs:365`) + soglia adattiva
  `effective_g1_threshold` (`scale_reason.rs`).
- **g5 CHIUSO** (=T3): scala 5 tier completata (mig `0547`, `decisions/tiers.rs`).
- **g7 CHIUSO**: catene tier consolidate su `agentic_tier_chain`
  (`model_routing.rs:311`; `core.rs:1057` delega). NB: punto unico diverso da
  quello ipotizzato in origine (`v_model_escalation_chain`) — la doppia fonte e'
  comunque risolta.
- Extra chiusi: `max_escalations` enforced (`progress_controller.rs:554`);
  `failover_downgrade_penalty` cablata (`escalation.rs:223-224`);
  `POLICY_TIER_EXCLUDED` deployato.
- final_gate molto evoluto: delta-aware (`final_gate.rs:206`), turno di grazia
  (`:938-950`), esiti tipizzati FailedDiagnosed/Completed(Un)Verified (`:911-927`),
  `completion_confirmed` delega-aware (`:602-627`).

### Gap residui (nuovo perimetro)
- **g2** Escalation di modello DENTRO `final_gate` quando la build resta rossa per
  N cicli, e breakdown "errori risolti/rimasti" alla chiusura forzata (oggi
  `unverified` opaco, `final_gate.rs:911-928`; nessun riferimento a
  escalation/upscale nel file).
- **g3** Diagnosi causa nel recovery orfani: il reaper e' consolidato
  (`run_reaper.rs:54,102`) ma marca tutto `interrupted` con messaggio generico
  (`:173`); manca classificazione timeout/billing/crash + escalation al resume.
- **g4 CHIUSO** (mig `0564`, 2026-07-12): versionati i 10 feature flag
  dell'orchestratore attivi solo sul DB vivo (planner, understanding, sub-agenti,
  verifier, worker mode, DAG parallelo). Vedi §0.2 per la lista e le esclusioni.
- **g6 (residuo)** Helper `intent_tier_capability` fail-visibly: sostituire il
  silenzioso `("light","chat")` (`core.rs:260-262`) con WARN+default onesto come
  gia' fatto sul path dinamico (`core.rs:886-905`), o errore esplicito.

Protezioni invariate: golden test sui meccanismi di convergenza, modifiche dietro
flag DB.

## 11. File chiave (righe al 2026-07-12)

Provider chat (pattern per Perplexity/OpenRouter/Groq, pre-F1):
`crates/nexus-gateway/src/providers/<p>.rs`, `providers/mod.rs`,
`server/bootstrap.rs:86-97,154-205`, `crates/mcp-core/src/provider_health_probe.rs:43`,
`crates/mcp-core/src/environment.rs:748` (T4), `crates/mcp-core/src/models.rs:192-202`
(T5), `config/policies/*.yaml`, migrazione `0564+` (settings + catalog +
capabilities + intent_capability + routing matrix fallback).

Citazioni: `types.rs`, `openai_compat.rs`, `nexus_gateway.rs`,
`chat_messages/agent_run.rs`, `persistence.rs`, `apps/web-ide/lib/api/chat.ts`,
`components/chat/message-list.tsx`.

Intent/tier: `crates/mcp-core/src/intent_classifier.rs:75-87`,
`orchestrator/{core,intent,model_routing,model_selection}.rs`,
`nexus-agent-graph/src/decisions/tiers.rs`, mig `0492` (pavimento), `0530`
(matrice=fallback), `0547` (scala 5 completata).

Cohere: `crates/nexus-orchestrator/src/embedder.rs`,
`crates/mcp-core/src/nexus_bridge.rs`, `packages/embeddings/src/*`,
`packages/rag/src/retrieval.ts`.

Admin: backend `crates/mcp-core/src/routes/admin.rs:295-512`, `environment.rs`,
`models.rs`, `model_catalog_sync.rs`; frontend `apps/web-ide/components/settings/*`
e `routing-config/shared.ts:62-107` (matrice TS da eliminare).

Orchestratore: `crates/nexus-agent-graph/src/{nodes,decisions}/*`,
`crates/mcp-core/src/agent_graph_adapter/*`, `chat_messages/agent_run.rs`
(`select_engine`), `run_reaper.rs`, mig `0150/0439` (plan_phase).

## 12. Ordine di esecuzione rivisto (2026-07-12)

0. Parte 0 — ESEGUITA (mig `0532`, commit `6d23f9a0`).
0-bis. **ESEGUITO 2026-07-12**: mig `0564` (versiona i 10 feature flag
   orchestratore, g4 chiuso) + bonifica doc-modulo `provider_error_classifier.rs`
   (rimossi i riferimenti stale al brain Python). Resta opzionale: rimozione fisica
   `brain_agent_client.rs` + `'python'` dal CHECK; rimozione flag morti (adaptive_*,
   meta_steps.*); audit `agent.*`.
1. Robustezza F step 1 — (a) health probe data-driven FATTO (2026-07-12); (b)
   base_url da DB rimandato a OpenRouter/Groq; (c) convergenza classificatore
   parziale (bonifica fatta, unificazione cooldown da fare con regression). Vedi
   §9 "Stato di esecuzione F step 1".
2. Generalizzazione F step 2: registry provider + factory `api_format`
   (assorbe i 10 punti hardcoded, inclusi T4/T5).
3. Admin MVP (Parte E): precondizione matrice TS → pagina unificata + wizard +
   test connessione + catalog editabile + preview + validazione coerenza (T1) +
   provider list data-driven (T4).
4. Groq.
5. OpenRouter (extra_headers + whitelist catalog_sync + routing diretto).
6. Perplexity — Fase 1 (pin + pulsante UI) subito; Fase 2 (intent `ricerca_web`
   non-agentico via `EligibilityFilter.capability` + `best_non_agentic_model` +
   seed `nexus_intent_capability`) dopo.
7. Cohere (impl provider + selezione DB + reindicizzazione via `signature()`).
8. F step 3/4 (quirk dal DB, `reasoning_dialect`; stato cooldown unico gia'
   avviato con mig `0536` + retry/backoff).
9. Admin completo (CRUD routing matrix, capabilities editor, catalog-sync config).

Parte G residua — trasversale, per priorita': g4 (mig 0564, banale), g6-residuo
(helper fail-visibly), g2 (final_gate), g3 (diagnosi recovery).

## 13. Verifica end-to-end (ambiente Windows nativo)

- Gate: `pnpm verify` (orchestratore `scripts/verify.mjs`: turbo typecheck/lint/test
  + cargo check/clippy/test workspace). Unit test per ogni provider nuovo (pattern
  `capacita_dichiarate` di `mistral.rs`) e per il parsing `citations` in
  `openai_compat.rs`.
- Migrazioni: numerazione libera da `0564`; nessun file gia' applicato modificato.
  Test wipe+re-migrate: il motore DEVE risultare `rust` E, post-0564,
  `plan_phase_enabled` DEVE risultare `'true'` su DB rigenerato.
- Deploy locale: `deploy/deploy-local.ps1` (build + restart servizi Windows/WinSW;
  parametri nello script); dev: `deploy/dev-start.ps1` / `dev-stop.ps1` /
  `dev-build.ps1`. Niente WSL.
- Smoke provider: `POST /v1/complete` con `pin_provider:"<provider>"`; per
  Perplexity presenza `citations` in risposta → `metadata` messaggio → pannello
  "Fonti consultate".
- Tier: (a) la validazione coerenza intercetta una riga matrix con modello
  disabilitato/inesistente e una priority `economica` incoerente col costo (classe
  mig `0530`/`0556`); (b) un intent non seedato in `nexus_intent_capability` non
  degrada in silenzio nei gate (post-g6 residuo); (c) `ricerca_web` seleziona
  Perplexity nel flusso dedicato e MAI nei turni agentici (`require_tool_use`);
  (d) i modelli seedati dei provider nuovi hanno `performance_tier` esplicito e
  compaiono nella dashboard admin (post-T4).
- Groq: latenza su purpose reinstradato. OpenRouter: model id `vendor/model` non
  spezzato (`model_alias_resolver.rs:191`). Cohere: coerenza dimensione vettori con
  la collection Qdrant (trigger `signature()`).
- Robustezza: dopo F1-F2, onboarding di un provider via solo registry+DB senza
  nuovo codice; probe/classify/cooldown automatici; regression sui quirk esistenti
  (o-series, XML DeepSeek, prompt cache Anthropic).
- Orchestratore: golden test invariati; soglia loop modificata da DB cambia il
  comportamento senza recompile (gia' vero, g1 chiuso — usare come sanity check).

## 14. Punti aperti / rischi (2026-07-12)

- Perplexity: request fee per search context size nel ledger. La Fase 2 dipende da
  come il selettore accogliera' `capability="web_search"` senza aprire la porta ai
  modelli sonar nei turni agentici (mitigato: il campo capability esiste gia' nel
  filtro, il pavimento agentico resta attivo sui path agentici).
- OpenRouter: markup ~5.5%, usarlo per copertura/nicchia.
- Cohere: reindicizzazione vettori come step esplicito (rischio ridotto dal
  meccanismo reindex-on-model-change gia' presente).
- Coordinamento col lavoro in corso: il branch attivo
  (`riparti-consiglio-completamento`) e il Consiglio delle Competenze toccano
  routing/purpose/provider — sincronizzarsi prima di F1/F2 per evitare conflitti
  sugli stessi moduli (`model_selection.rs`, `model_routing.rs`, purpose).
- Consolidamento cooldown e quirk→DB: cambi sensibili, incrementali, dietro
  verifica dei dati seed prima di attivare la lettura.
- Punti unici reali non ancora censiti nel catalogo di CLAUDE.md (regola L):
  `ServiceManager` (ADR 0038) e `agentic_tier_chain`/`decisions::tiers` —
  raccomandazione redazionale da fare in un cambio CLAUDE.md dedicato.
- Landmine di classe "flag attivato a mano" (regola H): la mig `0564` ha chiuso i
  10 flag `orchestrator.*` vivi. RESTA da auditare il namespace `agent.*` (72
  booleani `true`): la query di diff seed-vs-vivo e' la stessa (SELECT su `settings`
  con `lower(value) IN ('true','false')` confrontata con l'ultimo valore letterale
  nei file `db/migrations/`). Attenzione: `agent.*` include flag di sicurezza
  (`dlp_allow_cloud_tier2/tier3`) da NON versionare ciecamente a `true`. Vanno
  inoltre rimosse (non accese) le chiavi morte residue del brain Python
  (`orchestrator.adaptive_*`, `orchestrator.meta_steps.*`,
  `plan_rationale_persist_as_note`, `clarify.require_llm_classifier`).
