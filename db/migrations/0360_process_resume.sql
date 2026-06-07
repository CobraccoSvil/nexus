-- 0360_process_resume.sql
--
-- Cablaggio "process-completion -> agent-resume": quando un agent_process
-- (comando/servizio lanciato in background, tracciato) termina, l'agente del
-- run associato deve essere RISVEGLIATO per dare l'aggiornamento promesso
-- ("Ti aggiorno appena termina"). Prima mancava: l'agente chiudeva il turno e
-- nessuno lo richiamava al completamento del processo.
--
-- Questa migrazione aggiunge:
--   - resume_dispatched_at: marca i processi per cui il wake-up e' gia' stato
--     inviato (idempotenza: un solo resume per processo).
--   - i setting che governano il worker process_resume (regola G, niente
--     hardcode nel codice; cache lato Rust).
--
-- Il worker (process_resume.rs) consuma questi campi: per ogni processo
-- terminato di recente con session_id e resume_dispatched_at NULL, inietta un
-- messaggio di sintesi esito e avvia un nuovo turno agentico (spawn_agent_run),
-- poi marca resume_dispatched_at. Anti-loop: cap orario per sessione.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS, ON CONFLICT DO NOTHING sui setting.

ALTER TABLE agent_processes
    ADD COLUMN IF NOT EXISTS resume_dispatched_at TIMESTAMPTZ;

-- Indice parziale per la query del worker (processi da risvegliare).
CREATE INDEX IF NOT EXISTS idx_agent_processes_resume_pending
    ON agent_processes (stopped_at)
    WHERE resume_dispatched_at IS NULL AND session_id IS NOT NULL;

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.process_resume.enabled', 'true', 'agent',
    'Se true, alla terminazione di un processo background tracciato (agent_processes) '
    || 'con sessione associata, l''agente viene risvegliato per dare l''aggiornamento '
    || '(cablaggio process-completion -> agent-resume).'
),
(
    'agent.process_resume.poll_seconds', '10', 'agent',
    'Intervallo di polling del worker process_resume (secondi).'
),
(
    'agent.process_resume.max_per_session_hour', '12', 'agent',
    'Cap anti-loop: numero massimo di risvegli automatici per sessione in un''ora.'
),
(
    'agent.process_resume.output_tail_chars', '2000', 'agent',
    'Quanti caratteri di output finale del processo includere nel messaggio di '
    || 'risveglio iniettato all''agente.'
)
ON CONFLICT (key) DO NOTHING;
