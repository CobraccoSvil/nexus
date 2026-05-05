-- Aggiunge il percorso radice del repository al progetto.
-- Usato da mcp-core per risolvere path relativi e operazioni git.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS repository_root_path TEXT;
