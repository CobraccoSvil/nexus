-- 0632_plan_min_token_budget_zero.sql
--
-- CAUSA (diagnosi 21/07, regola O): dopo aver messo 'agentic_default' in
-- plan_intents (mig 0631), il planner ANCORA non si attivava. Secondo gate in
-- PlannerConfig::is_eligible: `token_budget < plan_min_token_budget` (800). Ma lo
-- stato del run NON popola `token_budget` (native_engine.rs: costruzione stato con
-- ..Default::default() -> token_budget=None -> unwrap_or(0)=0), quindi 0 < 800 e'
-- SEMPRE vero -> planner SEMPRE bloccato, a prescindere dall'intent. (Il gate
-- behavior_mode e' ok: PRIMARY_BEHAVIOR_MODE="bilanciata" e' in lista.)
--
-- FIX INTERIM (stopgap dichiarato, regola H): porta plan_min_token_budget a 0 cosi'
-- il gate token diventa no-op (0 < 0 = false) finche' il token_budget non e'
-- propagato nello stato. Combinato con 0631 (intent), rende i run agentic_default
-- (cioe' TUTTI, dato il classificatore stub) plan-eligibili -> il planner si attiva
-- -> il VerifierNode incrementale scatta.
--
-- FOLLOW-ON DEFINITIVO (regola H, codice): (1) propagare state.token_budget dal
-- run_token_budget reale nella costruzione dello stato (native_engine.rs ~2745) e
-- ripristinare una soglia sensata; (2) portare il classificatore LLM d'intent cosi'
-- che solo i task che meritano un piano (scaffold_app/architecture/...) siano
-- eligibili, invece di tutti. Con quei due, questo stopgap va rimosso.
--
-- Reversibile a caldo (regola G): refresh cache <=60s, nessun redeploy.
UPDATE settings
   SET value = '0', updated_at = NOW()
 WHERE key = 'orchestrator.plan_min_token_budget';
