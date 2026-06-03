-- 0242_code_graph.sql
--
-- M13.1 del piano "Impact analysis": fondazione del code graph.
--
-- Indicizza le dipendenze strutturali (import intra-progetto) tra i file di un
-- progetto, in modo da poter calcolare in seguito cosa una modifica impatta
-- (forward closure: chi importa X). Read-only sul comportamento dell'agente:
-- questa milestone SOLO popola il grafo, non lo usa ancora.
--
-- Le tabelle project_code_tests e project_impact_runs sono create ora ma
-- popolate piu' avanti (M13.3 / M13.4-5): qui solo lo schema.
--
-- Idempotente: CREATE TABLE IF NOT EXISTS + ON CONFLICT DO NOTHING sui settings.

-- ── Nodi: un file di codice indicizzato ───────────────────────────────────────
CREATE TABLE IF NOT EXISTS project_code_nodes (
    project_id   UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_path    TEXT        NOT NULL,
    lang         TEXT,
    content_hash TEXT,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, file_path)
);

-- ── Archi: dipendenza file -> file ────────────────────────────────────────────
-- edge_kind 'import'   = dipendenza strutturale estratta dal parser import.
-- edge_kind 'semantic' = vicinanza semantica (popolata da Qdrant in milestone
--                        successive, source='qdrant').
CREATE TABLE IF NOT EXISTS project_code_edges (
    project_id UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_path  TEXT        NOT NULL,
    to_path    TEXT        NOT NULL,
    edge_kind  TEXT        NOT NULL CHECK (edge_kind IN ('import', 'semantic')),
    weight     REAL        NOT NULL DEFAULT 1.0,
    source     TEXT        NOT NULL CHECK (source IN ('structural', 'qdrant')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, from_path, to_path, edge_kind)
);

-- ── Mappatura test -> file coperti (popolata in M13.3) ─────────────────────────
CREATE TABLE IF NOT EXISTS project_code_tests (
    project_id  UUID        NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    test_path   TEXT        NOT NULL,
    covers_path TEXT        NOT NULL,
    method      TEXT        NOT NULL CHECK (method IN ('naming', 'import', 'cochange', 'manual')),
    confidence  REAL        NOT NULL DEFAULT 0.6,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, test_path, covers_path)
);

-- ── Run di impact analysis (popolata in M13.4 / M13.5) ─────────────────────────
CREATE TABLE IF NOT EXISTS project_impact_runs (
    id                     UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id                 UUID,
    change_request_note_id UUID,
    project_id             UUID,
    seed_paths             TEXT[],
    impact_paths           JSONB,
    gate_status            TEXT,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Indici ─────────────────────────────────────────────────────────────────────
-- Forward closure (chi importa X): lookup per to_path.
CREATE INDEX IF NOT EXISTS idx_pce_to ON project_code_edges (project_id, to_path);
-- Outgoing edges di un file (cleanup/aggiornamento edge stale).
CREATE INDEX IF NOT EXISTS idx_pce_from ON project_code_edges (project_id, from_path);
CREATE INDEX IF NOT EXISTS idx_pcn_project ON project_code_nodes (project_id);
CREATE INDEX IF NOT EXISTS idx_pct_covers ON project_code_tests (project_id, covers_path);
CREATE INDEX IF NOT EXISTS idx_pir_project ON project_impact_runs (project_id, created_at DESC);

-- ── Settings di controllo ──────────────────────────────────────────────────────
-- Regola G (CLAUDE.md): soglie nel DB, nessun fallback hardcoded nel codice.
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('impact.enabled', 'true', 'impact', 'Abilita il popolamento del code graph durante reindex_single_file (M13.1).', FALSE),
    ('impact.depth_cap', '2', 'impact', 'Profondita'' massima di traversal nella forward closure dell''impact analysis (M13.4).', FALSE),
    ('impact.max_nodes', '60', 'impact', 'Numero massimo di nodi raccolti in una singola impact run (anti-esplosione).', FALSE)
ON CONFLICT (key) DO NOTHING;
