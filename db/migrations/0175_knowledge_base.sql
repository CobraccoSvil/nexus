-- ==========================================================================
-- 0175_knowledge_base.sql
-- Knowledge Base per-progetto (Obsidian-compatible)
-- ==========================================================================

-- ── Tabella principale: note ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS project_knowledge_notes (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  source_run_id UUID NULL REFERENCES agent_runs(id) ON DELETE SET NULL,
  source_message_id UUID NULL REFERENCES chat_messages(id) ON DELETE SET NULL,
  intent TEXT NULL,
  title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
  body_md TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'draft'
    CHECK (status IN ('draft','active','archived','deprecated')),
  qdrant_point_id TEXT NULL,
  tags TEXT[] NOT NULL DEFAULT '{}',
  file_paths TEXT[] NOT NULL DEFAULT '{}',
  vault_file_path TEXT NULL,
  vault_file_hash TEXT NULL,
  access_count INT NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  last_accessed_at TIMESTAMPTZ NULL
);

CREATE INDEX IF NOT EXISTS idx_pkn_project_status_intent
  ON project_knowledge_notes(project_id, status, intent);
CREATE INDEX IF NOT EXISTS idx_pkn_tags_gin
  ON project_knowledge_notes USING GIN (tags);
CREATE INDEX IF NOT EXISTS idx_pkn_file_paths_gin
  ON project_knowledge_notes USING GIN (file_paths);
CREATE INDEX IF NOT EXISTS idx_pkn_fts
  ON project_knowledge_notes
  USING GIN (to_tsvector('simple', coalesce(title,'') || ' ' || coalesce(body_md,'')));
CREATE UNIQUE INDEX IF NOT EXISTS idx_pkn_msg_unique
  ON project_knowledge_notes(source_message_id)
  WHERE source_message_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_pkn_vault_path_unique
  ON project_knowledge_notes(project_id, vault_file_path)
  WHERE vault_file_path IS NOT NULL;

-- ── Link tra note ─────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS project_knowledge_links (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  from_note_id UUID NOT NULL REFERENCES project_knowledge_notes(id) ON DELETE CASCADE,
  to_note_id   UUID NOT NULL REFERENCES project_knowledge_notes(id) ON DELETE CASCADE,
  rel_type TEXT NOT NULL CHECK (rel_type IN
    ('followup','correction','refinement','duplicate','blocks','blocked_by','relates')),
  created_by TEXT NOT NULL CHECK (created_by IN ('auto','user')),
  confidence REAL NOT NULL DEFAULT 1.0 CHECK (confidence BETWEEN 0 AND 1),
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CHECK (from_note_id <> to_note_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_pkl_triplet
  ON project_knowledge_links(from_note_id, to_note_id, rel_type);
CREATE INDEX IF NOT EXISTS idx_pkl_to
  ON project_knowledge_links(to_note_id);

-- ── Tag index per progetto ────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS project_knowledge_tags (
  project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  tag TEXT NOT NULL,
  note_count INT NOT NULL DEFAULT 0,
  last_used_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (project_id, tag)
);

-- ── Settings (soglie e flag configurabili) ────────────────────────────────
INSERT INTO settings (key, value) VALUES
  ('knowledge.similarity_banner_threshold', '0.80'),
  ('knowledge.autolink_threshold',         '0.65'),
  ('knowledge.cleanup_draft_days',         '30'),
  ('knowledge.link_worker_interval_secs',  '600'),
  ('knowledge.commit_vault_to_git',        'false'),
  ('knowledge.vault_watcher_debounce_ms',  '500')
ON CONFLICT (key) DO NOTHING;
