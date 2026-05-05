# INTEGRATION_ANALYSIS.md — Analisi Architettura Nexus per Integrazione LangGraph

## 1. Architettura Attuale del Microservizio Python

### Entry Point

**File**: `brain/grpc_server/main.py`

Il Neural Core espone due interfacce:
- **FastAPI HTTP** su porta `8001` (debug/tool call), avviato in thread daemon
- **gRPC** su porta `50051` (produzione), avviato nel thread principale

Servizi globali inizializzati all'avvio:
```
embeddings = EmbeddingService()
router     = SemanticRouter(embedding_service=embeddings)
providers  = ProviderRegistry()
```

### Endpoint FastAPI Esistenti

| Metodo | Path | Scopo |
|--------|------|-------|
| GET | `/health` | Health check |
| POST | `/classify-intent` | Classificazione intent utente |
| POST | `/route-model` | Routing provider+modello |
| POST | `/embed` | Generazione embedding singolo/batch |
| POST | `/search` | Semantic search su Qdrant |
| GET | `/providers/{provider}/models` | Lista modelli per provider |
| GET | `/providers/{provider}/health` | Health check provider |
| POST | `/complete` | Completamento LLM generico |
| POST | `/reload-settings` | Ricarica chiavi API da DB |
| POST | `/batch-analyze/submit` | Batch Anthropic |
| GET | `/batch-analyze/{id}/status` | Stato batch |
| GET | `/batch-analyze/{id}/results` | Risultati batch |
| POST | `/agent-turn/stream` | Streaming agent turn (SSE) |
| WS | `/ws/terminal/{session_id}` | Terminale WebSocket |

### Struttura Moduli Python

```
brain/
├── __init__.py
├── pyproject.toml           # Poetry, Python ^3.12
├── embeddings/
│   ├── __init__.py
│   └── service.py           # EmbeddingService (sentence-transformers + Qdrant)
├── providers/
│   ├── __init__.py
│   ├── base.py              # BaseProvider, ProviderResult, ProviderCatalogEntry
│   ├── registry.py          # ProviderRegistry (6 provider con fallback)
│   ├── openai_provider.py
│   ├── anthropic_provider.py
│   ├── google_provider.py
│   ├── deepseek_provider.py
│   ├── mistral_provider.py
│   ├── ollama_provider.py   # On-premise (Ollama locale)
│   ├── anthropic_batch.py   # Batch processing Anthropic
│   ├── google_batch.py
│   ├── error_handler.py
│   └── dns_transport.py     # Override DNS per httpx
├── router/
│   ├── __init__.py
│   └── service.py           # SemanticRouter con routing matrix behavior_mode x intent
├── documents/
│   ├── generator.py
│   ├── styles.py
│   └── templates.py
└── grpc_server/
    ├── __init__.py
    ├── main.py              # Entry point principale
    ├── neural_service.py    # Logica servizio gRPC
    └── generated/           # File protobuf auto-generati
```

---

## 2. Sistema di Persistenza Dati Esistente

### PostgreSQL (database principale)
- **URL**: `postgres://nexus:nexus@localhost:5433/nexus`
- **Env var**: `DATABASE_URL`
- **Uso attuale**: Chiavi API, impostazioni sistema, log terminal, progetti
- **Client usato**: `psycopg2` (import runtime in alcune funzioni)

### Qdrant (vector database)
- **URL**: `http://localhost:6333` (env: `QDRANT_URL`)
- **Porta alternativa**: `6334` (grpc)
- **Collection default**: `code_embeddings`
- **Dimensione vettori**: 384 (all-MiniLM-L6-v2)
- **Distanza**: COSINE

### Redis
- **URL**: `redis://localhost:6379`
- **Uso attuale**: Cache e session store (dal docker-compose)

### SQLite
- **Non in uso attuale** nel microservizio Python
- Ideale per il checkpointer LangGraph (nessun conflitto)

### Cross-Project Learning (Rust)
- **Crate**: `crates/mcp-learning/`
- Estrae pattern da codice sorgente
- Genera `KnowledgeBundle` con `ExtractedPattern`
- Supporta sync a: Claude Memory, OpenAI Custom GPT, Gemini Gem, Markdown, JSON
- Feedback loop via `apply_feedback()`

---

## 3. Configurazione Provider AI

### Provider Registry Personalizzato (NON LiteLLM)

Il sistema usa un registry custom con 6 provider:

| Provider | Tipo | Modelli Principali |
|----------|------|--------------------|
| `openai` | Cloud | gpt-4.1, gpt-4.1-mini, gpt-4.1-nano |
| `anthropic` | Cloud | claude-sonnet-4-6, claude-haiku-4-5-20251001, claude-opus-4-6 |
| `google` | Cloud | gemini-2.5-flash, gemini-2.5-flash-lite |
| `deepseek` | Cloud | deepseek-chat, deepseek-reasoner |
| `mistral` | Cloud | mistral-small-4, codestral-latest, open-mistral-nemo |
| `ollama` | On-premise | Qwen2.5, deepseek-r1 (configurabili) |

### Routing Matrix

Il routing combina `intent` x `behavior_mode`:

**Intent riconosciuti**: `fix_semplice`, `fix_complesso`, `refactor`, `test`, `docs`, `architecture`, `database_schema_change`, `chat_breve`, `chat_media`, `chat_lunga`

**Behavior modes**: `veloce`, `economica`, `bilanciata` (default), `approfondita`

### Fallback a Cascata

Il registry implementa fallback automatico:
1. Provider richiesto → se disabilitato → anthropic → openai
2. Errori `billing_error`/`rate_limit`/`overloaded` triggherano fallback al prossimo provider

### Configurazione Model Aliases (`config/model-aliases.yaml`)

```yaml
embedding:
  cloud_primary: openai/text-embedding-3-small
  onprem: BAAI/bge-m3
coder-small:
  cloud_primary: openai/gpt-4o-mini
  onprem: Qwen/Qwen2.5-Coder-7B-Instruct
reasoning-heavy:
  cloud_primary: openai/gpt-4.1
  onprem: deepseek-r1-distill-70b
sensitive-only:
  onprem: Qwen/Qwen2.5-72B-Instruct
```

### Policy di Sicurezza (`config/policies/default.yaml`)

- **Tier 3**: SOLO on-premise (Qwen 72B) — dati altamente sensibili
- **Tier 2**: Override esplicito richiesto
- **Presidio DLP**: Redaction PII abilitata
- Telemetry: OTLP + Prometheus

---

## 4. Dipendenze e Versioni Rilevanti

```toml
[tool.poetry.dependencies]
python = "^3.12"          # Compatibile con LangGraph (richiede >=3.9)
fastapi = "^0.115.12"
uvicorn = "^0.34.0"
pydantic = "^2.11.2"
grpcio = "^1.68"
sentence-transformers = "^3.3"
qdrant-client = "^1.12"
openai = "^1.58"
anthropic = "^0.40"
google-generativeai = "^0.8"
numpy = "^2.0"
tiktoken = "^0.8"
python-docx = "^1.1"
```

### Dipendenze da Aggiungere per LangGraph

```toml
langgraph = ">=0.2.0"
langchain-core = ">=0.3.0"
aiosqlite = ">=0.19.0"    # SQLite async per checkpointer
```

**NON è necessario** aggiungere `litellm` — il sistema usa il ProviderRegistry nativo.

### Assenza di Conflitti Critici

- Python 3.12 soddisfa il requisito LangGraph (>=3.9)
- `pydantic ^2.11.2` compatibile con langchain-core >=0.3
- `numpy ^2.0` compatibile con langchain-core

---

## 5. File Chiave da Modificare per l'Integrazione

### File da MODIFICARE

| File | Modifica |
|------|---------|
| `brain/pyproject.toml` | Aggiungere dipendenze langgraph, langchain-core, aiosqlite |
| `brain/grpc_server/main.py` | Aggiungere endpoint `/agent/*` per LangGraph |

### File da CREARE (nuovi)

| File | Scopo |
|------|-------|
| `brain/agents/__init__.py` | Package agents |
| `brain/agents/state.py` | Schema TypedDict AgentState |
| `brain/agents/nodes.py` | Nodi del grafo (router, executor, learner) |
| `brain/agents/graph.py` | Definizione e compilazione grafo LangGraph |
| `brain/agents/checkpointer.py` | Setup SqliteSaver + config path |
| `brain/memory/__init__.py` | Package memory |
| `brain/memory/storage.py` | LocalLearningStorage (SQLite) |
| `brain/memory/retrieval.py` | Similarity search per RAG |
| `tests/test_langgraph_integration.py` | Test coverage >= 80% |
| `docs/LANGGRAPH_INTEGRATION.md` | Documentazione architettura |

### Directory da Creare

```
brain/nexus_memory/langgraph_state/    # Directory checkpointer SQLite
```

---

## 6. Note Critiche per l'Integrazione

1. **NON usare LiteLLM**: il sistema usa `ProviderRegistry` nativo. I nodi LangGraph devono chiamare `providers.generate_completion_async()` o `providers.generate_agent_turn_sync()`.

2. **Thread safety**: il FastAPI gira in thread daemon mentre gRPC gira nel thread principale. I nodi async del grafo devono usare `asyncio` correttamente.

3. **Mypy strict**: il `pyproject.toml` ha `mypy strict = true`. Tutti i nuovi file devono avere typing completo.

4. **Ruff**: linting con regole `E, F, W, I, B, UP, SIM, RUF`, line-length 100.

5. **Checkpointer SQLite**: il database SQLite del checkpointer deve stare in `brain/nexus_memory/langgraph.db` (separato da PostgreSQL).

6. **Qdrant per embedding learning**: il `learner_node` deve usare `EmbeddingService` esistente per generare embedding e salvarli in Qdrant (collection `agent_interactions`), non reinventare il wheel.
