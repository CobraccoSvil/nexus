-- 0267: purpose 'planner_fallback' per il fallback tool-robust del planner.
--
-- Root cause: planner_node forza tool_choice su nexus_todo_write. Se il modello
-- primario (nexus_purpose_model.planner = mistral/mistral-large-latest, mig 0266)
-- NON emette la tool call -- tipico dei modelli thinking che ritornano
-- finish_reason MALFORMED_FUNCTION_CALL con output vuoto, ma puo' capitare anche
-- per degrado transitorio del provider primario -- il planner skippava subito e
-- il run a valle degenerava in "Nessuna risposta dal provider".
--
-- Fix: prima di rinunciare, planner_node tenta UNA sola volta con il modello
-- risolto qui (DB-driven, regola G). Niente ricorsione/loop.
--
-- Scelta modello: deepseek/deepseek-v4-pro.
--   - is_enabled = true, supports_tool_use = true (ai_price_catalog).
--   - NON-thinking: nessun conflitto thinking + tool_choice forzato.
--   - Provider DIVERSO dal primario (mistral): se mistral e' degradato/in
--     cooldown, un altro modello mistral non aiuterebbe. DeepSeek V4 espone
--     function calling in stile OpenAI affidabile.
--   - DIVERSO da (mistral, mistral-large-latest): il fallback parte solo se
--     provider/model differiscono dal primario (guardia in planner_node).
--
-- Idempotente: ON CONFLICT (purpose) DO UPDATE.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES (
    'planner_fallback',
    'deepseek',
    'deepseek-v4-pro',
    'medium',
    NULL,
    true,
    'planner_fallback: modello non-thinking tool-robust di provider diverso dal planner primario (mistral/mistral-large-latest). Usato da planner_node quando il primario non emette nexus_todo_write (MALFORMED_FUNCTION_CALL/output vuoto). Tentativo singolo, no loop (mig 0267).'
)
ON CONFLICT (purpose) DO UPDATE
SET provider            = EXCLUDED.provider,
    model_id            = EXCLUDED.model_id,
    tier                = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use   = EXCLUDED.requires_tool_use,
    notes               = EXCLUDED.notes,
    updated_at          = NOW();
