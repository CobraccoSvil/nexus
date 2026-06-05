-- 0328_plan_intent_columns.sql
--
-- Persistenza dell'intent e della behavior_mode con cui un piano agente fu
-- creato, per il fix "plan-reuse intent-aware" del planner.
--
-- Root cause: planner_node riusa il piano esistente per un thread_id SENZA
-- controllare se l'intent utente e' cambiato. Esempio reale: turno 1 "crea sito
-- web" (intent coding) -> piano con todo di scaffold; turno 2 "genera
-- documentazione" (intent docs, STESSO thread) -> il planner riusa il piano
-- coding e l'agente continua a scrivere codice invece di documentare.
--
-- Il piano e' l'entita' che memorizza il contesto decisionale (gia' rationale/
-- constraints/alternatives in mig 0206): vi aggiungiamo l'intent e la
-- behavior_mode di creazione, cosi' il planner puo' invalidare e rifare il
-- piano quando l'intent corrente diverge. Nullable per retrocompatibilita' con
-- i piani esistenti.

ALTER TABLE nexus_agent_plans
    ADD COLUMN IF NOT EXISTS user_intent TEXT;
ALTER TABLE nexus_agent_plans
    ADD COLUMN IF NOT EXISTS behavior_mode TEXT;

COMMENT ON COLUMN nexus_agent_plans.user_intent IS
'Intent utente (router_node) al momento della creazione del piano. Se l''intent corrente diverge, planner_node invalida e rifa'' il piano (no reuse cieco).';

COMMENT ON COLUMN nexus_agent_plans.behavior_mode IS
'behavior_mode al momento della creazione del piano. Concorre, con user_intent, all''invalidazione semantica del riuso.';
