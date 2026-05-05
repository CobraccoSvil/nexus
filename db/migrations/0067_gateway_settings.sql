-- Aggiunge le chiavi di configurazione del gateway nella tabella settings.
-- Tutti i valori sono configurabili dall'admin panel senza rebuild.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    -- Supervisor AI
    ('supervisor_provider',              'google',        'gateway', 'Provider usato dal supervisor AI (es: google, anthropic)',                    FALSE),
    ('supervisor_model',                 'gemini-2.5-flash', 'gateway', 'Modello usato dal supervisor AI',                                          FALSE),
    -- Rate limits (per tenant)
    ('rate_limit_per_tenant_requests',   '1000',          'gateway', 'Max richieste per tenant per finestra temporale',                             FALSE),
    ('rate_limit_per_tenant_window_ms',  '60000',         'gateway', 'Durata finestra rate limit tenant (ms)',                                      FALSE),
    -- Rate limits (per provider)
    ('rate_limit_per_provider_requests', '500',           'gateway', 'Max richieste per provider per finestra temporale',                           FALSE),
    ('rate_limit_per_provider_window_ms','60000',         'gateway', 'Durata finestra rate limit provider (ms)',                                    FALSE),
    -- Health check
    ('health_check_interval_ms',         '60000',         'gateway', 'Intervallo health check provider (ms)',                                       FALSE),
    -- Default completion
    ('default_max_tokens',               '4096',          'gateway', 'Token massimi di default per completion (se non specificati nella richiesta)',FALSE)
ON CONFLICT (key) DO NOTHING;
