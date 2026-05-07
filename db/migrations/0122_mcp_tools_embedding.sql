-- 0122_mcp_tools_embedding.sql
-- Ricerca semantica dei tool MCP tramite Qdrant.
--
-- Obiettivo: quando l'agente chiama nexus_mcp_tool_search, invece di un
-- semplice ILIKE %, si usa la similarity coseno tra l'embedding della query
-- e gli embedding pre-calcolati di ogni tool (nome + descrizione).
--
-- Architettura:
--   1. Il campo `embedding_vector` (JSONB) viene usato come cache locale
--      per confronti pg-side in caso Qdrant non sia disponibile.
--   2. La collection Qdrant `mcp_tools` (384D cosine) e' la fonte primaria
--      di ricerca semantica (stessa infrastruttura di prompt_corrections).
--   3. Il processo di indicizzazione e' scatenato:
--      a) Al boot (nexus_builtin::seed_tools_and_server)
--      b) Dopo ogni upsert di tool (mcp_connectors, nexus_builtin, plugins)
--      c) Manualmente via nexus_mcp_tool_reindex

-- Aggiunge campi embedding + hash per invalidazione cache
ALTER TABLE mcp_server_tools
    ADD COLUMN IF NOT EXISTS embedding_hash TEXT,
    ADD COLUMN IF NOT EXISTS embedded_at TIMESTAMPTZ;

-- Index su embedded_at per il worker di re-indicizzazione differenziale
CREATE INDEX IF NOT EXISTS idx_mcp_server_tools_embedded_at
    ON mcp_server_tools (embedded_at NULLS FIRST);

-- Setting: nome collection Qdrant per tool MCP
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'qdrant_mcp_tools_collection',
    'mcp_tools',
    'vector',
    'Nome della collection Qdrant per gli embedding dei tool MCP (nexus_mcp_tool_search semantico).',
    false
)
ON CONFLICT (key) DO NOTHING;

-- Setting: soglia discovery mode (se tool nel catalogo >= N, usa solo i 2 meta-tool)
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'mcp_tool_search_hard_limit',
    '20',
    'optimizer',
    'Numero minimo di tool nel catalogo oltre il quale il prompt usa solo nexus_mcp_tool_search (discovery mode, riduzione token). Default 20.',
    false
)
ON CONFLICT (key) DO NOTHING;

-- Setting: score minimo per risultati semantici (0.0-1.0, default 0.35)
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'mcp_tool_search_min_score',
    '0.35',
    'vector',
    'Score minimo coseno (0-1) per restituire un risultato dalla ricerca semantica tool MCP. Sotto soglia si usa ILIKE come fallback.',
    false
)
ON CONFLICT (key) DO NOTHING;
