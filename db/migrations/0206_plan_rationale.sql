-- Migrazione 0206: plan_rationale (Cluster 1 — avvicinamento orchestrator-worker).
--
-- Il planner forte oggi passa all'executor solo i todos "flat": razionale,
-- vincoli e alternative scartate (che il modello genera in prov_result.content)
-- vengono BRUCIATI. Questo riduce la continuita' semantica tra pianificazione
-- ed esecuzione. Aggiungiamo a nexus_agent_plans i campi per tramandare il
-- contesto decisionale, che l'executor inietta nel system_text e che viene
-- ri-vettorializzato come nota intent=decision per informare i turni futuri.
--
-- Tutto gated da settings default-OFF (categoria orchestrator). Nessun nuovo
-- purpose model: il planner resta tier heavy (mig 0203); la ri-vettorializzazione
-- usa l'embedding gia' fatto lato Rust da knowledge_create_note.

ALTER TABLE nexus_agent_plans
    ADD COLUMN IF NOT EXISTS rationale TEXT;
ALTER TABLE nexus_agent_plans
    ADD COLUMN IF NOT EXISTS constraints JSONB NOT NULL DEFAULT '[]'::jsonb;
ALTER TABLE nexus_agent_plans
    ADD COLUMN IF NOT EXISTS alternatives JSONB NOT NULL DEFAULT '[]'::jsonb;

COMMENT ON COLUMN nexus_agent_plans.rationale IS
'Cluster 1: razionale/strategia del piano prodotto dal planner forte, tramandato all''executor e ri-vettorializzato come nota decision.';

INSERT INTO settings (key, value, category, description) VALUES
    ('orchestrator.plan_rationale_enabled', 'false', 'orchestrator',
     'Se true, il planner recupera decisioni passate via RAG, produce rationale/constraints/alternatives e li tramanda all''executor.'),
    ('orchestrator.plan_rationale_rag_topk', '5', 'orchestrator',
     'Quante decisioni/interazioni passate recuperare per informare il razionale del planner.'),
    ('orchestrator.plan_rationale_min_score', '0.55', 'orchestrator',
     'Soglia minima di similarita'' per includere una decisione passata nel contesto del planner.'),
    ('orchestrator.plan_rationale_persist_as_note', 'false', 'orchestrator',
     'Se true, dopo la creazione del piano il razionale viene salvato come nota knowledge intent=decision (chiude il ciclo RAG).')
ON CONFLICT (key) DO NOTHING;
