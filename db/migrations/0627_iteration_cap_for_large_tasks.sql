-- 0627_iteration_cap_for_large_tasks.sql
-- Alza il soffitto di lavoro per-run (agent.executor.iteration_cap) da 60 a 100.
--
-- Diagnosi (progetto vendita-immobile, chat di creazione app): due run che
-- PROGREDIVANO davvero (86 tool, 14 write_file/11 edit_file su file diversi,
-- solo 3 stall_recovery) hanno colpito ESATTAMENTE 60 iterazioni e chiuso al cap
-- (23d081b6, e fe2462a0 cancelled a 50). "Costruisci un'app intera" e' un task
-- legittimamente grande che eccede 60 iterazioni: il cap lo tronca mentre lavora,
-- non perche' cicla.
--
-- Perche' e' sicuro alzarlo: iteration_cap NON e' la guardia anti-loop, e' il
-- soffitto grezzo di lavoro. Le vere anti-loop sono molto piu' strette e
-- INVARIATE (agent.stall_recovery.max_moves_per_session=6, agent.final_gate.
-- max_cycles=2, agent.g1_max_nudges=3): un run che cicla si ferma a 2-6
-- ripetizioni, ben prima di 60. Reggere fino a 100 iterazioni implica quasi
-- sempre progresso reale. I tetti di COSTO restano indipendenti e intatti
-- (agent.run_token_budget=400000, orchestrator.subagent_cost_cap_per_run_usd=5.00).
--
-- Il recursion_limit effettivo si ricalcola da qui (effective_recursion_limit:
-- 100*3 + margini stall/g1/final_gate -> ~411), scavalcando il floor 200.
-- Reversibile a caldo (regola G): UPDATE ... value='60' ... refresh <=60s.
UPDATE settings
SET value = '100', updated_at = NOW()
WHERE key = 'agent.executor.iteration_cap';

INSERT INTO settings (key, value, category, description)
SELECT 'agent.executor.iteration_cap', '100', 'agent',
       'Soffitto di iterazioni executor per-run (non e'' la guardia anti-loop, quelle sono stall/g1/final_gate). Alzato a 100 per i task grandi tipo scaffold app (mig 0627).'
WHERE NOT EXISTS (SELECT 1 FROM settings WHERE key = 'agent.executor.iteration_cap');
