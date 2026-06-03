-- 0241_settings_provider_layer.sql
-- M0 (provider abstraction) — Settings globali del layer provider/agent.
--
-- Ridirige a DB tutte le costanti globali (non per-modello) che erano hardcoded:
-- TTL delle cache, timeout HTTP infrastrutturali, soglie compressione schema,
-- max_tokens delle chiamate LLM interne agli agent. Regola G: nessun fallback
-- hardcoded nel codice; questi valori riproducono il comportamento attuale.
--
-- I valori per-modello (max_output_tokens risposta, tool_choice, dialetti) NON
-- stanno qui ma in nexus_provider_capabilities (mig 0240).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    -- TTL cache provider (secondi)
    ('providers.api_key_cache_ttl_seconds',      '60',  'providers', 'TTL cache chiavi API provider', FALSE),
    ('providers.catalog_cache_ttl_seconds',      '60',  'providers', 'TTL cache catalog provider (ai_price_catalog)', FALSE),
    ('providers.capability_cache_ttl_seconds',   '60',  'providers', 'TTL cache nexus_provider_capabilities', FALSE),
    ('providers.thinking_models_ttl_seconds',    '60',  'providers', 'TTL cache elenco modelli thinking', FALSE),
    ('providers.soft_failure_cache_ttl_seconds', '60',  'providers', 'TTL cache config soft-failure', FALSE),
    ('providers.price_cache_ttl_seconds',        '300', 'providers', 'TTL cache prezzi per record usage', FALSE),

    -- Timeout HTTP infrastrutturali (secondi)
    ('providers.default_request_timeout_seconds', '90', 'providers', 'Timeout default turno agente (override per-modello in capabilities)', FALSE),
    ('providers.dns_timeout_seconds',             '5',  'providers', 'Timeout risoluzione DNS custom resolver', FALSE),
    ('providers.cooldown_bridge_timeout_seconds', '5',  'providers', 'Timeout HTTP bridge cooldown verso mcp-core', FALSE),
    ('providers.ollama.list_timeout_seconds',     '3',  'providers', 'Timeout list_models Ollama (/api/tags)', FALSE),
    ('providers.ollama.generate_timeout_seconds', '120','providers', 'Timeout generate/chat Ollama', FALSE),

    -- Salute provider
    ('providers.billing_cooldown_seconds',  '600', 'providers', 'Durata cooldown su billing_error prima del retry', FALSE),
    ('providers.health_probe_max_tokens',   '10',  'providers', 'max_tokens della sonda di salute provider', FALSE),

    -- Compressione schema tool (default globali; override per-modello in capabilities)
    ('schema.descr_max',      '200', 'providers', 'Lunghezza massima description nei property schema', FALSE),
    ('schema.enum_max',       '10',  'providers', 'Numero massimo valori enum prima del troncamento', FALSE),
    ('schema.tool_descr_max', '400', 'providers', 'Lunghezza massima description di un tool', FALSE),

    -- Tool choice
    ('agent.firstturn.tool_choice_force', 'true', 'agent', 'Forza tool_choice al primo turno per intent con allegati strutturati', FALSE),

    -- max_tokens chiamate LLM interne agli agent (M5: H-21..H-26)
    ('agent.clarify_max_tokens',        '400',  'agent', 'max_tokens nodo clarify/intake', FALSE),
    ('agent.planner_full_max_tokens',   '4096', 'agent', 'max_tokens planner (piano completo)', FALSE),
    ('agent.planner_short_max_tokens',  '512',  'agent', 'max_tokens planner (piano breve)', FALSE),
    ('agent.summarizer_max_tokens',     '800',  'agent', 'max_tokens summarizer', FALSE),
    ('agent.subagent.default_max_iterations', '25', 'agent', 'Iterazioni default subagent (override YAML)', FALSE)
ON CONFLICT (key) DO NOTHING;
