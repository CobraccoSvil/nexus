-- Migration 0042: aggiunge la categoria 'chat' ai prompt templates
-- e sposta il template precheck nella categoria corretta.

ALTER TABLE nexus_prompt_templates
    DROP CONSTRAINT IF EXISTS nexus_prompt_templates_category_check;

ALTER TABLE nexus_prompt_templates
    ADD CONSTRAINT nexus_prompt_templates_category_check
    CHECK (category = ANY (ARRAY['system','quality','automation','profile','chat']));

-- Corregge il template inserito dalla 0041 che usava category='system' come fallback
UPDATE nexus_prompt_templates
SET category = 'chat'
WHERE key = 'chat.precheck_message' AND category = 'system';
