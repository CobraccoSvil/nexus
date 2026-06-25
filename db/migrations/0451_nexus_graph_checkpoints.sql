-- 0451_nexus_graph_checkpoints.sql
-- FASE 0 del porting dell'orchestrazione agentica da Python/LangGraph a Rust
-- (piano: /tmp/langgraph_plan.md, sezioni 4 e 8). Rischio di regressione NULLO:
-- queste tabelle servono SOLO al motore nativo, che in Fase 0 non viene mai
-- imboccato (select_engine ritorna sempre 'python').
--
-- Regola H (fix definitivo, non toppa): lo schema dei checkpoint nasce da una
-- migrazione VERSIONATA, NON da un `CREATE TABLE IF NOT EXISTS` eseguito a
-- runtime dal codice ad ogni avvio (come faceva il checkpointer Python su
-- `langgraph_checkpoints`). Lo schema vive qui, sotto controllo di versione.
--
-- Regola G: la selezione del motore (python/rust/shadow) e' configurata nel DB
-- (tabella nexus_orchestrator_engine), non da env var o default hardcoded.
--
-- Idempotente: CREATE TABLE IF NOT EXISTS + INSERT ON CONFLICT DO NOTHING +
-- ADD COLUMN IF NOT EXISTS.

-- ---------------------------------------------------------------------------
-- (a) Snapshot di stato per-superstep del grafo nativo (punto unico della
--     persistenza: crates/nexus-agent-graph/src/checkpoint_pg.rs).
--
-- `superstep` e' un BIGINT MONOTONO (non un id random): il resume e'
-- deterministico ("riprendi dall'ultimo superstep completo"). `next_node` e' il
-- puntatore di esecuzione ESPLICITO (in LangGraph era implicito nei
-- channel_versions): il record salvato DOPO il route contiene gia' il prossimo
-- nodo, cosi' il resume riparte da li' senza ricalcolo. `state` e' serde_json
-- dello stato Rust (JSONB), NON il formato langchain dumps {lc,type,id,kwargs}.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_graph_checkpoints (
    run_id     UUID NOT NULL,
    superstep  BIGINT NOT NULL,
    next_node  TEXT NOT NULL,
    state      JSONB NOT NULL,
    metadata   JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, superstep)
);

-- Resume: ultimo checkpoint = superstep massimo per run_id.
CREATE INDEX IF NOT EXISTS idx_nexus_graph_checkpoints_run_superstep
    ON nexus_graph_checkpoints (run_id, superstep DESC);

-- ---------------------------------------------------------------------------
-- (b) Routing del motore di orchestrazione (strangler-fig, regola G).
--
-- Il routing e' PER-RUN, non per-nodo: il checkpoint non e' interscambiabile a
-- meta' run tra i due motori. Lo `scope_key` e' una sessione (UUID testuale),
-- un progetto (UUID testuale) o il jolly '*' (default globale). `engine` in
-- {python, rust, shadow}; `percent` per il rollout percentuale graduale.
--
-- La riga jolly '*' -> 'python' e' il DEFAULT esplicito: in Fase 0 select_engine
-- ritorna SEMPRE python (nessun hardcode di emergenza, la fonte e' qui).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_orchestrator_engine (
    scope_key  TEXT NOT NULL,
    scope_kind TEXT NOT NULL DEFAULT 'global'
               CHECK (scope_kind IN ('global', 'session', 'project')),
    engine     TEXT NOT NULL DEFAULT 'python'
               CHECK (engine IN ('python', 'rust', 'shadow')),
    percent    INT NOT NULL DEFAULT 100
               CHECK (percent BETWEEN 0 AND 100),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (scope_key)
);

-- Default globale: tutto il traffico resta sul motore Python finche' la parita'
-- non e' validata (Fase 5). Questa riga e' la fonte unica letta da select_engine.
INSERT INTO nexus_orchestrator_engine (scope_key, scope_kind, engine, percent)
VALUES ('*', 'global', 'python', 100)
ON CONFLICT (scope_key) DO NOTHING;

-- ---------------------------------------------------------------------------
-- (c) Motore con cui un run e' stato eseguito. Il recovery all'avvio di mcp-core
--     deve sapere su quale motore girava un run interrotto per riprenderlo sul
--     runtime giusto (i checkpoint dei due motori non sono interscambiabili).
--     NULL = run storici (pre-porting): il recovery li tratta come 'python'.
-- ---------------------------------------------------------------------------
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS engine TEXT;

COMMENT ON COLUMN agent_runs.engine IS
    'Motore di orchestrazione del run: python | rust | shadow. NULL = run pre-porting (trattato come python). Fonte: select_engine (crates/mcp-core/src/chat_messages/agent_run.rs), tabella nexus_orchestrator_engine.';
