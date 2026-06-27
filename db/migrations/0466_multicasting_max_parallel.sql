-- 0466_multicasting_max_parallel.sql
-- "Abilita al massimo il multicasting": ampiezza delle ondate di sub-agente +
-- selezione topologica del DAG todo. Tutto DB-driven (regola G, niente hardcode).
--
-- Contesto: il motore agentico NATIVO (Rust) lancia pochi sub-agenti simultanei
-- rispetto al fan-out possibile. Due leve, entrambe gia' lette dal codice:
--   1. orchestrator.max_parallel_subagents: tetto dell'ondata concorrente del tool
--      dispatch_subagents (subagent_native.rs: chunks(max_parallel) + join_all,
--      punto unico read_max_parallel_subagents). Default di codice 3; hard cap di
--      sicurezza 8 (oltre, i sub-run LLM saturano rate-limit/billing dei provider).
--      Lo portiamo al MASSIMO consentito = 8.
--   2. orchestrator.dag_topological_enabled: abilita la selezione topologica del
--      DAG dei todo (dag_scheduler::pick_next_todo / compute_ready_layer, punto
--      unico - regola L). Prerequisito perche' piu' todo indipendenti siano
--      eleggibili contemporaneamente e dispatchabili in ondata.
--      Letto da native_engine::load_todo_runner_config / load_verifier_config.
--
-- DO UPDATE (non DO NOTHING): l'intento e' esplicito "abilita al massimo", quindi
-- forziamo i valori anche se la chiave esiste gia' con un valore inferiore.
-- Idempotente. Si applica al prossimo avvio di mcp-core (sqlx migrate!).
INSERT INTO settings (key, value) VALUES
  ('orchestrator.max_parallel_subagents', '8'),
  ('orchestrator.dag_topological_enabled', 'true')
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value;
