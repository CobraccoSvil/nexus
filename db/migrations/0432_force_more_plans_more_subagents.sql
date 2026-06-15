-- 0432_force_more_plans_more_subagents.sql
-- Forza di piu' l'attivazione del planner -> piu' sotto-agenti isolati (richiesta
-- utente: "forzare di piu' nella scrittura dei piani per usare di piu' i sotto-agenti").
--
-- Razionale: i sub-run isolati (DAG parallelo via dispatch_subagents, e todo_runner
-- sequenziale mig 0431) girano con context FRESCO -> ctx basso, niente accumulo, e
-- soprattutto meno pseudo-tool-call testuali ("<execute_bash>...", "<execute_tool>...")
-- che il modello emette quando il context e' ingombro e che il sistema NON esegue
-- (causa dell'abort "modello non risponde con azione"). Piu' piani = piu' sub-agent
-- freschi = piu' affidabilita'.
--
-- Il planner (orchestrator_config.is_eligible_adaptive, adaptive_gating ON) si
-- attivava solo per task MOLTO agentici (agentic_score >= 0.85) o complexity=high,
-- e con budget >= 1500. Soglie abbassate per coprire anche i task agentici MEDI:
--   - adaptive_agentic_score_min: 0.85 -> 0.60 (coerente con agent.tier_floor.agentic_score_min=0.6)
--   - adaptive_low_confidence_max: 0.50 -> 0.60 (anche task a confidence media -> piano)
--   - plan_min_token_budget: 1500 -> 800 (gate HARD budget meno restrittivo)
--
-- I task conversazionali/smalltalk restano fuori (agentic_score < 0.3, vedi
-- clarify.smalltalk_agentic_score_max): il planner NON si attiva per la chat.
-- DB-driven (regola G), cache 60s lato brain, nessun restart. Idempotente.

UPDATE settings SET value = '0.60', updated_at = NOW()
 WHERE key = 'orchestrator.adaptive_agentic_score_min' AND value <> '0.60';
-- vecchio: 0.85

UPDATE settings SET value = '0.60', updated_at = NOW()
 WHERE key = 'orchestrator.adaptive_low_confidence_max' AND value <> '0.60';
-- vecchio: 0.50

UPDATE settings SET value = '800', updated_at = NOW()
 WHERE key = 'orchestrator.plan_min_token_budget' AND value <> '800';
-- vecchio: 1500
