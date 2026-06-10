-- 0389_deepseek_v4_tool_use_realign.sql
--
-- Riallinea la tool-capability dei modelli DeepSeek V4 (deepseek-v4-pro,
-- deepseek-v4-flash): in catalog erano supports_tool_use=FALSE con
-- capability_source='manual', ma il modello esegue regolarmente i tool nei
-- run agentici (v4-pro: run reali con 20-24 tool step il 2026-06-10).
--
-- ROOT CAUSE (verificata su DB + codice + probe API live):
--   1. VERITA': entrambi i V4 SUPPORTANO function calling. Probe diretto
--      all'API DeepSeek (2026-06-10): finish_reason=tool_calls, tool call ben
--      formata, reasoning attivo insieme ai tool. Nel tool-loop l'adapter li
--      fa girare non-thinking (extra_body.thinking=disabled, policy
--      'disable_for_tools', ADR 0025).
--   2. Il FALSE nasce dal tracking runtime dei tool-failure
--      (chat_messages/agent_run, mig 0269): i run hollow del thinking mode
--      (0 step, content vuoto — problema di budget reasoning nelle richieste,
--      NON di function calling) incrementavano consecutive_tool_failures e a
--      soglia scrivevano supports_tool_use=false + reason
--      'malformed_tool_calls' SENZA il guard capability_source='auto' che la
--      mig 0381 aveva imposto al tool-probe. Le righe deepseek-v4, curate
--      'manual' dalla mig 0318 (protezione flag thinking), venivano quindi
--      degradate dall'unico writer rimasto senza guard.
--   3. Catch-22 incrociato: un degrado runtime ('malformed_tool_calls') non
--      veniva MAI ri-testato dal tool-probe (che ri-testava solo i degradi
--      'tool_probe_failed:%') ne' riabilitato dal suo reset. La riabilitazione
--      dipendeva solo da un run reale riuscito — possibile per v4-pro (ancora
--      in routing matrix) ma instabile, impossibile per modelli fuori matrix.
--
--   Fix nel codice (gia' applicato, regola H causa radice):
--     - nuovo punto unico crates/mcp-core/src/tool_capability.rs (regola L):
--       increment counter + degrado a soglia + ripristino con guard
--       capability_source='auto' in UN solo posto; runtime e probe delegano.
--     - il tool-probe ri-testa anche i degradi runtime e il reset-su-successo
--       pulisce entrambe le reason (chiude il catch-22 incrociato).
--     - model_catalog_sync::classify_capabilities: supports_tool_use=true
--       uniforme per la famiglia deepseek-v4 (come per i magistral, mig 0381).
--
-- Questa migrazione allinea i DATI ESISTENTI e riporta capability_source='auto'
-- (pattern mig 0340/0381): il classificatore auto ora riproduce per intero la
-- curatela dei V4 (is_thinking=true, uses_thinking_mode=true,
-- agentic_thinking_policy='disable_for_tools', supports_tool_use=true), quindi
-- il prossimo catalog_sync li governa senza override manuali. NON tocca
-- is_enabled (ortogonale). Idempotente: la WHERE evita update inutili.
--
-- NOTA pre-ordinamento (model_routing::best_model_for_tier, ramo non-agentico):
-- con supports_tool_use=true i V4 escono dal predicato di retrocessione
-- "(uses_thinking_mode AND NOT supports_tool_use)", che torna a coprire solo i
-- veri reasoner-puri. Il rischio hollow sulle chiamate testuali dei V4 resta
-- mitigato dal cascade fallback su soft-failure e dal counter hollow generico
-- (consecutive_failures -> is_enabled=false a soglia).

BEGIN;

UPDATE ai_price_catalog
SET supports_tool_use         = true,
    is_thinking               = true,                 -- esclusione concetto A invariata
    uses_thinking_mode        = true,                 -- thinking di default
    agentic_thinking_policy   = 'disable_for_tools',  -- non-thinking nei tool-loop
    consecutive_tool_failures = 0,                    -- reset del degrado runtime
    -- pulisce SOLO i motivi dei writer automatici, preserva altri reason
    auto_disabled_reason      = CASE
                                    WHEN auto_disabled_reason = 'malformed_tool_calls'
                                      OR auto_disabled_reason LIKE 'tool_probe_failed:%'
                                        THEN NULL
                                    ELSE auto_disabled_reason
                                END,
    capability_source         = 'auto',
    updated_at                = NOW()
WHERE provider = 'deepseek'
  AND model IN ('deepseek-v4-pro', 'deepseek-v4-flash')
  AND (supports_tool_use IS DISTINCT FROM true
       OR is_thinking IS DISTINCT FROM true
       OR uses_thinking_mode IS DISTINCT FROM true
       OR agentic_thinking_policy IS DISTINCT FROM 'disable_for_tools'
       OR consecutive_tool_failures IS DISTINCT FROM 0
       OR capability_source IS DISTINCT FROM 'auto'
       OR auto_disabled_reason = 'malformed_tool_calls'
       OR auto_disabled_reason LIKE 'tool_probe_failed:%');

COMMIT;
