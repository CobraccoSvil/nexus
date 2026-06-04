-- Migrazione 0287 — ADR 0016 Fase A.5: cache tool_result via Postgres.
--
-- Backing della cache tool_result cross-turn. Key = sha256(tool_name + canonical_args).
-- TTL via expires_at + cleanup job (vedi knowledge_workers per pattern).
-- Lookup hot path: idx_btree su key, prune via idx_btree su expires_at.

CREATE TABLE IF NOT EXISTS agent_tool_result_cache (
    cache_key       TEXT PRIMARY KEY,
    tool_name       TEXT NOT NULL,
    payload         TEXT NOT NULL,
    payload_bytes   INTEGER NOT NULL,
    hit_count       INTEGER NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_hit_at     TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_tool_result_cache_expires
    ON agent_tool_result_cache (expires_at);

CREATE INDEX IF NOT EXISTS idx_agent_tool_result_cache_tool
    ON agent_tool_result_cache (tool_name, created_at DESC);
