-- Migrazione 0284: VIEW unificata wiki_docs (read-only).
--
-- Espone meta-docs e KB di progetto su un modello comune per la UI wiki.
-- Le tabelle base restano i custodi delle scritture (le route specifiche
-- continuano a scriverle direttamente); la vista serve a query "scope-agnostic"
-- come quelle del frontend wiki (`/api/wiki/list`, search, tree).
--
-- Normalizza i nomi divergenti:
--   meta: kind=text, no status/intent, source_files come file_paths
--   KB:   kind+intent+status, file_paths
--
-- Sola lettura: non e' updatable in modo automatico (UNION ALL). Gli INSERT/
-- UPDATE/DELETE vanno fatti sulle tabelle base (lo capisce gia' il backend
-- attraverso `DocScope::base_table()`).
CREATE OR REPLACE VIEW wiki_docs AS
  SELECT
      'meta'::text                    AS scope,
      d.id,
      NULL::uuid                      AS project_id,
      d.kind,
      NULL::text                      AS intent,
      NULL::text                      AS status,
      d.title,
      d.slug,
      d.body_md,
      d.tags,
      COALESCE(d.source_files, '{}'::text[]) AS file_paths,
      d.vault_file_path,
      d.vault_file_hash,
      d.auto_generated,
      d.manually_edited,
      d.edit_lock,
      d.protected_sections,
      d.generated_hash,
      d.edited_hash,
      d.last_generated_at,
      d.last_edited_at,
      d.edited_by,
      d.current_version,
      d.qdrant_point_id,
      d.created_at,
      d.updated_at
  FROM nexus_meta_docs d
  UNION ALL
  SELECT
      'project'::text                 AS scope,
      n.id,
      n.project_id,
      n.kind,
      n.intent,
      n.status,
      n.title,
      NULL::text                      AS slug,
      n.body_md,
      n.tags,
      n.file_paths,
      n.vault_file_path,
      n.vault_file_hash,
      (n.kind <> 'manual')            AS auto_generated,
      n.manually_edited,
      n.edit_lock,
      n.protected_sections,
      n.generated_hash,
      n.edited_hash,
      n.last_generated_at,
      n.last_edited_at,
      n.edited_by,
      n.current_version,
      n.qdrant_point_id,
      n.created_at,
      n.updated_at
  FROM project_knowledge_notes n
  WHERE n.archived_at IS NULL;
