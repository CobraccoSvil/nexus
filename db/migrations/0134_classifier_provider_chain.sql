-- 0134: Chain di provider per il classifier agentico
--
-- Problema risolto: prima il classifier usava UN SOLO provider/model letto
-- da `settings.routing.classifier_provider/model` (mig 0111+0132). Quando
-- quel provider va giu' (throttling, quota, network), il classifier cade
-- sul safety-net keyword-based — perdita di precisione.
--
-- Pattern: identico a quello gia' usato per `nexus_provider_default_model`
-- (mig 0101) e `nexus_routing_matrix` (mig 0101 multi-provider). La chain
-- viene scorsa per priority DESC; il primo provider NON in cooldown e che
-- risponde con JSON valido vince. Se tutti falliscono → fallback keyword.
--
-- Le settings `routing.classifier_provider/model` (mig 0132) restano come
-- legacy compatibility: se la chain e' vuota, mcp-core/brain le usano come
-- chain a 1 elemento.

BEGIN;

CREATE TABLE IF NOT EXISTS nexus_classifier_provider_chain (
    id          BIGSERIAL PRIMARY KEY,
    provider    TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    priority    INT  NOT NULL DEFAULT 100,
    is_active   BOOLEAN NOT NULL DEFAULT TRUE,
    rationale   TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (provider, model_id)
);

CREATE INDEX IF NOT EXISTS idx_classifier_chain_priority
    ON nexus_classifier_provider_chain (priority DESC)
    WHERE is_active = TRUE;

-- ─────────────────────────────────────────────────────────────────────
-- SEED: chain per uso normale.
-- ─────────────────────────────────────────────────────────────────────
-- Criteri: modelli LIGHT/FAST per non sprecare token su una classificazione.
-- Ordine: piu' economici/veloci prima.
INSERT INTO nexus_classifier_provider_chain
    (provider, model_id, priority, rationale) VALUES
    -- Primary: Gemini Flash (cost~$0.075/MTok, fast, generalmente disponibile)
    ('google', 'gemini-2.5-flash', 100,
     'Preferito: piu economico, veloce, buona qualita JSON output'),
    -- Fallback 1: Mistral Small (cost~$0.20/MTok, ottimo JSON mode)
    ('mistral', 'mistral-small-latest', 90,
     'Fallback: Mistral Small ha ottima resa su JSON structured output'),
    -- Fallback 2: OpenAI gpt-4.1-mini (cost~$0.40/MTok, robusto)
    ('openai', 'gpt-4.1-mini', 80,
     'Fallback: GPT-4.1-mini molto robusto su istruzioni JSON-only'),
    -- Fallback 3: DeepSeek chat (cost~$0.27/MTok)
    ('deepseek', 'deepseek-chat', 70,
     'Fallback: DeepSeek chat economico'),
    -- Fallback 4: Anthropic Haiku (cost~$0.80/MTok, sempre affidabile)
    ('anthropic', 'claude-haiku-4-5-20251001', 60,
     'Ultimo fallback: Haiku affidabile ma piu costoso')
ON CONFLICT (provider, model_id) DO NOTHING;

COMMIT;
