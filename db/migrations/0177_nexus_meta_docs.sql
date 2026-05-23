-- Migrazione meta-docs vault: documentazione del meta-progetto Nexus
-- come vault Obsidian-compatible su filesystem + indice DB + collection Qdrant.
--
-- Pattern clonato da Knowledge Base per-progetto (mig 0175).
-- Path vault: docs/.nexus-vault/ dentro la repository Nexus stessa.

-- ----------------------------------------------------------------------------
-- 1. Tabella principale: note del vault
-- ----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS nexus_meta_docs (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind              TEXT NOT NULL CHECK (kind IN
        ('architecture','adr','api','schema','runbook','changelog','decision','other')),
    title             TEXT NOT NULL,
    slug              TEXT NOT NULL,
    body_md           TEXT NOT NULL,
    vault_file_path   TEXT NOT NULL,                       -- relativo a docs/.nexus-vault/
    vault_file_hash   TEXT NOT NULL,                       -- sha256 del file persistito
    source_commit     TEXT NULL,                           -- SHA git che ha generato/aggiornato la nota
    source_files      TEXT[] NOT NULL DEFAULT '{}',        -- file sorgente che hanno influenzato il contenuto
    auto_generated    BOOLEAN NOT NULL DEFAULT TRUE,       -- false = curato manualmente
    tags              TEXT[] NOT NULL DEFAULT '{}',
    qdrant_point_id   TEXT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_nmd_vault_path_unique
    ON nexus_meta_docs(vault_file_path);
CREATE INDEX IF NOT EXISTS idx_nmd_kind
    ON nexus_meta_docs(kind);
CREATE INDEX IF NOT EXISTS idx_nmd_tags_gin
    ON nexus_meta_docs USING GIN (tags);
CREATE INDEX IF NOT EXISTS idx_nmd_source_files_gin
    ON nexus_meta_docs USING GIN (source_files);
CREATE INDEX IF NOT EXISTS idx_nmd_fts
    ON nexus_meta_docs
    USING GIN (to_tsvector('simple', coalesce(title,'') || ' ' || coalesce(body_md,'')));

-- ----------------------------------------------------------------------------
-- 2. Link tra note (wikilink + auto-inferiti)
-- ----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS nexus_meta_doc_links (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    from_doc_id  UUID NOT NULL REFERENCES nexus_meta_docs(id) ON DELETE CASCADE,
    to_doc_id    UUID NOT NULL REFERENCES nexus_meta_docs(id) ON DELETE CASCADE,
    rel_type     TEXT NOT NULL DEFAULT 'relates'
        CHECK (rel_type IN ('relates','supersedes','depends','illustrates','contradicts')),
    created_by   TEXT NOT NULL CHECK (created_by IN ('auto','user')),
    confidence   REAL NOT NULL DEFAULT 1.0 CHECK (confidence BETWEEN 0 AND 1),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (from_doc_id <> to_doc_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_nmdl_triplet
    ON nexus_meta_doc_links(from_doc_id, to_doc_id, rel_type);
CREATE INDEX IF NOT EXISTS idx_nmdl_to
    ON nexus_meta_doc_links(to_doc_id);

-- ----------------------------------------------------------------------------
-- 3. Cronologia commit processati per evitare ri-processi
-- ----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS nexus_meta_doc_changes (
    id                BIGSERIAL PRIMARY KEY,
    commit_sha        TEXT NOT NULL,
    commit_msg        TEXT NOT NULL DEFAULT '',
    author            TEXT NULL,
    files_changed     TEXT[] NOT NULL DEFAULT '{}',
    significance      REAL NOT NULL DEFAULT 0.5 CHECK (significance BETWEEN 0 AND 1),
    generated_doc_id  UUID NULL REFERENCES nexus_meta_docs(id) ON DELETE SET NULL,
    processed_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_nmdc_commit_sha
    ON nexus_meta_doc_changes(commit_sha);
CREATE INDEX IF NOT EXISTS idx_nmdc_processed
    ON nexus_meta_doc_changes(processed_at DESC);

-- ----------------------------------------------------------------------------
-- 4. Tabella di supporto per ChangeDrafter: draft di modifica in attesa di approvazione
-- ----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS change_drafts (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id          UUID NULL REFERENCES projects(id) ON DELETE CASCADE,  -- NULL = meta-Nexus
    requested_by_user   UUID NULL,
    trigger_kind        TEXT NOT NULL CHECK (trigger_kind IN
        ('user_chat','autofix','review','manual','sub_agent')),
    summary             TEXT NOT NULL DEFAULT '',
    draft_json          JSONB NOT NULL,
    status              TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','approved','rejected','applied','superseded','dismissed')),
    applied_at          TIMESTAMPTZ NULL,
    related_commit_sha  TEXT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_change_drafts_status
    ON change_drafts(status);
CREATE INDEX IF NOT EXISTS idx_change_drafts_project
    ON change_drafts(project_id);
CREATE INDEX IF NOT EXISTS idx_change_drafts_created
    ON change_drafts(created_at DESC);

-- ----------------------------------------------------------------------------
-- 5. Tabella per i run E2E smoke di NexusE2eTester
-- ----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS nexus_e2e_runs (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scenario          TEXT NOT NULL,             -- es. 'chat_send_message', 'compact_session', 'knowledge_panel_responsive'
    status            TEXT NOT NULL CHECK (status IN ('passed','failed','error','skipped')),
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    artifact_path     TEXT NULL,                  -- path screenshot/video locale
    log_excerpt       TEXT NULL,
    failed_assertion  TEXT NULL,
    started_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at      TIMESTAMPTZ NULL
);

CREATE INDEX IF NOT EXISTS idx_e2e_runs_status
    ON nexus_e2e_runs(status);
CREATE INDEX IF NOT EXISTS idx_e2e_runs_started
    ON nexus_e2e_runs(started_at DESC);

-- ----------------------------------------------------------------------------
-- 6. Settings di configurazione (no env hardcoded)
-- ----------------------------------------------------------------------------

INSERT INTO settings (key, value, category, description) VALUES
    ('meta_docs.enabled',                    'true',                 'meta_docs', 'Abilita la generazione documentazione meta-progetto'),
    ('meta_docs.vault_path',                 'docs/.nexus-vault',    'meta_docs', 'Path relativo del vault dentro la repository Nexus'),
    ('meta_docs.changelog_min_significance', '0.4',                  'meta_docs', 'Soglia di significance LLM per generare entry changelog'),
    ('meta_docs.refresh_worker_interval_secs','900',                 'meta_docs', 'Failsafe refresh ogni N secondi (default 15 min)'),
    ('meta_docs.autofix_enabled',            'true',                 'meta_docs', 'Abilita NexusAutoFixAgent'),
    ('meta_docs.autofix_target_branch',      'main',                 'meta_docs', 'Branch base per le PR di autofix'),
    ('meta_docs.e2e_smoke_url',              'http://localhost:3000','meta_docs', 'URL base per smoke test E2E di Nexus stesso'),
    ('meta_docs.e2e_smoke_cron',             '0 2 * * *',            'meta_docs', 'Cron schedule per smoke test notturno'),
    ('meta_docs.watcher_debounce_ms',        '500',                  'meta_docs', 'Debounce file watcher su docs/.nexus-vault/')
ON CONFLICT (key) DO NOTHING;

-- ----------------------------------------------------------------------------
-- 7. Purpose model entries per i nuovi task LLM (no nomi modello hardcoded)
-- Riferimento: nexus_purpose_model gestisce il routing per task interni.
-- Default: openai/gpt-4.1-mini (cheap, veloce); l'admin puo' cambiare via UI.
-- ----------------------------------------------------------------------------

INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('changelog_significance', 'openai',    'gpt-4.1-mini',               'Valuta la significance (0-1) di un commit per la generazione changelog'),
    ('decision_extractor',     'openai',    'gpt-4.1-mini',               'Estrae decisioni di design da conversazioni chat'),
    ('change_drafter',         'anthropic', 'claude-sonnet-4-5-20250929', 'Genera draft strutturato di modifica codice/doc con impact analysis'),
    ('autofix_planner',        'anthropic', 'claude-sonnet-4-5-20250929', 'Pianifica patch automatica a partire da log di test fallito')
ON CONFLICT (purpose) DO NOTHING;
