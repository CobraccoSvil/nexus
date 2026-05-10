-- Migrazione 0126: rimozione tabelle orfane (nessun riferimento nel codice runtime)
-- Tabelle identificate con audit grep su crates/ brain/ apps/ (2026-05-10)
-- Ordine di DROP rispetta le dipendenze FK: le tabelle figlie vengono droppate prima dei genitori.
-- Uso CASCADE come guardia supplementare contro FK residui.

-- === Cluster parsed_blocks ===
DROP TABLE IF EXISTS parsed_block_steps CASCADE;
DROP TABLE IF EXISTS parsed_blocks       CASCADE;

-- === Cluster applied_changes / fix_suggestions / rollback_points ===
DROP TABLE IF EXISTS rollback_points  CASCADE;
DROP TABLE IF EXISTS applied_changes  CASCADE;
DROP TABLE IF EXISTS fix_suggestions  CASCADE;

-- === reasoning_antipatterns (FK verso reasoning_patterns che è attiva) ===
DROP TABLE IF EXISTS reasoning_antipatterns CASCADE;

-- === Cluster knowledge_bundles ===
DROP TABLE IF EXISTS conflicts                CASCADE;
DROP TABLE IF EXISTS knowledge_sync_runs      CASCADE;
DROP TABLE IF EXISTS knowledge_versions       CASCADE;
DROP TABLE IF EXISTS provider_knowledge_items CASCADE;
DROP TABLE IF EXISTS knowledge_bundles        CASCADE;

-- === Cluster patterns ===
DROP TABLE IF EXISTS pattern_occurrences CASCADE;
DROP TABLE IF EXISTS pattern_reviews     CASCADE;

-- === memory (memory_namespaces è attiva, queste due sono orfane) ===
DROP TABLE IF EXISTS memory_snapshots      CASCADE;
DROP TABLE IF EXISTS memory_subscriptions  CASCADE;

-- === Cluster orchestrators / chat_profiles ===
DROP TABLE IF EXISTS chat_profiles CASCADE;
DROP TABLE IF EXISTS orchestrators CASCADE;

-- === Tabelle singole senza dipendenti ===
DROP TABLE IF EXISTS anonymization_logs      CASCADE;
DROP TABLE IF EXISTS ast_indexes             CASCADE;
DROP TABLE IF EXISTS branches                CASCADE;
DROP TABLE IF EXISTS change_reports          CASCADE;
DROP TABLE IF EXISTS chat_context_snapshots  CASCADE;
DROP TABLE IF EXISTS cross_project_memories  CASCADE;
DROP TABLE IF EXISTS db_findings             CASCADE;
DROP TABLE IF EXISTS job_artifacts           CASCADE;
DROP TABLE IF EXISTS job_steps               CASCADE;
DROP TABLE IF EXISTS validation_reports      CASCADE;
