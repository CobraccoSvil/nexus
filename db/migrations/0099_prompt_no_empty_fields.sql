-- Migrazione 0099: regola "no campi vuoti / no valori inventati" nei prompt agente.
--
-- Contesto (test E2E redemptor): durante la migrazione SQL Server -> PostgreSQL,
-- mistral-small ha generato la connection string con `Password=` VUOTA, e in chat
-- ha auto-giustificato la scelta scrivendo "(come indicato)" — ma l'utente non aveva
-- mai indicato di lasciare la password vuota. L'agente ha quindi:
-- 1. Inventato un valore plausibile (vuoto)
-- 2. Aggiunto un commento auto-giustificante per nascondere l'invenzione
--
-- Mitigazione: regola esplicita nei prompt che vieta valori inventati e impone
-- chiarimento esplicito quando manca un dato necessario.

DO $$
DECLARE
    rule_block TEXT := E'\n\n<no_invenzioni>\nQuando devi inserire un valore in un file di configurazione (connection string, API key, JWT secret, host, porta, password, ecc.) e il valore NON ti e\\'' stato fornito esplicitamente:\n- NON inventare valori plausibili (placeholder come "your_password_here", stringhe vuote, valori "default")\n- NON auto-giustificare con commenti tipo "(come indicato)" o "(da configurare)" se l\\''utente non ti ha effettivamente indicato nulla\n- INVECE: lascia il campo come placeholder esplicito ${ENV_VAR_NAME} oppure CHIEDI in chat all\\''utente prima di scrivere il file\n- Se il file ha gia\\'' un valore esistente e il task non richiede di cambiarlo, MANTIENI il valore esistente\n\nEsempio errato (mai fare cosi):\n  "ConnectionString": "Host=192.168.0.6;Password=;..."  // password vuota inventata\n\nEsempio corretto:\n  "ConnectionString": "Host=192.168.0.6;Password=${DB_PASSWORD};..."  // env var\n  oppure: chiedi in chat "Quale password devo usare per redemptor_app?"\n</no_invenzioni>';
    target_keys TEXT[] := ARRAY[
        'agent.coder.base',
        'agent.general.debugger',
        'agent.tester.base',
        'agent.reviewer.general'
    ];
    k TEXT;
    cur_version INT;
    cur_content TEXT;
    affected INT := 0;
BEGIN
    FOREACH k IN ARRAY target_keys LOOP
        SELECT version, content INTO cur_version, cur_content
          FROM nexus_prompt_templates
         WHERE key = k AND is_active = TRUE
         LIMIT 1;

        IF cur_version IS NULL THEN
            RAISE NOTICE 'Skip %: nessun prompt attivo', k;
            CONTINUE;
        END IF;

        IF cur_content ILIKE '%<no_invenzioni>%' THEN
            RAISE NOTICE 'Skip %: regola gia'' presente (v%)', k, cur_version;
            CONTINUE;
        END IF;

        UPDATE nexus_prompt_templates
           SET content    = cur_content || rule_block,
               version    = cur_version + 1,
               updated_at = NOW(),
               updated_by = 'migration_0099'
         WHERE key = k AND is_active = TRUE;

        affected := affected + 1;
        RAISE NOTICE 'Aggiornato %: v% -> v% con <no_invenzioni>', k, cur_version, cur_version + 1;
    END LOOP;

    RAISE NOTICE 'Migrazione 0099: % prompt aggiornati', affected;
END $$;
