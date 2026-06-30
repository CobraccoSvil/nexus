-- Migration 0032: Extended model catalog with capabilities + 5 providers (21 models)

-- Extend ai_price_catalog with capability columns
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS display_name TEXT NOT NULL DEFAULT '',
  ADD COLUMN IF NOT EXISTS context_window INTEGER NOT NULL DEFAULT 8192,
  ADD COLUMN IF NOT EXISTS performance_tier TEXT NOT NULL DEFAULT 'medium'
    CHECK (performance_tier IN ('light','medium','heavy')),
  ADD COLUMN IF NOT EXISTS speed_tier TEXT NOT NULL DEFAULT 'medium'
    CHECK (speed_tier IN ('fast','medium','slow')),
  ADD COLUMN IF NOT EXISTS capabilities JSONB NOT NULL DEFAULT '[]'::JSONB,
  ADD COLUMN IF NOT EXISTS supports_tool_use BOOLEAN NOT NULL DEFAULT TRUE,
  ADD COLUMN IF NOT EXISTS batch_discount_pct INTEGER NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS is_featured BOOLEAN NOT NULL DEFAULT FALSE;

-- Add UNIQUE constraint to allow ON CONFLICT upserts
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint WHERE conname = 'uq_price_catalog_provider_model'
  ) THEN
    ALTER TABLE ai_price_catalog
      ADD CONSTRAINT uq_price_catalog_provider_model UNIQUE (provider, model);
  END IF;
END $$;

-- Populate all 21 models across 5 providers
INSERT INTO ai_price_catalog
  (provider, model, display_name, input_cost_per_million_tokens, output_cost_per_million_tokens, currency,
   performance_tier, speed_tier, capabilities, context_window, supports_tool_use,
   batch_discount_pct, is_featured, is_enabled)
VALUES
  -- Anthropic: 4 generazioni
  ('anthropic','claude-opus-4-6','Claude Opus 4.6',
   5.0, 25.0, 'USD', 'heavy', 'slow',
   '["reasoning","architecture","code","docs"]', 200000, TRUE, 0, TRUE, TRUE),

  ('anthropic','claude-sonnet-4-6','Claude Sonnet 4.6',
   3.0, 15.0, 'USD', 'medium', 'medium',
   '["code","fix","refactor","docs"]', 200000, TRUE, 0, TRUE, TRUE),

  ('anthropic','claude-haiku-4-5-20251001','Claude Haiku 4.5',
   0.80, 4.0, 'USD', 'light', 'fast',
   '["code","chat","fix"]', 200000, TRUE, 0, TRUE, TRUE),

  ('anthropic','claude-3-haiku-20240307','Claude 3 Haiku (legacy)',
   0.25, 1.25, 'USD', 'light', 'fast',
   '["chat","simple"]', 200000, TRUE, 0, FALSE, TRUE),

  -- OpenAI: 6 modelli
  ('openai','o3','o3 (reasoning)',
   10.0, 40.0, 'USD', 'heavy', 'slow',
   '["reasoning","architecture","math"]', 200000, TRUE, 50, TRUE, TRUE),

  ('openai','o4-mini','o4-mini',
   1.10, 4.40, 'USD', 'medium', 'medium',
   '["reasoning","code","fix"]', 200000, TRUE, 50, TRUE, TRUE),

  ('openai','gpt-4.1','GPT-4.1',
   2.0, 8.0, 'USD', 'medium', 'medium',
   '["code","docs","chat"]', 1000000, TRUE, 50, TRUE, TRUE),

  ('openai','gpt-4.1-mini','GPT-4.1 Mini',
   0.40, 1.60, 'USD', 'light', 'fast',
   '["code","chat","fix","test"]', 1000000, TRUE, 50, TRUE, TRUE),

  ('openai','gpt-4.1-nano','GPT-4.1 Nano',
   0.10, 0.40, 'USD', 'light', 'fast',
   '["chat","simple"]', 1000000, TRUE, 50, TRUE, TRUE),

  ('openai','gpt-4o-mini','GPT-4o Mini',
   0.15, 0.60, 'USD', 'light', 'fast',
   '["chat","code"]', 128000, TRUE, 50, FALSE, TRUE),

  -- Google: 5 modelli
  ('google','gemini-2.5-pro','Gemini 2.5 Pro',
   1.25, 10.0, 'USD', 'medium', 'medium',
   '["reasoning","code","long-context"]', 1000000, TRUE, 50, TRUE, TRUE),

  ('google','gemini-2.5-flash','Gemini 2.5 Flash',
   0.15, 0.60, 'USD', 'light', 'fast',
   '["code","chat","fix"]', 1000000, TRUE, 50, TRUE, TRUE),

  ('google','gemini-2.5-flash-lite','Gemini 2.5 Flash-Lite',
   0.10, 0.40, 'USD', 'light', 'fast',
   '["chat","simple"]', 1000000, TRUE, 50, TRUE, TRUE),

  ('google','gemini-2.0-flash','Gemini 2.0 Flash',
   0.10, 0.40, 'USD', 'light', 'fast',
   '["code","chat"]', 1000000, TRUE, 50, FALSE, TRUE),

  ('google','gemini-1.5-flash','Gemini 1.5 Flash',
   0.075, 0.30, 'USD', 'light', 'fast',
   '["chat"]', 1000000, TRUE, 50, FALSE, TRUE),

  -- DeepSeek: 3 modelli
  ('deepseek','deepseek-chat','DeepSeek V3',
   0.28, 0.42, 'USD', 'medium', 'medium',
   '["code","chat","fix","refactor"]', 128000, TRUE, 0, TRUE, TRUE),

  ('deepseek','deepseek-reasoner','DeepSeek R1',
   0.55, 2.19, 'USD', 'heavy', 'slow',
   '["reasoning","architecture","math"]', 128000, FALSE, 0, TRUE, TRUE),

  ('deepseek','deepseek-coder','DeepSeek Coder',
   0.28, 0.42, 'USD', 'medium', 'medium',
   '["code","fix","test"]', 128000, TRUE, 0, TRUE, TRUE),

  -- Mistral: 4 modelli
  ('mistral','mistral-large-2411','Mistral Large',
   2.0, 6.0, 'USD', 'medium', 'medium',
   '["code","reasoning","docs"]', 131000, TRUE, 0, TRUE, TRUE),

  ('mistral','mistral-small-4','Mistral Small 4',
   0.15, 0.60, 'USD', 'light', 'fast',
   '["code","chat","fix"]', 262000, TRUE, 0, TRUE, TRUE),

  ('mistral','codestral','Codestral',
   0.20, 0.60, 'USD', 'medium', 'fast',
   '["code","fix","test"]', 256000, TRUE, 0, TRUE, TRUE),

  ('mistral','mistral-nemo','Mistral Nemo',
   0.15, 0.15, 'USD', 'light', 'fast',
   '["chat","simple"]', 128000, TRUE, 0, FALSE, TRUE)

ON CONFLICT (provider, model) DO UPDATE SET
  display_name                   = EXCLUDED.display_name,
  input_cost_per_million_tokens  = EXCLUDED.input_cost_per_million_tokens,
  output_cost_per_million_tokens = EXCLUDED.output_cost_per_million_tokens,
  performance_tier               = EXCLUDED.performance_tier,
  speed_tier                     = EXCLUDED.speed_tier,
  capabilities                   = EXCLUDED.capabilities,
  context_window                 = EXCLUDED.context_window,
  supports_tool_use              = EXCLUDED.supports_tool_use,
  batch_discount_pct             = EXCLUDED.batch_discount_pct,
  is_featured                    = EXCLUDED.is_featured;

-- New settings
INSERT INTO settings (key, value, category, description, is_secret) VALUES
  ('nexus_behavior_mode',       'bilanciata', 'routing',  'Modalità comportamento Nexus: veloce|economica|bilanciata|approfondita', FALSE),
  ('provider_model_deepseek',   'deepseek-chat',   'routing',  'Modello default DeepSeek',  FALSE),
  ('provider_model_mistral',    'mistral-small-4', 'routing',  'Modello default Mistral',   FALSE),
  ('model_catalog_last_sync',   '',                'routing',  'Timestamp ultimo sync catalogo da LiteLLM', FALSE),
  ('deepseek_api_key',          '',                'providers','DeepSeek API Key',           TRUE),
  ('mistral_api_key',           '',                'providers','Mistral API Key',            TRUE)
ON CONFLICT (key) DO NOTHING;

-- Update provider_hierarchy to include deepseek and mistral
UPDATE settings
SET value = 'anthropic,openai,google,deepseek,mistral'
WHERE key = 'provider_hierarchy'
  AND (value = 'anthropic,openai,google' OR value NOT LIKE '%deepseek%');
