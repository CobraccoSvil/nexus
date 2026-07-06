# Studio emendato: provider (Perplexity + OpenRouter/Groq/Cohere), admin, robustezza provider, orchestratore

Revisione 2026-07-06. Emenda e sostituisce lo studio originale, ri-ancorandolo allo
stato reale del codice verificato su `D:\ideai` (checkout canonico, migrazioni fino
a `0530`). Ogni riferimento file:riga di questo documento e' stato verificato sul
tree attuale; le premesse dello studio originale non piu' vere sono elencate in 0.1.

## 0. Premesse verificate e delta rispetto allo studio originale

### 0.1 Cosa e' cambiato (verificato)

| Premessa originale | Stato reale |
|---|---|
| brain Python (LangGraph) e' il path di produzione; Rust in shadow | Falso. Zero file `.py`, `brain/` rimosso. `Engine::Rust` e' il primario instradato globalmente (`agent_run.rs:2436,4114`); `run_via_brain` e' rollback legacy. Mig `0462`/`0463` versionano parte della dismissione brain |
| intent `ricerca_web` richiede lavoro sul classifier brain Python | Falso. Il classifier e' Rust (`intent_classifier.rs`), cutover versionato (mig `0458` flag + `0460` cutover) |
| Parte G: "decidere dove applicare i cambi (Python prod vs Rust shadow)" | Risolto dalla realta': un solo target, Rust. La Parte G si riduce ai gap residui (sez. 10) |
| Migrazione `NNNN` generica | Numerazione viva (due file `0531_*` gia' presenti); la Parte 0 e' la `0532` → le prossime partono da **0533** |
| Routing primario = `nexus_routing_matrix` | La matrice e' oggi **solo fallback**: il sistema gira in `behavior_mode='dinamico'`, selezione cost-first dal catalog per capability+tier (header mig `0530`) |
| `orchestrator.rs` monolitico | Ristrutturato in `crates/mcp-core/src/orchestrator/{core,intent,model_routing,model_selection,neural_client}.rs` |
| Parte D: "creare trait EmbeddingProvider" | Il trait `Embedder` esiste gia' (`crates/nexus-orchestrator/src/embedder.rs:18-43`); resta l'impl Cohere + selezione DB-driven |

### 0.2 Scoperta collaterale critica (regola H violata)

La mig `0451` seed-a `nexus_orchestrator_engine` con `'*' -> 'python'` e **nessuna
migrazione successiva la porta a `rust`**: il cutover del motore in produzione e'
stato un UPDATE manuale sul DB live. Su wipe + re-migrate il sistema tornerebbe a
instradare verso un brain inesistente. Bonifica in Parte 0 (sez. 3).

### 0.3 Problemi di routing per tier ancora attivi (T1-T6)

Verificati sul tree canonico; indirizzati puntualmente nelle parti indicate.

| # | Problema attivo | Evidenza | Indirizzato in |
|---|---|---|---|
| T1 | Routing matrix senza FK/validazione verso `ai_price_catalog`; config stantia ricorrente (mig `0530`: 36/49 righe `economica` con priority che premiava il modello piu' caro, corrette una tantum) | mig `0101` (nessun vincolo); header mig `0530` | Parte E (validazione coerenza promossa a MVP) |
| T2 | Fallback silenzioso intent → `("light","chat")` se l'intent manca in `nexus_intent_capability` | `orchestrator/core.rs:578,580,998,1000` | Parte A (seed obbligatorio) + Parte G g6 (fail visibly) |
| T3 | Mismatch scala tier 5 vs 3: catalog a 5 livelli (mig `0528`) ma `nexus_intent_capability.base_tier` CHECK a 3 (`0110:17`, mai allargato); il codice gestisce gia' 5 (`core.rs:1005`, `model_routing.rs:301`) | mig `0110` vs `0528` | Parte G g5 |
| T4 | `KNOWN_PROVIDERS` hardcoded a 5 in `environment.rs:736` — la dashboard admin (`/api/gateway/providers`) itera la costante: provider nuovi invisibili in UI | `environment.rs:736,807,1275` | Parte E + F1 (registry) |
| T5 | `provider_map` del sync LiteLLM hardcoded a 5 provider: i modelli dei provider nuovi skippati per sempre dal sync prezzi | `models.rs:192` | Parte F1 + migrazioni provider |
| T6 | Conflitto `ricerca_web` x routing dinamico: convenzione `intent != "chat"` ⇒ turno agentico ⇒ pavimento tier (mig `0492`) + `require_tool_use` ⇒ sonar (`tools=false`) escluso proprio dal percorso che l'intent attiverebbe | `model_routing.rs:204-241`; mig `0492`, `0530` | Parte A (A3/A4 riscritte) |

Mitigato, non un problema: `sensitivity_tier: 0` hardcoded in `core.rs:1306` — il
gateway ri-classifica con un proprio `SensitivityClassifier` e applica
`effective_tier = max(claim, classificazione)` + `validate_tier_claim`
(`nexus-gateway/src/server/routes.rs:142-145`). L'enforcement e' nel punto unico
corretto; resta igiene del claim (nota in Parte 0).

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
L (punto unico), F (niente segreti nei log).

## 2. Architettura accertata (stato reale)

Catena di produzione, oggi realmente 100% Rust:

```
mcp-core (intent Rust: orchestrator/intent.rs + intent_classifier.rs;
          selezione: orchestrator/model_routing.rs 'dinamico' cost-first dal catalog,
          nexus_routing_matrix come fallback)
  -> nexus-agent-graph (grafo agentico di produzione, nodes/ + decisions/)
  -> nexus-gateway (policy sensitivity/DLP + fallback + cooldown)
  -> provider Rust (OpenAI-compat -> OpenAiCompatClient; Anthropic/Google -> adapter dedicati)
```

- Punto unico OpenAI-compat: `crates/nexus-gateway/src/providers/openai_compat.rs`
  (endpoint `{base_url}/chat/completions`, Bearer, SSE, tool calling, usage).
  Confermato privo di `extra_headers` e di campo `citations`.
- Registrazione provider: `crates/nexus-gateway/src/server/bootstrap.rs`
  (`ProviderKeys` a campi fissi + blocchi `if let` per provider).
- Health probe: `crates/mcp-core/src/provider_health_probe.rs:45`
  (`PROBED_PROVIDERS` hardcoded a 5).
- Capability/costi: `ai_price_catalog` (con `performance_tier` a 5 livelli, mig
  `0528`, inferito a ogni sync da `infer_tier_from_name` —
  `models.rs:274`, `model_catalog_sync.rs:205`) + `nexus_provider_capabilities`
  (letta quasi solo per `tool_choice_style` via vista `v_model_capabilities`).
- Escalation agentica: `nexus-agent-graph/src/decisions/escalation.rs` +
  `mcp-core/src/agent_graph_adapter/escalation_port.rs`, su
  `v_model_escalation_chain` (tier_ord domina il rank).
- Embedding: trait `Embedder` in `nexus-orchestrator/src/embedder.rs:18-43`
  (`OnnxMiniLmEmbedder` 384-dim + fallback), esposto da `nexus_bridge.rs`
  (`POST /api/embed`). Rerank: `packages/embeddings/src/reranker.ts` (classe
  concreta ONNX, nessuna interfaccia), usato da `packages/rag/src/retrieval.ts`.

## 3. Parte 0 — Bonifica cutover (NUOVA, prerequisito) — ESEGUITA 2026-07-06

Stato di esecuzione (modifiche in working tree, non committate):

1. FATTO — Migrazione `db/migrations/0532_orchestrator_engine_cutover_rust.sql`:
   versiona il cutover manuale (`'*'` → `rust` in `nexus_orchestrator_engine`) e
   rimuove l'entry `brain` da `agent.watchdog.services` (chiave
   `brain_rest_port` droppata dalla 0463). Il valore `'python'` resta nel CHECK,
   documentato come INERTE.
2. FATTO — Default difensivo → Rust: `select_engine`/`resolve_engine_from_rows`
   (`chat_messages/agent_run.rs`) e `select_classifier_engine`
   (`orchestrator/intent.rs`) non ripiegano piu' su Python (servizio
   inesistente) per riga assente / DB down / valore ignoto; test aggiornati.
   `task_watchdog.rs` non riavvia piu' `nexus-brain` sui fallimenti embedder
   (in-process); rimossa `try_restart_systemd_or_process` (dead code).
   `brain_agent_client.rs` marcato QUARANTENA nel doc-comment di modulo; la
   rimozione fisica del modulo (+ valore `'python'` dal CHECK) resta come passo
   successivo dedicato. Il gate health `brain_rest_ok` risultava GIA' rimosso
   upstream (`main.rs:1570`).
3. FATTO — Claim sensitivity onesto: `orchestrator/core.rs` ora invia
   `classify_sensitivity(&composed_prompt) as u8` invece di `0` (l'enforcement
   resta nel gateway: `validate_tier_claim`, `routes.rs:142-145`).
4. FATTO (parte residua) — `.env.example`: rimosse le righe morte
   `LANGGRAPH_DB_PATH`/`LEARNING_DB_PATH`; CLAUDE.md: aggiornati i 3 riferimenti
   prescrittivi al "brain" (righe 36/196/206). README e regola G/L risultavano
   GIA' bonificati upstream.

## 4. Parte A — Perplexity (search/grounding + citazioni UI)

Ruolo invariato: provider dedicato alla ricerca web citata, NON nodo agentico.

### A1. Provider Rust (invariato)
- Nuovo `crates/nexus-gateway/src/providers/perplexity.rs` su template
  `mistral.rs` (wrapper sottile su `OpenAiCompatClient`, regola L):
  `DEFAULT_BASE_URL = "https://api.perplexity.ai"`, `name() = "perplexity"`,
  `supports_streaming() = true`, `supports_tools() = false`,
  `tier_compatibility() = [0,1,2]`.
- Export in `providers/mod.rs`; registrazione in `bootstrap.rs` (campo
  `perplexity` in `ProviderKeys`, `keyed(db, "perplexity_api_key",
  "perplexity_enabled")`); `"perplexity"` in `PROBED_PROVIDERS`
  (`provider_health_probe.rs:45`). Se F1 (registry) e' gia' fatto, questi punti
  collassano in righe DB.

### A2. Propagazione citazioni (invariata, confermata necessaria)
`openai_compat.rs` non ha alcun campo `citations` (verificato). Sei hop, tutti
`Option`/`#[serde(default)]`, canale `metadata` JSONB esistente su
`chat_messages` (nessun ALTER):
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

### A3. Instradamento (RISCRITTA — vincoli T2/T6)

Il percorso primario del routing e' il dinamico cost-first dal catalog; la
matrice e' solo fallback (mig `0530`). Inoltre la convenzione `intent != "chat"`
attiva il ramo agentico: pavimento di tier (mig `0492`) + `require_tool_use`, che
escluderebbe sonar (`tools=false`). Quindi:

**Fase 1 (obbligata, non alternativa): pin esplicito + pulsante UI.**
- Pulsante "Cerca sul web" in chat che invia con `pin_provider:"perplexity"` (+
  modello dal catalog). Il pin bypassa il selettore dinamico agentico in modo
  pulito. Alias `web-search -> perplexity/sonar-pro` in
  `config/model-aliases.yaml` per uso via API.

**Fase 2: intent end-to-end come flusso NON-agentico.**
- Capability `web_search: true` nel JSONB `capabilities` di `ai_price_catalog`
  per i modelli sonar; il punto unico di selezione
  (`orchestrator/model_routing.rs` / `model_selection.rs`) impara a richiederla
  per l'intent `ricerca_web`.
- Intent `ricerca_web` in `intent_classifier.rs` (`ALLOWED_INTENTS`) — lavoro
  interamente Rust (cutover classifier gia' fatto, mig `0460`).
- Il flusso `ricerca_web` deve essere trattato come NON-agentico: fuori dal
  pavimento `0492` e dal `require_tool_use` (altrimenti T6 lo svuota).
- Righe in `nexus_routing_matrix` per `ricerca_web` → (`perplexity`,`sonar-pro`),
  fallback `sonar`: coprono il solo percorso di fallback.

**Migrazione `0531+` (unica, per T2 e regola G):**
- `settings`: `perplexity_api_key` (is_secret), `perplexity_enabled`.
- `ai_price_catalog`: `sonar`, `sonar-pro`, `sonar-reasoning-pro` con prezzi
  (1/1, 3/15, 2/8 $/Mtok + request fee da annotare), `context_window`
  (~127k / ~200k), capabilities `{tools:false, vision:true, web_search:true}`,
  `performance_tier` esplicito (non affidarsi all'inferenza per nomi nuovi).
- `nexus_provider_capabilities`: `tool_choice_style='none'`.
- **`nexus_intent_capability`: seed di `ricerca_web`** (obbligatorio — senza
  questo il tier degrada in silenzio a light, T2 / `core.rs:578`).
- `config/policies/default.yaml` (+ hybrid): aggiungere `perplexity` al blocco
  `providers:`.

### A4. Tool-off garantito (confermata, semplificata)
- Il selettore unico esclude gia' i modelli `tools=false` dai path agentici
  (verificato: filtro `supports_tool_use=TRUE` in tutte le query di selezione).
  E' la stessa ragione del vincolo T6: garanzia per l'agentico, ostacolo per il
  percorso dedicato — da cui la Fase 1/Fase 2 di A3.
- Verificare che `resolve_tool_choice` non forzi `tool_choice` quando il
  provider e' `perplexity`.

## 5. Parte B — OpenRouter (gateway orizzontale)

Invariata nella sostanza, tutte le premesse confermate:
- Provider Rust `providers/openrouter.rs` (template mistral), export, bootstrap,
  probe, policy yaml (o registry F1 se gia' disponibile).
- Header custom: `OpenAiCompatClient` NON ha `extra_headers` (verificato) —
  aggiungere campo opzionale `extra_headers: HashMap<String,String>` nel punto
  unico, valorizzato dal costruttore del provider (`HTTP-Referer`, `X-Title`).
  Non bloccante (header raccomandati, non obbligatori), ma consigliato.
- Model id `vendor/model`: `model_alias_resolver.rs:191`
  (`.split('/').nth(1)`) confermato — instradare OpenRouter con model id diretti
  dalla routing matrix; se servono alias logici, correggere il parser per id
  multi-livello (regola H).
- catalog_sync: filtro/whitelist per OpenRouter su `/models` (solo i modelli
  realmente instradati).
- T5: aggiungere i prefissi OpenRouter a `provider_map` (`models.rs:192`) oppure
  attendere il registry F1 che lo rende data-driven; senza questo il sync prezzi
  LiteLLM ignora per sempre i modelli OpenRouter.
- DB: settings + righe catalog/capabilities per i soli modelli usati, con
  `performance_tier` esplicito (l'inferenza per nome sui nomi `vendor/model` non
  e' garantita).

## 6. Parte C — Groq (velocita' per task interni)

Invariata: `base_url = "https://api.groq.com/openai/v1"`, integrazione identica a
Mistral. Aggancio ai `nexus_purpose_model` ad alta frequenza (chat_title,
classifier, summarizer) via migrazione (i purpose sono gia' tier-aware, mig
`0203`/`0338`). T5: prefissi Groq in `provider_map` (o registry F1). Tier
espliciti nel seed catalog.

## 7. Parte D — Cohere (rerank + embedding RAG) — RIDIMENSIONATA

- **Embedding (Rust): il trait esiste gia'** (`Embedder`,
  `nexus-orchestrator/src/embedder.rs:18-43`, con impl ONNX + fallback hash).
  Lavoro reale: `CohereEmbeddingProvider` (reqwest verso
  `api.cohere.com/v1/embed`), setting DB `agent.rag.embedding_provider`
  (oggi mig `0200` configura endpoint/dim ma non la selezione provider),
  selezione runtime in `nexus_bridge.rs`.
- **Rerank (TS): confermato senza interfaccia** — creare `RerankerProvider` in
  `packages/embeddings`, refactor ONNX come impl, `CohereRerankerProvider`
  (`/v1/rerank`, `rerank-3.5`), factory + DI in `retrieval.ts`, provider da
  setting DB.
- Chiave `cohere_api_key` (is_secret). Prefissi Cohere in `provider_map` se si
  vogliono i prezzi embed nel catalog.
- Rischio invariato e centrale: cambiare embedder cambia `embedding_dim` →
  reindicizzazione Qdrant o collection separata, come migrazione dati esplicita.
- Stima ridotta: ~2.5-3 giorni (era 3-4).

## 8. Parte E — Admin provider/modelli — RAFFORZATA

### E0. Pain point confermati (tutti verificati)
- Frammentazione su 5+ pannelli (`provider-settings`, `provider-budget`,
  `gateway-config`, `catalog-maintenance`, `routing-config/*`): confermata.
- Endpoint gia' esistenti e riusabili, confermati sul codice:
  `GET /api/gateway/providers`, `POST /api/admin/sync-model-catalog`,
  `POST /api/admin/probe-models`, `GET/PUT /api/admin/routing/purpose-models`
  (`routes/admin.rs:295,302,418,432`),
  `POST /api/admin/routing-matrix/auto-promote-now` (`environment.rs:1307`),
  `GET /api/models/routing-preview` (`models.rs:97`).
- **Doppia fonte routing UI: CONFERMATA** — `NEXUS_ROUTING_MATRIX` hardcoded in
  `apps/web-ide/components/settings/routing-config/shared.ts:58-103` con modelli
  inesistenti/deprecati (`gpt-4.1-mini`, `mistral-small-4`) divergenti dal DB.
- **T4**: la dashboard provider itera `KNOWN_PROVIDERS` (`environment.rs:736`):
  un provider onboardato via wizard non comparirebbe nei LED.

### E1-E2. Design e endpoint mancanti (invariati)
Pagina unica "AI - Provider e Modelli" (tabella provider, wizard "Aggiungi
provider", catalog editabile, routing editabile + preview). Endpoint da
aggiungere: `POST /api/admin/providers/:name/test`, `GET/PUT /api/admin/catalog*`,
CRUD `nexus_routing_matrix`, `GET/PUT provider-capabilities`,
`POST /api/admin/validate/routing-matrix`. Tutti delegano ai punti unici
esistenti (regola L).

### E4. Ambito EMENDATO
- **Precondizione bloccante del MVP**: eliminare `NEXUS_ROUTING_MATRIX` da
  `shared.ts` e leggere dal DB (regola G/L) — prima di costruire CRUD/preview.
- **Promossa a MVP (era "completo")**: validazione coerenza catalog↔routing
  (T1). La classe di incidenti e' reale e ricorrente: la mig `0530` e' il terzo
  workaround una-tantum (dopo `0143`, riallineamenti tier). Il validator con
  badge/fix inline e' il fix strutturale.
- **Aggiunta al MVP**: dashboard provider data-driven (derivare la lista dal
  catalog/settings invece di `KNOWN_PROVIDERS`, T4).
- MVP risultante: pagina unificata + wizard + test connessione + catalog
  editabile + preview routing + validazione coerenza + provider list data-driven.
- Completo: CRUD routing matrix, capabilities editor, catalog-sync config.

## 9. Parte F — Robustezza e generalizzazione — INVENTARIO AGGIORNATO

### F0. Diagnosi (confermata, con 2 punti in piu')
Punti da toccare oggi per aggiungere UN provider OpenAI-compatible (~10, erano ~8):
1. `providers/<x>.rs`; 2. `providers/mod.rs`; 3-4. `ProviderKeys` + `load` +
`build_providers` in `bootstrap.rs`; 5. import in bootstrap; 6.
`PROBED_PROVIDERS` (`provider_health_probe.rs:45`); 7. pattern
`provider_error_classifier.rs` / `is_billing_error` (`openai_compat.rs`, pattern
OpenAI-specifici, confermato sdoppiato); 8. policy yaml + settings/catalog DB;
**9. `KNOWN_PROVIDERS` (`environment.rs:736`, dashboard admin — T4); 10.
`provider_map` sync LiteLLM (`models.rs:192` — T5).**

Confermati inoltre: quirk per-nome (`is_o_series` in `openai.rs`, parsing XML +
gate thinking in `deepseek.rs`, prompt cache in `anthropic.rs`); colonna
`reasoning_dialect` assente in `nexus_provider_capabilities`; `base_url`
accettato dai costruttori ma il bootstrap passa `None` (nessun setting
`<provider>_base_url`); doppio cooldown gateway (reattivo, re-probe ~10min) vs
mcp-core (durate statiche) — logiche DIVERSE e in parte intenzionali
(reattivo per chat, conservativo per loop agente).

### F1-F5 (invariate, con priorita' ritoccata)
- F1 registry data-driven + factory per `api_format`: include ora anche T4 e T5
  (lista provider della dashboard e provider_map del sync derivate dal registry).
- F2 quirk/capability dal DB (aggiungere `reasoning_dialect`; leggere
  `tool_call_format`, prompt cache dal DB; eliminare doppie fonti del trait).
- F3 resilienza: classificatore errori unico con pattern DB; health probe
  data-driven; retry con `Retry-After`. Il consolidamento del doppio cooldown
  resta ma con priorita' ridotta: la divergenza e' in parte design intenzionale —
  consolidare lo STATO (fonte unica in DB) preservando i due comportamenti.
- F4/F5 invariati. Aggiunta a F: bonifica residui brain (Parte 0, punto 2) come
  stesso pattern "rimozione doppie fonti".

## 10. Parte G — Orchestratore — RISCRITTA (gap residui)

Un solo target: il grafo Rust (`nexus-agent-graph`). Gran parte dello studio
originale risulta GIA' implementata (verificato):
- No-progress detection con segnali strutturati (exploration, repeated_action,
  tool error, g1_over_cap) in `decisions/progress_controller.rs`;
- escalation con tetto esplicito (`max_escalations`, default 3);
- soglie DB-driven (mig `0213` exploration, `0407` g1 nudges);
- `todo_runner` attivo e re-entrant (sub-run isolati); `planner` presente;
- `ForceDiagnose` (mig `0386`); reap orfani al boot (mig `0470`);
- final_gate deterministico con conteggio errori build, in evoluzione
  (mig `0455`, `0465`, `0467`).

### Gap residui (il nuovo perimetro della Parte G)
- **g1** `LOOP_THRESHOLD = 3` hardcoded in `decisions/loop_signatures.rs:26` →
  portarlo in DB (regola G) come le altre soglie.
- **g2** Escalation di modello DENTRO `final_gate` quando la build resta rossa
  per N cicli; in chiusura forzata riportare lo stato parziale (errori
  risolti/rimasti) invece di "unverified" opaco.
- **g3** Recovery orfani con diagnosi causa (timeout vs billing vs crash) ed
  eventuale escalation al resume; validazione coerenza checkpoint.
- **g4** `plan_phase` OFF di default: attivarlo per task non banali
  (plan-then-execute rinforzato).
- **g5 (T3)** Completare la scala a 5 tier: allargare il CHECK di
  `nexus_intent_capability.base_tier` (`0110:17`) e rivedere i seed intent;
  oggi nessun intent puo' domandare high/frontier.
- **g6 (T2)** Sostituire il fallback silenzioso `("light","chat")`
  (`core.rs:578,580,998,1000`) con errore esplicito (fail visibly, regola G) o
  auto-seed con warn.
- **g7** Riconciliare le catene tier hardcoded (`core.rs:1005`,
  `model_routing.rs:301`) con `v_model_escalation_chain` come punto unico
  (regola L) — oggi il mapping tier→ordine vive in due posti.

Protezioni invariate: golden test sui meccanismi di convergenza, modifiche
dietro flag DB.

## 11. File chiave (aggiornati)

Provider chat (pattern per Perplexity/OpenRouter/Groq, pre-F1):
`crates/nexus-gateway/src/providers/<p>.rs`, `providers/mod.rs`,
`server/bootstrap.rs`, `crates/mcp-core/src/provider_health_probe.rs:45`,
`crates/mcp-core/src/environment.rs:736` (T4),
`crates/mcp-core/src/models.rs:192` (T5), `config/policies/*.yaml`,
migrazione `053x` (settings + catalog + capabilities + intent_capability +
routing matrix fallback).

Citazioni: `types.rs`, `openai_compat.rs`, `nexus_gateway.rs`,
`chat_messages/agent_run.rs`, `persistence.rs`,
`apps/web-ide/lib/api/chat.ts`, `components/chat/message-list.tsx`.

Intent/tier: `crates/mcp-core/src/intent_classifier.rs`,
`orchestrator/{core,intent,model_routing,model_selection}.rs`,
mig `0110` (CHECK base_tier), `0492` (pavimento), `0528` (5 livelli),
`0530` (matrice=fallback).

Cohere: `crates/nexus-orchestrator/src/embedder.rs`,
`crates/mcp-core/src/nexus_bridge.rs`, `packages/embeddings/src/*`,
`packages/rag/src/retrieval.ts`.

Admin: backend `crates/mcp-core/src/routes/admin.rs`, `environment.rs`,
`models.rs`, `model_catalog_sync.rs`; frontend
`apps/web-ide/components/settings/*` e `routing-config/shared.ts:58-103`
(matrice TS da eliminare).

Orchestratore: `crates/nexus-agent-graph/src/{nodes,decisions}/*`,
`crates/nexus-graph`, `crates/mcp-core/src/agent_graph_adapter/*`,
`chat_messages/agent_run.rs` (`select_engine`), mig `0451` (seed engine).

## 12. Ordine di esecuzione rivisto

0. **Bonifica cutover (Parte 0)** — ESEGUITA (mig `0532` + default difensivi →
   Rust + quarantena brain_agent_client + doc). Resta il follow-up: rimozione
   fisica di `brain_agent_client.rs` e del valore `'python'` dal CHECK.
1. Robustezza F step 1: health probe data-driven + base_url da DB +
   classificatore errori unificato con pattern DB.
2. Generalizzazione F step 2: registry provider + factory `api_format`
   (assorbe i 10 punti hardcoded, inclusi T4/T5).
3. Admin MVP (Parte E emendata): precondizione matrice TS → pagina unificata +
   wizard + test connessione + catalog editabile + preview + **validazione
   coerenza (T1)** + provider list data-driven (T4).
4. Groq.
5. OpenRouter (extra_headers + whitelist catalog_sync + routing diretto).
6. Perplexity — Fase 1 (pin + pulsante UI) subito; Fase 2 (intent
   `ricerca_web` non-agentico + capability `web_search` + seed
   `nexus_intent_capability`) dopo.
7. Cohere (impl provider + selezione DB + reindicizzazione esplicita).
8. F step 3/4 (quirk dal DB, `reasoning_dialect`; stato cooldown unico +
   retry/backoff).
9. Admin completo (CRUD routing matrix, capabilities editor, catalog-sync
   config).

Parte G residua — trasversale, per priorita': g6+g5 (tier: fail-visibly e
scala 5, sinergici col lavoro provider), g1 (soglia in DB), g2 (final_gate),
g4, g3, g7.

## 13. Verifica end-to-end (aggiornata)

- Gate: `pnpm verify` (`scripts/verify.sh`). Unit test per ogni provider nuovo
  (pattern `capacita_dichiarate` di `mistral.rs`) e per il parsing `citations`
  in `openai_compat.rs`.
- Migrazioni: numerazione libera da `0533` (la `0532` e' la Parte 0); nessun
  file gia' applicato modificato. Test wipe+re-migrate: dopo la Parte 0, il
  motore DEVE risultare `rust` su DB rigenerato.
- Deploy: `./deploy/deploy-local.sh --rust` / `--web` (verificati esistenti).
- Smoke provider: `POST /v1/complete` con `pin_provider:"<provider>"`;
  per Perplexity presenza `citations` in risposta → `metadata` messaggio →
  pannello "Fonti consultate".
- Tier (nuovi, da T1-T6): (a) la validazione coerenza intercetta una riga
  matrix con modello disabilitato/inesistente e una priority `economica`
  incoerente col costo (caso mig `0530`); (b) un intent non seedato in
  `nexus_intent_capability` produce errore esplicito, non light silenzioso
  (post-g6); (c) `ricerca_web` seleziona Perplexity nel flusso dedicato e NON
  viene mai selezionato nei turni agentici (`require_tool_use`); (d) un intent
  con base_tier `high`/`frontier` e' rappresentabile e instradato (post-g5);
  (e) i modelli seedati dei provider nuovi hanno `performance_tier` esplicito
  e compaiono nella dashboard admin (post-T4).
- Groq: latenza su purpose reinstradato. OpenRouter: model id `vendor/model`
  non spezzato. Cohere: coerenza dimensione vettori con la collection Qdrant.
- Robustezza: dopo F1-F2, onboarding di un provider via solo registry+DB senza
  nuovo codice; probe/classify/cooldown automatici; regression sui quirk
  esistenti (o-series, XML DeepSeek, prompt cache Anthropic).
- Orchestratore: golden test invariati; soglia loop modificata da DB cambia il
  comportamento senza recompile (post-g1).

## 14. Punti aperti / rischi (aggiornati)

- Perplexity: request fee per search context size nel ledger (invariato).
  La Fase 2 dipende da come il selettore dinamico accogliera' la capability
  `web_search` senza aprire la porta ai modelli sonar nei turni agentici.
- OpenRouter: markup ~5.5%, usarlo per copertura/nicchia (invariato).
- Cohere: reindicizzazione vettori come step esplicito (invariato).
- Rimozione `run_via_brain`: elimina l'ultimo rollback formale del motore.
  Mitigazione: quarantena dietro flag per un periodo, poi rimozione definitiva.
- Consolidamento cooldown e quirk→DB: cambi sensibili, incrementali, dietro
  verifica dei dati seed prima di attivare la lettura (invariato).
- La doppia implementazione Python/Rust NON e' piu' un rischio (risolta dalla
  realta'); resta il rischio inverso: parti del codice/DB che ancora "credono"
  al Python (Parte 0).
