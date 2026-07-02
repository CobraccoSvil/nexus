-- 0507: decommissiona nel META-DB le tabelle del dominio chat/run migrate ai
-- DB-progetto (set db/migrations/project, cutover separazione DB 2026-07-01).
--
-- Perche': dopo il cutover queste tabelle nel meta sono VUOTE ma esistenti.
-- Un call site rimasto col pool meta non fallisce: legge "0 righe" e la logica
-- degenera in silenzio (classe di bug ricorrente: pannello step vuoto, history
-- del turno vuota, enforcement porte mai scattato). Col RENAME ogni accesso
-- residuo fallisce con "relation does not exist": errore strutturato e visibile
-- al primo run (regole H e M), non un comportamento sbagliato da diagnosticare.
--
-- RENAME (non DROP): rollback immediato possibile con la migrazione inversa.
-- Il DROP definitivo arrivera' in una migrazione successiva dopo un periodo di
-- osservazione con log puliti.
--
-- Guardia fail-fast: se una tabella contiene righe la migrazione FALLISCE
-- (mai scartare dati in silenzio). Verificate tutte a 0 righe il 2026-07-02.
--
-- Escluse dal perimetro:
--   - project_open_sessions: contiene dati vivi nel meta (scrittore da
--     chiarire: possibile ulteriore pool-gap o dominio legittimamente meta);
--   - nexus_agent_clarifications, nexus_conversation_summaries: non esistono
--     nel meta (create solo dal set project);
--   - project_runtime_issues, terminal_commands: NON migrate, restano nel
--     meta; le loro FK verso il dominio migrato vengono pero' rimosse (un
--     riferimento cross-DB non e' esprimibile: la tabella referenziata vive
--     nel DB-progetto e la copia meta resterebbe vuota per sempre).

DO $$
DECLARE
    tables text[] := ARRAY[
        'agent_processes',
        'agent_runs',
        'agent_steps',
        'ai_response_feedback',
        'chat_message_attachments',
        'chat_messages',
        'chat_sessions',
        'jobs',
        'langgraph_checkpoints',
        'nexus_agent_meta_steps',
        'nexus_agent_plans',
        'nexus_agent_todos',
        'nexus_agent_traces',
        'nexus_agent_verifier_runs',
        'nexus_graph_checkpoints',
        'nexus_session_worklog',
        'nexus_session_worklog_events',
        'nexus_subagent_runs',
        'orchestrator_audit_events',
        'orchestrator_runs',
        'prompt_corrections'
    ];
    t text;
    n bigint;
    fk record;
BEGIN
    -- 1. Rimuove le FK di tabelle ESTERNE al dominio migrato che referenziano
    --    tabelle del dominio (oggi: project_runtime_issues -> agent_runs/
    --    agent_steps, terminal_commands -> chat_sessions). Senza questo drop,
    --    dopo il rename ogni INSERT nelle referenzianti fallirebbe FK contro
    --    una tabella decommissionata vuota per sempre.
    FOR fk IN
        SELECT conrelid::regclass::text AS tbl, conname
        FROM pg_constraint
        WHERE contype = 'f'
          AND confrelid::regclass::text = ANY(tables)
          AND NOT (conrelid::regclass::text = ANY(tables))
    LOOP
        RAISE NOTICE 'decommission meta: drop FK % su % (riferimento cross-DB non applicabile)',
            fk.conname, fk.tbl;
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', fk.tbl, fk.conname);
    END LOOP;

    -- 2. Rename fail-fast delle tabelle migrate (solo se esistono e sono vuote).
    FOREACH t IN ARRAY tables LOOP
        IF EXISTS (
            SELECT 1 FROM pg_tables WHERE schemaname = 'public' AND tablename = t
        ) THEN
            EXECUTE format('SELECT count(*) FROM %I', t) INTO n;
            IF n > 0 THEN
                RAISE EXCEPTION
                    'decommission meta: la tabella % contiene % righe. I dati del dominio chat/run vivono nei DB-progetto: migrare o ripulire ESPLICITAMENTE prima di applicare 0507.',
                    t, n;
            END IF;
            EXECUTE format('ALTER TABLE %I RENAME TO %I', t, 'zz_decommissioned_' || t);
        END IF;
    END LOOP;
END $$;
