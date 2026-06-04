-- ADR 0020: Build graph derivato automaticamente dai config di progetto.
--
-- Tabella cache che persiste, per ogni progetto, la mappa autoritativa di
-- quali path sono nel build graph (include/exclude glob, entry point,
-- monorepo members, directory generate). I dati sono derivati dal parsing
-- dei file di config (tsconfig.json, Cargo.toml, pyproject.toml, go.mod)
-- da resolver per linguaggio. La cache e' invalidata quando uno dei file
-- in `sources` cambia (via wiki::watcher esteso) oppure quando scade il
-- TTL (default 600s).
--
-- Sostituisce ADR 0019 L1 (preflight grep) + L2 (directory policy DB).

CREATE TABLE IF NOT EXISTS nexus_project_build_graph (
    project_id        UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    language          TEXT NOT NULL,
    include_globs     JSONB NOT NULL DEFAULT '[]'::jsonb,
    exclude_globs     JSONB NOT NULL DEFAULT '[]'::jsonb,
    entry_points      JSONB NOT NULL DEFAULT '[]'::jsonb,
    monorepo_members  JSONB NOT NULL DEFAULT '[]'::jsonb,
    generated_dirs    JSONB NOT NULL DEFAULT '[]'::jsonb,
    sources           JSONB NOT NULL DEFAULT '[]'::jsonb,
    computed_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ttl_secs          INT NOT NULL DEFAULT 600
);

CREATE INDEX IF NOT EXISTS idx_pbg_language ON nexus_project_build_graph (language);

INSERT INTO settings (key, value) VALUES
  ('agent.build_graph.default_ttl_secs', '600'),
  ('agent.build_graph.refresh_on_watcher', 'true'),
  ('agent.build_graph.warn_on_unknown', 'true')
ON CONFLICT (key) DO NOTHING;
