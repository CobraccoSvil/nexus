-- 0519_remove_alignment_module.sql
-- Rimozione completa del modulo "guideline alignment" (mig 0346/0347): worker
-- GuidelineAlignmentWorker, route admin /alignment/*, pagina frontend + API TS,
-- settings, purpose orfano e schema. La feature dipendeva dal brain Python
-- (POST /agent/prompt-revise), rimosso: il worker girava a vuoto
-- (call_prompt_revise -> None). Mai attivata in produzione (alignment_enabled
-- sempre false). Rimossa alla radice (codice + config + schema) col pattern
-- "config/schema seguono il codice" (regola G, come mig 0463/0518).
--
-- Il purpose 'prompt_conformance_check' NON viene rimosso: e' condiviso con il
-- PromptOptimizerWorker (punto unico prompt_variants::call_prompt_revise, regola L).
-- Il purpose 'guideline_extract' (Fase 3 GuidelineSyncWorker mai implementata) e'
-- orfano e viene rimosso.
--
-- Settings categoria 'alignment' (mig 0346; alignment_sync_enabled gia' rimosso in
-- 0406). DROP tabelle in ordine di FK: proposal->conformance, guideline->source.
--
-- Idempotente: DELETE / DROP TABLE IF EXISTS sono no-op se gia' assenti.

DELETE FROM settings WHERE key IN (
    'alignment_enabled',
    'alignment_conformance_threshold',
    'alignment_check_interval_hours',
    'alignment_max_checks_per_tick',
    'alignment_autovariant_enabled'
);

DELETE FROM nexus_purpose_model WHERE purpose = 'guideline_extract';

DROP TABLE IF EXISTS nexus_alignment_proposal;
DROP TABLE IF EXISTS nexus_prompt_conformance;
DROP TABLE IF EXISTS nexus_prompt_guideline;
DROP TABLE IF EXISTS nexus_guideline_source;
