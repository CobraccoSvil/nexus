-- Migrazione 0282: fase EXPAND dell'unificazione wiki (meta-docs + KB progetto).
--
-- Additivo e idempotente: ADD COLUMN IF NOT EXISTS, nuova tabella revisioni,
-- nuove settings. Nessun rename, nessun drop. Sicuro su DB con dati e su DB
-- vuoto. Le tabelle base (nexus_meta_docs, project_knowledge_notes) restano i
-- custodi dei dati con le loro FK forti; qui si aggiunge solo il superset di
-- colonne per editing/protezione/versioning e una tabella revisioni
-- polimorfica condivisa dai due scope.
--
-- Vedi piano wiki (docs admin al livello KB progetti + esperienza Confluence).
-- La VIEW unificata wiki_docs e il backfill dati arrivano nelle migrazioni
-- successive (fase unificazione storage / fase protezione).

-- ===========================================================================
-- A. Colonne condivise per EDITING + PROTEZIONE RIGENERAZIONE + VERSIONING.
--    Identiche su entrambe le tabelle.
-- ===========================================================================

ALTER TABLE project_knowledge_notes
  ADD COLUMN IF NOT EXISTS manually_edited    BOOLEAN     NOT NULL DEFAULT FALSE,
  ADD COLUMN IF NOT EXISTS edit_lock          TEXT        NOT NULL DEFAULT 'none'
    CHECK (edit_lock IN ('none','protected','frozen')),
  ADD COLUMN IF NOT EXISTS protected_sections TEXT[]      NOT NULL DEFAULT '{}',
  ADD COLUMN IF NOT EXISTS generated_hash     TEXT        NULL,
  ADD COLUMN IF NOT EXISTS edited_hash        TEXT        NULL,
  ADD COLUMN IF NOT EXISTS last_generated_at  TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS last_edited_at     TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS edited_by          TEXT        NULL,
  ADD COLUMN IF NOT EXISTS current_version    INT         NOT NULL DEFAULT 1;

ALTER TABLE nexus_meta_docs
  ADD COLUMN IF NOT EXISTS manually_edited    BOOLEAN     NOT NULL DEFAULT FALSE,
  ADD COLUMN IF NOT EXISTS edit_lock          TEXT        NOT NULL DEFAULT 'none'
    CHECK (edit_lock IN ('none','protected','frozen')),
  ADD COLUMN IF NOT EXISTS protected_sections TEXT[]      NOT NULL DEFAULT '{}',
  ADD COLUMN IF NOT EXISTS generated_hash     TEXT        NULL,
  ADD COLUMN IF NOT EXISTS edited_hash        TEXT        NULL,
  ADD COLUMN IF NOT EXISTS last_generated_at  TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS last_edited_at     TIMESTAMPTZ NULL,
  ADD COLUMN IF NOT EXISTS edited_by          TEXT        NULL,
  ADD COLUMN IF NOT EXISTS current_version    INT         NOT NULL DEFAULT 1;

-- ===========================================================================
-- B. VERSIONING — tabella polimorfica unica per entrambi gli scope.
--    Storage = full snapshot del body_md (revert atomico, lettura O(1),
--    nessuna catena di diff da ricostruire). La crescita e' contenuta dalla
--    retention (worker, fase successiva) e dalla dedup per body_hash.
--    Nessuna FK fisica su doc_id: i target sono due tabelle distinte (lo scope
--    discrimina). L'integrita' referenziale e' garantita applicativamente.
-- ===========================================================================

CREATE TABLE IF NOT EXISTS wiki_doc_revisions (
    id            BIGSERIAL PRIMARY KEY,
    scope         TEXT NOT NULL CHECK (scope IN ('meta','project')),
    doc_id        UUID NOT NULL,
    project_id    UUID NULL,                 -- copiato per filtro/retention; NULL per meta
    version_no    INT  NOT NULL,
    title         TEXT NOT NULL,
    body_md       TEXT NOT NULL,             -- snapshot completo
    body_hash     TEXT NOT NULL,             -- sha256(body_md), per dedup
    tags          TEXT[] NOT NULL DEFAULT '{}',
    source        TEXT NOT NULL CHECK (source IN ('auto','manual','import','revert')),
    author        TEXT NULL,                 -- user id/email; NULL = generatore
    edit_summary  TEXT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (version_no >= 1),
    CHECK (scope = 'project' OR project_id IS NULL)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_wiki_rev_doc_version
    ON wiki_doc_revisions (scope, doc_id, version_no);
CREATE INDEX IF NOT EXISTS idx_wiki_rev_doc_latest
    ON wiki_doc_revisions (scope, doc_id, version_no DESC);
CREATE INDEX IF NOT EXISTS idx_wiki_rev_retention
    ON wiki_doc_revisions (created_at);
CREATE INDEX IF NOT EXISTS idx_wiki_rev_project
    ON wiki_doc_revisions (project_id) WHERE project_id IS NOT NULL;

-- ===========================================================================
-- C. SETTINGS condivise per versioning + protezione (namespace wiki.*).
-- ===========================================================================

INSERT INTO settings (key, value, category, description, is_secret) VALUES
  ('wiki.versioning_enabled',            'true',  'wiki', 'Abilita lo storico revisioni per i doc wiki (meta + progetto).', FALSE),
  ('wiki.retention_max_versions',        '50',    'wiki', 'Numero massimo di revisioni conservate per doc (0 = illimitato). Le piu vecchie vengono potate, mantenendo sempre le manual.', FALSE),
  ('wiki.retention_max_age_days',        '365',   'wiki', 'Eta massima (giorni) delle revisioni auto prima della potatura. 0 = nessun limite.', FALSE),
  ('wiki.retention_keep_all_manual',     'true',  'wiki', 'Se true, le revisioni con source manual/revert non vengono mai potate.', FALSE),
  ('wiki.protect_manual_edits',          'true',  'wiki', 'Se true, la rigenerazione non sovrascrive doc con modifiche manuali salvo edit_lock=none.', FALSE),
  ('wiki.regen_section_merge',           'true',  'wiki', 'Se true, la rigenerazione fa merge a livello di sezione preservando protected_sections.', FALSE),
  ('wiki.lock_on_external_edit',         'true',  'wiki', 'Se true, un edit esterno fuori dai blocchi manuali porta il doc a edit_lock=protected.', FALSE),
  ('wiki.retention_worker_interval_secs','86400', 'wiki', 'Intervallo del worker di potatura revisioni (default giornaliero).', FALSE)
ON CONFLICT (key) DO NOTHING;
