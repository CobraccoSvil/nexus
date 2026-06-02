-- 0249_intent_health_routing.sql
-- Fase E (M7) — Q-value routing basato su nexus_provider_intent_health.
--
-- Il brain registra l'esito di ogni turno agente per (provider, model, intent)
-- e, quando un provider supera una soglia di fallimenti su un intent, lo mette
-- in cooldown e lo salta nel fallback chain. Tutto gated da un singolo flag
-- (default OFF) per non alterare il routing finche' non si decide di attivarlo
-- con dati storici raccolti: con il flag OFF non c'e' overhead ne' rischio.
-- Soglie configurabili (regola G: niente costanti hardcoded). Idempotente.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('routing.intent_health_enabled', 'false', 'routing',
     'M7 Q-value: registra esiti per (provider,model,intent) e salta i provider in cooldown nel fallback. OFF di default (attivare dopo aver raccolto dati).', 'f'),
    ('routing.intent_health_min_attempts', '8', 'routing',
     'Numero minimo di tentativi (success+failure) su un intent prima di poter mettere un provider in cooldown M7.', 'f'),
    ('routing.intent_health_failure_threshold_pct', '60', 'routing',
     'Percentuale di fallimenti su un intent oltre cui un provider entra in cooldown M7.', 'f'),
    ('routing.intent_health_cooldown_secs', '600', 'routing',
     'Durata (secondi) del cooldown M7 di un provider su un intent dopo aver superato la soglia di fallimenti.', 'f')
ON CONFLICT (key) DO NOTHING;
