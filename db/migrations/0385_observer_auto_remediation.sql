-- 0385_observer_auto_remediation.sql
--
-- Attiva l'auto-remediation iterativa del service_observer: quando un servizio
-- di progetto non parte, Nexus diagnostica (LLM) -> avvia il debugger -> riavvia
-- il servizio -> ri-verifica, finche' il processo parte (readiness OK) o si
-- raggiunge il cap anti-spirale.
--
--   - agent.observer.auto_diagnose_enabled = true: abilita il trigger automatico
--     del debugger su crash rilevato (prima opt-in/false). Il loop e' gated da
--     cooldown per firma (diagnose_cooldown_seconds) + cap orario per progetto.
--   - agent.observer.diagnose_max_per_hour = 8: cap di auto-debug/ora per progetto
--     (alzato da 5) per dare iterazioni sufficienti a convergere, restando un
--     freno anti-loop su problemi non auto-risolvibili.
--
-- DB-driven (regola G): nessun hardcode nel codice; disattivabile a caldo.
-- Idempotente: forza i valori (DO UPDATE) anche se le righe esistono gia'.

BEGIN;

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.observer.auto_diagnose_enabled',
    'true',
    'agent',
    'Se true, il service_observer avvia automaticamente il debugger agentico quando rileva un servizio di progetto non funzionante (auto-remediation). Gated da cooldown per firma + cap orario. Disattivabile a caldo.'
)
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, description = EXCLUDED.description;

INSERT INTO settings (key, value, category, description) VALUES (
    'agent.observer.diagnose_max_per_hour',
    '8',
    'agent',
    'Numero massimo di auto-debug per progetto/ora (cap anti-spirale del loop di auto-remediation). Oltre la soglia il service_observer smette di ritriggerare fino al rientro della finestra oraria.'
)
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, description = EXCLUDED.description;

COMMIT;
