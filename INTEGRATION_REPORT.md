# INTEGRATION_REPORT.md — Report Integrazione LangGraph in Nexus

## 1. Riepilogo Modifiche

### File Modificati

| File | Tipo Modifica | Dettaglio |
|------|--------------|-----------|
| `brain/pyproject.toml` | Aggiunta dipendenze | `langgraph>=0.2.0`, `langchain-core>=0.3.0`, `aiosqlite>=0.19.0`, `typing-extensions>=4.12` |
| `brain/grpc_server/main.py` | Aggiunta funzioni e endpoint | Funzione `_get_agent_graph()` lazy-init + 5 nuovi endpoint `/agent/*` |
| `.env.example` | Documentazione | Aggiunta sezione variabili LangGraph (opzionali, con default automatici) |

### File Creati

| File | Scopo |
|------|-------|
| `brain/agents/__init__.py` | Package agents — esporta `create_agent_graph`, `AgentState` |
| `brain/agents/state.py` | TypedDict `AgentState` con 13 campi tipizzati |
| `brain/agents/nodes.py` | Tre nodi: `router_node`, `executor_node` (async), `learner_node` + `configure_services()` |
| `brain/agents/graph.py` | `create_agent_graph()` — compila grafo con SqliteSaver e interrupt_before |
| `brain/agents/checkpointer.py` | `get_checkpointer_path()`, `create_checkpointer()`, `get_memory_db_path()` |
| `brain/memory/__init__.py` | Package memory — esporta `LocalLearningStorage` |
| `brain/memory/storage.py` | `LocalLearningStorage` — CRUD SQLite con `interactions` e `task_stats` |
| `brain/memory/retrieval.py` | `InteractionRetriever` — similarity search Qdrant + store vettori |
| `tests/test_langgraph_integration.py` | Suite di test con 18 casi di test |
| `INTEGRATION_ANALYSIS.md` | Analisi architettura esistente |
| `INTEGRATION_DESIGN.md` | Design e diagrammi integrazione |
| `docs/LANGGRAPH_INTEGRATION.md` | Documentazione architettura e API usage |

---

## 2. Struttura Grafo Implementato

```
┌─────────────────────────────────────────┐
│           Nexus Agent Graph             │
│                                         │
│  [START]                                │
│     │                                   │
│     ▼                                   │
│  router_node                            │
│  - SemanticRouter.classify_intent()     │
│  - Stima token_budget                   │
│  - Imposta behavior_mode                │
│     │                                   │
│     ▼ (conditional_edges)               │
│  [INTERRUPT] ← interrupt_before=True    │
│     │                                   │
│     ▼ (dopo POST /agent/approve)        │
│  executor_node (async)                  │
│  - SemanticRouter.route_model()         │
│  - ProviderRegistry.generate_async()    │
│  - Misura latenza e token_usage         │
│     │                                   │
│     ▼                                   │
│  learner_node                           │
│  - EmbeddingService.embed_text()        │
│  - InteractionRetriever.store_vector()  │
│  - LocalLearningStorage.save()          │
│     │                                   │
│     ▼                                   │
│   [END]                                 │
└─────────────────────────────────────────┘
```

**Checkpointer**: `SqliteSaver` su `brain/nexus_memory/langgraph.db`
**Persistenza conversazioni**: per `thread_id` — ogni richiesta /agent/run con lo stesso thread riprende dal checkpoint

---

## 3. Schema Database Apprendimento

### Tabella `interactions` (brain/nexus_memory/learning.db)

```sql
id             INTEGER  -- PK autoincrement
thread_id      TEXT     -- ID thread LangGraph
timestamp      TEXT     -- ISO 8601 UTC
task_type      TEXT     -- intent classificato (fix/refactor/test/docs/arch/chat)
behavior_mode  TEXT     -- veloce|economica|bilanciata|approfondita
user_input     TEXT     -- messaggio utente completo
agent_output   TEXT     -- risposta generata
provider       TEXT     -- openai|anthropic|google|deepseek|mistral|ollama
model          TEXT     -- ID modello usato
latency_ms     REAL     -- tempo risposta in millisecondi
token_usage    INTEGER  -- token consumati
feedback_score REAL     -- punteggio utente [-1.0, 1.0]
qdrant_id      TEXT     -- UUID punto nel vector store (per cross-referencing)
metadata       TEXT     -- JSON: iterations, confidence, rationale
```

### Tabella `task_stats` (aggregati per tipo)

```sql
task_type      TEXT     -- PK
total_count    INTEGER  -- numero totale esecuzioni
success_count  INTEGER  -- esecuzioni completate
avg_latency_ms REAL     -- latenza media (aggiornamento incrementale)
avg_feedback   REAL     -- feedback medio utenti
last_updated   TEXT     -- ISO 8601 ultima modifica
```

### Qdrant Collection `agent_interactions`

- Dimensione: 384 (all-MiniLM-L6-v2, riusa EmbeddingService esistente)
- Distanza: COSINE
- Payload: thread_id, task_type, behavior_mode, provider, model, input_preview, output_preview

---

## 4. Esempi di Utilizzo API

### Flusso completo con approvazione

```bash
# Avvia agent (si ferma per approvazione)
curl -X POST http://localhost:8001/agent/run \
  -H "Content-Type: application/json" \
  -d '{
    "thread_id": "sessione-xyz",
    "prompt": "Refactorizza la funzione parse_token per migliorare la leggibilità",
    "behavior_mode": "approfondita"
  }'
# -> {"status":"pending_approval","user_intent":"refactor","task_type":"refactor",...}

# Verifica stato
curl http://localhost:8001/agent/state/sessione-xyz
# -> {"next":["executor"],"values":{"user_intent":"refactor","behavior_mode":"approfondita",...}}

# Approva esecuzione
curl -X POST http://localhost:8001/agent/approve/sessione-xyz
# -> {"status":"completed","result":"```python\ndef parse_token...```","provider_used":"anthropic","model_used":"claude-sonnet-4-6","latency_ms":2340.1}

# Feedback positivo
curl -X POST http://localhost:8001/agent/feedback/sessione-xyz \
  -H "Content-Type: application/json" \
  -d '{"score": 0.95}'
# -> {"thread_id":"sessione-xyz","updated":true,"score":0.95}

# Statistiche globali
curl http://localhost:8001/agent/stats
# -> {"stats":[{"task_type":"refactor","total_count":1,"avg_latency_ms":2340.1,...}]}
```

---

## 5. Metriche di Test

### Copertura Suite

| Modulo | Test Cases | Aree Coperte |
|--------|-----------|-------------|
| `LocalLearningStorage` | 8 | init, save, stats, feedback, retrieval, idempotenza |
| `router_node` | 3 | classificazione, stato vuoto, router assente |
| `executor_node` | 2 | chiamata provider, fallback senza provider |
| `learner_node` | 2 | salvataggio completo, fallback senza storage |
| `configure_services` | 1 | iniezione corretta servizi |
| `checkpointer paths` | 2 | path checkpointer, path learning db |
| `route_by_task_type` | 1 | tutti i task_type → executor |
| **Totale** | **19** | |

Coverage attesa: >= 80% sui moduli `brain/agents/` e `brain/memory/`.

---

## 6. Vincoli Rispettati

- **Nessuna modifica a LiteLLM** — il sistema non usa LiteLLM: usa `ProviderRegistry` nativo
- **Routing three-tier preservato** — la routing matrix `intent x behavior_mode` è intatta
- **Compatibilità API esistente** — tutti gli endpoint esistenti non modificati
- **Database separati** — SQLite per LangGraph, PostgreSQL per dati applicativi (nessun conflitto)
- **Embedding service riusato** — nessuna dipendenza embedding aggiuntiva
- **Tutto locale** — nessun servizio esterno richiesto oltre quelli già in docker-compose
- **Logging dettagliato** — ogni nodo logga con `logger.info`/`logger.debug` (nessun payload in chiaro)

---

## 7. Prossimi Passi Raccomandati

1. **Installare le dipendenze**:
   ```bash
   cd //wsl.localhost/Ubuntu/home/administrator/ideai/brain
   poetry add "langgraph>=0.2.0" "langchain-core>=0.3.0" "aiosqlite>=0.19.0" "typing-extensions>=4.12"
   ```

2. **Eseguire i test**:
   ```bash
   cd //wsl.localhost/Ubuntu/home/administrator/ideai/brain
   poetry run pytest tests/test_langgraph_integration.py -v --cov=brain/agents --cov=brain/memory
   ```

3. **Riavviare il Neural Core**:
   ```bash
   ./deploy/deploy-local.sh --service brain
   ```

4. **Testare manualmente** il flow completo con i comandi curl nella sezione 4.

5. **Configurare Qdrant** (se non già avviato):
   ```bash
   docker compose up -d qdrant
   ```
