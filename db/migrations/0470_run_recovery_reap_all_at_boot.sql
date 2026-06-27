-- 0470_run_recovery_reap_all_at_boot.sql
-- Flag per il reaper di bootstrap dei run orfani
-- (run_reaper::reap_orphaned_runs_at_boot, regola H).
--
-- Causa radice: dopo un restart di mcp-core ogni run 'running' e' ORFANO (il task
-- che lo eseguiva, heartbeat agent_runs.updated_at incluso, e' morto col processo
-- precedente). Il recovery al boot usava la stessa soglia time-gated dello sweep
-- periodico (agent.run_recovery.stale_after_seconds, default 900s): un orfano con
-- updated_at recente (< soglia) NON veniva marcato 'interrupted' e restava
-- 'running', bloccando i nuovi run sulla sessione (gate 409 / session_has_active_run).
--
-- Questo flag (default true) fa marcare al boot TUTTI i run 'running' senza
-- time-gate; se false il reaper di boot ricade sul time-gating periodico.
-- Regola G: niente hardcode di comportamento nel codice, il default true vale solo
-- se la chiave manca. Idempotente.
INSERT INTO settings (key, value) VALUES
  ('agent.run_recovery.reap_all_at_boot', 'true')
ON CONFLICT (key) DO NOTHING;
