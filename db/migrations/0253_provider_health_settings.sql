-- 0252_provider_health_settings.sql
-- Health/cooldown provider: tempi di polling e durate cooldown resi DB-driven
-- (regola G). Prima erano costanti hardcoded in provider_cooldown.rs /
-- provider_health_probe.rs / main.rs (interval recovery loop = 60s letterale).
--
-- Inoltre introduce il timeout del probe usato dalla nuova logica
-- "probe-then-reenable": il billing_cooldown_recovery_loop, prima di riabilitare
-- un provider il cui cooldown e' scaduto, esegue un probe attivo e riabilita
-- SOLO se il provider risponde sano. Vedi provider_cooldown::billing_cooldown_recovery_loop.
--
-- Idempotente: ON CONFLICT DO NOTHING. Default calibrati sui valori storici
-- (nessun cambio di comportamento, sola ridirezione della fonte).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('provider.billing_recovery_interval_s', '60', 'providers',
     'Cadenza (secondi) del billing_cooldown_recovery_loop che riabilita i provider a cooldown scaduto previo probe.', 'f'),
    ('provider.recovery_probe_timeout_s', '30', 'providers',
     'Timeout (secondi) del probe attivo eseguito prima di riabilitare un provider (probe-then-reenable).', 'f'),
    ('provider.cooldown_default_s', '300', 'providers',
     'Durata cooldown provider di default (secondi) quando il Retry-After non e fornito.', 'f'),
    ('provider.cooldown_min_s', '10', 'providers',
     'Cap inferiore (secondi) del cooldown provider per evitare hammering.', 'f'),
    ('provider.cooldown_max_s', '3600', 'providers',
     'Cap superiore (secondi) del cooldown provider.', 'f'),
    ('provider.cooldown_long_s', '21600', 'providers',
     'Durata cooldown lungo (secondi) per errori billing/quota non risolvibili a breve. Default 6h.', 'f'),
    ('provider.circuit_breaker_window_s', '60', 'providers',
     'Finestra (secondi) del circuit breaker provider: N fallimenti entro questa finestra aprono il breaker.', 'f'),
    ('provider.circuit_breaker_threshold', '3', 'providers',
     'Numero di fallimenti entro la finestra che apre il circuit breaker provider.', 'f'),
    ('provider.circuit_breaker_extended_cooldown_s', '600', 'providers',
     'Cooldown esteso (secondi) applicato quando il circuit breaker provider scatta.', 'f'),
    ('provider.health_probe_timeout_s', '30', 'providers',
     'Timeout (secondi) per la singola chiamata del provider_health_probe. Oltre la soglia il provider e considerato slow.', 'f'),
    ('provider.slow_cooldown_s', '60', 'providers',
     'Cooldown breve (secondi) applicato a un provider slow/transient dal provider_health_probe.', 'f'),
    ('provider.outage_threshold', '3', 'providers',
     'Numero di provider falliti nello stesso round oltre cui si assume outage locale (rollback dei cooldown).', 'f')
ON CONFLICT (key) DO NOTHING;
