-- 0578_gemini3_thinking_native.sql
--
-- Fix RC-1 empty-completion gemini-3 (finish_reason=length): gemini-3.x e' un modello
-- a thinking OBBLIGATORIO che RIFIUTA thinkingBudget=0 (HTTP 400). La policy
-- 'disable_for_tools' (che forza budget 0 sui turni con tool) provocava, sul 400, il
-- retry-senza-thinkingConfig -> thinking DEFAULT ILLIMITATO -> divora il tetto di
-- output (16384) -> content vuoto -> finish MAX_TOKENS -> "length" -> empty_completion.
--
-- Il valore ONESTO e' 'native' (gia' ammesso dal CHECK chk_agentic_thinking_policy,
-- mig 0319: "reasoning con tool nativi senza forcing"): l'adapter mcp-core
-- (capability::resolve_mandatory_thinking_budget) legge 'native' e inietta un thinking
-- budget BOUNDED, cosi' il gateway (google.rs::resolve_thinking) emette Enabled(budget)
-- invece di DisabledForTools. Distinto da 'disable_for_tools' dei gemini-2.5, che
-- ACCETTANO budget 0 e vanno lasciati com'e'.
--
-- Solo i modelli chat/reasoning gemini-3 (quelli oggi 'disable_for_tools'); le varianti
-- image/tts/live (gia' 'none') restano invariate.

UPDATE ai_price_catalog
   SET agentic_thinking_policy = 'native',
       uses_thinking_mode      = TRUE,
       updated_at              = NOW()
 WHERE provider = 'google'
   AND model LIKE 'gemini-3%'
   AND agentic_thinking_policy = 'disable_for_tools';
