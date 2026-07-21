-- 0631_plan_intents_agentic_default.sql
--
-- CAUSA RADICE (diagnosi 21/07, regola O dal log del router): il classificatore
-- d'intent nel grafo agentico Rust NON e' ancora portato ("classificazione LLM non
-- ancora portata (PR successivo)") e ritorna SEMPRE l'intent neutro
-- 'agentic_default'. La gate di eligibilita' del planner (PlannerConfig::is_eligible)
-- richiede intent IN orchestrator.plan_intents; 'agentic_default' NON era in lista
-- -> NESSUN run diventa plan-eligibile via intent -> il planner non si attiva MAI
-- -> nessun piano/todo -> il VerifierNode incrementale (gated su plan_phase_active)
-- resta inerte -> tutta la verifica build/review e' end-of-run (i difetti si
-- accumulano). E' il motivo per cui i run di creazione app non pianificano.
--
-- FIX B1 (far pianificare): aggiunge 'agentic_default' a plan_intents. Il gate
-- residuo token_budget >= plan_min_token_budget (800) filtra i task piccoli, quindi
-- pianificano solo i task COMPLESSI (creazione app, refactor ampi) — desiderato.
-- Fix DEFINITIVO a monte (separato, "PR successivo"): portare il classificatore LLM
-- cosi' che "crea un'app" -> intent 'scaffold_app'/'code'/'architecture' (gia' in
-- lista) invece del catch-all. Follow-on: garantire che i todo del piano portino un
-- criterio run_command build/typecheck cosi' il VerifierNode verifichi il build
-- incrementalmente (non solo l'esistenza file).
--
-- Reversibile a caldo (regola G): refresh cache <=60s, nessun redeploy. Idempotente
-- (append solo se assente).
UPDATE settings
   SET value = value || ',agentic_default', updated_at = NOW()
 WHERE key = 'orchestrator.plan_intents'
   AND value NOT LIKE '%agentic_default%';
