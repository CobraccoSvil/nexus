-- 0566_groq_provider.sql
-- Onboarding del provider Groq (inferenza OpenAI-compatibile ultra-veloce) via il
-- registry provider F2 (mig 0565): ZERO nuovo codice Rust, solo dati (regola G).
-- E' la dimostrazione che il registry funziona: un provider OpenAI-compat nuovo =
-- una riga nel registry + righe catalog, costruito dal provider generico.
--
-- ADDITIVO e OPT-IN, zero impatto sul sistema attuale:
--   - `groq_api_key` e' VUOTA -> il gateway non costruisce il provider (activation
--     'api_key' richiede chiave non vuota);
--   - i modelli sono `is_enabled=false` -> il routing dinamico non li seleziona e
--     l'health probe non li sonda (probed_providers legge is_enabled).
-- Per attivare Groq l'admin: (1) inserisce groq_api_key, (2) abilita i modelli
-- desiderati, (3) opzionale: instrada purpose interni ad alta frequenza
-- (chat_title/summarizer) su un modello Groq per la latenza (decisione di costo,
-- non forzata qui).
--
-- Model id e prezzi ($/Mtok) da groq.com/pricing + console.groq.com/docs/models
-- (luglio 2026). context 131072 per tutti. capability_source='manual' -> tier e
-- capacita' protetti dal catalog_sync/infer_tier (Groq non e' nel provider_map del
-- sync LiteLLM, T5: i prezzi si aggiornano a mano finche' T5 non e' data-driven).

-- 1) Settings: chiave (segreta) + flag. category='providers' per la dashboard
--    (fetch_api_key_configured legge category='providers' AND key LIKE '%_api_key').
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('groq_api_key', '', 'providers',
     'API key Groq (inferenza veloce, OpenAI-compatibile). Vuota = provider inattivo.', true),
    ('groq_enabled', 'true', 'providers',
     'Abilita il provider Groq. Attivo solo se groq_api_key e'' presente.', false)
ON CONFLICT (key) DO NOTHING;

-- 2) Registry provider (mig 0565): formato openai_compat -> provider generico.
INSERT INTO nexus_provider_registry
    (name, api_format, key_setting, enabled_setting, base_url_setting, base_url_default, activation, tiers, max_context_tokens, supports_tools, sort_order)
VALUES
    ('groq', 'openai_compat', 'groq_api_key', 'groq_enabled', 'groq_base_url', 'https://api.groq.com/openai/v1', 'api_key', '{0,1,2}', 131072, true, 70)
ON CONFLICT (name) DO NOTHING;

-- 3) Catalog: 4 modelli di produzione, DISABILITATI (opt-in). Nota: gli id
--    'openai/gpt-oss-*' contengono uno slash (come i vendor/model OpenRouter):
--    instradarli con model_id DIRETTO (niente alias logici, per non incrociare il
--    parser split('/') in model_alias_resolver).
INSERT INTO ai_price_catalog
    (provider, model, display_name, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, is_enabled, context_window, performance_tier, speed_tier, capabilities, supports_tool_use, capability_source)
VALUES
    ('groq', 'llama-3.1-8b-instant',    'Llama 3.1 8B (Groq)',  0.05,  0.08, 'USD', false, 131072, 'light',  'fast', '["chat","code"]'::jsonb, true, 'manual'),
    ('groq', 'llama-3.3-70b-versatile', 'Llama 3.3 70B (Groq)', 0.59,  0.79, 'USD', false, 131072, 'medium', 'fast', '["chat","code"]'::jsonb, true, 'manual'),
    ('groq', 'openai/gpt-oss-20b',      'GPT-OSS 20B (Groq)',   0.075, 0.30, 'USD', false, 131072, 'light',  'fast', '["chat","code"]'::jsonb, true, 'manual'),
    ('groq', 'openai/gpt-oss-120b',     'GPT-OSS 120B (Groq)',  0.15,  0.60, 'USD', false, 131072, 'high',   'fast', '["chat","code"]'::jsonb, true, 'manual')
ON CONFLICT (provider, model) DO NOTHING;
