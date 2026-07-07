-- 0543: guardia NO-PROGRESS sui figli background impantanati (Fase D fan-in).
--
-- Sintomo: un figlio background (es. frontend_implementer) resta `running` con
-- 0 iterazioni / 0 token per 5+ minuti, tenendo APPESO il padre sospeso in
-- awaiting_subagents. L'unica rete oggi e' l'orphan_timeout del backstop
-- (orchestrator.background_fanin_orphan_timeout_seconds, default 900s), pensato
-- per gli ORFANI crash-detectable e basato sulla sola ETA' della sub-run: troppo
-- lungo e non distingue un figlio che LAVORA da uno IMPANTANATO (0 progresso).
--
-- Fix (regola H, causa radice): il backstop fan-in guadagna un secondo check
-- (fanin_worker.rs) che marca `timeout` le sub-run background rimaste
-- `running`/`paused` create da piu' di `subagent_no_progress_timeout_seconds` E
-- SENZA alcun progresso (0 agent_steps E 0 iterazioni: segnale STRUTTURATO, non
-- prosa, regola M). Cosi' il figlio impantanato viene abortito, la COUNT del
-- fan-in scende e il padre viene riaccodato e ripreso (con l'esito timeout del
-- figlio). Distinto dall'orphan_timeout: quello colpisce QUALSIASI sub-run vecchia
-- (anche una che sta lavorando), questo SOLO quelle senza progresso -> soglia
-- piu' aggressiva senza uccidere i figli lenti-ma-vivi.
--
-- Chiavi (DB-driven, regola G; i default nel codice coincidono coi valori qui):
--   - subagent_no_progress_timeout_seconds: eta' minima (s) di una sub-run
--     background SENZA progresso oltre cui e' considerata impantanata e marcata
--     timeout. Minimo consigliato 30s (sotto rischia falsi positivi su figli in
--     cold start / prima chiamata LLM lenta).
--   - subagent_no_progress_check_enabled: kill-switch del check ('false' -> resta
--     solo l'orphan_timeout storico, comportamento pre-fix).
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.subagent_no_progress_timeout_seconds', '300', 'orchestrator',
   'Eta minima in secondi di una sub-run BACKGROUND senza progresso (0 agent_steps e 0 iterazioni) oltre cui il backstop fan-in la marca timeout, liberando il padre appeso. Minimo consigliato 30s (sotto rischia falsi positivi su cold start). DB-driven, regola G.'),
  ('orchestrator.subagent_no_progress_check_enabled', 'true', 'orchestrator',
   'Kill-switch del check no-progress sui figli background impantanati nel backstop fan-in. false = resta solo orchestrator.background_fanin_orphan_timeout_seconds (comportamento pre-fix). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
