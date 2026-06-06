-- 0344_purpose_model_tier_only.sql
--
-- Tier-only totale per nexus_purpose_model: ogni purpose deve risolversi tramite
-- il ROUTING per tier (best_model_for_tier dal catalog), eliminando la selezione
-- basata sul (provider, model_id) statico come fallback. Prerequisito per la
-- rimozione del ramo statico in resolve_purpose_core (regola H: niente fallback
-- nascosti; regola L: un solo meccanismo di selezione).
--
-- Il tier assegnato a ciascun purpose deriva dal performance_tier del modello
-- attualmente configurato (preserva la "fascia" di capacita'); la scelta del
-- modello specifico passa ora al routing (tier + required_capability) invece di
-- essere pinnata. required_capability e' valorizzato dove il purpose e'
-- specializzato:
--   - vision_describe / visual_compare -> 'vision' (filtrata via colonna
--     ai_price_catalog.supports_vision in best_model_for_tier);
--   - planner -> 'code'.
--
-- Idempotente: gli UPDATE filtrano su tier IS NULL (non sovrascrivono righe gia'
-- tierizzate ne' override admin successivi).

-- light: modelli leggeri (gemini-2.5-flash / -lite, mistral-small)
UPDATE nexus_purpose_model SET tier = 'light', updated_at = NOW()
WHERE tier IS NULL AND purpose IN (
    'admin_fallback_default', 'admin.tool_selection', 'agent_tier_haiku',
    'agent_tier_sonnet', 'anthropic_batch', 'autofix_planner', 'change_drafter',
    'changelog_significance', 'chat_feedback_generator', 'chat_title_generator',
    'choices_extractor', 'clarify_expand', 'code_doc', 'custom_instructions',
    'decision_extractor', 'explorer', 'functional_spec_extractor', 'google_batch',
    'loop_fallback_default', 'prompt_optimizer', 'provider_test_connection.anthropic',
    'reviewer', 'ui_hint_classifier', 'verifier', 'wiki_title_gen',
    'wiki_triple_extract', 'vision_describe', 'visual_compare'
);

-- medium
UPDATE nexus_purpose_model SET tier = 'medium', updated_at = NOW()
WHERE tier IS NULL AND purpose IN ('planner');

-- heavy
UPDATE nexus_purpose_model SET tier = 'heavy', updated_at = NOW()
WHERE tier IS NULL AND purpose IN ('agent_tier_opus');

-- required_capability per i purpose specializzati
UPDATE nexus_purpose_model SET required_capability = 'vision', updated_at = NOW()
WHERE purpose IN ('vision_describe', 'visual_compare');

UPDATE nexus_purpose_model SET required_capability = 'code', updated_at = NOW()
WHERE purpose = 'planner' AND required_capability IS NULL;

-- Guardia: dopo questa migrazione nessun purpose deve restare senza tier.
-- Se la INSERT di un purpose nuovo dimenticasse il tier, la rimozione del ramo
-- statico (resolve_purpose_core) lo farebbe risolvere in NotFound -> errore
-- esplicito a runtime (fail-loud), non un fallback silenzioso.
DO $$
DECLARE n integer;
BEGIN
    SELECT count(*) INTO n FROM nexus_purpose_model WHERE tier IS NULL;
    IF n > 0 THEN
        RAISE WARNING 'nexus_purpose_model: % purpose senza tier dopo 0344 (saranno non risolvibili tier-only)', n;
    END IF;
END $$;
