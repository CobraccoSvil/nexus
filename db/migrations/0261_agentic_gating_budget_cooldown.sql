-- 0261_agentic_gating_budget_cooldown.sql
--
-- Tre fix di configurazione (regola G/H: tutto nel DB via migrazione versionata)
-- osservati dal vivo su task banali (es. "elenca i file"):
--
-- 1) OVER-ORCHESTRAZIONE: il planner forte + i sub-agenti si attivavano anche per
--    task banali. Cause: plan_min_token_budget=50 (qualunque messaggio lo supera),
--    file_ops fra gli intent che abilitano il planner, e soglia agentic bassa
--    (0.7) mentre il classifier assegna agentic=0.98 a "elenca file".
--    Fix: alzare la soglia di token, togliere file_ops dagli intent del planner,
--    alzare la soglia agentic. Il planner resta per i task realmente complessi
--    (scaffold/implement/refactor/architecture) ma non per letture/listing.
--
-- 2) BUDGET ITERAZIONI: max=300 permetteva esplorazioni infinite (osservate ~55
--    iterazioni con context gonfio a 778K token). Abbassato il tetto e la base.
--
-- 3) COOLDOWN PROVIDER: providers.billing_cooldown_seconds=600 (10 min) faceva
--    riprovare di continuo i provider senza credito (anthropic/openai -> 400/429).
--    Allineato al cooldown lungo (6h = provider.cooldown_long_s) cosi' un provider
--    in billing_error resta fuori finche' non torna un 200 reale.

-- 1) Over-orchestrazione
UPDATE settings SET value = '1500', updated_at = now() WHERE key = 'orchestrator.plan_min_token_budget';
UPDATE settings SET value = '0.85', updated_at = now() WHERE key = 'orchestrator.adaptive_agentic_score_min';
UPDATE settings SET value = 'code,implement,fix,refactor,scaffold_app,architecture', updated_at = now()
  WHERE key = 'orchestrator.plan_intents';

-- 2) Budget iterazioni
UPDATE settings SET value = '100', updated_at = now() WHERE key = 'agent.iteration_budget.max';
UPDATE settings SET value = '40', updated_at = now() WHERE key = 'agent.iteration_budget.base';

-- 3) Cooldown provider in billing_error
UPDATE settings SET value = '21600', updated_at = now() WHERE key = 'providers.billing_cooldown_seconds';
