-- 0411: Worklog di sessione — storia di lavoro canonica provider-agnostica.
--
-- Continuita' operativa cross-run e cross-provider (supersede last-wins,
-- cascade fallback, run interrupted, compattazione): gli eventi strutturati
-- (file toccati, comandi con esito, errori, tentativi falliti) vengono
-- derivati deterministicamente dagli agent_steps e materializzati in un
-- digest testuale neutro iniettato nel system_text di ogni run della
-- sessione. Il dettaglio completo resta in tabella, servito on-demand dal
-- tool nexus_get_worklog (scrittore/renderer: crates/mcp-core/src/session_worklog.rs;
-- lettore brain: brain/agents/session_worklog.py).
--
-- Idempotente: CREATE TABLE IF NOT EXISTS + ON CONFLICT DO NOTHING.

CREATE TABLE IF NOT EXISTS nexus_session_worklog_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id  UUID NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    project_id  UUID,
    run_id      UUID,
    kind        TEXT NOT NULL CHECK (kind IN (
                    'file_touched', 'command', 'error', 'retry_ok',
                    'failed_attempt', 'status', 'decision'
                )),
    payload     JSONB NOT NULL,
    source      TEXT NOT NULL DEFAULT 'deterministic'
                CHECK (source IN ('deterministic', 'distilled')),
    dedup_key   TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (session_id, dedup_key)
);

CREATE INDEX IF NOT EXISTS idx_session_worklog_events_session_created
    ON nexus_session_worklog_events (session_id, created_at);

CREATE TABLE IF NOT EXISTS nexus_session_worklog (
    session_id     UUID PRIMARY KEY REFERENCES chat_sessions(id) ON DELETE CASCADE,
    project_id     UUID,
    rendered_block TEXT NOT NULL DEFAULT '',
    events_count   INT NOT NULL DEFAULT 0,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO settings (key, value, category, description, is_secret, updated_at) VALUES
    ('agent.worklog.enabled', 'true', 'agent',
     'Abilita il worklog di sessione: ingest eventi a fine run/supersede/reaper e iniezione del digest nel system_text (scrittore: crates/mcp-core/src/session_worklog.rs; lettore brain: brain/agents/session_worklog.py).',
     false, NOW()),
    ('agent.worklog.inject_mode', 'digest', 'agent',
     'Modalita'' di iniezione del blocco <session_worklog>: digest (compatto, drill-down via tool nexus_get_worklog) oppure full (rendering completo entro inject_max_chars).',
     false, NOW()),
    ('agent.worklog.inject_max_chars', '1200', 'agent',
     'Budget massimo in caratteri del blocco <session_worklog> iniettato nel system_text (riduzione token: il dettaglio resta on-demand via nexus_get_worklog).',
     false, NOW()),
    ('agent.worklog.digest_max_items', '8', 'agent',
     'Numero massimo di voci per sezione nel digest del worklog (priorita'': failed_attempt > error > file_touched > command).',
     false, NOW()),
    ('agent.worklog.tool_page_size', '50', 'agent',
     'Dimensione pagina di default del tool nexus_get_worklog (limit massimo per chiamata).',
     false, NOW()),
    ('agent.worklog.events_max_per_session', '300', 'agent',
     'Tetto eventi per sessione: oltre la soglia il pruning rimuove gli eventi piu'' vecchi non critici (mai failed_attempt/status correnti).',
     false, NOW()),
    ('agent.worklog.error_excerpt_max_chars', '200', 'agent',
     'Lunghezza massima dell''estratto di errore salvato nel payload degli eventi error (mai nei log tracing, regola F).',
     false, NOW())
ON CONFLICT (key) DO NOTHING;
