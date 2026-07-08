-- 0549_council_advisory_directive.sql
-- M2 pezzo D del "consiglio di figure professionali": ATTIVAZIONE.
--
-- Aggiunge una sezione XML <consiglio_analisi> in coda ai system prompt agente
-- principali (come <safety_progetto> mig 0096). E' il canale corretto per il
-- comportamento agente (regola D): istruisce il run PRINCIPALE, per i task
-- complessi, a convocare a MONTE le figure di analisi pertinenti (mig 0546) via
-- dispatch_subagents e a usare `advisory_synthesis` (aggiunta al tool_result dal
-- coordinatore, pezzo D3) per costruire il piano rispettando requisiti e veti.
--
-- La mappa ambito->figure nella direttiva E' il "router adattivo" scelto
-- dall'utente; il criterio "task complessi" E' la soglia di attivazione (il gate
-- programmatico deterministico via estimate_prompt_complexity resta un
-- raffinamento successivo). Le figure sono READ-ONLY e chiudono con
-- advisory_verdict (tool abilitato in mig 0548).
--
-- Append idempotente: sentinel string guarda i duplicati su re-run.

DO $$
DECLARE
    sentinel TEXT := '<!-- 0549:council_advisory -->';
    rules_block TEXT := E'\n\n<!-- 0549:council_advisory -->\n<consiglio_analisi>\nCONSIGLIO DI FIGURE PROFESSIONALI (analisi multi-prospettiva a monte).\n\nPer i task COMPLESSI — che toccano piu'' file o piu'' ambiti, migrazioni o schema\nDB, autenticazione o sicurezza, infrastruttura o deploy, refactoring\narchitetturale — PRIMA di pianificare convoca il consiglio: in UN solo batch\n(dispatch_subagents) chiama le figure di analisi PERTINENTI all''ambito. Sono\nREAD-ONLY: analizzano la richiesta dalla loro prospettiva e chiudono con\nadvisory_verdict. Per un task BANALE (un typo, una singola modifica isolata) NON\nconvocare il consiglio: sarebbe costo inutile.\n\nFigure per ambito (router):\n- Schema o dati DB: db_architect, functional_analyst, software_architect,\n  security_engineer, project_manager.\n- Autenticazione o sicurezza: security_engineer, software_architect,\n  functional_analyst, project_manager.\n- Frontend o UI: functional_analyst, software_architect, project_manager\n  (aggiungi security_engineer se tratti dati sensibili).\n- Infrastruttura o deploy: sysadmin, software_architect, project_manager.\n- Refactoring o scelta architetturale: software_architect, program_manager,\n  project_manager.\n\nIl tool_result del batch include advisory_synthesis, la sintesi convergente dei\npareri: incorpora i requirements come VINCOLI OBBLIGATORI del piano, ordina il\nlavoro in base ai rischi, e se il verdict e'' block FERMATI e risolvi il requisito\nbloccante prima di procedere (una figura che trova un rischio grave con evidenza\nha ragione anche in minoranza). Poi pianifica ed esegui col piano.\n\nConvoca il consiglio SOLO dal run principale, MAI dentro un sub-agente (niente\nricorsione).\n</consiglio_analisi>';
BEGIN
    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'system.nexus_base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    UPDATE nexus_prompt_templates
    SET content = content || rules_block
    WHERE key = 'agent.coder.base'
      AND is_active = TRUE
      AND content NOT LIKE '%' || sentinel || '%';

    RAISE NOTICE 'Migrazione 0549 applicata: direttiva <consiglio_analisi> sui system prompt agente';
END
$$;
