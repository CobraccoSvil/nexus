-- 0575_provider_registry_litellm_prefixes.sql
--
-- Rende data-driven il catalog sync da LiteLLM (era la mappa prefisso->provider
-- hardcoded a 5 in crates/mcp-core/src/models.rs, punto T5): i prefissi di
-- matching e la politica di inserimento vivono nel registry (regola G/L), cosi'
-- i provider onboardati (Groq/OpenRouter/Perplexity) ricevono l'aggiornamento
-- prezzi dal sync senza toccare il codice.
--
-- ADDITIVA e idempotente.
--   - litellm_prefixes: array di prefissi del model_id LiteLLM che appartengono
--     al provider. NULL = provider escluso dal sync (vllm/ollama self-host).
--   - litellm_sync_inserts: se false, il sync AGGIORNA solo i prezzi/context dei
--     modelli GIA' presenti nel catalog e NON auto-inserisce modelli nuovi. Serve
--     ai provider con listino curato a mano (LiteLLM espone centinaia di modelli
--     openrouter/*: l'import indiscriminato inquinerebbe il catalog e il routing).

ALTER TABLE nexus_provider_registry
    ADD COLUMN IF NOT EXISTS litellm_prefixes    TEXT[],
    ADD COLUMN IF NOT EXISTS litellm_sync_inserts BOOLEAN NOT NULL DEFAULT true;

-- Prefissi: replica ESATTA della mappa hardcoded per i 5 storici (zero cambi di
-- comportamento) + i 3 nuovi provider.
UPDATE nexus_provider_registry SET litellm_prefixes = CASE name
    WHEN 'anthropic'  THEN ARRAY['claude-']
    WHEN 'openai'     THEN ARRAY['gpt-', 'o1', 'o3', 'o4']
    WHEN 'google'     THEN ARRAY['gemini/']
    WHEN 'deepseek'   THEN ARRAY['deepseek/']
    WHEN 'mistral'    THEN ARRAY['mistral/', 'codestral/']
    WHEN 'groq'       THEN ARRAY['groq/']
    WHEN 'openrouter' THEN ARRAY['openrouter/']
    WHEN 'perplexity' THEN ARRAY['perplexity/']
    ELSE litellm_prefixes
END
WHERE name IN (
    'anthropic', 'openai', 'google', 'deepseek', 'mistral',
    'groq', 'openrouter', 'perplexity'
);

-- Provider a listino curato: solo aggiornamento prezzi, niente auto-insert.
UPDATE nexus_provider_registry SET litellm_sync_inserts = false
    WHERE name IN ('groq', 'openrouter', 'perplexity');
