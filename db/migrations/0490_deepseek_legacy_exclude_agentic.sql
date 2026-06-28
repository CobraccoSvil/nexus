-- Esclude i modelli completion/chat LEGACY di deepseek dai run agentici.
--
-- Causa radice ("i run agentici usano deepseek-coder invece dei V4"): deepseek-coder
-- (V2 completion) e deepseek-chat (V3) hanno agentic_thinking_policy='none' e
-- capability 'code'/'fix'/'test'. Sui task a tier light + capability code
-- (intent fix / fix_semplice / test, vedi nexus_intent_capability) scavalcavano i
-- V4 reasoning (agentic_thinking_policy='disable_for_tools'), perche' vivevano nello
-- stesso pool tier+capability ed erano 'none'. Sono tool-capable ma inadatti ai
-- tool-loop agentici complessi (il run di test si bloccava ripetendo lo stesso
-- comando -> ABORT).
--
-- Fix definitivo allineato a classify_capabilities (model_catalog_sync.rs):
-- agentic_thinking_policy='exclude' li toglie dal selettore agentico
-- (select_models_tierchain applica WHERE agentic_thinking_policy <> 'exclude'),
-- cosi' i task agentici scelgono i V4 (deepseek-v4-flash/pro). Restano eleggibili
-- per i path NON agentici (best_model_for_tier require_thinking_non_exclude=false).
-- Il catalog_sync mantiene il valore perche' l'euristica ora produce 'exclude'.

UPDATE ai_price_catalog
   SET agentic_thinking_policy = 'exclude'
 WHERE provider = 'deepseek'
   AND (model LIKE '%coder%' OR model LIKE '%chat%');
