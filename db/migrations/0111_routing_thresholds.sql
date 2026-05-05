-- Migrazione 0111: settings.routing.* per parametri configurabili del routing
--
-- Sposta in DB le costanti hardcoded in:
--   - crates/mcp-core/src/orchestrator.rs:247  LLM_CLASSIFIER_MIN_CONFIDENCE
--   - brain/router/agentic_classifier.py:38-43 CACHE_TTL_SECONDS, CLASSIFIER_PROVIDER, CLASSIFIER_MODEL, ecc.
--   - crates/mcp-core/src/orchestrator.rs:455-460 token thresholds 3000/6000
--   - crates/mcp-core/src/orchestrator.rs:559-562 token thresholds 400/1500 per chat_breve/media/lunga
--
-- Cambiare il classifier model diventa un UPDATE su settings + 60s di refresh
-- cache, niente patch+redeploy.
-- Riusa la tabella settings esistente con prefisso 'routing.'.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('routing.llm_classifier_min_confidence', '0.60', 'routing',
     'Soglia confidence sotto cui il risultato del classifier LLM viene scartato e si usa il fallback keyword. Valore [0.0, 1.0].',
     false),
    ('routing.llm_classifier_timeout_seconds', '5.0', 'routing',
     'Timeout in secondi per la chiamata HTTP al classifier LLM (POST /classify-intent-agentic). Su timeout, fallback keyword.',
     false),
    ('routing.classifier_cache_ttl_seconds', '86400', 'routing',
     'TTL della cache in-memory del classifier LLM (default 24h). Riduce le chiamate ripetute LLM su prompt identici.',
     false),
    ('routing.classifier_cache_max_entries', '10000', 'routing',
     'Numero massimo entry nella cache LRU del classifier LLM.',
     false),
    ('routing.classifier_provider', 'google', 'routing',
     'Provider per il classifier intent agentic (deve esistere in nexus_provider_default_model).',
     false),
    ('routing.classifier_model', 'gemini-2.5-flash', 'routing',
     'Modello specifico per il classifier intent agentic. Cambiare con UPDATE.',
     false),
    ('routing.token_threshold_chat_breve', '400', 'routing',
     'Soglia in token sotto cui chat e considerata breve (chat_breve key in routing matrix).',
     false),
    ('routing.token_threshold_chat_media', '1500', 'routing',
     'Soglia in token sopra cui chat passa da media a lunga.',
     false),
    ('routing.token_threshold_complex_fix', '3000', 'routing',
     'Soglia in token sopra cui fix/refactor passa da fix_semplice a fix_complesso (route_model_with_mode).',
     false),
    ('routing.token_threshold_long_context', '6000', 'routing',
     'Soglia in token sopra cui anche intent generici (chat) richiedono modello tier=medium nel catalog dynamic routing.',
     false)
ON CONFLICT (key) DO NOTHING;
