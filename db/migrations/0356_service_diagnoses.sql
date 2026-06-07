-- 0356_service_diagnoses.sql
-- Storico delle diagnosi/anomalie rilevate dal service_observer sulle app utente
-- (capacita' 1 crash-auto-debug, 2 build-error, 3 anomaly). Le metriche grezze
-- (capacita' 4) NON vivono qui: restano effimere sull'event-stream + ring
-- in-memory (confine anti-over-engineering). Qui si storicizzano solo gli eventi
-- significativi e si garantisce l'idempotenza del trigger Debugger.
--
-- Regola E: ogni riga e' vincolata a un project_id (scope progetto).
-- Idempotenza auto-debug: (project_id, unit, error_signature_hash) + status +
-- cooldown_until evitano di rilanciare l'agente sullo stesso crash.
-- Idempotente.

CREATE TABLE IF NOT EXISTS service_diagnoses (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id           UUID NOT NULL,
    unit                 TEXT NOT NULL,
    ts                   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- 'anomaly' | 'crash' | 'build_error'
    signal_kind          TEXT NOT NULL,
    -- per signal_kind='anomaly': latency|restart|error_rate|cpu|rss
    metric               TEXT,
    value                DOUBLE PRECISION,
    threshold            DOUBLE PRECISION,
    -- firma stabile dell'errore per deduplica/idempotenza (hash del pattern)
    error_signature_hash TEXT,
    -- per signal_kind='build_error': [{file,line,column,severity,message}]
    build_findings       JSONB,
    -- run dell'agente Debugger eventualmente avviato per questa diagnosi
    triggered_run_id     UUID,
    -- 'open' | 'diagnosing' | 'resolved'
    status               TEXT NOT NULL DEFAULT 'open',
    -- finestra di cooldown prima di poter ritriggerare la stessa firma
    cooldown_until       TIMESTAMPTZ,
    detail               TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Lookup principale: diagnosi aperte per servizio di un progetto.
CREATE INDEX IF NOT EXISTS idx_service_diagnoses_project_unit_status
    ON service_diagnoses (project_id, unit, status);

-- Idempotenza/cooldown del trigger Debugger sulla firma errore.
CREATE INDEX IF NOT EXISTS idx_service_diagnoses_signature
    ON service_diagnoses (project_id, unit, error_signature_hash);

-- Cap orario per progetto: conteggio diagnosi recenti.
CREATE INDEX IF NOT EXISTS idx_service_diagnoses_project_ts
    ON service_diagnoses (project_id, ts DESC);
