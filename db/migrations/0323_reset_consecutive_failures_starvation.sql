-- 0323_reset_consecutive_failures_starvation.sql  (ADR 0025)
--
-- Rompe il deadlock di STARVATION del catalog modelli.
--
-- Causa: select_agentic_model filtrava `consecutive_failures = 0` (oltre a
-- is_enabled). Ma il model_health_probe, su probe-OK, NON resetta il counter
-- (commento in model_health_probe.rs: "solo i run reali resettano"). Quindi un
-- modello che accumulava anche 1 solo fail transitorio veniva escluso dai run
-- reali -> non veniva mai piu' scelto -> il counter non veniva mai resettato ->
-- escluso dal pool agentico PER SEMPRE. Risultato osservato: solo 2 modelli
-- non-thinking sopravvivevano eleggibili (mistral-small-latest, gemini-2.5-flash)
-- e i task heavy cadevano sistematicamente su gemini-2.5-pro (thinking,
-- inaffidabile sotto tool_choice forzato) -> "completamento vuoto".
--
-- Fix codice (commit collegato): rimosso il filtro ridondante
-- `consecutive_failures = 0` da select_agentic_model. is_enabled=TRUE gia'
-- garantisce salute: il probe fa AUTO-DISABLE (is_enabled=false) oltre
-- failure_threshold, quindi un enabled ha per costruzione fails < threshold.
--
-- Questa migrazione ripulisce lo stato gia' avvelenato: azzera il counter sui
-- modelli ENABLED (i loro fail erano transitori, non hanno raggiunto la soglia
-- di disable). Idempotente. NON tocca i modelli disabilitati (auto_disabled).

UPDATE ai_price_catalog
SET consecutive_failures = 0, updated_at = NOW()
WHERE is_enabled = TRUE
  AND consecutive_failures <> 0;
