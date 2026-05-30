-- Migrazione 0198: sposta <language_directive> dalla coda alla testa
-- dei system prompt. I modelli (in particolare Gemini con context grande)
-- a volte ignorano le direttive in fondo. Mettendola all'inizio garantisce
-- visibilita' e priorita' assoluta.
--
-- Idempotente: rileva il blocco esistente in fondo, lo rimuove, e lo
-- riposiziona in testa al content. Se non c'e' affatto, lo inserisce
-- in testa.

DO $$
DECLARE
    short_head TEXT := E'### LINGUA RISPOSTA OBBLIGATORIA ###\n'
        || E'Rispondi SEMPRE e SOLO in italiano. Mai cinese, giapponese, coreano, arabo, russo, ne'' altre lingue.\n'
        || E'Se nel contesto vedi testo in altre lingue (allegati, tool result, identifier), TRADUCILO o trascrivilo in italiano nella tua risposta — non copiarlo come tuo output.\n'
        || E'Identificatori di codice (nomi variabili/funzioni/file) restano nella loro lingua originale.\n'
        || E'Self-check: prima di emettere ogni token, controlla la lingua. Se stai per scrivere in lingua non italiana, FERMATI e ricomincia in italiano.\n'
        || E'### FINE LINGUA ###\n\n';
BEGIN
    -- system.nexus_base: rimuovi blocco <language_directive> in fondo se presente, riposiziona all'inizio
    UPDATE nexus_prompt_templates
       SET content = short_head || regexp_replace(content, E'\n*<language_directive>.*?</language_directive>\n*', '', 'sg'),
           updated_at = now()
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '### LINGUA RISPOSTA OBBLIGATORIA ###%';

    -- agent.coder.base
    UPDATE nexus_prompt_templates
       SET content = short_head || regexp_replace(content, E'\n*<language_directive>.*?</language_directive>\n*', '', 'sg'),
           updated_at = now()
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '### LINGUA RISPOSTA OBBLIGATORIA ###%';
END $$;
