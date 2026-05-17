-- PR-3 (Codex pattern) — AGENTS.md / CLAUDE.md / .cursorrules analogo per Nexus.
-- Il file `.nexus/project-instructions.md` vive nel progetto utente; il router_node
-- lo carica e lo inietta nel system_text di ogni run su quel progetto.
--
-- Cache via `content_cache` + `content_hash` per evitare FS hit ad ogni invocazione;
-- invalidata dal file watcher (project_instructions.rs Rust side).
CREATE TABLE IF NOT EXISTS nexus_project_instructions (
    project_id    UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    file_path     TEXT NOT NULL DEFAULT '.nexus/project-instructions.md',
    content_cache TEXT,
    content_hash  TEXT,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_npi_updated_at ON nexus_project_instructions(updated_at DESC);

COMMENT ON TABLE nexus_project_instructions IS
  'Cache del file <project>/.nexus/project-instructions.md (pattern AGENTS.md). '
  'Iniettato nel system_text di ogni agent run sul progetto. Auto-aggiornato dal '
  'file watcher Rust quando il file su FS cambia (content_hash verifica).';

-- Setting per controllo runtime.
INSERT INTO settings (key, value, updated_at) VALUES
    ('orchestrator.project_instructions_file', '.nexus/project-instructions.md', NOW()),
    ('orchestrator.project_instructions_max_chars', '8000', NOW())
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value, updated_at = NOW()
WHERE settings.value IS DISTINCT FROM EXCLUDED.value;
