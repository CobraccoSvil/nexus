-- 0319_agentic_thinking_policy.sql  (ADR 0025, Parte 2)
--
-- "Thinking/reasoning" e' una MODALITA' PER-CHIAMATA, non una proprieta' fissa.
-- Il flag cieco `is_thinking` (mig 0317) escludeva dagli agentici modelli capaci
-- (es. deepseek-v4, dual-mode: puo' fare tool-loop in NON-THINKING mode).
--
-- Introduce un campo canonico che guida sia l'eleggibilita' agentica (Rust) sia
-- il toggle modalita' nel brain (adapter verticali):
--   none              -> non-thinking, tool normali.
--   disable_for_tools -> DUAL-MODE: nei run con tool l'adapter forza non-thinking;
--                        agentic-eligibile (deepseek-v4, claude 4.x, gemini-2.5).
--   native            -> reasoning con tool nativi senza forcing (OpenAI o1/o3/o4).
--   exclude           -> reasoning-only senza function calling (deepseek-reasoner):
--                        escluso dai run agentici.
--
-- Fonti ufficiali: DeepSeek thinking_mode (toggle extra_body.thinking; reasoner R1
-- no function calling); Anthropic extended thinking (no tool_choice forzato);
-- OpenAI o-series (tool nativi). Vedi ADR 0025.
--
-- Idempotente.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS agentic_thinking_policy text NOT NULL DEFAULT 'none';

ALTER TABLE ai_price_catalog DROP CONSTRAINT IF EXISTS chk_agentic_thinking_policy;
ALTER TABLE ai_price_catalog
    ADD CONSTRAINT chk_agentic_thinking_policy
    CHECK (agentic_thinking_policy IN ('none', 'disable_for_tools', 'native', 'exclude'));

-- Riconciliazione dei modelli correnti (la classificazione automatica del
-- catalog_sync manterra' questo per i futuri, vedi classify_capabilities).
-- exclude: reasoner-only senza tool.
UPDATE ai_price_catalog
SET agentic_thinking_policy = 'exclude', updated_at = NOW()
WHERE provider = 'deepseek' AND model LIKE 'deepseek-reasoner%';

-- native: reasoning OpenAI con tool nativi (o1/o3/o4).
UPDATE ai_price_catalog
SET agentic_thinking_policy = 'native', updated_at = NOW()
WHERE provider = 'openai' AND model ~ '^o[1-9]';

-- disable_for_tools: dual-mode (thinking + tool-capable in non-thinking mode).
UPDATE ai_price_catalog
SET agentic_thinking_policy = 'disable_for_tools', updated_at = NOW()
WHERE (provider = 'deepseek' AND model LIKE 'deepseek-v4%')
   OR (provider = 'anthropic' AND model ~ 'claude-(opus|sonnet|haiku)')
   OR (provider = 'google' AND model LIKE 'gemini-2.5%')
   OR (provider = 'mistral' AND model LIKE 'magistral%');

-- Tutto il resto resta 'none' (default): modelli non-thinking, tool normali.

-- Vista: espone il nuovo campo al brain (colonna aggiunta in coda, compatibile
-- con CREATE OR REPLACE VIEW). Stessa struttura della 0318 + agentic_thinking_policy.
CREATE OR REPLACE VIEW v_model_capabilities AS
SELECT cap.provider,
       cap.model,
       COALESCE(c.supports_tool_use, true)   AS tool_use,
       COALESCE(c.supports_vision, false)    AS vision,
       COALESCE(c.uses_thinking_mode, false) AS thinking,
       cap.max_context_tokens,
       cap.default_max_output_tokens,
       cap.max_output_tokens_hard,
       cap.tool_choice_style,
       cap.tool_choice_first_turn_force,
       cap.schema_strict,
       cap.schema_dialect,
       cap.tool_call_format,
       cap.max_tools_in_request,
       cap.supports_prompt_cache,
       cap.prompt_cache_dialect,
       cap.supports_parallel_tools,
       cap.stop_reason_dialect,
       cap.soft_failure_iter_threshold,
       cap.soft_failure_content_threshold,
       cap.history_keep_recent_messages,
       cap.history_max_old_tool_result_chars,
       cap.request_timeout_seconds,
       cap.connect_timeout_seconds,
       cap.tool_result_max_chars,
       cap.tool_result_max_bytes,
       cap.tool_result_max_lines,
       COALESCE(c.agentic_thinking_policy, 'none') AS agentic_thinking_policy
FROM nexus_provider_capabilities cap
LEFT JOIN ai_price_catalog c
       ON c.provider = cap.provider AND c.model = cap.model;
