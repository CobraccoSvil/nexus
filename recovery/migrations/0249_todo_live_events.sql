-- 0249_todo_live_events.sql
--
-- M15.1 del piano "Eventi todo live".
--
-- Setting di controllo per l'emissione di eventi SSE live quando lo status di
-- uno o piu' todo cambia (action check/update di nexus_todo_write). Quando
-- attivo, mcp-core emette un ProjectEvent::TodoUpdated per ogni todo aggiornato
-- piu' un ProjectEvent::PlanUpdated finale, DOPO il commit della transazione
-- (mai eventi fantasma su rollback).
--
-- Gate letto con cache 60s lato Rust (todos_live_events_enabled in
-- crates/mcp-core/src/agent_tools/todos.rs). Niente fallback hardcoded: la
-- configurazione vive solo nel DB (regola G/H di CLAUDE.md).

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.todos.live_events', 'true', 'agent',
     'M15.1: emette eventi SSE live (TodoUpdated per todo + PlanUpdated finale) quando lo status di un todo cambia, dopo il commit della transazione.', FALSE)
ON CONFLICT (key) DO NOTHING;
