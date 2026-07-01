-- 0498_seed_retention_settings.sql
-- Finestre del worker di retention DB (crates/mcp-core/src/db_retention.rs).
-- Regola G: config nel DB, non hardcoded. Il worker usa comunque safe-default se
-- una chiave manca (come run_reaper); il seed serve a renderle visibili/tunabili
-- dal pannello admin. Chiudono alla causa la crescita illimitata di
-- nexus_graph_checkpoints (~10MB/run) e della telemetria *_health_history.
INSERT INTO settings (key, value, category, description) VALUES
  ('db.retention.enabled', 'true', 'database',
   'Abilita il worker di retention DB (pruning checkpoint run terminali + TTL telemetria). Default true.'),
  ('db.retention.interval_secs', '21600', 'database',
   'Intervallo (secondi) tra i cicli di retention DB. Default 21600 (6h), minimo 3600.'),
  ('db.retention.checkpoint_grace_hours', '168', 'database',
   'Grace (ore) prima di potare i nexus_graph_checkpoints dei run TERMINALI (non resumibili). I run running/awaiting_confirmation/blocked_needs_input sono sempre tenuti. Default 168 (7 giorni).'),
  ('db.retention.health_history_days', '30', 'database',
   'TTL (giorni) sulla telemetria provider ai_model_health_history / nexus_provider_health_history. Default 30.')
ON CONFLICT (key) DO NOTHING;
