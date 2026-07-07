-- 0540: numero massimo di task in un batch dispatch_subagents (Fase C3, regola G).
--
-- Prima il tetto era hardcoded (`if tasks.len() > 8` in subagent_native.rs): un
-- valore di business nel codice (regola H). Ora e' un setting DB-driven, con un
-- backstop assoluto a codice (BATCH_MAX_TASKS_HARD_CAP=32) che previene valori
-- insensati. Il default qui coincide col comportamento storico (8).
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.subagent_batch_max_tasks', '8', 'orchestrator',
   'Fase C3: numero massimo di task in un singolo batch dispatch_subagents. Default 8 (comportamento storico), clampato al backstop di codice 32. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
