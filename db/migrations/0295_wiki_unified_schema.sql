-- Migrazione 0295 — Knowledge Graph unificato (ADR 0017 v2).
--
-- Obiettivo: una sola tabella documenti (`wiki_docs`) discriminata da colonna
-- `scope IN ('meta','project')`, una sola tabella link (`wiki_links`), una
-- sola tabella triple semantiche (`wiki_concept_triples`) e versioning
-- polimorfico in `wiki_doc_revisions`. Tutto il codice (storage, link, triple,
-- worker, search, UI) cessa di distinguere meta da project: la differenza
-- vive solo nel valore della colonna `scope` + middleware ACL `WikiAcl`.
--
-- Migrazione DESTRUCTIVE (autorizzata esplicitamente dall'utente 2026-06-04):
--   - Drop di `nexus_meta_docs`, `nexus_meta_doc_links`, `nexus_meta_doc_changes`,
--     `project_knowledge_notes`, `project_knowledge_links`, `project_knowledge_tags`
--     e della VIEW v1 `wiki_docs` (se esiste).
--   - I dati attuali (356 doc + 2.620 link + 317 vettori Qdrant) sono perdibili e
--     verranno rigenerati da:
--       * vault Markdown (`docs/.nexus-vault/` per scope=meta,
--         `<project_root>/.nexus-vault/` per scope=project) tramite worker
--         `wiki_reingest` (fase F3).
--       * recompute-links worker (fase F4).
--       * triple extractor LLM (fase F5).
--   - Backup forensico pre-mig:
--     `backups/postgres/wiki_pre_unification_20260604_1455.sql.gz`.
--
-- Idempotenza:
--   - DROP ... IF EXISTS + CREATE EXTENSION IF NOT EXISTS + CREATE TABLE
--     IF NOT EXISTS + CREATE INDEX IF NOT EXISTS rendono la migrazione
--     applicabile piu' volte senza errore. Al secondo run le DROP sono no-op
--     (le tabelle vecchie non esistono piu') e le CREATE saltano perche'
--     le nuove tabelle gia' presenti.
--
-- Dipendenze:
--   - Estensione `pgcrypto` (per `gen_random_uuid()`): gia' presente nel DB
--     Nexus dalle migrazioni iniziali.
--   - Estensione `pg_trgm` (per `gin_trgm_ops`): creata qui se mancante.
--   - Tabella `projects(id)`: gia' presente, FK CASCADE.

BEGIN;

-- =============================================================================
-- Step 1: Drop vecchie tabelle (ordine: foglie prima delle radici per le FK).
-- =============================================================================

DROP TABLE IF EXISTS project_knowledge_tags  CASCADE;
DROP TABLE IF EXISTS project_knowledge_links CASCADE;
DROP TABLE IF EXISTS project_knowledge_notes CASCADE;
DROP TABLE IF EXISTS nexus_meta_doc_links    CASCADE;
DROP TABLE IF EXISTS nexus_meta_doc_changes  CASCADE;
DROP TABLE IF EXISTS nexus_meta_docs         CASCADE;

-- v1 dell'ADR 0017 aveva creato `wiki_docs` come VIEW UNION ALL sulle due
-- tabelle legacy: ora diventa tabella reale. Il DROP deve essere robusto
-- rispetto allo stato in cui `wiki_docs` puo' essere VIEW oppure TABLE
-- (idempotenza: una rerun della mig su DB gia' migrato deve riuscire).
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_class WHERE relname = 'wiki_docs' AND relkind = 'v') THEN
        EXECUTE 'DROP VIEW IF EXISTS wiki_docs CASCADE';
    END IF;
END$$;
DROP TABLE IF EXISTS wiki_doc_revisions  CASCADE;
DROP TABLE IF EXISTS wiki_concept_triples CASCADE;
DROP TABLE IF EXISTS wiki_links          CASCADE;
DROP TABLE IF EXISTS wiki_docs           CASCADE;

-- =============================================================================
-- Step 2: Estensione pg_trgm per indici GIN trigram su title/obj_text.
-- =============================================================================

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- =============================================================================
-- Step 3: Tabella `wiki_docs` (sostituisce nexus_meta_docs +
--         project_knowledge_notes).
-- =============================================================================

CREATE TABLE IF NOT EXISTS wiki_docs (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope               TEXT NOT NULL CHECK (scope IN ('meta','project')),
    project_id          UUID REFERENCES projects(id) ON DELETE CASCADE,
    slug                TEXT NOT NULL,
    title               TEXT NOT NULL,
    body_md             TEXT NOT NULL DEFAULT '',
    body_hash           TEXT,                            -- sha256(body_md)
    kind                TEXT NOT NULL,                   -- adr|note|runbook|architecture|api|changelog|concept|decision
    intent              TEXT,                            -- legacy per note progetti (debug|todo|reflection|...)
    tags                TEXT[] NOT NULL DEFAULT '{}',
    vault_file_path     TEXT,                            -- relativo al vault dello scope
    qdrant_point_id     TEXT,
    edit_lock           TEXT NOT NULL DEFAULT 'none'
                          CHECK (edit_lock IN ('none','protected','frozen')),
    protected_sections  TEXT[] NOT NULL DEFAULT '{}',
    manually_edited     BOOLEAN NOT NULL DEFAULT FALSE,
    generated_hash      TEXT,                            -- hash ultima auto-generazione
    edited_hash         TEXT,                            -- hash ultima edit manuale
    last_generated_at   TIMESTAMPTZ,
    last_edited_at      TIMESTAMPTZ,
    edited_by           TEXT,                            -- email o agent name
    current_version     INT  NOT NULL DEFAULT 1,
    auto_generated      BOOLEAN NOT NULL DEFAULT FALSE,
    public_read         BOOLEAN NOT NULL DEFAULT FALSE,  -- meta-doc consultabile da tutti i progetti
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Vincolo cardine: project_id obbligatorio se scope=project, vietato se scope=meta
    CONSTRAINT scope_project_consistency CHECK (
        (scope = 'project' AND project_id IS NOT NULL)
        OR
        (scope = 'meta'    AND project_id IS NULL)
    ),
    -- public_read ha senso solo per scope=meta
    CONSTRAINT public_read_meta_only CHECK (
        public_read = FALSE OR scope = 'meta'
    )
);

-- Slug unico per (scope, project_id): i progetti possono avere stesso slug,
-- meta e' globale. COALESCE su stringa vuota per gestire NULL nei meta.
CREATE UNIQUE INDEX IF NOT EXISTS uq_wiki_docs_slug
    ON wiki_docs (scope, COALESCE(project_id::text, ''), slug);

CREATE INDEX IF NOT EXISTS idx_wiki_docs_scope    ON wiki_docs (scope);
CREATE INDEX IF NOT EXISTS idx_wiki_docs_project  ON wiki_docs (project_id)
    WHERE scope = 'project';
CREATE INDEX IF NOT EXISTS idx_wiki_docs_kind     ON wiki_docs (kind);
CREATE INDEX IF NOT EXISTS idx_wiki_docs_tags     ON wiki_docs USING gin (tags);
CREATE INDEX IF NOT EXISTS idx_wiki_docs_updated  ON wiki_docs (updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_wiki_docs_title_trgm
    ON wiki_docs USING gin (title gin_trgm_ops);

-- =============================================================================
-- Step 4: Tabella `wiki_links` (sostituisce nexus_meta_doc_links +
--         project_knowledge_links + cross-scope custom).
-- =============================================================================

CREATE TABLE IF NOT EXISTS wiki_links (
    from_doc_id  UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    to_doc_id    UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    rel_type     TEXT NOT NULL DEFAULT 'relates',
    confidence   REAL NOT NULL DEFAULT 1.0,
    created_by   TEXT NOT NULL DEFAULT 'auto',
    evidence     TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (from_doc_id, to_doc_id, rel_type),
    CONSTRAINT wiki_links_no_self CHECK (from_doc_id <> to_doc_id),
    CONSTRAINT wiki_links_rel_type_check CHECK (rel_type IN (
        'relates','supersedes','depends_on','illustrates','contradicts',
        'followup','correction_of','refines','duplicate_of',
        'blocks','blocked_by','mentions','implements','tests'
    )),
    CONSTRAINT wiki_links_created_by_check CHECK (created_by IN (
        'auto','user','agent','llm','external'
    ))
);

CREATE INDEX IF NOT EXISTS idx_wiki_links_from
    ON wiki_links (from_doc_id, confidence DESC);
CREATE INDEX IF NOT EXISTS idx_wiki_links_to
    ON wiki_links (to_doc_id);
CREATE INDEX IF NOT EXISTS idx_wiki_links_predicate
    ON wiki_links (rel_type, confidence DESC);

-- =============================================================================
-- Step 5: Tabella `wiki_concept_triples` (knowledge graph reale).
-- =============================================================================

CREATE TABLE IF NOT EXISTS wiki_concept_triples (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subj_doc_id   UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    predicate     TEXT NOT NULL,
    obj_doc_id    UUID REFERENCES wiki_docs(id) ON DELETE CASCADE,
    obj_text      TEXT,                       -- concept libero ("RAG pipeline", "OAuth flow")
    obj_external  TEXT,                       -- URL o riferimento esterno
    source        TEXT NOT NULL,              -- wikilink|semantic|llm|user|agent|external
    confidence    REAL NOT NULL DEFAULT 0.5,
    evidence      TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- L'oggetto e' uno e uno solo: doc, concetto libero, o riferimento esterno.
    CONSTRAINT triple_obj_one CHECK (
        (obj_doc_id   IS NOT NULL)::int +
        (obj_text     IS NOT NULL)::int +
        (obj_external IS NOT NULL)::int = 1
    ),
    CONSTRAINT triple_predicate_check CHECK (predicate IN (
        'relates','supersedes','depends_on','illustrates','contradicts',
        'followup','correction_of','refines','duplicate_of',
        'blocks','blocked_by','mentions','implements','tests'
    )),
    CONSTRAINT triple_source_check CHECK (source IN (
        'wikilink','semantic','llm','user','agent','external'
    ))
);

CREATE INDEX IF NOT EXISTS idx_wct_subj
    ON wiki_concept_triples (subj_doc_id);
CREATE INDEX IF NOT EXISTS idx_wct_obj_doc
    ON wiki_concept_triples (obj_doc_id)
    WHERE obj_doc_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_wct_predicate
    ON wiki_concept_triples (predicate, confidence DESC);
CREATE INDEX IF NOT EXISTS idx_wct_obj_text_trgm
    ON wiki_concept_triples USING gin (obj_text gin_trgm_ops)
    WHERE obj_text IS NOT NULL;

-- =============================================================================
-- Step 6: Tabella `wiki_doc_revisions` (versioning unificato).
-- =============================================================================

CREATE TABLE IF NOT EXISTS wiki_doc_revisions (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_id        UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    version_no    INT  NOT NULL,
    title         TEXT NOT NULL,
    body_md       TEXT NOT NULL,
    body_hash     TEXT NOT NULL,
    tags          TEXT[] NOT NULL DEFAULT '{}',
    source        TEXT NOT NULL CHECK (source IN ('auto','manual','import','revert')),
    author        TEXT,
    edit_summary  TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (doc_id, version_no)
);

CREATE INDEX IF NOT EXISTS idx_wdr_doc_version
    ON wiki_doc_revisions (doc_id, version_no DESC);

COMMIT;
