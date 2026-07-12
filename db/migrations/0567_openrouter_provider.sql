-- 0567_openrouter_provider.sql
-- Onboarding del provider OpenRouter (gateway orizzontale verso 300+ modelli,
-- OpenAI-compatibile) via il registry F2 (mig 0565): ZERO nuovo codice Rust.
-- OpenRouter e' TRANSPORT: i model id restano nel DB (regola G); si usa per
-- copertura e modelli emergenti (Grok, GLM, Kimi, Qwen...), non per i volumi gia'
-- coperti dai provider diretti (markup OpenRouter ~5.5%).
--
-- ADDITIVO e OPT-IN (come Groq, mig 0566): key vuota -> gateway non costruisce il
-- provider; modelli is_enabled=false -> non selezionati dal routing ne' sondati.
-- L'admin: (1) inserisce openrouter_api_key, (2) aggiunge/abilita i model id
-- desiderati (OpenRouter ne ha centinaia: qui solo 2 esempi, non si inonda il
-- catalog — coerente col filtro/whitelist previsto per il sync).
--
-- NOTA header (Parte B, attrito 1): OpenRouter RACCOMANDA gli header HTTP-Referer
-- e X-Title (ranking/attribuzione), ma NON sono obbligatori: l'API funziona con il
-- solo Bearer. Restano un miglioramento opzionale (campo extra_headers nel client
-- OpenAiCompatClient + registry), non necessario per l'uso base.
--
-- NOTA model id: OpenRouter usa id `vendor/model` con slash (come groq gpt-oss):
-- instradare con model_id DIRETTO (niente alias logici -> parser split('/')).
-- Prezzi $/Mtok da openrouter.ai/api/v1/models (luglio 2026); verificare prima di
-- abilitare (i prezzi OpenRouter includono il markup).

-- 1) Settings.
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('openrouter_api_key', '', 'providers',
     'API key OpenRouter (gateway verso 300+ modelli, OpenAI-compatibile). Vuota = provider inattivo.', true),
    ('openrouter_enabled', 'true', 'providers',
     'Abilita il provider OpenRouter. Attivo solo se openrouter_api_key e'' presente.', false)
ON CONFLICT (key) DO NOTHING;

-- 2) Registry provider (mig 0565): formato openai_compat -> provider generico.
INSERT INTO nexus_provider_registry
    (name, api_format, key_setting, enabled_setting, base_url_setting, base_url_default, activation, tiers, max_context_tokens, supports_tools, sort_order)
VALUES
    ('openrouter', 'openai_compat', 'openrouter_api_key', 'openrouter_enabled', 'openrouter_base_url', 'https://openrouter.ai/api/v1', 'api_key', '{0,1,2}', 500000, true, 80)
ON CONFLICT (name) DO NOTHING;

-- 3) Catalog: 2 modelli emergenti come ESEMPIO, DISABILITATI (opt-in). L'admin
--    aggiunge gli altri model id OpenRouter che gli servono.
INSERT INTO ai_price_catalog
    (provider, model, display_name, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, is_enabled, context_window, performance_tier, speed_tier, capabilities, supports_tool_use, capability_source)
VALUES
    ('openrouter', 'x-ai/grok-4.5', 'Grok 4.5 (OpenRouter)', 2.00, 6.00, 'USD', false, 500000,  'frontier', 'medium', '["chat","code"]'::jsonb, true, 'manual'),
    ('openrouter', 'z-ai/glm-5.2',  'GLM 5.2 (OpenRouter)',  0.42, 1.32, 'USD', false, 1048576, 'high',     'medium', '["chat","code"]'::jsonb, true, 'manual')
ON CONFLICT (provider, model) DO NOTHING;
