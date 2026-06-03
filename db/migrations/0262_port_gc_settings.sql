-- 0262_port_gc_settings.sql
-- Parametri del garbage-collector delle porte orfane (worker periodico in
-- mcp-core, port_registry::port_gc_loop). Rilascia le allocazioni "dynamic"
-- senza listener oltre la grace period (residui dei tentativi falliti degli
-- agenti). Regola G: configurazione nel DB. Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.port_gc.interval_seconds', '120', 'agent',
   'Intervallo (secondi) del GC delle porte orfane in mcp-core.'),
  ('agent.port_gc.grace_seconds', '180', 'agent',
   'Grace period (secondi) prima di rilasciare un''allocazione porta dynamic senza listener.')
ON CONFLICT (key) DO NOTHING;
