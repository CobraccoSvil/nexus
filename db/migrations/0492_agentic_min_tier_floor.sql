-- Mig 0492 — Pavimento di tier per i turni AGENTICI (routing agentic-aware).
--
-- Causa radice: per i task agentici multi-step (tool-loop) il routing dinamico
-- (route_model_from_catalog -> select_agentic_model) poteva scegliere un modello
-- LIGHT debole (il piu' economico del pool tool-capable) che diverge / non
-- converge, invece di partire da un modello capace. La mig 0490 ha tamponato un
-- caso specifico (deepseek-coder/chat via capability='exclude'); questo e' il fix
-- strutturale e generale: un PAVIMENTO di tier minimo per i turni agentici.
--
-- Il pavimento e' DB-driven (regola G): il codice legge questo setting col punto
-- unico get_setting (cache 60s); se assente usa il default 'medium' nel codice.
-- Valori validi: light | medium | heavy. La selezione resta GRACEFUL: se il tier
-- minimo non ha candidati disponibili (tutti in cooldown), la tier-chain degrada
-- comunque verso il basso fino a 'light' invece di fallire.
--
-- Applicato SOLO ai turni agentici (intent != "chat"): la chat semplice resta
-- libera di usare 'light'. Aggancio nel punto unico orchestrator/model_routing.rs
-- (route_model_from_catalog, floor_tier_for_agentic). Idempotente.

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.routing.agentic_min_tier',
        'medium',
        'agent',
        'Pavimento di tier (light|medium|heavy) per i turni AGENTICI (intent != chat). Il routing dinamico non parte da un modello sotto questo tier; degrada graceful verso il basso se il tier minimo e'' tutto in cooldown. La chat semplice non e'' toccata. Default medium.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
