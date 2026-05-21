-- Migrazione del learning storage dal SQLite locale del brain a PostgreSQL.
-- Sostituisce brain/nexus_memory/learning.db con tabelle condivise.

CREATE TABLE IF NOT EXISTS brain_learning_interactions (
    id             BIGSERIAL PRIMARY KEY,
    thread_id      TEXT        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    task_type      TEXT        NOT NULL,
    behavior_mode  TEXT        NOT NULL DEFAULT 'bilanciata',
    user_input     TEXT        NOT NULL,
    agent_output   TEXT        NOT NULL DEFAULT '',
    provider       TEXT,
    model          TEXT,
    latency_ms     REAL,
    token_usage    INTEGER,
    feedback_score REAL,
    qdrant_id      TEXT,
    metadata       JSONB
);

CREATE INDEX IF NOT EXISTS idx_bli_thread    ON brain_learning_interactions(thread_id);
CREATE INDEX IF NOT EXISTS idx_bli_task_type ON brain_learning_interactions(task_type);
CREATE INDEX IF NOT EXISTS idx_bli_created   ON brain_learning_interactions(created_at DESC);

CREATE TABLE IF NOT EXISTS brain_task_stats (
    task_type      TEXT PRIMARY KEY,
    total_count    INTEGER     NOT NULL DEFAULT 0,
    success_count  INTEGER     NOT NULL DEFAULT 0,
    avg_latency_ms REAL        NOT NULL DEFAULT 0.0,
    avg_feedback   REAL        NOT NULL DEFAULT 0.0,
    last_updated   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
