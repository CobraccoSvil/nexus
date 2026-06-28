-- Tracce gateway LLM per iterazione (AITraceEvent: provider/model effettivi,
-- token, stop_reason, cascade/escalation/fallback) pubblicate in chat.
--
-- Schema persistenza speculare a nexus_agent_meta_steps (mig 0168): il canale
-- primario di pubblicazione e' SSE realtime (eventi `agent_trace` ritrasmessi
-- al frontend). Questa tabella consente di ricostruire le tracce di un run a
-- posteriori (trace panel) dopo un reload: prima le tracce vivevano solo in
-- sessionStorage del browser e sparivano cambiando dispositivo o pulendo lo
-- storage, divergendo dal rendering live.
--
-- Insert avviene best-effort dal motore (mcp-core, sqlx): un fallimento NON
-- blocca il run (gli eventi SSE arrivano comunque). Il `payload` contiene
-- l'AITraceEvent serializzato (camelCase, stessa forma dell'evento SSE).

CREATE TABLE IF NOT EXISTS nexus_agent_traces (
    id BIGSERIAL PRIMARY KEY,
    session_id UUID NOT NULL,
    run_id UUID NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_nexus_agent_traces_session
    ON nexus_agent_traces (session_id);
CREATE INDEX IF NOT EXISTS idx_nexus_agent_traces_run
    ON nexus_agent_traces (run_id, seq);

COMMENT ON TABLE nexus_agent_traces IS
    'Tracce gateway LLM per iterazione (provider/model effettivi, token, stop_reason). '
    'Canale primario SSE (agent_trace); questa tabella e'' la copia persistente per '
    'ricostruire il trace panel dopo un reload.';
