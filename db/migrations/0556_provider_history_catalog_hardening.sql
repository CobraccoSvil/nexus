-- 0556_provider_history_catalog_hardening.sql
--
-- Fix strutturale post-audit provider API (run f0ad0337):
--   1. Disabilita alias modello Mistral deprecati (mistral-small API-invalid);
--   2. Allinea context_window deepseek-v4-flash in ai_price_catalog (regola G);
--   3. Esclude modelli gia' marcati invalid_model dal pool agentico (defense in depth
--      oltre al filtro Rust in select_models_tierchain).
--
-- Idempotente. Regola H: niente UPDATE ad-hoc fuori migrazione.

-- 1. Alias API-invalid / deprecated (Mistral: mistral-small -> mistral-small-latest)
UPDATE ai_price_catalog
SET is_enabled = false,
    auto_disabled_at = COALESCE(auto_disabled_at, NOW()),
    auto_disabled_reason = COALESCE(
        NULLIF(auto_disabled_reason, ''),
        'invalid_model:deprecated_alias'
    ),
    updated_at = NOW()
WHERE provider = 'mistral'
  AND model IN ('mistral-small', 'mistral-small-2402', 'mistral-small-2407')
  AND is_enabled = true;

-- Reindirizza eventuali purpose/routing residui verso l'alias live
UPDATE nexus_purpose_model
SET model_id = 'mistral-small-latest',
    updated_at = NOW()
WHERE provider = 'mistral'
  AND model_id IN ('mistral-small', 'mistral-small-2402', 'mistral-small-2407');

UPDATE nexus_routing_matrix
SET model_id = 'mistral-small-latest',
    updated_at = NOW()
WHERE provider = 'mistral'
  AND model_id IN ('mistral-small', 'mistral-small-2402', 'mistral-small-2407');

-- 2. Context window deepseek-v4-flash (completa 0258 se catalog_sync ha resettato)
UPDATE ai_price_catalog
SET context_window = 131072,
    updated_at = NOW()
WHERE model IN ('deepseek-v4-flash', 'deepseek-v4-pro')
  AND (context_window IS NULL OR context_window < 131072);

UPDATE nexus_provider_capabilities
SET max_context_tokens = 131072,
    updated_at = NOW()
WHERE model IN ('deepseek-v4-flash', 'deepseek-v4-pro')
  AND max_context_tokens < 131072;

-- 3. Marca invalid_model gia' osservati in log (failover pool li escludera')
UPDATE ai_price_catalog
SET is_enabled = false,
    auto_disabled_at = COALESCE(auto_disabled_at, NOW()),
    auto_disabled_reason = COALESCE(
        NULLIF(auto_disabled_reason, ''),
        'invalid_model'
    ),
    updated_at = NOW()
WHERE is_enabled = true
  AND (
    (provider = 'mistral' AND model = 'mistral-small')
    OR auto_disabled_reason LIKE 'invalid_model%'
  );
