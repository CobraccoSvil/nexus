-- 0532_orchestrator_engine_cutover_rust.sql
-- Zero-Python (bonifica cutover): versiona il cutover del motore agentico a
-- 'rust' e rimuove il servizio 'brain' dal watchdog dei servizi.
--
-- Causa radice (regola H): il cutover del motore di produzione (riga jolly '*'
-- di nexus_orchestrator_engine, seed 'python' in mig 0451) era stato eseguito
-- con un UPDATE manuale sul DB live, mai versionato. Su un wipe + re-migrate il
-- default sarebbe tornato 'python' -> select_engine avrebbe instradato i run
-- verso il brain Python, servizio FERMATO e DISABILITATO (mig 0462) e non piu'
-- presente nel repo (zero file .py). Stessa classe di problema per il watchdog:
-- il seed di agent.watchdog.services (mig 0272) contiene ancora
-- {"name":"brain","port_setting_key":"brain_rest_port"} con la chiave
-- brain_rest_port DROPPATA dalla mig 0463 -> il watchdog proberebbe (e
-- tenterebbe di riavviare) un servizio inesistente.
--
-- Il valore 'python' resta ammesso dal CHECK della tabella ma e' INERTE
-- (nessun servizio dietro): raggiungibile solo con una riga per-scope esplicita.
-- Nello stesso commit il fallback difensivo lato codice
-- (select_engine/resolve_engine_from_rows in chat_messages/agent_run.rs) passa
-- da Engine::Python a Engine::Rust, e task_watchdog non riavvia piu' 'brain'
-- sui fallimenti dell'embedder (che e' in-process, ONNX).
--
-- Idempotente.

UPDATE nexus_orchestrator_engine
   SET engine = 'rust', updated_at = now()
 WHERE scope_key = '*' AND engine <> 'rust';

-- Rimozione entry 'brain' dalla lista servizi watchdog (pattern mig 0501).
UPDATE settings
SET value = (
        SELECT COALESCE(jsonb_agg(elem ORDER BY ord), '[]'::jsonb)::text
        FROM jsonb_array_elements(value::jsonb) WITH ORDINALITY AS t(elem, ord)
        WHERE elem->>'name' <> 'brain'
    ),
    updated_at = NOW()
WHERE key = 'agent.watchdog.services'
  AND value::jsonb @> '[{"name":"brain"}]';
