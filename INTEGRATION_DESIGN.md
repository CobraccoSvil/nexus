# INTEGRATION_DESIGN.md — Design Integrazione LangGraph in Nexus

## 1. Diagramma ASCII del Grafo LangGraph

```
                    ┌─────────────────────────────────────────────┐
                    │          Nexus Agent Graph                  │
                    │                                             │
  User Request      │   ┌──────────┐                             │
  ──────────────────┼──▶│  router  │                             │
  (thread_id,       │   └──────────┘                             │
   messages,        │        │                                   │
   behavior_mode)   │        │ classifica intent                  │
                    │        ▼ (fix/refactor/test/docs/           │
                    │   ┌──────────────────────────┐  arch/chat) │
                    │   │  conditional_edges        │             │
                    │   └──────────────────────────┘             │
                    │        │                                   │
                    │        ▼                                   │
                    │   ┌──────────┐  ◀── interrupt_before       │
                    │   │ executor │      (human-in-the-loop)    │
                    │   └──────────┘                             │
                    │        │                                   │
                    │        │ chiama ProviderRegistry            │
                    │        │ con routing matrix esistente       │
                    │        ▼                                   │
                    │   ┌──────────┐                             │
                    │   │ learner  │                             │
                    │   └──────────┘                             │
                    │        │                                   │
                    │        │ salva in SQLite + Qdrant           │
                    │        ▼                                   │
                    │       END                                  │
                    └─────────────────────────────────────────────┘

  Persistenza:
  ┌─────────────────────────────────────────────────────────────┐
  │  SqliteSaver (checkpointer)   brain/nexus_memory/langgraph.db │
  │  LocalLearningStorage (log)   brain/nexus_memory/learning.db  │
  │  Qdrant (vettori)             collection: agent_interactions   │
  └─────────────────────────────────────────────────────────────┘
```

---

## 2. Schema AgentState (TypedDict)

```python
class AgentState(TypedDict):
    messages: Annotated[Sequence[BaseMessage], add]   # Conversazione
    user_intent: str                                   # fix/refactor/test/docs/architecture/chat
    task_type: str                                     # alias usato per conditional_edges
    behavior_mode: str                                 # veloce|economica|bilanciata|approfondita
    token_budget: int                                  # stima token input
    result: str | None                                 # output finale dell'executor
    provider_used: str | None                          # quale provider ha risposto
    model_used: str | None                             # quale modello ha risposto
    feedback_score: float | None                       # score opzionale da utente
    latency_ms: float | None                           # latenza chiamata LLM
    token_usage: int | None                            # token consumati
    iterations: int                                    # contatore iterazioni
    thread_id: str                                     # ID thread per checkpointer
```

---

## 3. Schema Database per Apprendimento Locale

### File: `brain/nexus_memory/learning.db` (SQLite)

#### Tabella `interactions`

```sql
CREATE TABLE IF NOT EXISTS interactions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id    TEXT    NOT NULL,
    timestamp    TEXT    NOT NULL,     -- ISO 8601 UTC
    task_type    TEXT    NOT NULL,     -- intent classificato
    behavior_mode TEXT   NOT NULL,     -- modalità routing
    user_input   TEXT    NOT NULL,     -- testo messaggio utente
    agent_output TEXT    NOT NULL,     -- risposta generata
    provider     TEXT,                 -- provider usato (openai/anthropic/...)
    model        TEXT,                 -- modello usato
    latency_ms   REAL,                 -- latenza in millisecondi
    token_usage  INTEGER,              -- token totali
    feedback_score REAL,               -- feedback utente [-1.0, 1.0]
    qdrant_id    TEXT,                 -- ID punto Qdrant (per retrieval)
    metadata     TEXT                  -- JSON extra (es. confidence, rationale)
);
```

#### Tabella `task_stats`

```sql
CREATE TABLE IF NOT EXISTS task_stats (
    task_type        TEXT PRIMARY KEY,
    total_count      INTEGER DEFAULT 0,
    success_count    INTEGER DEFAULT 0,
    avg_latency_ms   REAL    DEFAULT 0.0,
    avg_feedback     REAL    DEFAULT 0.0,
    last_updated     TEXT    NOT NULL
);
```

### Qdrant Collection `agent_interactions`

- Dimensione vettori: 384 (all-MiniLM-L6-v2, riutilizzando EmbeddingService esistente)
- Distanza: COSINE
- Payload per punto:
  ```json
  {
    "thread_id": "...",
    "task_type": "fix",
    "timestamp": "2026-04-23T...",
    "input_preview": "...",
    "output_preview": "...",
    "feedback_score": null
  }
  ```

---

## 4. Lista Modifiche ai File Esistenti

### `brain/pyproject.toml`

Aggiungere sotto `[tool.poetry.dependencies]`:
```toml
langgraph = ">=0.2.0"
langchain-core = ">=0.3.0"
aiosqlite = ">=0.19.0"
```

### `brain/grpc_server/main.py`

Aggiungere:
1. Import di `AgentRequest` e `create_agent_graph`
2. Inizializzazione globale `agent_graph = create_agent_graph(...)`
3. Tre nuovi endpoint: `POST /agent/run`, `POST /agent/approve/{thread_id}`, `GET /agent/state/{thread_id}`
4. Endpoint `POST /agent/feedback/{thread_id}` per raccogliere feedback

---

## 5. Nuovi File da Creare

```
brain/
├── agents/
│   ├── __init__.py          # Export pubblico: create_agent_graph, AgentState
│   ├── state.py             # TypedDict AgentState
│   ├── nodes.py             # router_node, executor_node, learner_node
│   ├── graph.py             # create_agent_graph() con SqliteSaver
│   └── checkpointer.py      # get_checkpointer() — setup path e connessione
└── memory/
    ├── __init__.py          # Export: LocalLearningStorage
    ├── storage.py           # CRUD su SQLite learning.db
    └── retrieval.py         # similarity_search(), get_similar_interactions()

brain/nexus_memory/          # Directory dati persistenti (gitignored)
    langgraph.db             # SqliteSaver checkpointer
    learning.db              # LocalLearningStorage

tests/
    test_langgraph_integration.py   # Test completo coverage >= 80%

docs/
    LANGGRAPH_INTEGRATION.md        # Documentazione architettura + usage
```

---

## 6. Decisioni di Design

### Checkpointer: SqliteSaver

Scelta `SqliteSaver` (non `PostgresSaver`) perché:
- Il PostgreSQL di Nexus è gestito da Rust/mcp-db — non è sicuro aggiungere tabelle LangGraph senza migrazioni
- SQLite è zero-config, puramente locale, ideale per persistenza conversazioni agent
- Separazione delle responsabilità: PostgreSQL = dati applicativi, SQLite = stato agent

### Integrazione Provider: ProviderRegistry nativo

I nodi LangGraph chiamano direttamente `providers.generate_completion_async()` invece di LiteLLM, perché:
- Il ProviderRegistry è il contratto esistente con fallback a cascata già implementato
- Evita dipendenza extra e potenziali conflitti di routing
- Preserva la routing matrix `intent x behavior_mode` già validata in produzione

### Embedding: EmbeddingService esistente

Il `learner_node` usa `EmbeddingService.embed_text()` (all-MiniLM-L6-v2) e `EmbeddingService.store_vectors()` su Qdrant, senza aggiungere nuove dipendenze di embedding.

### Human-in-the-loop: interrupt_before executor

`interrupt_before=["executor"]` blocca il grafo prima della chiamata LLM costosa, permettendo all'utente di approvare via `POST /agent/approve/{thread_id}`. Per task non critici (chat), l'utente può inviare direttamente senza approval.

---

## 7. Flusso Dati Completo

```
[1] POST /agent/run
    body: { thread_id, prompt, behavior_mode="bilanciata" }
    
    ▼ router_node
    - classifica intent via SemanticRouter.classify_intent()
    - stima token_budget
    - imposta task_type, behavior_mode, token_budget nello stato
    
    ▼ [INTERRUPT] prima di executor_node
    - stato salvato in SqliteSaver (langgraph.db)
    - risposta HTTP: { "status": "pending_approval", "thread_id": ... }
    
[2] POST /agent/approve/{thread_id}
    - riprende grafo dal checkpoint
    
    ▼ executor_node
    - chiama SemanticRouter.route_model(intent, token_budget, behavior_mode)
    - ottiene RoutingDecision (provider, model, rationale)
    - chiama providers.generate_completion_async(provider, model, prompt)
    - misura latenza
    - salva result, provider_used, model_used, latency_ms, token_usage
    
    ▼ learner_node
    - genera embedding via EmbeddingService.embed_text()
    - salva in learning.db (LocalLearningStorage)
    - salva vettore in Qdrant collection "agent_interactions"
    - aggiorna task_stats (success_count, avg_latency)
    
    ▼ END
    - risposta HTTP: { result, provider_used, model_used, latency_ms }

[3] POST /agent/feedback/{thread_id}
    body: { score: 0.8 }
    - aggiorna feedback_score in learning.db
    - aggiorna avg_feedback in task_stats
```
