-- FASE 2 orchestrazione (PR4): kill-switch dell'isolamento fisico dei sub-run
-- paralleli-che-scrivono (git worktree effimero + apply atomico serializzato).
--
-- Opt-in, default OFF (regola G: unica fonte DB, niente fallback hardcoded).
-- Con OFF (default) il punto unico `tool_dispatch_subagents` (mcp-core) esegue
-- SEMPRE il ramo sequenziale/condiviso: comportamento BIT-IDENTICO a oggi.
-- Con ON il ramo isolato scatta solo se la root e' un repo git isolabile (probe
-- fail-closed) E i `write_scope` dei task sono banalmente disgiunti
-- (subtasks_are_disjoint, PR1); altrimenti degrada comunque a sequenziale.
--
-- Numero 0515 (non 0514: la 0514 vive su un altro branch non ancora mergiato in
-- main -> si evita la collisione al futuro merge). Le colonne di audit
-- worktree_path/base_commit su nexus_subagent_runs vivono nel set project
-- (db/migrations/project/0005_subagent_worktree_columns.sql): quella tabella e'
-- migrata ai DB-progetto (mig 0507 la decommissiona nel meta).
--
-- Cache lato Rust: 60s (nexus_cache::TtlCache in subagent_native.rs). Toggle a
-- runtime senza redeploy: UPDATE settings + attesa <=60s.
INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.subagent_isolation_enabled', 'false', 'orchestrator',
   'Abilita l''isolamento fisico dei sub-run paralleli-che-scrivono (git worktree effimero per sub-run + apply atomico serializzato alla root). OFF (default) = ramo sequenziale/condiviso, bit-identico allo storico. ON = ramo isolato solo se root git isolabile E write_scope disgiunti, altrimenti degrada a sequenziale.')
ON CONFLICT (key) DO NOTHING;
