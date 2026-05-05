# LangGraph Integration — Nexus Neural Core

## Architettura

Il modulo LangGraph estende il Neural Core di Nexus con un grafo di agenti persistente che:

1. Classifica l'intent utente tramite il `SemanticRouter` esistente
2. Esegue il task via `ProviderRegistry` con la routing matrix `intent x behavior_mode`
3. Salva ogni interazione in SQLite e i relativi embedding in Qdrant per apprendimento locale

### Struttura del Grafo

```
router_node → [interrupt] → executor_node → learner_node → END
```

- **router_node**: classifica intent, stima token budget, determina behavior_mode
- **executor_node**: chiama il provider corretto via ProviderRegistry, misura latenza
- **learner_node**: salva l'interazione in SQLite (`learning.db`) e l'embedding in Qdrant

### Persistenza

| Storage | File | Contenuto |
|---------|------|-----------|
| SqliteSaver (LangGraph) | `brain/nexus_memory/langgraph.db` | Stato conversazione per thread_id |
| LocalLearningStorage | `brain/nexus_memory/learning.db` | Log interazioni + statistiche aggregate |
| Qdrant | collection `agent_interactions` | Vettori per similarity search (RAG) |

---

## Endpoint API

### `POST /agent/run`

Avvia un'esecuzione dell'agent. Il grafo si ferma prima di `executor` per approvazione umana.

**Body**:
```json
{
  "thread_id": "conv-123",
  "prompt": "Fissa il bug nel modulo auth",
  "behavior_mode": "bilanciata"
}
```

**Risposta** (pending):
```json
{
  "status": "pending_approval",
  "thread_id": "conv-123",
  "next": ["executor"],
  "user_intent": "fix",
  "task_type": "fix",
  "routing_mode": "bilanciata"
}
```

**behavior_mode validi**: `veloce`, `economica`, `bilanciata` (default), `approfondita`

---

### `POST /agent/approve/{thread_id}`

Riprende l'esecuzione dal checkpoint (approval human-in-the-loop).

**Risposta**:
```json
{
  "status": "completed",
  "thread_id": "conv-123",
  "result": "Ecco la correzione al bug...",
  "provider_used": "anthropic",
  "model_used": "claude-haiku-4-5-20251001",
  "latency_ms": 1234.5,
  "token_usage": 320
}
```

---

### `GET /agent/state/{thread_id}`

Recupera lo snapshot di stato corrente di un thread.

**Risposta**:
```json
{
  "thread_id": "conv-123",
  "next": ["executor"],
  "values": {
    "user_intent": "fix",
    "task_type": "fix",
    "behavior_mode": "bilanciata",
    "token_budget": 850
  }
}
```

---

### `POST /agent/feedback/{thread_id}`

Registra il feedback dell'utente per l'ultima interazione.

**Body**:
```json
{ "score": 0.9 }
```

Score range: `-1.0` (negativo) → `1.0` (positivo)

---

### `GET /agent/stats`

Statistiche aggregate per tipo di task.

**Risposta**:
```json
{
  "stats": [
    {
      "task_type": "fix",
      "total_count": 42,
      "success_count": 42,
      "avg_latency_ms": 1250.3,
      "avg_feedback": 0.82,
      "last_updated": "2026-04-23T..."
    }
  ]
}
```

---

## Flusso Completo

```bash
# 1. Avvia il task
curl -X POST http://localhost:8001/agent/run \
  -H "Content-Type: application/json" \
  -d '{"thread_id":"c1","prompt":"fix auth bug","behavior_mode":"bilanciata"}'
# -> {"status":"pending_approval","next":["executor"],...}

# 2. Approva l'esecuzione
curl -X POST http://localhost:8001/agent/approve/c1
# -> {"status":"completed","result":"...","provider_used":"anthropic",...}

# 3. Fornisci feedback
curl -X POST http://localhost:8001/agent/feedback/c1 \
  -H "Content-Type: application/json" \
  -d '{"score":0.9}'

# 4. Visualizza statistiche
curl http://localhost:8001/agent/stats
```

---

## Configurazione

### Variabili d'Ambiente

Nessuna nuova variabile richiesta — il modulo usa le stesse variabili del Neural Core:

| Variabile | Default | Scopo |
|-----------|---------|-------|
| `QDRANT_URL` | `http://localhost:6333` | Vector store per embedding interazioni |
| `DATABASE_URL` | — | PostgreSQL (usato solo per chiavi API provider) |

I database SQLite vengono creati automaticamente in `brain/nexus_memory/`.

### Dipendenze Aggiunte

```toml
langgraph = ">=0.2.0"
langchain-core = ">=0.3.0"
aiosqlite = ">=0.19.0"
typing-extensions = ">=4.12"
```

Installa con:
```bash
cd brain
poetry install
```

---

## Apprendimento Locale

Il `learner_node` salva ogni interazione completata con:

- **Input utente** (testo completo)
- **Output generato** (risposta del provider)
- **Metadata**: provider, modello, latenza, token usati, behavior_mode
- **Embedding** (384-dim, all-MiniLM-L6-v2) in Qdrant per similarity search

Le statistiche aggregate per `task_type` permettono di monitorare:
- Volume e frequenza per categoria
- Latenza media per tipo di task
- Score di feedback medio nel tempo

---

## Estensione del Sistema

### Aggiungere un nuovo nodo

1. Definisci la funzione in `brain/agents/nodes.py`
2. Aggiungila al grafo in `brain/agents/graph.py`
3. Aggiungi edge appropriati

### Modificare il routing

La routing matrix è in `brain/router/service.py:_ROUTING_MATRIX`. Non modificare il grafo LangGraph per cambiare il provider — modifica la matrix esistente.

### Disabilitare human-in-the-loop

In `brain/agents/graph.py`, rimuovi `interrupt_before=["executor"]` dalla chiamata a `workflow.compile()`. Non raccomandato per task critici.
