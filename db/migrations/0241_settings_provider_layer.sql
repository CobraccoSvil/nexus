-- 0241_settings_provider_layer.sql
-- M0 (provider abstraction) — Settings globali del layer provider.
-- TTL cache, timeout HTTP infrastrutturali, soglie compressione schema, soglie
-- salute provider. Regola G: nessun fallback hardcoded nel codice.
-- Valori derivati dallo stato applicato in produzione. Idempotente.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('providers.api_key_cache_ttl_seconds', '60', 'providers', 'TTL cache api_key_loader (H-07)', 'f'),
    ('providers.billing_cooldown_seconds', '600', 'providers', 'Durata cooldown billing-error per provider (H-11)', 'f'),
    ('providers.catalog_cache_ttl_seconds', '60', 'providers', 'TTL cache ai_price_catalog (H-08)', 'f'),
    ('providers.cooldown_bridge_timeout_seconds', '5', 'providers', 'Timeout HTTP cooldown bridge (H-09)', 'f'),
    ('providers.cooldown_circuit_breaker_threshold', '3', 'providers', 'Soglia consecutive failure → circuit breaker (H-78)', 'f'),
    ('providers.dns_timeout_seconds', '5', 'providers', 'Timeout DNS resolver in dns_transport (H-10)', 'f'),
    ('providers.health_probe_max_tokens', '10', 'providers', 'Max tokens per health probe Anthropic (H-05)', 'f'),
    ('providers.health_probe_outage_threshold', '3', 'providers', 'Soglia consecutive failure → outage (H-77)', 'f'),
    ('providers.ollama.list_timeout_seconds', '3', 'providers', 'Timeout Ollama list_models (H-22)', 'f'),
    ('providers.quota_cooldown_seconds', '3600', 'providers', 'Durata (s) del cooldown locale del brain per quota/credito esaurito persistente (insufficient_quota). Piu lungo del transitorio. DB-driven, cache 60s.', 'f'),
    ('providers.test_connection_timeout_seconds', '15', 'providers', 'Timeout test_connection in sync wrap (H-12)', 'f'),
    ('providers.thinking_models_ttl_seconds', '60', 'providers', 'TTL cache modulo Anthropic per detection modelli con thinking abilitato (H-01)', 'f'),
    ('schema.descr_max', '200', 'schema', 'Max char per description di property in JSON Schema (H-17)', 'f'),
    ('schema.enum_max', '10', 'schema', 'Max numero enum values prima del troncamento (H-18)', 'f'),
    ('schema.tool_descr_max', '400', 'schema', 'Max char per tool description (H-19)', 'f')
ON CONFLICT (key) DO NOTHING;
