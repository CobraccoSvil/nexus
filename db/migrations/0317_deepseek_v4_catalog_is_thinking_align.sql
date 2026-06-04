-- 0317_deepseek_v4_catalog_is_thinking_align.sql
--
-- Root cause (incidente reale, run eec9bffe, task "Fix AdminLoginPage"):
-- i modelli DeepSeek V4 (deepseek-v4-pro, deepseek-v4-flash) girano in
-- "thinking mode". L'API DeepSeek lo conferma: su una conversazione agentica
-- multi-turno risponde
--   HTTP 400 invalid_request: "The reasoning_content in the thinking mode must
--   be passed back to the API."
-- e il provider Python (brain/providers) NON implementa il passback di
-- reasoning_content -> ogni run agentico instradato su deepseek-v4-pro fallisce
-- subito (descrive il piano senza eseguire i tool, chat "bloccata").
--
-- Drift di dato che ha causato il routing errato:
--   - nexus_provider_capabilities.thinking = TRUE  (corretto, fissato da mig 0256)
--   - ai_price_catalog.is_thinking          = FALSE (MAI allineato)
-- Il capability-gate del routing (best_model_for_tier, apply_tool_use_capability_gate)
-- legge ai_price_catalog.is_thinking: vedendo FALSE NON escludeva deepseek-v4-pro
-- dai run agentici, e la routing matrix lo aveva persino promosso primo su tutti
-- gli intent (vedi mig 0274, che lo trattava erroneamente come "non-thinking
-- tool-robust").
--
-- Fix definitivo (regola G/H, niente UPDATE ad-hoc fuori migrazione):
-- allinea ai_price_catalog.is_thinking alla realta' del modello e alla
-- capabilities table. Combinato col fix di codice nel capability-gate
-- (decide_tool_capability_gate ora esclude is_thinking=true dai run agentici),
-- i modelli DeepSeek V4 vengono reindirizzati automaticamente a runtime verso il
-- miglior modello NON-thinking tool-capable fuori cooldown (best_model_for_tier),
-- senza dover riscrivere a mano le priorita' della routing matrix.
--
-- Nota: questo NON disabilita deepseek-v4-pro. Resta usabile per gli intent
-- non-agentici (chat), dove il tool_choice non e' forzato. Il gate interviene
-- solo sui run agentici (intent != chat).
--
-- Idempotente.

UPDATE ai_price_catalog
SET is_thinking = TRUE,
    updated_at = NOW()
WHERE provider = 'deepseek'
  AND model IN ('deepseek-v4-pro', 'deepseek-v4-flash')
  AND is_thinking IS DISTINCT FROM TRUE;
