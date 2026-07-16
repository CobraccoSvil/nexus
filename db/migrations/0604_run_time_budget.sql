-- 0604_run_time_budget.sql
-- Deadline dell'INTERO run (terzo asse del budget: tempo, accanto a token e
-- dollari) — fase 3 del paradigma di orchestrazione dimensionata.
--
-- Root cause: il vincolo "tempo" non esisteva a livello di run — solo timeout
-- per-subrun (300s). Il resolver di dimensionamento (mig 0602) deve poter
-- stringere i panel sul budget di TEMPO residuo, e l'executor deve poter
-- chiudere pulito (reason canonico `time_budget`) un run che sfora.
--
-- Meccanica: `AgentState.run_started_at_epoch_s` (checkpointato -> misura il
-- run INTERO anche dopo resume) + enforcement nell'executor gemello del cap di
-- spesa. Il residuo e' derivato dal punto unico `run_time_remaining_s`
-- (agent_runs.created_at + questo setting) e clampa anche il timeout dei
-- sub-run in prepare: sotto `subagent_min_timeout_s` la figura NON parte
-- (prepare_reject `deadline_exhausted`, regola M).
--
-- 0 = disabilitato (bit-identico). L'attivazione del paradigma resta alla mig
-- 0607; questa chiave puo' comunque essere alzata indipendentemente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.run_time_budget_s', '0', 'agent',
   'Deadline in secondi dell''INTERO run agentico (tempo di parete dall''avvio, checkpointato: sopravvive ai resume). Al raggiungimento l''executor chiude d''autorita'' con reason time_budget, gemello del cap di spesa. Il residuo stringe anche il dimensionamento dei panel (resolver mig 0602) e il timeout dei sub-run. 0 = disabilitato. DB-driven, regola G.'),
  ('orchestrator.subagent_min_timeout_s', '30', 'orchestrator',
   'Floor (secondi) del timeout di un sub-run sotto deadline: se il tempo residuo del run e'' inferiore, la figura NON viene convocata (prepare_reject deadline_exhausted) — un timeout ridicolo produce solo spesa senza esito. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
