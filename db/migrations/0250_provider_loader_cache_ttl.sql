-- 0250_provider_loader_cache_ttl.sql
-- Fase F (hardcode sweep) — TTL di cache dei loader provider resi DB-driven.
--
-- api_key_loader e catalog_loader avevano _TTL_S = 60.0 hardcoded; ora leggono
-- il TTL da settings (come gia' faceva capability_loader). I timeout HTTP di
-- rete (ollama, dns_transport, cooldown_bridge) restano nel codice: sono
-- protezioni infrastrutturali, non parametri di business. Idempotente.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('providers.api_key_cache_ttl_seconds', '60', 'providers',
     'TTL (secondi) della cache delle API key provider in brain/providers/api_key_loader.py.', 'f'),
    ('providers.catalog_cache_ttl_seconds', '60', 'providers',
     'TTL (secondi) della cache del catalogo modelli provider in brain/providers/catalog_loader.py.', 'f'),
    ('providers.capability_cache_ttl_seconds', '60', 'providers',
     'TTL (secondi) della cache delle capability provider in brain/providers/capability_loader.py.', 'f')
ON CONFLICT (key) DO NOTHING;
