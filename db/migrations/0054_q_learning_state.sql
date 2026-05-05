-- Migration 0054: Q-Learning state — persistenza Q-values del router.
--
-- Tabelle:
--   nexus_q_values        → Q-table: (task_type, agent_type) → q_value + stats
--   nexus_routing_history → log delle decisioni di routing (audit trail)
--
-- La Q-table viene caricata in memoria all'avvio del QLearningRouter
-- e aggiornata in background dopo ogni esecuzione (fire-and-forget).
-- La routing_history mantiene un audit trail comprimibile (partition by month).

-- ---------------------------------------------------------------------------
-- Tabella: nexus_q_values
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_q_values (
    -- Chiave composita: (task_type, agent_key) identifica univocamente una entry
    task_type       TEXT        NOT NULL,
    agent_key       TEXT        NOT NULL,
    -- Q-value corrente (aggiornato con Bellman equation)
    q_value         REAL        NOT NULL DEFAULT 0.5,
    -- Statistiche di utilizzo
    visit_count     BIGINT      NOT NULL DEFAULT 0,
    success_count   BIGINT      NOT NULL DEFAULT 0,
    failure_count   BIGINT      NOT NULL DEFAULT 0,
    -- Ultima reward ricevuta (per debug)
    last_reward     REAL,
    avg_reward      REAL        NOT NULL DEFAULT 0.0,
    -- Timestamps
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (task_type, agent_key)
);

COMMENT ON TABLE nexus_q_values IS
    'Q-table persistita del QLearningRouter. Caricata in memoria all''avvio.';

COMMENT ON COLUMN nexus_q_values.q_value IS
    'Q(task_type, agent_key) — valore atteso reward [0, 1.5]. Aggiornato con Bellman.';

-- Indici per caricamento veloce all'avvio
CREATE INDEX IF NOT EXISTS idx_nexus_q_values_task_type
    ON nexus_q_values (task_type);

CREATE INDEX IF NOT EXISTS idx_nexus_q_values_agent
    ON nexus_q_values (agent_key);

-- Indice per query di introspection: "top agents per task_type"
CREATE INDEX IF NOT EXISTS idx_nexus_q_values_q_desc
    ON nexus_q_values (task_type, q_value DESC);

-- ---------------------------------------------------------------------------
-- Tabella: nexus_routing_history
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS nexus_routing_history (
    id              BIGSERIAL   PRIMARY KEY,
    -- Contesto della decisione
    task_id         TEXT        NOT NULL,
    task_type       TEXT        NOT NULL,
    project_id      UUID,
    -- Agente selezionato
    selected_agent  TEXT        NOT NULL,
    -- Q-value e confidence al momento della decisione
    q_value         REAL        NOT NULL,
    confidence      REAL        NOT NULL,
    -- Strategia usata (exploitation/exploration/cold_start)
    strategy        TEXT        NOT NULL,
    -- Latenza decisione in microsecondi
    decision_us     INTEGER,
    -- Outcome (NULL finché non completato)
    success         BOOLEAN,
    quality_score   REAL,
    execution_ms    BIGINT,
    reward          REAL,
    -- Timestamp
    decided_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ
);

COMMENT ON TABLE nexus_routing_history IS
    'Audit trail delle decisioni di routing Q-Learning. Utile per analisi canary.';

-- Indici temporali per query di analisi
CREATE INDEX IF NOT EXISTS idx_nexus_routing_history_decided_at
    ON nexus_routing_history (decided_at DESC);

CREATE INDEX IF NOT EXISTS idx_nexus_routing_history_task_type
    ON nexus_routing_history (task_type, decided_at DESC);

CREATE INDEX IF NOT EXISTS idx_nexus_routing_history_project
    ON nexus_routing_history (project_id, decided_at DESC)
    WHERE project_id IS NOT NULL;

-- Indice per join con agent_runs
CREATE INDEX IF NOT EXISTS idx_nexus_routing_history_task_id
    ON nexus_routing_history (task_id);

-- ---------------------------------------------------------------------------
-- View: performance per agent (utile per dashboard)
-- ---------------------------------------------------------------------------
CREATE OR REPLACE VIEW nexus_agent_performance AS
SELECT
    q.agent_key,
    q.task_type,
    q.q_value,
    q.visit_count,
    q.success_count,
    q.failure_count,
    CASE WHEN q.visit_count > 0
         THEN ROUND((q.success_count::REAL / q.visit_count * 100)::NUMERIC, 1)
         ELSE NULL
    END AS success_rate_pct,
    q.avg_reward,
    q.updated_at
FROM nexus_q_values q
ORDER BY q.q_value DESC;

COMMENT ON VIEW nexus_agent_performance IS
    'Vista aggregata Q-values per monitoring dashboard.';
