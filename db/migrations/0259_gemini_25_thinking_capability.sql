-- 0259_gemini_25_thinking_capability.sql
--
-- Stesso bug della migrazione 0256 (deepseek-v4), questa volta per Gemini 2.5.
--
-- Root cause: gemini-2.5-flash e gemini-2.5-pro sono modelli "thinking"
-- (reasoning interno; il google_provider stesso li tratta come tali per il
-- thinking_budget), ma nexus_provider_capabilities.thinking era = false. Con
-- thinking=false e tool_choice_first_turn_force=true, adapter_base.resolve_tool_choice
-- FORZA il tool_choice ("any") al primo turno agentico. Un modello in thinking
-- mode con tool_choice forzato chiude il turno con output VUOTO (nessun testo,
-- nessuna tool call) -> HOLLOW COMPLETION steps=0 -> la chat non risponde.
--
-- Il guard `and not cap.thinking` in resolve_tool_choice degrada gia' il
-- tool_choice ad "auto" per i modelli thinking: basta allineare il flag al
-- comportamento reale del modello. Cosi' Gemini decide da se' quando invocare i
-- tool, senza forzatura incompatibile.
--
-- context_window resta invariato (gemini-2.5 ha 1.048.576 token, gia' corretto
-- in ai_price_catalog). Regola G/H: verita' nel DB via migrazione versionata.
-- Idempotente.

UPDATE nexus_provider_capabilities
SET thinking = true,
    updated_at = now()
WHERE model IN ('gemini-2.5-flash', 'gemini-2.5-pro')
  AND thinking IS DISTINCT FROM true;
