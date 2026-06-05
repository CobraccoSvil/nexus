-- Migrazione 0334: prompt delle scelte (<suggested_actions>) NON ambiguo.
--
-- Completa il fix lato codice in brain/agents/next_actions.py
-- (_build_extractor_prompt, ramo FALLBACK) anche per il ramo PRIMARIO: il blocco
-- <suggested_actions> che l'agente emette direttamente (direttiva versionata in
-- mig 0329). Problema: il campo `prompt` di ogni scelta veniva generato come
-- frase vaga in prima persona ("Vorrei approfondire la proposta..."), che il
-- router classifica come task operativo e che costringe l'assistente del nuovo
-- turno a chiedere chiarimenti (clarify + cascade + completamento vuoto) invece
-- di agire. Il prompt deve AIUTARE Nexus, non confonderlo.
--
-- Questa migrazione SOSTITUISCE la direttiva <next_actions> esistente nei system
-- prompt (system.nexus_base, agent.coder.base) con una versione che impone, per
-- il campo `prompt`: istruzione completa, in seconda persona, output atteso
-- esplicito, niente formule vaghe ("approfondisci", "parlami di"), clausola
-- "senza modificare i file" per le scelte di sola spiegazione.
--
-- Idempotente: marcatore "VIETATE le formule vaghe" (presente solo nella nuova
-- versione). Rimuove il vecchio blocco <next_actions>...fine e riappende.
-- Regola G/H: niente UPDATE manuali ad-hoc, il prompt e' dato versionato.
--
-- Riferimenti:
--  - Direttiva originale: db/migrations/0329_next_actions_system_prompt.sql
--  - Punto unico feature + ramo fallback: brain/agents/next_actions.py

DO $$
DECLARE
    directive TEXT := E'\n\n<next_actions>\n'
        || E'Quando la tua risposta PROPONE all''utente delle SCELTE su come proseguire (varianti, opzioni, prossimi passi facoltativi: "Vuoi aggiungere X?", "Preferisci Y o Z?", "Posso anche fare W"), aggiungi ALLA FINE della risposta — dopo il testo normale — un blocco machine-readable con le scelte, cosi'' l''interfaccia le mostra come pulsanti cliccabili:\n\n'
        || E'<suggested_actions>\n'
        || E'[{"label":"<testo breve del pulsante, max 40 caratteri>","prompt":"<istruzione completa e non ambigua, pronta da inviare come messaggio utente per proseguire con quella scelta>"}]\n'
        || E'</suggested_actions>\n\n'
        || E'Regole del blocco:\n'
        || E'- Emettilo SOLO quando proponi davvero delle scelte. Se la risposta non offre opzioni, NON aggiungere il blocco.\n'
        || E'- label: conciso e orientato all''azione (es. "Aggiungi galleria immagini").\n'
        || E'- prompt: ISTRUZIONE COMPLETA e NON AMBIGUA in seconda persona verso l''assistente (es. "Descrivimi...", "Genera...", "Modifica..."). Dichiara SEMPRE in modo esplicito l''OUTPUT ATTESO e l''OGGETTO preciso (quale sezione/elemento/file e con quale obiettivo), cosi'' chi la ricevera'' (un nuovo turno) possa eseguire SENZA chiedere chiarimenti.\n'
        || E'- VIETATE le formule vaghe ("approfondisci", "parlami di", "esplora la proposta", "vorrei capire meglio"): non dicono cosa produrre e costringono l''assistente a chiedere chiarimenti. Trasformale in richieste concrete (es. invece di "approfondisci la Hero Section" -> "Descrivimi in dettaglio come rinnovare la Hero Section: struttura, contenuti, stile e testo della call-to-action").\n'
        || E'- Se la scelta e'' una spiegazione/discussione e NON una modifica al codice, esplicitalo aggiungendo in coda: "Per ora forniscimi solo la proposta dettagliata, senza modificare i file."\n'
        || E'- Il contenuto deve essere JSON valido (un array di oggetti). Massimo 6 scelte.\n'
        || E'- Il blocco NON viene mostrato all''utente come testo: viene rimosso e convertito in pulsanti. Scrivi quindi la tua risposta normale completa PRIMA del blocco.\n'
        || E'</next_actions>';
BEGIN
    -- Rimuove la direttiva <next_actions> precedente (mig 0329) dai prompt che
    -- non hanno ancora la versione nuova. `.` matcha anche newline (default
    -- Postgres, flag 'n' OFF): il blocco e' appeso in coda, quindi `.*` arriva a
    -- fine content.
    UPDATE nexus_prompt_templates
       SET content = regexp_replace(content, E'\\s*<next_actions>.*', '', 'g'),
           updated_at = now()
     WHERE key IN ('system.nexus_base', 'agent.coder.base')
       AND content LIKE '%<next_actions>%'
       AND content NOT LIKE '%VIETATE le formule vaghe%';

    -- Riappende la direttiva aggiornata.
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key IN ('system.nexus_base', 'agent.coder.base')
       AND content NOT LIKE '%VIETATE le formule vaghe%';
END $$;
