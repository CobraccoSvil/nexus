-- 0243_regression_gate_settings.sql
--
-- M13.4 del piano "Impact analysis": regression gate SOFT-only.
--
-- A fine di ogni agent run, dopo che l'agente ha modificato dei file, il nodo
-- LangGraph regression_gate_node (brain/agents/regression_gate_node.py) esegue
-- i test che coprono l'impact set dei file toccati e — in modalita' SOFT —
-- emette SOLO warning + nota KB regression_warning + todo di follow-up se i
-- test falliscono. NON blocca mai il run (il blocco hard sara' M13.5, gestito
-- da un setting separato non ancora introdotto, default-OFF).
--
-- Niente nuova tabella: questi sono soli settings di controllo. L'endpoint
-- mcp-core /api/internal/impact/tests-for-run e' gia' gated da `impact.enabled`
-- (mig 0242); qui aggiungiamo il controllo lato gate brain.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING (stile 0240).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('regression_gate.enabled', 'true', 'regression_gate', 'Abilita il regression gate SOFT a fine run (M13.4): esegue i test dell impact set e avvisa senza bloccare.', FALSE),
    ('regression_gate.soft_only', 'true', 'regression_gate', 'Forza modalita SOFT (solo warning, nota e todo). Il blocco hard e M13.5, non ancora implementato.', FALSE),
    ('regression_gate.max_tests', '10', 'regression_gate', 'Numero massimo di test dell impact set eseguiti dal gate per run (cap anti-latenza).', FALSE),
    ('regression_gate.test_timeout_s', '120', 'regression_gate', 'Timeout in secondi per singolo test eseguito dal regression gate.', FALSE)
ON CONFLICT (key) DO NOTHING;
