-- 0535: narrazione live del sub-agente sul run PADRE (ADR 0037).
--
-- Sintomo: durante un tool dispatch_subagent/dispatch_subagents il run padre
-- resta in attesa BLOCCANTE del figlio anche per molti minuti e la chat non
-- mostra nulla (nessun meta-step, updated_at fermo): il run sembra bloccato
-- mentre il sub-agente lavora (run 82b1ab20: 49 chiamate LLM in 8 minuti,
-- invisibili). Causa radice: il canale SSE del sub-run era scartato alla
-- creazione (`let (sub_tx, _sub_rx) = ...` in subagent_native.rs) e il tool non
-- conosceva il canale del run invocante -> feature muta (stesso pattern
-- dell'incidente narrazione 2026-07-02).
--
-- Fix nel codice (stesso commit): il grafo nativo passa run_id+canale SSE del
-- run invocante ai tool (ParentNarration) e il dispatch emette sul PADRE, via
-- il punto unico emit_phase_meta_correlated (regola L), i meta-step
--   subagent_started / subagent_progress (tool conclusi + heartbeat nei
--   silenzi) / subagent_completed / subagent_failed
-- tutti con correlation_id = subagent_run_id (nexus_agent_meta_steps).
--
-- Queste chiavi governano la narrazione (DB-driven, regola G; i default nel
-- codice coincidono coi valori qui sotto come safe-default se la riga manca):
--   - narration_enabled: kill-switch UX ('false' -> dispatch muto come prima);
--   - narration_heartbeat_s: cadenza del meta-step "al lavoro" emesso SOLO nei
--     silenzi (nessun tool concluso nel periodo, es. LLM call lunghe). '0' lo
--     disabilita lasciando avvio/progressi/chiusura.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.subagent_narration_enabled', 'true', 'orchestrator',
   'Narrazione live del sub-agente sul run padre (meta-step subagent_started/progress/completed con correlation_id=subagent_run_id, ADR 0037). false = dispatch muto (comportamento storico). DB-driven, regola G.'),
  ('orchestrator.subagent_narration_heartbeat_s', '20', 'orchestrator',
   'Cadenza in secondi del meta-step heartbeat "al lavoro" del sub-agente, emesso solo nei periodi senza tool conclusi (es. chiamate LLM lunghe). 0 = heartbeat disabilitato (restano avvio/progressi/chiusura). DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
