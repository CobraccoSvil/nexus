-- Aggiunge le impostazioni enable/disable per ogni provider LLM.
-- Tutti i provider sono abilitati per default (value = 'true').

INSERT INTO settings (key, value, category, description, is_secret, updated_at)
VALUES
    ('anthropic_enabled', 'true', 'providers', 'Abilita il provider Anthropic (Claude)', false, NOW()),
    ('openai_enabled',    'true', 'providers', 'Abilita il provider OpenAI (GPT)',       false, NOW()),
    ('google_enabled',    'true', 'providers', 'Abilita il provider Google (Gemini)',     false, NOW()),
    ('deepseek_enabled',  'true', 'providers', 'Abilita il provider DeepSeek',            false, NOW()),
    ('mistral_enabled',   'true', 'providers', 'Abilita il provider Mistral',             false, NOW())
ON CONFLICT (key) DO NOTHING;
