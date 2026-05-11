-- Migrazione 0131: rimozione tabelle morte (nessun INSERT né SELECT nel codice attivo)
-- Audit eseguito 2026-05-11 su crates/ brain/ apps/ (grep completo).
-- Le tabelle in 0126 erano un set diverso (parsed_blocks, knowledge_bundles, ecc.).
-- Queste 19 tabelle non hanno alcun riferimento nel codice runtime.
--
-- Pre-check obbligatorio (tutti devono restituire 0):
-- SELECT
--   (SELECT COUNT(*) FROM intent_policies)            AS intent_policies,
--   (SELECT COUNT(*) FROM nexus_agent_skills)         AS nexus_agent_skills,
--   (SELECT COUNT(*) FROM nexus_routing_history)      AS nexus_routing_history,
--   (SELECT COUNT(*) FROM provider_configs)           AS provider_configs,
--   (SELECT COUNT(*) FROM provider_models)            AS provider_models,
--   (SELECT COUNT(*) FROM provider_model_sync_runs)   AS provider_model_sync_runs,
--   (SELECT COUNT(*) FROM provider_test_runs)         AS provider_test_runs,
--   (SELECT COUNT(*) FROM file_snapshots)             AS file_snapshots,
--   (SELECT COUNT(*) FROM nexus_anthropic_batches)    AS nexus_anthropic_batches,
--   (SELECT COUNT(*) FROM patterns)                   AS patterns,
--   (SELECT COUNT(*) FROM prompt_bindings)            AS prompt_bindings,
--   (SELECT COUNT(*) FROM prompt_evals)               AS prompt_evals,
--   (SELECT COUNT(*) FROM prompt_eval_runs)           AS prompt_eval_runs,
--   (SELECT COUNT(*) FROM prompt_feedback)            AS prompt_feedback,
--   (SELECT COUNT(*) FROM prompt_mcp_tools)           AS prompt_mcp_tools,
--   (SELECT COUNT(*) FROM prompt_templates)           AS prompt_templates,
--   (SELECT COUNT(*) FROM prompt_versions)            AS prompt_versions,
--   (SELECT COUNT(*) FROM refusals)                   AS refusals,
--   (SELECT COUNT(*) FROM resource_policies)          AS resource_policies;

BEGIN;

-- == Provider/routing obsolete (sezione 7b del piano) ==
-- Duplicate di settings DB + api_key_loader
DROP TABLE IF EXISTS provider_test_runs       CASCADE;
DROP TABLE IF EXISTS provider_model_sync_runs CASCADE;
DROP TABLE IF EXISTS provider_models          CASCADE;
DROP TABLE IF EXISTS provider_configs         CASCADE;
-- nexus_routing_history (mig 0054) sostituita da nexus_routing_decisions (mig 0112)
DROP TABLE IF EXISTS nexus_routing_history    CASCADE;
-- intent_policies: 0 ref, ridondante con nexus_intent_capability
DROP TABLE IF EXISTS intent_policies          CASCADE;
-- nexus_agent_skills: feature mai implementata, 0 ref
DROP TABLE IF EXISTS nexus_agent_skills       CASCADE;

-- == Prompt registry obsoleto (sezione 7c.3 del piano) ==
-- prompt_templates sostituita da nexus_prompt_templates (98 righe)
DROP TABLE IF EXISTS prompt_templates         CASCADE;
-- prompt_versions sostituita da nexus_prompt_template_history
DROP TABLE IF EXISTS prompt_versions          CASCADE;
-- prompt_evals e prompt_eval_runs: schema vecchio A/B, sostituito da prompt_ab_experiments
DROP TABLE IF EXISTS prompt_eval_runs         CASCADE;
DROP TABLE IF EXISTS prompt_evals             CASCADE;
-- prompt_feedback sostituita da ai_response_feedback
DROP TABLE IF EXISTS prompt_feedback          CASCADE;
-- prompt_bindings: design non implementato, 0 ref
DROP TABLE IF EXISTS prompt_bindings          CASCADE;
-- prompt_mcp_tools: intent superato da nexus_intent_capability, 0 ref
DROP TABLE IF EXISTS prompt_mcp_tools         CASCADE;

-- == Feature mai implementate (sezione 7c.3 del piano) ==
DROP TABLE IF EXISTS file_snapshots           CASCADE;
DROP TABLE IF EXISTS nexus_anthropic_batches  CASCADE;
-- patterns: predecessore di reasoning_patterns, 0 ref
DROP TABLE IF EXISTS patterns                 CASCADE;
DROP TABLE IF EXISTS refusals                 CASCADE;
DROP TABLE IF EXISTS resource_policies        CASCADE;

COMMIT;
