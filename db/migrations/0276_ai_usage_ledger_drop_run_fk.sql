-- 0276: il ledger di billing deve registrare SEMPRE il consumo AI, anche per
-- gli agent-turn del brain che non creano un orchestrator_run formale.
--
-- Root cause: ai_usage_ledger.run_id ha una FK verso orchestrator_runs
-- (ai_usage_ledger_run_id_fkey). Gli agent-turn LangGraph del brain hanno un
-- thread_id/run_id proprio che NON e' inserito in orchestrator_runs (solo i run
-- "orchestrator" formali lo sono). Risultato: ogni INSERT nel ledger per un
-- agent-turn viola la FK ->
--   "billing ledger insert fallito ...: Key (run_id)=(...) is not present in
--    table orchestrator_runs"
-- Il consumo AI di quei turni NON viene contabilizzato (perdita di dati di
-- costo) e il log si riempie di WARNING ad ogni chiamata provider.
-- registry.py:447 documenta gia' che "il ledger del turno puo'" non avere un
-- orchestrator_run: la FK rigida e' quindi semanticamente sbagliata per questa
-- tabella di contabilita'.
--
-- Fix: rimuovi il vincolo FK bloccante. run_id resta una colonna libera (utile
-- per join opzionali quando l'orchestrator_run esiste), ma la registrazione del
-- consumo non e' piu' subordinata alla presenza del run in orchestrator_runs.
-- Idempotente.

ALTER TABLE ai_usage_ledger
    DROP CONSTRAINT IF EXISTS ai_usage_ledger_run_id_fkey;
