-- Traccia l'hash SHA256 dei file indicizzati nel vector store
-- Permette di rilevare file modificati dopo l'ultima indicizzazione
CREATE TABLE IF NOT EXISTS file_index_hashes (
    project_id   UUID    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_path    TEXT    NOT NULL,
    content_hash TEXT    NOT NULL,
    indexed_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (project_id, file_path)
);
CREATE INDEX IF NOT EXISTS idx_file_index_hashes_project ON file_index_hashes(project_id);
