---
id: 0016-rag-pipeline-completion
kind: adr
title: "Completamento pipeline RAG strutturale + safety net context overflow"
slug: 0016-rag-pipeline-completion
tags:
  - architecture
  - rag
  - context-window
  - agent
  - performance
auto_generated: false
created_at: 2026-06-04T00:00:00Z
updated_at: 2026-07-02T00:00:00Z
nexus_meta_version: 1
---

# ADR 0016 — Completamento pipeline RAG strutturale + safety net context overflow

> **Status**: implementato — fasi A/B/C complete in precedenza; fasi D1 e D2 completate il 2026-07-02
> **Aggiornamento 2026-07-02 (as-built)**: fase D2 (fail-fast context overflow) implementata: funzioni pure `check_hard_cap`/`render_overflow_message` in `crates/nexus-agent-graph/src/decisions/context_reduction.rs`; gate nell'executor dopo upscale+brake con meta_step `context_overflow` + `extra.error_class`; messaggio dal template DB `system.context_overflow` (setting `agent.context.hard_cap_ratio`, mig 0286). Fase D1 completata nello stesso giorno: porta sincrona `TokenCounter` (runtime/ports) iniettata nell'executor via `with_token_counter`; punto unico `estimate_history_tokens` per upscale/brake/hard-cap/forced-RAG; testo dal punto unico `flatten_context_text` (stesso perimetro della stima char); BPE cl100k cacheata in `mcp-token` (LazyLock, fallback char/4 senza panico); selezione dal DB via `agent.context.tokenizer` (mig 0286, `cl100k_base`), qualunque altro valore o chiave assente = stima char-based storica.
> **Decisori**: team Nexus
> **Supersede**: completa [[0015-rag-strutturale-unificato]] e [[0014-context-size-management]]
> **Trigger**: chat 6 Beauty-Book (run `1a6e367e-…`) ha mandato **1.226.560 token** a un modello con **131.072 ctx** (936% del window) ricevendo una risposta hallucinata in cinese su una parola random ("starts") catturata dal payload troncato dal provider.

## Contesto

ADR 0015 ha definito la pipeline RAG strutturale come obiettivo architetturale. Lo stato attuale copre **~20%** della pipeline target:

### Implementato

1. Tool `nexus_search_semantic` per retrieval on-demand (5 sorgenti: `attachment`, `kb`, `chat_history`, `tool_result`, `code`).
2. `_smart_truncate_lossless` per tool result `>= MAX_TOOL_RESULT_CHARS`: indicizza in Qdrant collection `tool_results_chunks`, lascia head+tail+pointer nel prompt.
3. `build_pointer` con istruzione esplicita all'agente "ref=… recupera con nexus_search_semantic".
4. `verifier_node` e `understanding_node` lo invocano proattivamente.

### Mancante (causa diretta dell'incidente chat 6)

5. **Offload preventivo system prompt + project context**: project dump, file list, KB snapshot iniettati a inizio turno NON passano dal lossless.
6. **Discovery on-demand tool**: 81 tool registrati = 19.245 token di tool definitions iniettati ogni turno (15% di 128k window).
7. **Rolling summary cross-turno**: messaggi assistant dei turni N-3..N-K restano interi nel context.
8. **Forced offload trigger**: nessun meccanismo costringe l'agente a usare `nexus_search_semantic` quando il context cresce.
9. **Cross-turn tool_result dedup**: stesso `read_file(X)` replayato al turno 7 dopo essere stato visto al turno 3.
10. **KB graph summary mode**: `knowledge_search top_k=50` ritorna 50 body, non un summary grafo.

Sintomi misurati nei log brain (chat 6, run `1a6e367e`):

- 14 iterazioni consecutive di `executor_node: provider=deepseek model=deepseek-v4-flash intent=test tools=9`
- **Zero righe** "TOKEN-based" / "context cap" / "compressione aggressiva"
- Cascade fallback passa a `mistral-small-latest` (anch'esso 131k ctx) senza rivalutare il context
- Il provider Mistral riceve 1.2M token, tronca lato server, il modello hallucina

## Decisione

Implementiamo i 5 layer architetturali in ordine di ROI decrescente. **Nessun layer è una toppa**: ognuno chiude una classe strutturale di bug, tutti sono configurabili via DB (regola G), tutti hanno smoke test E2E che replica chat 6.

```
┌──────────────────────────────────────────────────────────────────┐
│  Utente → router_node                                            │
│                ↓                                                 │
│  executor_node                                                   │
│   1. RAG STRUTTURALE  (Fase A — 6 punti, fix definitivo)         │
│      → context input al provider ≤ 50% window nel 95% dei casi   │
│   2. COMPATTAZIONE TOOL DEFINITIONS  (Fase B — pulizia)          │
│      → -8K token/turno indipendentemente dal contenuto utente    │
│   3. SMART UPSCALE  (Fase C — safety net architetturale)         │
│      → escalation modello con ctx maggiore se A+B insufficienti  │
│   4. BRAKE CON TOKENIZER REALE  (Fase D — ultima difesa)         │
│      → fail-fast con messaggio chiaro se anche C non basta       │
│                ↓                                                 │
│   Provider call (garantito ≤ 70% ctx_window)                     │
└──────────────────────────────────────────────────────────────────┘
```

### Fase A — Completare la pipeline RAG strutturale (vero ROI)

**Obiettivo**: ridurre il payload al provider da 1.2M → 80-100k nei casi limite tipo chat 6.

#### A1. Offload preventivo system prompt + project context

- **Cosa**: `_smart_truncate_lossless` esteso a `build_system_prompt`. Project context dump (file list, deps tree, KB snapshot, recent commits) sopra `agent.context.system_prompt_offload_threshold_tokens` → indicizzato in `tool_results_chunks` con `source_kind=system_context`, sostituito da blocco riassuntivo + pointer.
- **Dove**: `brain/agents/nodes.py::build_system_prompt` (chiamato da `executor_node` prima di ogni turno).
- **Settings DB**:
  - `agent.context.system_prompt_offload_threshold_tokens = 8000`
  - `agent.context.system_prompt_summary_max_tokens = 800`
- **Atteso**: -10/30K token per chat con progetti grandi.

#### A2. Discovery on-demand dei tool

- **Cosa**: dei 81 tool, restano inline nel prompt solo i **"core 15"** statisticamente più usati + `nexus_mcp_tool_search` + `nexus_mcp_tool_call`. Gli altri 66 tool sono indicizzati in Qdrant collection `agent_tools_descriptors` con embedding del nome+description; agente li scopre con `nexus_mcp_tool_search("voglio fare X")` → top-3 tool con schema, poi `nexus_mcp_tool_call(name, args)`.
- **Dove**: `crates/mcp-core/src/brain_agent_client.rs::build_tools_json_for_agent` (esiste già con threshold MCP esterni, va esteso ai tool interni).
- **Settings DB**:
  - `agent.tools.inline_core_count = 15`
  - `agent.tools.inline_core_whitelist` (CSV)
  - `agent.tools.discovery_enabled = true`
- **Atteso**: -14K token/turno solo dalla riduzione delle definitions inline.

#### A3. Rolling summary cross-turno

- **Cosa**: ogni `N` turni (default 5), i turni `[N-K..N-K+5]` vengono sostituiti da un mini-summary (≤500 token) generato in background con un modello "economica". I messaggi originali restano in Qdrant `chat_history_rolling` con `session_id` + `turn_range`, retrievable via `nexus_search_semantic(source_kinds=["chat_history"])`.
- **Dove**: nuovo modulo `brain/agents/rolling_compactor.py`, invocato da `executor_node` post-turn-write.
- **Settings DB**:
  - `agent.context.rolling_summary_enabled = true`
  - `agent.context.rolling_window_turns = 5`
  - `agent.context.rolling_keep_recent_turns = 3`
  - `agent.context.rolling_summary_model = google/gemini-2.5-flash-lite`
- **Atteso**: in chat lunghe (>20 turni) -50% token cumulativi.

#### A4. Forced offload trigger nel system prompt

- **Cosa**: quando il context stimato supera `agent.context.forced_rag_threshold_ratio * window` (default 0.40), il system prompt riceve un'iniezione assertiva:

  > "Il contesto disponibile e' parzialmente offloadato in `tool_results_chunks`. **Prima di rispondere a richieste che richiedono dettagli specifici, chiama `nexus_search_semantic(query=...)`**. Non assumere di vedere tutto il contesto: chiedi quello che ti serve."

- **Dove**: `brain/agents/nodes.py::_inject_language_reminder` ha gia' il pattern; aggiungiamo `_inject_rag_reminder` con la stessa semantica (system + recency su ultimo HumanMessage).
- **Settings DB**:
  - `agent.context.forced_rag_threshold_ratio = 0.40`
  - `agent.context.forced_rag_reminder_text` (DB-overridable per A/B testing)
- **Atteso**: l'agente impara a fare retrieval mirato → -30% tool result rilevati come duplicati.

#### A5. Cross-turn tool_result dedup via cache

- **Cosa**: ogni `tool_result` viene memorizzato con chiave `hash(tool_name + canonical_args)` in Redis con TTL 30 min. Alla chiamata successiva con stessa chiave, il system prompt riceve un pointer `cache_ref=<id>` invece del payload; agente decide se rileggere con `nexus_cache_get(id)` o tirare avanti.
- **Dove**: nuovo `crates/mcp-core/src/tool_runner_server.rs::dispatch` wrap con cache layer + nuovo tool `nexus_cache_get`.
- **Settings DB**:
  - `agent.tools.result_cache_enabled = true`
  - `agent.tools.result_cache_ttl_seconds = 1800`
  - `agent.tools.result_cache_skip_for` (CSV, tool che non vanno mai cachati, es. `run_command`)
- **Atteso**: in run agentici lunghi (>10 turni) -40% replay di tool_result identici.

#### A6. KB graph summary mode

- **Cosa**: `knowledge_search` con `top_k > 20` cambia output: invece di N body, ritorna `{clusters: [{theme, count, sample_titles}], total}`. L'agente sceglie un cluster, poi `knowledge_search(query, cluster_id)` per i body veri.
- **Dove**: `crates/mcp-core/src/agent_tools/knowledge.rs::tool_knowledge_search`.
- **Settings DB**:
  - `agent.kb.graph_summary_threshold_topk = 20`
  - `agent.kb.cluster_method = embedding_kmeans` (futuro: `lda`, `manual_tag`)
- **Atteso**: -80% token su richieste "panoramica del progetto".

### Fase B — Compattazione tool definitions (pulizia, ROI moderato)

**Obiettivo**: ridurre i 19.245 token/turno delle definitions a ~11.000 senza degrado tool-selection.

#### B1. Guidelines redazionali per description

Nuovo file `docs/.nexus-vault/runbook/tool-description-guidelines.md`:

- Description **una frase**, max 40 token, formato: `"<verbo imperativo>. Usa quando <trigger>."`
- Esempio: ❌ `"FASE 1 resa Figma Make. Estrae il code-snapshot React/TypeScript/Tailwind GIA' PRESENTE…"` (226 token)
- Esempio: ✅ `"Estrai code-snapshot React da .make Figma e scrivi su disco. Usa per resa Figma fase 1."` (28 token)

#### B2. Schema senza description ridondanti

- Rimuovere `"description": "..."` su parametri il cui significato e' ovvio dal nome (`query: string`, `path: string`, `top_k: integer`).
- Mantenere description solo dove un default non ovvio o un range va comunicato.
- Esempio: `nexus_todo_write` 431 token → ~150 con schema snello.

#### B3. Suite test non-regression tool selection

- 30 prompt rappresentativi (estratti da `agent_runs` reali ultimi 30 giorni).
- Per ogni prompt, executor con tool definitions vecchie vs nuove → confronto del primo `tool_use` emesso.
- Pass criteria: identita' tool name ≥95%, identita' arg shape ≥90%.
- Esecuzione: nightly via worker `tool_selection_regression_worker`.

#### B4. Esclusione esplicita

**Sigle nomi (es. `nss` per `nexus_search_semantic`)**: NON IMPLEMENTATO.
Motivazione misurata: tiktoken cl100k_base tokenizza `knowledge_get_note` → 3 token e `kb_get_note` → 3 token (BPE comprime gia' i prefissi frequenti). Risparmio: 0-100 token/turno. Costo: degrado tool-selection documentato (Anthropic Tool Use Best Practices, OpenAI Function Calling Guide). Decisione: **no**.

### Fase C — Smart upscale (safety net architetturale)

**Obiettivo**: per i casi residui (5%) in cui A+B non comprimono abbastanza (richieste legittime che richiedono context grande), escalation automatica a modello con ctx maggiore.

#### C1. Routing context-aware

- Prima di chiamare il provider, `executor_node` stima `est_tokens`.
- Se `est_tokens > model.context_window * 0.9`, cerca nella routing matrix:
  ```
  SELECT model_id, context_window
  FROM nexus_routing_matrix r
  JOIN ai_price_catalog c ON c.model = r.model_id
  WHERE r.intent = current_intent
    AND r.behavior_mode = current_mode
    AND c.context_window >= est_tokens * 1.2
    AND c.is_enabled = TRUE
  ORDER BY c.context_window ASC, c.input_cost_per_million_tokens ASC
  LIMIT 1
  ```
- Se trovato, switch al modello upscaled (decisione tracciata in `agent_runs.upscale_reason = 'context_overflow'`).
- Se non trovato, fallthrough alla Fase D.

#### C2. UI esplicita

- Badge sotto il messaggio: "Modello: deepseek-v4-flash → claude-opus-4-6 (context: 1.2M token)" con tooltip "Ho cambiato modello automaticamente perche' la richiesta supera il context window del modello iniziale".
- Visibile in `chat_messages.meta.upscale = {from, to, reason, est_tokens}`.

#### C3. Settings DB

- `agent.upscale.enabled = true`
- `agent.upscale.target_overhead_ratio = 1.2` (margine sicurezza vs est_tokens)
- `agent.upscale.preferred_targets` (CSV: `claude-opus-4-6,gemini-2.5-pro,gpt-5.5`)
- `agent.upscale.cost_cap_usd_per_run = 0.50` (se il modello upscaled costerebbe > cap, errore in UI)

### Fase D — Brake con tokenizer reale + fail-fast UI

**Obiettivo**: per i casi in cui A+B+C falliscono (richiesta enorme, nessun modello disponibile, cost cap superato), errore chiaro all'utente — niente hallucinazione silenziosa.

#### D1. Tokenizer reale

- Sostituire `_estimate_context_chars(messages) // 4` con tiktoken `cl100k_base` (gia' in deps brain).
- Cache LRU sulle tokenizzazioni per messaggi gia' visti (key: hash content).
- Stima accurata ±2% vs char/4 che sottostima fino a 3-5x su contenuti densi (cinese, JSON tool_result, base64).

#### D2. Fail-fast UI

- Se dopo upscale + offload il payload resta > 95% window: `executor_node` ritorna errore `ContextOverflow` con messaggio:

  > "Il contesto della chat supera la capacita' di tutti i modelli configurati (stima: 1.2M token). Suggerimenti:
  > 1. Avvia una nuova chat per spezzare il context (la KB del progetto resta accessibile via search)
  > 2. Riduci gli allegati attivi (rimuovi dalla chat quelli non necessari)
  > 3. Chiedi all'admin di abilitare modelli con context window superiore"

- L'utente vede sempre cosa sta succedendo, non riceve mai risposte hallucinate.

#### D3. Settings DB

- `agent.context.tokenizer = cl100k_base`
- `agent.context.hard_cap_ratio = 0.95`
- `agent.context.overflow_message_key = system.context_overflow` (testo overridabile da `nexus_prompt_templates`)

## Sequenza implementativa

Ordine di priorita' per ROI:

| Sprint | Layer | Tasks | Effort | Risparmio per turno (atteso) |
|---|---|---|---|---|
| 1 | Fase A.1 + A.4 | offload system prompt + forced reminder | 3 gg | -30k |
| 2 | Fase A.2 | tool discovery on-demand | 4 gg | -14k |
| 3 | Fase A.3 + A.5 | rolling summary + tool_result cache | 5 gg | -40k cumulativi |
| 4 | Fase A.6 | KB graph summary mode | 2 gg | -varia |
| 5 | Fase B | compattazione description+schema + test suite | 2 gg | -8k |
| 6 | Fase C | smart upscale + UI badge | 2 gg | safety net |
| 7 | Fase D | tokenizer reale + UI overflow | 1 gg | safety net |

**Totale**: 19 giornate-uomo. Risparmio cumulativo atteso per chat tipica lunga (~20 turni): da 800k token medi → 250k token (-69%).

## Metriche di successo

Definition of Done:
- **chat 6 replicabile**: ripetere lo stesso prompt produce un run con `est_tokens_to_provider < 100k` (vs 1.2M attuali)
- **Test suite tool-selection**: ≥95% accuracy vs baseline pre-Fase-B
- **Replay 100 chat reali ultimi 30gg**: zero `ContextOverflow` errors, mediana token al provider -60%
- **Zero risposte in lingua sbagliata** (worker `language_audit` con regex CJK chars su risposte italiane)

## Configurazione DB nuova (regola G)

Tutti i settings sopra elencati vanno in una migrazione `NNNN_rag_pipeline_completion.sql`. Nessun fallback hardcoded nel codice (panico esplicito se DB down dopo retry).

## Migrazioni previste

| Migrazione | Contenuto |
|---|---|
| `NNNN_rag_pipeline_settings.sql` | tutti i settings `agent.context.*`, `agent.tools.*`, `agent.kb.*`, `agent.upscale.*` |
| `NNNN_agent_tools_descriptors_collection.sql` | inizializzazione Qdrant collection `agent_tools_descriptors` |
| `NNNN_chat_history_rolling_collection.sql` | inizializzazione Qdrant collection `chat_history_rolling` |
| `NNNN_agent_runs_upscale_columns.sql` | colonne `upscale_from`, `upscale_to`, `upscale_reason`, `est_tokens_at_call` |

## Test E2E (Playwright + smoke Python)

| Test | Asserzione |
|---|---|
| `replay_chat_6_overflow.py` | Stesso prompt di chat 6, run completa con `total_tokens_to_provider < 150k` |
| `tool_discovery_unit.py` | `nexus_mcp_tool_search("scrivi file")` ritorna `write_file` in top-3 |
| `rolling_summary_e2e.py` | Chat 25 turni, turno 26 ha context ≤30k token |
| `upscale_decision.py` | Prompt simulato che genera est=900k token → switch automatico a gemini-2.5-pro |
| `forced_rag_reminder.py` | Context >40% ctx → 1° tool call dell'agente e' `nexus_search_semantic` |
| `language_audit.py` | 50 chat italiane, 0 risposte con caratteri CJK |

## Rischi

| Rischio | Mitigazione |
|---|---|
| Fase A.2 (tool discovery) degrada tool-selection sui modelli o-series (sensibili al numero di tool) | Mantenere whitelist `o_series_essential_tools` esistente che gia' include i discovery tool. Test specifico su o3/o4-mini |
| Fase A.3 (rolling summary) perde informazione critica | Summary generato con modello capace (gemini-2.5-flash-lite o claude-haiku-4-5), originali sempre retrievable via `nexus_search_semantic` |
| Fase C (upscale) genera costi imprevisti | `cost_cap_usd_per_run` rigido, errore in UI se superato |
| Fase A.5 (cache tool_result) ritorna stale per tool side-effect (`run_command`) | Skiplist esplicita in `agent.tools.result_cache_skip_for` |

## Cosa NON facciamo (decisioni esplicite)

- **Sigle tool nomi**: 0 ROI misurato + degrado qualita' (vedi B4).
- **Riassunto LLM del system prompt ogni turno**: troppo costo, troppo rischio di drift. L'offload va in Qdrant inalterato.
- **Truncation distruttiva**: niente fix che butta via dati senza retrievability. Regola H, sempre.

## Riferimenti

- [[0014-context-size-management]] — FIX A-D context size (precursore brake)
- [[0015-rag-strutturale-unificato]] — definizione obiettivo pipeline RAG
- `brain/agents/context_offload.py` — modulo offload esistente
- `brain/agents/nodes.py::_apply_token_brake` — brake intra-turno (mig 0280)
- `crates/mcp-core/src/agent_tools/rag_search.rs` — tool `nexus_search_semantic`
- chat 6 Beauty-Book run `1a6e367e-4254-4baa-8894-3aa66c6d26a2` — trigger incident
