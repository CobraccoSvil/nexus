-- Riallinea i performance_tier dei modelli deepseek alla capacita' reale.
--
-- Causa radice ("i run agentici usano deepseek-coder invece dei deepseek-v4"):
-- infer_tier_from_name (crates/mcp-core/src/model_catalog_sync.rs) assegnava
-- 'medium' a TUTTI i deepseek, mettendo i modelli completion/chat LEGACY
-- (deepseek-coder = V2 coder, deepseek-chat = V3) nello stesso pool agentico
-- 'medium' dei reasoning V4 (deepseek-v4-flash/pro). Essendo i legacy
-- agentic_thinking_policy='none' e i V4 'disable_for_tools', il routing agentico
-- (select_models_tierchain) li faceva vincere sui V4. Il fix di codice ha gia':
--   - declassato il pre-ordinamento ADR 0025 a TIE-BREAKER (model_selection.rs),
--   - corretto l'euristica infer_tier_from_name per deepseek (model_catalog_sync.rs).
-- Questa migrazione allinea lo STATO CORRENTE del catalog (one-shot); il
-- catalog_sync manterra' questi valori perche' l'euristica ora produce gli stessi.

UPDATE ai_price_catalog
   SET performance_tier = 'light'
 WHERE provider = 'deepseek'
   AND (model LIKE '%coder%' OR model LIKE '%chat%');

UPDATE ai_price_catalog
   SET performance_tier = 'heavy'
 WHERE provider = 'deepseek'
   AND model LIKE '%pro%';

-- deepseek-v4-flash resta 'medium': e' il modello forte del pool agentico medium
-- (1M context, reasoning), che deve vincere sui completion/legacy.
