-- 0584_openrouter_medium_reasoning_models.sql
--
-- Completa la COPERTURA DI CATALOG dei nuovi provider nella fascia (tier=medium,
-- capability=reasoning), che restava scoperta: gli intent agentici medium+reasoning
-- (agentic_default, file_ops) non trovavano alcun candidato groq/openrouter perche'
-- il seed iniziale (mig 0566-0568) non includeva modelli reasoning di fascia media.
-- Il routing per tier (select_models_tierchain) e' il decisore e resta invariato: gli
-- si forniscono i CANDIDATI mancanti, non un pin per intent (regola L).
--
-- Curazione DATA-DRIVEN: i due modelli sono stati verificati via l'endpoint
-- list-models di OpenRouter (fatto oggettivo, non preferenza): esistono, espongono
-- supported_parameters con 'tools' (require_tool_use agentico) e 'reasoning', con i
-- prezzi qui sotto. Scelti solidi (famiglie note Qwen/GLM), piu' economici sull'input
-- di deepseek-v4-flash (0.14) cosi' il routing cost-first (input_cost ASC) li puo'
-- preferire nel tier medium. NB: groq NON e' stato curato qui: il suo endpoint
-- list-models e' irraggiungibile dall'ambiente di sviluppo (Cloudflare 403 error 1010),
-- quindi i suoi modelli reasoning non sono verificabili senza rischio di model-id stale.
--
-- pricing_state non e' specificato: il trigger di mig 0583 lo deriva a 'priced' (cost>0).
-- is_enabled=true: OpenRouter e' gia' attivo (openrouter_enabled='true', altri modelli
-- openrouter gia' abilitati); i nuovi entrano subito nella selezione (cache 60s).

INSERT INTO ai_price_catalog
    (provider, model, display_name,
     input_cost_per_million_tokens, output_cost_per_million_tokens, currency,
     is_enabled, context_window, performance_tier, speed_tier,
     capabilities, supports_tool_use, capability_source)
VALUES
    ('openrouter', 'qwen/qwen3-32b', 'Qwen3 32B (OpenRouter)',
     0.08, 0.28, 'USD',
     true, 131072, 'medium', 'medium',
     '["chat","code","reasoning"]'::jsonb, true, 'manual'),
    ('openrouter', 'z-ai/glm-4.7-flash', 'GLM 4.7 Flash (OpenRouter)',
     0.06, 0.40, 'USD',
     true, 202752, 'medium', 'fast',
     '["chat","code","reasoning"]'::jsonb, true, 'manual')
ON CONFLICT (provider, model) DO NOTHING;
