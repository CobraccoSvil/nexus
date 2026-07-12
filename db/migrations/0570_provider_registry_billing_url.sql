-- 0570_provider_registry_billing_url.sql
--
-- Consolida in DB (regola G/L) la mappa `billingUrls` finora hardcoded nel
-- frontend (apps/web-ide/components/settings/provider-settings.tsx). Aggiunge la
-- colonna `billing_url` a nexus_provider_registry e la popola per ogni provider
-- del registry. Cosi' la dashboard admin mostra il link "console billing"
-- data-driven per OGNI provider, inclusi i nuovi Groq/OpenRouter/Perplexity,
-- senza alcun elenco hardcoded lato TypeScript.
--
-- ADDITIVA e idempotente: ADD COLUMN IF NOT EXISTS + UPDATE per name. vllm
-- (self-host) resta NULL: nessuna console di billing.

ALTER TABLE nexus_provider_registry
    ADD COLUMN IF NOT EXISTS billing_url TEXT;

UPDATE nexus_provider_registry SET billing_url = CASE name
    WHEN 'anthropic'  THEN 'https://console.anthropic.com/settings/billing'
    WHEN 'openai'     THEN 'https://platform.openai.com/account/billing'
    WHEN 'google'     THEN 'https://console.cloud.google.com/billing'
    WHEN 'deepseek'   THEN 'https://platform.deepseek.com/api-keys'
    WHEN 'mistral'    THEN 'https://console.mistral.ai/api-keys'
    WHEN 'groq'       THEN 'https://console.groq.com/keys'
    WHEN 'openrouter' THEN 'https://openrouter.ai/settings/keys'
    WHEN 'perplexity' THEN 'https://www.perplexity.ai/settings/api'
    ELSE billing_url
END
WHERE name IN (
    'anthropic', 'openai', 'google', 'deepseek', 'mistral',
    'groq', 'openrouter', 'perplexity'
);
