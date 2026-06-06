-- 0340_gemini_25_thinking_tool_policy.sql
--
-- Riconcilia la capability dei modelli Gemini 2.5 thinking (pro e flash, NON
-- flash-lite). Erano stati impostati MANUALMENTE in modo incoerente:
-- gemini-2.5-flash aveva supports_tool_use=true ma agentic_thinking_policy='none'.
-- Combinazione rotta: Gemini 2.5 ha il thinking attivo di default, quindi col
-- function calling in modalita' thinking il provider ritorna
-- finish_reason=MALFORMED_FUNCTION_CALL con output vuoto (vedi mig 0274). Ogni
-- run agentico instradato su gemini-2.5-flash falliva sistematicamente.
--
-- Root cause nel codice gia' corretto: classify_capabilities
-- (crates/mcp-core/src/model_catalog_sync.rs) ora riconosce i Gemini 2.5 come
-- dual-mode thinking (`gemini_25_thinking`) e deriva
-- agentic_thinking_policy='disable_for_tools' (girano non-thinking nei tool-loop,
-- restando tool-capable ed eleggibili per l'agentico). Bug storico: l'euristica
-- assumeva che solo i *-pro fossero thinking ("i flash NON lo sono"), lasciando
-- gemini-2.5-flash con policy 'none'.
--
-- Questa migrazione allinea i DATI ESISTENTI e riporta a capability_source='auto'
-- i record toccati a mano, cosi' il prossimo catalog_sync li governa con la
-- classificazione corretta (niente piu' override manuali incoerenti, regola H).
-- flash-lite NON e' thinking di default: resta escluso (policy 'none').
-- Idempotente: la WHERE evita update inutili a re-applicazione.

BEGIN;

UPDATE ai_price_catalog
SET agentic_thinking_policy = 'disable_for_tools',
    supports_tool_use = true,
    capability_source = 'auto'
WHERE provider = 'google'
  AND model LIKE 'gemini-2.5-%'
  AND model NOT LIKE '%flash-lite%'
  AND (agentic_thinking_policy IS DISTINCT FROM 'disable_for_tools'
       OR supports_tool_use IS DISTINCT FROM true
       OR capability_source IS DISTINCT FROM 'auto');

COMMIT;
