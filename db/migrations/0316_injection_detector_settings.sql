-- ADR 0021 — SQL injection detector unificato.
--
-- Setting opzionali per tarare il detector unico (mcp_quality::injection).
-- Default: enabled, soglia minima medium. Il detector e' comunque sempre attivo
-- nel codice corrente; questi setting predispongono il tuning DB-driven (regola G)
-- per una futura lettura con cache 60s, senza fallback hardcoded.

BEGIN;

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.scanner.sql_injection_enabled', 'true', 'scanner',
     'Abilita il detector di SQL injection sul codice applicativo (.rs/.py/.ts/.js). ADR 0021', FALSE),
    ('agent.scanner.sql_injection_min_severity', 'medium', 'scanner',
     'Soglia minima di severity riportata dal detector (medium|high). ADR 0021', FALSE)
ON CONFLICT (key) DO NOTHING;

COMMIT;
