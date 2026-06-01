-- 0243_code_graph.sql
-- M13 (impact analysis) — Code graph dedicato + registro impact run.
--
-- Grafo file-level per progetto: nodi=file, archi=import (strutturale) o
-- semantico (Qdrant). Usato dall'impact analysis (crates/mcp-core/src/knowledge/
-- code_graph.rs + impact.rs) per la closure transitiva e dal regression gate.
-- project_code_tests mappa test->codice; project_impact_runs registra ogni run
-- con seed/impact/gate_status (letto da agent_types.rs per skip-commit).
-- Tabelle di runtime (nessun seed). Ricostruzione fedele. Idempotente.

CREATE TABLE IF NOT EXISTS project_code_nodes (
    project_id   uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_path    text NOT NULL,
    lang         text,
    content_hash text,
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT project_code_nodes_pkey PRIMARY KEY (project_id, file_path)
);
CREATE INDEX IF NOT EXISTS idx_pcn_project ON project_code_nodes USING btree (project_id);

CREATE TABLE IF NOT EXISTS project_code_edges (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_path  text NOT NULL,
    to_path    text NOT NULL,
    edge_kind  text NOT NULL,
    weight     real NOT NULL DEFAULT 1.0,
    source     text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT project_code_edges_pkey PRIMARY KEY (project_id, from_path, to_path, edge_kind),
    CONSTRAINT project_code_edges_edge_kind_check CHECK (edge_kind IN ('import', 'semantic')),
    CONSTRAINT project_code_edges_source_check CHECK (source IN ('structural', 'qdrant'))
);
CREATE INDEX IF NOT EXISTS idx_pce_from ON project_code_edges USING btree (project_id, from_path);
CREATE INDEX IF NOT EXISTS idx_pce_to   ON project_code_edges USING btree (project_id, to_path);

CREATE TABLE IF NOT EXISTS project_code_tests (
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    test_path   text NOT NULL,
    covers_path text NOT NULL,
    method      text NOT NULL,
    confidence  real NOT NULL DEFAULT 0.6,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT project_code_tests_pkey PRIMARY KEY (project_id, test_path, covers_path),
    CONSTRAINT project_code_tests_method_check CHECK (method IN ('naming', 'import', 'cochange', 'manual'))
);
CREATE INDEX IF NOT EXISTS idx_pct_covers ON project_code_tests USING btree (project_id, covers_path);

CREATE TABLE IF NOT EXISTS project_impact_runs (
    id                     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id                 uuid,
    change_request_note_id uuid,
    project_id             uuid,
    seed_paths             text[],
    impact_paths           jsonb,
    gate_status            text,
    created_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_pir_run_id ON project_impact_runs USING btree (run_id);
CREATE INDEX IF NOT EXISTS idx_pir_project ON project_impact_runs USING btree (project_id, created_at DESC);
