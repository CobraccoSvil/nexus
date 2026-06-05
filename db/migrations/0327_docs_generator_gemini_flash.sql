-- 0327_docs_generator_gemini_flash.sql
--
-- Rende PERMANENTE il modello del purpose 'docs_generator' (regole G/H).
--
-- Root cause: la mig 0102 seeda 'docs_generator' = openai/gpt-4.1-nano, un
-- modello debole che generava documenti con sezioni a contenuto vuoto (solo
-- l'indice). Il modello era stato corretto a google/gemini-2.5-flash via UPDATE
-- MANUALE (notes "bypass openai cooldown E2E"): una toppa che NON sopravvive a
-- un wipe DB + re-apply delle migrazioni (il sistema tornerebbe a gpt-4.1-nano
-- e rigenererebbe documenti vuoti). Questa migrazione versiona la scelta.
--
-- gemini-2.5-flash e' un modello capace, verificato a riprodurre content
-- ricchi e strutturati per nexus_doc_generate (handle_doc_generate -> /complete).

UPDATE nexus_purpose_model
SET provider = 'google',
    model_id = 'gemini-2.5-flash',
    notes    = 'Generatore documenti (nexus_doc_generate). Modello capace per output strutturato lungo. Versionato in mig 0327 (era gpt-4.1-nano in 0102, produceva sezioni vuote).',
    updated_at = NOW()
WHERE purpose = 'docs_generator';

-- Se il purpose non esistesse (DB parziale), lo crea con il modello corretto.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes)
SELECT 'docs_generator', 'google', 'gemini-2.5-flash',
       'Generatore documenti (nexus_doc_generate). Seed mig 0327.'
WHERE NOT EXISTS (
    SELECT 1 FROM nexus_purpose_model WHERE purpose = 'docs_generator'
);
