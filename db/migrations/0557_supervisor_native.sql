-- 0557: supervisore worker nel motore nativo Rust (regola G).
-- Il dropdown UI (none/anomaly/interleaved/continuous) governa la frequenza;
-- modello e provider risolti dal purpose `supervisor_monitoring`.

INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, requires_tool_use, notes)
VALUES (
    'supervisor_monitoring',
    'google',
    'gemini-2.5-flash',
    'light',
    false,
    'Supervisore worker: monitora l avanzamento del run e puo continuare, redirectare o abbandonare. Template automation.supervisor_monitoring.'
)
ON CONFLICT (purpose) DO NOTHING;

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.supervisor.interleaved_interval', '5', 'agent',
   'In modalita interleaved, invoca il supervisore ogni N iterazioni executor.'),
  ('agent.supervisor.anomaly_step_threshold', '20', 'agent',
   'Soglia step count per segnalare anomalia high_step_count.'),
  ('agent.supervisor.timeout_s', '25', 'agent',
   'Timeout (s) della chiamata LLM del supervisore. Clamp 5-300 lato codice.')
ON CONFLICT (key) DO NOTHING;
