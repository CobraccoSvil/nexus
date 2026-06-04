-- Migrazione 0283: fase MIGRATE dell'unificazione wiki.
--
-- Backfill idempotente:
--   * inizializza generated_hash sul body corrente dei doc gia' esistenti
--     (proxy: stato "ultimo generato" salvo doc curati a mano);
--   * marca i doc gia' curati manualmente (auto_generated=FALSE per meta,
--     kind='manual' per KB) come manually_edited + edit_lock='protected', cosi'
--     la prima rigenerazione post-deploy non cancella il lavoro esistente;
--   * crea la revisione baseline (version_no=1) per ogni doc, dove non gia'
--     presente.
--
-- Tutte le operazioni sono no-op se rieseguite (WHERE ... IS NULL, NOT EXISTS).
-- pgcrypto/digest() sono gia' disponibili (verificato pre-applicazione).

-- ── 1. Inizializza generated_hash ─────────────────────────────────────────
UPDATE nexus_meta_docs
   SET generated_hash    = encode(digest(body_md, 'sha256'), 'hex'),
       last_generated_at = COALESCE(last_generated_at, updated_at)
 WHERE generated_hash IS NULL;

UPDATE project_knowledge_notes
   SET generated_hash    = encode(digest(body_md, 'sha256'), 'hex'),
       last_generated_at = COALESCE(last_generated_at, updated_at)
 WHERE generated_hash IS NULL;

-- ── 2. Marca i doc gia' curati a mano come protected ─────────────────────
UPDATE nexus_meta_docs
   SET manually_edited = TRUE,
       edit_lock       = 'protected',
       edited_hash     = encode(digest(body_md, 'sha256'), 'hex'),
       last_edited_at  = COALESCE(last_edited_at, updated_at)
 WHERE auto_generated = FALSE AND manually_edited = FALSE;

UPDATE project_knowledge_notes
   SET manually_edited = TRUE,
       edit_lock       = 'protected',
       edited_hash     = encode(digest(body_md, 'sha256'), 'hex'),
       last_edited_at  = COALESCE(last_edited_at, updated_at)
 WHERE kind = 'manual' AND manually_edited = FALSE;

-- ── 3. Snapshot iniziale come version_no = 1 ─────────────────────────────
INSERT INTO wiki_doc_revisions
    (scope, doc_id, project_id, version_no, title, body_md, body_hash, tags, source, author, edit_summary, created_at)
SELECT 'meta', d.id, NULL, 1, d.title, d.body_md,
       encode(digest(d.body_md,'sha256'),'hex'), d.tags,
       CASE WHEN d.auto_generated THEN 'auto' ELSE 'manual' END,
       NULL, 'initial snapshot (backfill 0283)', d.created_at
  FROM nexus_meta_docs d
 WHERE NOT EXISTS (
     SELECT 1 FROM wiki_doc_revisions r
      WHERE r.scope = 'meta' AND r.doc_id = d.id AND r.version_no = 1);

INSERT INTO wiki_doc_revisions
    (scope, doc_id, project_id, version_no, title, body_md, body_hash, tags, source, author, edit_summary, created_at)
SELECT 'project', n.id, n.project_id, 1, n.title, n.body_md,
       encode(digest(n.body_md,'sha256'),'hex'), n.tags,
       CASE WHEN n.kind = 'manual' THEN 'manual' ELSE 'auto' END,
       NULL, 'initial snapshot (backfill 0283)', n.created_at
  FROM project_knowledge_notes n
 WHERE NOT EXISTS (
     SELECT 1 FROM wiki_doc_revisions r
      WHERE r.scope = 'project' AND r.doc_id = n.id AND r.version_no = 1);

-- ── 4. Allinea current_version sulle tabelle base ────────────────────────
UPDATE nexus_meta_docs d
   SET current_version = 1
 WHERE current_version < 1 OR current_version IS NULL;

UPDATE project_knowledge_notes n
   SET current_version = 1
 WHERE current_version < 1 OR current_version IS NULL;
