-- 0266: il planner deve usare un modello NON-thinking con function calling
-- affidabile.
--
-- Root cause: planner_node forza tool_choice su nexus_todo_write (deve produrre
-- il piano come tool call). nexus_purpose_model.planner era google/gemini-2.5-pro,
-- un modello THINKING: thinking + tool_choice forzato -> finish_reason
-- MALFORMED_FUNCTION_CALL, output vuoto, nessun piano, planner in loop e poi
-- "Nessuna risposta dal provider". Stesso conflitto thinking+tool_choice gia'
-- visto sull'executor, ma il planner forza per design.
--
-- Fix: il planner richiede un modello tool-robust non-thinking.
-- mistral-large-latest e' capace e con function calling affidabile.
-- gemini-2.5-pro resta valido per chat/escalation (dove tool_choice non e'
-- forzato), ma e' inadatto al tool-forcing del planner.
--
-- Idempotente: UPDATE sulla riga esistente (purpose chiave).
UPDATE nexus_purpose_model
SET provider   = 'mistral',
    model_id   = 'mistral-large-latest',
    notes      = 'planner: modello non-thinking tool-robust; gemini-2.5-pro (thinking) causa MALFORMED_FUNCTION_CALL sul tool-forcing del piano (mig 0266)',
    updated_at = NOW()
WHERE purpose = 'planner';
