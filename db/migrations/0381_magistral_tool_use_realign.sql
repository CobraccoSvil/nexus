-- 0381_magistral_tool_use_realign.sql
--
-- Riallinea la capability dei modelli Mistral magistral (linea reasoning).
-- Erano incoerenti tra varianti: supports_tool_use=true su alcune, false su
-- altre (magistral-medium-latest/-2509, magistral-small-2509).
--
-- ROOT CAUSE (verificata su DB + codice):
--   1. VERITA': i magistral SUPPORTANO function calling
--      (docs.mistral.ai/capabilities/function_calling elenca Magistral Small 1.2
--      e Medium 1.2 tra i modelli function-calling). Quindi supports_tool_use
--      deve essere TRUE per tutta la famiglia.
--   2. L'incoerenza nasce a RUNTIME dal tool-probe (model_health_probe,
--      mig 0269/0271) combinato con un catch-22:
--        a) in passato il tool-probe marco' supports_tool_use=false alcune
--           varianti magistral (consecutive_tool_failures fino a 36), ANCHE su
--           righe capability_source='manual' (non rispettava il guard manual);
--        b) una volta a false, il tool-probe NON ri-testava piu' quei modelli
--           (girava solo sui supports_tool_use=true) -> il re-enable era
--           IRRAGGIUNGIBILE e il degrado PERMANENTE, anche dopo che l'API
--           Mistral tornava a gestire correttamente il function calling. Le
--           varianti non degradate (o disabled, non testate) restavano true ->
--           incoerenza tra varianti.
--      Verificato live (2026-06): i magistral chiamano i tool col tool_choice
--      forzato MANTENENDO il reasoning attivo; reasoning_effort NON e' supportato
--      (HTTP 400). Il degrado era quindi un residuo storico mai recuperabile.
--
--   Fix nel codice (gia' applicato, regola H causa radice):
--     - model_health_probe: (i) il tool-probe rispetta capability_source='auto'
--       (non degrada/non conta fallimenti sulle righe manual); (ii) ri-testa i
--       modelli auto-degradati dal tool-probe stesso -> chiude il catch-22, il
--       re-enable torna raggiungibile.
--     - model_catalog_sync::classify_capabilities: supports_tool_use=true
--       uniforme per la famiglia magistral (ignora il metadata LiteLLM incoerente).
--
-- Questa migrazione allinea i DATI ESISTENTI e riporta capability_source='auto'
-- (come fece la mig 0340 per i Gemini 2.5): cosi' il prossimo catalog_sync
-- governa i magistral con la classificazione corretta, senza override manuali
-- incoerenti. NON tocca is_enabled (ortogonale, gestito da model_selection_policy).
-- Idempotente: la WHERE evita update inutili a re-applicazione.

BEGIN;

UPDATE ai_price_catalog
SET supports_tool_use         = true,
    is_thinking               = true,                 -- linea reasoning
    uses_thinking_mode        = true,                 -- thinking di default (spento nei tool)
    agentic_thinking_policy   = 'disable_for_tools',  -- non-thinking nei tool-loop
    consecutive_tool_failures = 0,                    -- reset del degrado del probe
    -- pulisce SOLO il motivo del tool-probe, preserva altri reason (es. policy)
    auto_disabled_reason      = CASE
                                    WHEN auto_disabled_reason LIKE 'tool_probe_failed:%'
                                        THEN NULL
                                    ELSE auto_disabled_reason
                                END,
    capability_source         = 'auto',
    updated_at                = NOW()
WHERE provider = 'mistral'
  AND model LIKE 'magistral%'
  AND (supports_tool_use IS DISTINCT FROM true
       OR is_thinking IS DISTINCT FROM true
       OR uses_thinking_mode IS DISTINCT FROM true
       OR agentic_thinking_policy IS DISTINCT FROM 'disable_for_tools'
       OR consecutive_tool_failures IS DISTINCT FROM 0
       OR capability_source IS DISTINCT FROM 'auto'
       OR auto_disabled_reason LIKE 'tool_probe_failed:%');

COMMIT;
