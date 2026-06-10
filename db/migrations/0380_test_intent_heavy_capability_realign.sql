-- 0380_test_intent_heavy_capability_realign.sql
-- Riallinea le required_capabilities dello slot test/approfondita/heavy.
--
-- Causa radice (regola H: si corregge il dato sorgente sbagliato, non si aggira):
-- la riga (intent='test', behavior_mode='approfondita', preferred_tier='heavy',
-- requires_tool_use=true) aveva required_capabilities = {code,test,reasoning}.
-- Il problema NON e' la capability "test" in se' (e' realmente posseduta da 2
-- modelli code-specializzati di tier basso/medio: mistral/codestral-latest e
-- openai/gpt-4.1-mini), ma la sua COMBINAZIONE con "reasoning" su questo slot:
-- nei modelli attuali "test" e "reasoning" si escludono a vicenda (i modelli con
-- "test" non hanno "reasoning" e non sono heavy; i flagship con "reasoning" non
-- hanno "test"). Nessun modello del catalog soddisfa il set completo.
--
-- Effetto sul punto unico di selezione (capability_match_pct in
-- crates/mcp-core/src/routing_matrix_auto_promoter.rs, soglia hard 0.5 + peso nello
-- score): per i flagship heavy "test" non viene mai matchata, quindi il cap_score
-- e' fisso a 2/3 = 0.667 invece di 1.0 (penalizzazione ingiusta) e lo slot resta a
-- una sola capability di distanza dal collasso a 0 candidati (fragile se la soglia
-- diventasse match-totale o si aggiungesse un 4o requisito). Con match-totale lo
-- slot e' GIA' a 0 candidati.
--
-- Fix data-driven: si rimuove la sola "test" da questo slot, riallineandolo ad
-- agentic_default/approfondita ({code,reasoning}), intent agentico fratello.
-- Validazione sul catalog (is_enabled=true, supports_tool_use=true):
--   {code,test,reasoning} -> 0 candidati a match-totale (peso morto su tutti i flagship)
--   {code,reasoning}      -> 23 candidati di tier heavy (claude-opus-4-x, gpt-5.x,
--                            gemini-2.5-pro) a cap_score 1.0
-- NON si tocca {code,fix,reasoning} (1 solo candidato, fragile quanto il problema).
--
-- Ambito chirurgico: le righe test/bilanciata, test/economica, test/veloce
-- conservano {code,test} INTENZIONALMENTE: li' "test" premia legittimamente
-- codestral-latest / gpt-4.1-mini (cap_score 1.0) per i test a basso costo, mentre
-- i modelli con solo "code" passano comunque la soglia a 0.5. Non sono rotte e non
-- vanno cambiate.
-- Idempotente.

UPDATE nexus_intent_routing_requirements
SET required_capabilities = '{code,reasoning}'
WHERE intent = 'test'
  AND behavior_mode = 'approfondita'
  AND preferred_tier = 'heavy'
  AND requires_tool_use = true
  AND required_capabilities = '{code,test,reasoning}';
