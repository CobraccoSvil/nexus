-- 0603_fanout_process_cap.sql
-- Tetto GLOBALE di processo per i fan-out di sub-run top-level.
--
-- Root cause: da quando consiglio e multi-provider girano in parallelo
-- (tokio::join! in spawn_agent_run, fase 2 del paradigma di orchestrazione
-- dimensionata) i fan-out non condividono piu' la serializzazione implicita:
-- ogni invocazione di spawn_fanout crea il PROPRIO semaforo locale
-- (orchestrator.subagent_fanout_max_parallel, mig 0596), quindi K panel
-- insieme = K x permits sub-run in volo senza alcun tetto complessivo.
-- Questo setting dimensiona il semaforo di PROCESSO che i fan-out TOP-LEVEL
-- (convocati dal coordinatore: consiglio, review panel, multi-provider,
-- debate) acquisiscono in ordine fisso locale -> processo.
--
-- I fan-out NESTED (dentro un sub-run) NON toccano questo semaforo: un membro
-- padre che tenesse un permesso di processo mentre il figlio ne attende un
-- altro dallo stesso semaforo creerebbe hold-and-wait (deadlock di classe).
-- La loro concorrenza resta bounded dal semaforo locale + depth guard.
--
-- NB: il semaforo e' dimensionato UNA volta al primo fan-out del processo
-- (un semaforo non si ridimensiona a caldo senza stati transitori): una
-- modifica del valore richiede il riavvio di mcp-core.
--
-- Default 12 = 2 panel pieni (2 x 6): il fan-out nominale dei due panel
-- pre-run in parallelo, nessun tetto piu' stretto del comportamento atteso.

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.fanout_process_max_parallel', '12', 'orchestrator',
   'Tetto GLOBALE di sub-run in volo per l''intero processo mcp-core, applicato ai fan-out top-level (consiglio, review panel, multi-provider, debate) quando girano in parallelo. I fan-out nested non lo acquisiscono (anti hold-and-wait). Letto UNA volta al primo fan-out: la modifica richiede il riavvio del servizio. DB-driven, regola G.')
ON CONFLICT (key) DO NOTHING;
