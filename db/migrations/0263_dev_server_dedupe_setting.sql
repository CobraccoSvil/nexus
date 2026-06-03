-- 0263_dev_server_dedupe_setting.sql
-- Gating del process-GC dei dev-server duplicati per progetto
-- (port_registry::cleanup_duplicate_dev_servers, chiamato dal port_gc_loop).
-- Termina le istanze Vite/Next/`pnpm dev` duplicate avviate fuori dal registry
-- (Vite auto-incrementa la porta lasciando vive le istanze precedenti, non
-- tracciate in nexus_port_allocations) tenendo solo la piu' recente per
-- project_root. Regola G: configurazione nel DB. Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.port_gc.dedupe_dev_servers', 'true', 'agent',
   'Se true, il GC termina i dev-server duplicati (Vite/Next/pnpm dev) per progetto, tenendo solo la istanza piu'' recente.')
ON CONFLICT (key) DO NOTHING;
