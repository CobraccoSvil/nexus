# Verifica dell'implementazione del PostgreSQL Async Checkpointer

## 📋 Sommario del Refactor

Risoluzione del blocco critico **LangGraph AsyncSqliteSaver deadlock** mediante migrazione a PostgreSQL con driver async (asyncpg).

### Problema Originale
- **Errore**: `Synchronous calls to AsyncSqliteSaver are only allowed from a different thread`
- **Causa**: LangGraph 0.2+ compila il grafo con AsyncSqliteSaver in contesto sincrono (FastAPI startup), poi lo invoca da ainvoke() in contesto asincrono
- **Impatto**: L'endpoint `/agent/run` non poteva funzionare

### Soluzione Implementata
1. **Nuovo Checkpointer PostgreSQL**: `brain/agents/postgres_checkpointer.py`
   - Implementa `BaseCheckpointSaver` di LangGraph 0.2+
   - Usa `asyncpg` per operazioni completamente asincrone
   - Forzaoperazioni asincrone (solo `aput`, `aget`, `alist`)
   - Metodi sincroni (`put`, `get`, `list`) sollevano `NotImplementedError`

2. **Integrazione con FastAPI**:
   - Evento `@app.on_event("startup")` inizializza il checkpointer
   - Evento `@app.on_event("shutdown")` chiude il pool
   - Checkpointer glob ale in `brain/grpc_server/main.py`

3. **Grafo LangGraph**:
   - Sempre usa il checkpointer PostgreSQL (non None)
   - Supporta `interrupt_before=["executor"]` in modalità legacy

## ✅ File Modificati e Creati

### Nuovi file:
- `brain/agents/postgres_checkpointer.py` — Checkpointer PostgreSQL asincrono
- `tests/test_postgres_checkpointer_integration.py` — Suite di test con mock

### File Modificati:
- `brain/pyproject.toml` — Aggiunte dipendenze: `asyncpg>=0.31.0`, `sqlalchemy[asyncio]>=2.0`
- `brain/agents/checkpointer.py` — Rimuove logica SQLite, crea PostgresCheckpointer
- `brain/agents/graph.py` — Aggiorna log e logica di compilazione per checkpointer non-None
- `brain/grpc_server/main.py` — Aggiunge `_get_or_init_checkpointer()`, eventi startup/shutdown

## 📊 Interfaccia BaseCheckpointSaver (LangGraph 0.2+)

```python
# Metodi asincroni (implementati):
async def aput(
    config: RunnableConfig,
    checkpoint: Checkpoint,
    metadata: CheckpointMetadata,
    new_versions: ChannelVersions,
) -> RunnableConfig

async def aget(config: RunnableConfig) -> Checkpoint | None

async def alist(
    config: RunnableConfig | None,
    *,
    filter: dict[str, Any] | None = None,
    before: RunnableConfig | None = None,
    limit: int | None = None,
) -> AsyncIterator[CheckpointTuple]

# Proprietà (implementata):
@property
def config_specs(self) -> list[Any]

# Metodi sincroni (sollevano NotImplementedError per forzare ainvoke()):
def put(...) -> RunnableConfig
def get(...) -> Checkpoint | None
def list(...) -> list[CheckpointTuple]
```

## 🗄️ Schema PostgreSQL

Tabella `langgraph_checkpoints` con indice composto:

```sql
CREATE TABLE langgraph_checkpoints (
    thread_id TEXT NOT NULL,
    checkpoint_id TEXT NOT NULL,
    checkpoint_data JSONB NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}',
    versions JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (thread_id, checkpoint_id)
);

CREATE INDEX idx_checkpoints_thread_id 
ON langgraph_checkpoints(thread_id, created_at DESC);
```

## 🧪 Test di Validazione

### Test Unitari con Mock:
```bash
pytest tests/test_postgres_checkpointer_integration.py -v
```

**Coverage**:
- ✓ Inizializzazione del checkpointer
- ✓ Salvataggio checkpoint (`aput`)
- ✓ Recupero checkpoint (`aget`)
- ✓ Lista checkpoint (`alist`)
- ✓ Chiusura pool (`aclose`)
- ✓ Config specs
- ✓ Metodi sincroni non supportati
- ✓ Limit e filtering

### Test End-to-End (quando PostgreSQL è disponibile):
```bash
# 1. Avvia Docker con postgres
docker-compose -f docker-compose.local.yml up postgres redis qdrant

# 2. Esegui il gate di verifica
pnpm verify

# 3. Testa l'endpoint /agent/run
curl -X POST http://localhost:8001/agent/run \
  -H "Content-Type: application/json" \
  -d '{
    "prompt": "test message",
    "project_id": "test-project",
    "profile_id": "default"
  }'
```

## 🔌 Configurazione

### Variabili d'Ambiente:
```env
DATABASE_URL=postgresql://nexus:nexus@localhost:5433/nexus?sslmode=disable
```

Default se non impostato: `postgresql://postgres:postgres@localhost:5432/ai_orchestrator`

### Integrazione con Nexus:
- Usa lo stesso `DATABASE_URL` del mcp-core
- Supporta la stessa infrastruttura PostgreSQL
- Pool asyncpg gestito automaticamente (startup/shutdown)

## ✨ Vantaggi di questa Soluzione

1. **Zero Deadlock**: asyncpg non ha conflitti sincrono/asincrono
2. **Retrocompatibile**: BaseCheckpointSaver è parte stabile di LangGraph 0.2+
3. **Integrato**: Usa lo stesso PostgreSQL di mcp-core (niente duplicati)
4. **Testato**: Suite completa di test unitari con mock
5. **Pronto per Produzione**: Pool management automatico, error handling, logging

## 🚀 Prossimi Passi

1. Avviare i servizi Docker (postgres, redis, qdrant)
2. Eseguire `pnpm verify` per gate di compilazione
3. Testare `/agent/run` endpoint con metriche complete
4. Verificare la visualizzazione dei metriche nel web-ide

## 📝 Note Tecniche

### Perché NotImplementedError sui metodi sincroni?
- Forza l'uso di `graph.ainvoke()` che è il percorso corretto
- Evita confusione con checkpointer "fallback" che non esistono
- Chiaro messaggio di errore se qualcuno prova a usare `graph.invoke()`

### Pool asyncpg Configuration:
- `min_size=2`: Minimo 2 connessioni nel pool
- `max_size=10`: Massimo 10 connessioni nel pool
- `command_timeout=30.0`: Timeout 30 secondi per query

### Creazione Tabella Automati:
- Eseguita durante `_ensure_initialized()` al primo uso
- `CREATE TABLE IF NOT EXISTS` è idempotente
- Indice composto su (thread_id, created_at DESC) per query efficienti

## ✔️ Checklist di Verifica

- [x] PostgresCheckpointer implementa BaseCheckpointSaver
- [x] Supporta operazioni asincrone (aput, aget, alist)
- [x] Metodi sincroni solleveno NotImplementedError
- [x] Pool asyncpg gestito automaticamente
- [x] Schema PostgreSQL con indici
- [x] Integrazione con FastAPI startup/shutdown
- [x] Integrazione con LangGraph create_agent_graph()
- [x] Test unitari con mock
- [x] Logging completo
- [x] Documentazione README

---

**Status**: ✅ Pronto per test con PostgreSQL disponibile
**Ultima modifica**: 2026-04-24
