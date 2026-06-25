-- 0455_final_gate_endpoint_check.sql
-- B1 (gap agente): il final_gate verificava la BUILD ma non la chiamata HTTP
-- reale dell'endpoint. Cosi' l'agente chiudeva "completed" con la build verde ma
-- il login ancora rotto (incidente Beauty-Book: 500 dal proxy verso una porta
-- vuota). Aggiunge il supporto a un criterio HTTP FUNZIONALE nel final_gate,
-- de-lessicalizzato: scatta SOLO se il progetto ha una run_configurations con
-- role='endpoint' e uno spec HTTP in http_spec, mai dal testo del task.
--
-- http_spec JSONB: {url, method?, body?, headers?, expected_status?, body_contains?}.
-- Letto da brain/agents/final_gate.py::_resolve_endpoint_check; eseguito da
-- criteria_runner._check_http (esteso per body/headers e status multipli).
-- Setting (regola G; il default nel codice e' solo rete di sicurezza per DB down):
--   agent.final_gate.endpoint_check_enabled  (gate, default true)
--   agent.final_gate.endpoint_timeout_seconds (default 15)
-- Idempotente: ADD COLUMN IF NOT EXISTS + INSERT ON CONFLICT DO NOTHING.

ALTER TABLE run_configurations ADD COLUMN IF NOT EXISTS http_spec JSONB;

COMMENT ON COLUMN run_configurations.http_spec IS
    'Spec di una verifica HTTP funzionale (final_gate B1) per le run_configurations con role=endpoint: {url, method, body, headers, expected_status, body_contains}. NULL = non un endpoint test.';

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.final_gate.endpoint_check_enabled', 'true', 'agent',
    'Se true, il final_gate aggiunge un criterio HTTP funzionale per i progetti con una run_configurations role=endpoint (http_spec valorizzato): verifica con una chiamata reale che l''endpoint risponda come atteso prima di chiudere completed. De-lessicalizzato (solo config strutturale, non il testo del task). N/A se nessun endpoint e'' configurato, quindi non blocca i progetti senza endpoint.'
),
(
    'agent.final_gate.endpoint_timeout_seconds', '15', 'agent',
    'Timeout (secondi) della chiamata HTTP del criterio endpoint del final_gate (B1).'
)
ON CONFLICT (key) DO NOTHING;
