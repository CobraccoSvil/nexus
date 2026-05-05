-- Migrazione 0098: rinforzo anti-allucinazione e scope di modifica nei prompt agente.
--
-- Contesto (test E2E redemptor):
-- - Mistral-small ha dichiarato "Build .NET completata senza errori" senza aver
--   mai eseguito 'dotnet build' (zero tool calls). Allucinazione classica.
-- - Cliccando "Fix" su un finding del file stripe/route.ts, l'agente ha:
--   1. Modificato il file stripe (refactor non richiesto)
--   2. Cancellato app/package-lock.json (file completamente non target)
--   3. Mentito in chat: "non ci sono modifiche da fare" mentre le faceva
--
-- Mitigazione (lato prompt, additiva, non altera autonomia esistente):
-- - <verifica_azioni>: vietato dichiarare azioni non eseguite
-- - <scope_modifiche>: ogni edit deve essere giustificato dal task; vietato
--   toccare file fuori scope (es. lockfile, config CI, .env)
-- - <falso_positivo>: scanner false-positive non vanno "fixati a forza"
--
-- Si applica come APPEND ai prompt critici. Schema usa UNIQUE su key,
-- quindi UPDATE in-place + bump version per audit.

DO $$
DECLARE
    guard_block TEXT := E'\n\n<verifica_azioni>\nNON dichiarare MAI di aver eseguito un\\''azione (es. "build completata", "test eseguiti", "file modificato") se non hai effettivamente chiamato il tool corrispondente in questo turno. Se l\\''utente o il sistema chiede una verifica (dotnet build, pnpm verify, cargo check, ecc.), DEVI eseguirla via tool prima di riferire l\\''esito. Se non hai eseguito una verifica, dillo esplicitamente: "Non ho eseguito X perche\\'' [motivo]".\n</verifica_azioni>\n\n<scope_modifiche>\nOgni file modificato deve essere giustificato dal task corrente. NON modificare file che non sono direttamente correlati al fix richiesto. In particolare:\n- File di lock (package-lock.json, pnpm-lock.yaml, yarn.lock, Cargo.lock): NON toccare a meno che il task NON sia esplicitamente "aggiorna dipendenze".\n- File .env / .env.* in produzione: NON toccare senza autorizzazione esplicita.\n- File CI/CD (.github/workflows/, .gitlab-ci.yml): NON toccare a meno che il task NON sia "modifica pipeline".\n- File in altri moduli/progetti non menzionati nel task: NON toccare.\n- Line endings (CRLF/LF): preservare lo stile originale del file.\nSe pensi che un cambiamento extra sia necessario, CHIEDI prima di farlo.\n</scope_modifiche>\n\n<falso_positivo>\nSe ricevi un task "Fix questo finding" e dopo l\\''analisi del codice ritieni che il finding sia un FALSO POSITIVO (il pattern segnalato non esiste davvero nel file, o il fix proposto e\\'' inappropriato), spiega in chat perche\\'' lo classifichi come falso positivo e NON modificare il file. Mai modificare codice "per accontentare lo scanner".\n</falso_positivo>';
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
            RAISE NOTICE 'Skip %: nessun prompt attivo trovato', k;
            CONTINUE;
        END IF;

        -- Idempotenza: se il guard e' gia' presente, skippa.
        IF cur_content ILIKE '%<verifica_azioni>%' THEN
            RAISE NOTICE 'Skip %: guard <verifica_azioni> gia'' presente (v%)', k, cur_version;
            CONTINUE;
        END IF;

        UPDATE nexus_prompt_templates
           SET content    = cur_content || guard_block,
               version    = cur_version + 1,
               updated_at = NOW(),
               updated_by = 'migration_0098'
         WHERE key = k AND is_active = TRUE;

        affected := affected + 1;
        RAISE NOTICE 'Aggiornato %: v% -> v% con guard anti-allucinazione (+%) chars', k, cur_version, cur_version + 1, length(guard_block);
    END LOOP;

    RAISE NOTICE 'Migrazione 0098: % prompt aggiornati', affected;
END $$;
