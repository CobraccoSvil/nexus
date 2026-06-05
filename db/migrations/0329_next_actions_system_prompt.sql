-- Migrazione 0329: direttiva <next_actions> nei system prompt agente.
--
-- Feature "scelte di proseguimento": quando una risposta dell'agente propone
-- all'utente delle SCELTE su come continuare (es. "Vuoi aggiungere immagini? /
-- Vuoi integrare un form? / Vuoi una sezione Testimonianze?"), il brain emette
-- un meta_step strutturato (kind="next_actions") che il frontend rende come
-- pulsanti cliccabili.
--
-- Approccio ibrido (vedi brain/agents/next_actions.py):
--   PRIMARIO  - l'agente emette, SOLO quando propone scelte, un blocco
--               machine-readable <suggested_actions> alla fine della risposta;
--               il brain lo estrae, lo parsa e lo RIMUOVE dal testo visibile.
--   FALLBACK  - se il blocco manca ma la risposta sembra contenere scelte, un
--               modello leggero (purpose 'choices_extractor', mig 0330) le
--               estrae.
--
-- Questa migrazione versiona la sola direttiva PRIMARIA nei system prompt
-- (regola G/H: niente UPDATE manuali, il prompt e' dato versionato). Si applica
-- ai prompt dell'agente che risponde all'utente in chat: system.nexus_base e
-- agent.coder.base (stesso target di mig 0192).
--
-- Idempotente: rileva la presenza del blocco prima di appenderlo.
--
-- Riferimenti:
--  - Helper meta_step: brain/agents/meta_steps.py
--  - Punto unico feature: brain/agents/next_actions.py
--  - Emissione: brain/agents/nodes/__init__.py (executor_node, return end_turn)

DO $$
DECLARE
    directive TEXT := E'\n\n<next_actions>\n'
        || E'Quando la tua risposta PROPONE all''utente delle SCELTE su come proseguire (varianti, opzioni, prossimi passi facoltativi: "Vuoi aggiungere X?", "Preferisci Y o Z?", "Posso anche fare W"), aggiungi ALLA FINE della risposta — dopo il testo normale — un blocco machine-readable con le scelte, cosi'' l''interfaccia le mostra come pulsanti cliccabili:\n\n'
        || E'<suggested_actions>\n'
        || E'[{"label":"<testo breve del pulsante, max 40 caratteri>","prompt":"<prompt completo e autocontenuto, pronto da inviare come messaggio utente per proseguire con quella scelta>"}]\n'
        || E'</suggested_actions>\n\n'
        || E'Regole del blocco:\n'
        || E'- Emettilo SOLO quando proponi davvero delle scelte. Se la risposta non offre opzioni, NON aggiungere il blocco.\n'
        || E'- label: conciso e orientato all''azione (es. "Aggiungi galleria immagini").\n'
        || E'- prompt: frase completa e autonoma. Chi la ricevera'' (un nuovo turno) NON vede questa conversazione: includi tutto il contesto necessario per eseguire quella scelta.\n'
        || E'- Il contenuto deve essere JSON valido (un array di oggetti). Massimo 6 scelte.\n'
        || E'- Il blocco NON viene mostrato all''utente come testo: viene rimosso e convertito in pulsanti. Scrivi quindi la tua risposta normale completa PRIMA del blocco.\n'
        || E'</next_actions>';
BEGIN
    -- system.nexus_base
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<next_actions>%';

    -- agent.coder.base
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<next_actions>%';
END $$;
