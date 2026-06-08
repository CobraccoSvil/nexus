-- 0370_agent_runs_cooperative_cancel.sql
-- Enforcement strutturale "al piu' un run agentico attivo per session_id" +
-- cancellazione cooperativa reale nel brain.
--
-- Causa radice dell'incidente (3+ run paralleli sulla stessa chat -> loop
-- infinito + context explosion): (1) la guard anti-concorrenza viveva solo in
-- send_chat_message, bypassata dai worker che creano run (process_resume,
-- service_observer, resume); (2) era "reject" con finestra stale, non
-- "last-wins"; (3) marcare un run 'cancelled' nel DB NON fermava il loop in
-- memoria perche' il grafo LangGraph non legge mai agent_runs.status.
--
-- Fix: supersede_active_runs (Rust) applica last-wins nel punto unico
-- spawn_agent_run; questa colonna e' il segnale di stop persistente che il
-- brain controlla tra le iterazioni (route_after_executor -> _check_superseded)
-- per terminare davvero il run superato. Persistente (sopravvive a restart),
-- non un flag volatile. Idempotente.

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS cancellation_requested TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cancellation_reason TEXT;

-- Lookup veloce dei run attivi di una sessione (supersede + check cooperativo).
CREATE INDEX IF NOT EXISTS idx_agent_runs_session_active
    ON agent_runs (session_id)
    WHERE status IN ('running', 'awaiting_confirmation');

-- Setting (regola G): throttle del check cooperativo nel brain (secondi tra due
-- letture di agent_runs.status per lo stesso run). Default 2s.
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.cooperative_cancel.check_interval_seconds', '2', 'agent',
     'Intervallo minimo (secondi) tra due controlli di cancellazione cooperativa che il grafo del brain fa su agent_runs per il proprio run. Evita di interrogare il DB ad ogni micro-iterazione.',
     NOW())
ON CONFLICT (key) DO NOTHING;
