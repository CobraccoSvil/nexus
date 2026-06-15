-- 0431_continuous_todo_isolation.sql
--
-- Esecuzione SEQUENZIALE dei todo del planner come SUB-RUN ISOLATE (strategia
-- Claude Code, opzione 3: sub-agent con context fresco).
--
-- Causa radice (osservata sulla UI Nexus): i todo sequenziali del planner girano
-- oggi nel loop executor principale, che ACCUMULA history turno dopo turno. Il
-- contesto cresce, il modello degrada, il task end-to-end non si chiude.
--
-- Fix (regola L: punto unico riusato): un nuovo nodo `todo_runner_node` tra
-- planner ed executor esegue ogni todo come sub-run ISOLATA (context fresco,
-- thread_id figlio, no history del main) delegando allo STESSO meccanismo gia'
-- usato dal DAG parallelo (`run_subagent` via il tool MCP `dispatch_subagents`),
-- con max_parallel=1, un todo per volta in ordine di seq. Il loop principale
-- (planner->executor->verifier) resta INTATTO ed e' il fallback con setting OFF.
--
-- Reversibilita' (regola H): tutto gated da `todo_isolation_enabled` DEFAULT
-- FALSE. Con OFF il grafo si comporta esattamente come prima (edge
-- planner->executor incondizionato). Tre livelli di fallback al comportamento
-- storico: edge condizionale, guard interno del nodo, cap iterazioni.
--
-- DB-driven (regola G): 4 setting con cache 60s lato brain, niente env var,
-- niente fallback hardcoded. Nessun ALTER schema, solo seed in `settings`.
-- Idempotente (ON CONFLICT DO NOTHING).

BEGIN;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.continuous.todo_isolation_enabled',
    'false',
    'agent',
    'Mig 0431: gate principale dell''esecuzione SEQUENZIALE dei todo come sub-run ISOLATE (context fresco, no accumulo di history nel loop executor). DEFAULT false = sistema invariato (edge planner->executor storico). Quando true, e con modalita'' Continuo/Automatico + piano attivo, il nodo todo_runner esegue ogni todo come sub-run via run_subagent (dispatch_subagents, max_parallel=1). Cache 60s lato brain.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.continuous.todo_isolation_on_failure',
    'stop',
    'agent',
    'Mig 0431: politica quando un todo eseguito in sub-run isolata fallisce (status failed/timeout). "stop" (default): marca il todo blocked, cascade-skip dei discendenti, chiude la catena onestamente passando per il final_gate (verifica E2E) e learner; il run NON risulta completed a vuoto. "retry": un solo retry (cap todo_isolation_max_retries) con context arricchito dall''errore, poi degrada a stop. "continue": blocca il todo e prosegue col prossimo pending non dipendente (best-effort, piani a todo indipendenti). Cache 60s.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.continuous.todo_isolation_max_retries',
    '1',
    'agent',
    'Mig 0431: numero massimo di retry di uno stesso todo quando todo_isolation_on_failure = "retry". Default 1. Ignorato per le altre politiche. Cache 60s lato brain.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.continuous.todo_isolation_kind',
    'implement',
    'agent',
    'Mig 0431: kind del sub-agent usato per eseguire un todo isolato. Deve essere presente in orchestrator.subagent_kinds_whitelist e in nexus_subagent_definitions. Allineato al kind di default usato dal DAG parallelo (dag_scheduler). Cache 60s lato brain.'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
