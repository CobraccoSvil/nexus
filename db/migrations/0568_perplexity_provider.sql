-- 0568_perplexity_provider.sql
-- Onboarding del provider Perplexity (Sonar: ricerca web citata / grounding),
-- OpenAI-compatibile, via il registry F2 (mig 0565). ZERO nuovo codice per la
-- REGISTRAZIONE. Perplexity e' un provider di RICERCA dedicato, NON un nodo
-- agentico: `sonar` RIFIUTA le richieste con tool definitions (HTTP 400), quindi
-- il provider dichiara supports_tools=false e i modelli hanno supports_tool_use=
-- false -> il selettore (require_tool_use) li esclude dai path agentici (garanzia
-- A4/T6: la selezione agentica non sceglie mai un modello che ripudia i tool).
--
-- ADDITIVO e OPT-IN (come Groq/OpenRouter): key vuota + modelli is_enabled=false
-- -> zero impatto sul routing e sul probe finche' l'admin non attiva.
--
-- PARZIALE per il valore distintivo: la propagazione delle CITAZIONI (array
-- top-level `citations` della risposta Perplexity) end-to-end fino al pannello
-- "Fonti consultate" e l'intent `ricerca_web` come flusso non-agentico sono lo
-- STEP SUCCESSIVO (toccano openai_compat.rs + il frontend). Qui solo il provider.
--
-- Prezzi $/Mtok (perplexity.ai, lug 2026) DA VERIFICARE prima di abilitare: oltre
-- ai token c'e' un request fee per "search context size" (basso/medio/alto) NON
-- modellato in ai_price_catalog. capability 'web_search' marcata per il futuro
-- flusso dedicato.

-- 1) Settings.
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('perplexity_api_key', '', 'providers',
     'API key Perplexity (Sonar: ricerca web citata). Vuota = provider inattivo.', true),
    ('perplexity_enabled', 'true', 'providers',
     'Abilita il provider Perplexity. Attivo solo se perplexity_api_key e'' presente.', false)
ON CONFLICT (key) DO NOTHING;

-- 2) Registry provider (mig 0565): openai_compat MA supports_tools=false (sonar
--    rifiuta le tool definitions). Il provider generico dichiara tools=false.
INSERT INTO nexus_provider_registry
    (name, api_format, key_setting, enabled_setting, base_url_setting, base_url_default, activation, tiers, max_context_tokens, supports_tools, sort_order)
VALUES
    ('perplexity', 'openai_compat', 'perplexity_api_key', 'perplexity_enabled', 'perplexity_base_url', 'https://api.perplexity.ai', 'api_key', '{0,1,2}', 200000, false, 90)
ON CONFLICT (name) DO NOTHING;

-- 3) Catalog: 3 modelli Sonar, DISABILITATI (opt-in). supports_tool_use=false
--    (garanzia agentica). capability 'web_search' per il futuro flusso dedicato.
INSERT INTO ai_price_catalog
    (provider, model, display_name, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, is_enabled, context_window, performance_tier, speed_tier, capabilities, supports_tool_use, capability_source)
VALUES
    ('perplexity', 'sonar',               'Sonar (Perplexity)',               1.00, 1.00,  'USD', false, 128000, 'medium', 'medium', '["chat","web_search"]'::jsonb, false, 'manual'),
    ('perplexity', 'sonar-pro',           'Sonar Pro (Perplexity)',           3.00, 15.00, 'USD', false, 200000, 'high',   'medium', '["chat","web_search"]'::jsonb, false, 'manual'),
    ('perplexity', 'sonar-reasoning-pro', 'Sonar Reasoning Pro (Perplexity)', 3.00, 15.00, 'USD', false, 128000, 'high',   'medium', '["chat","web_search"]'::jsonb, false, 'manual')
ON CONFLICT (provider, model) DO NOTHING;
