-- 0465_final_gate_build_check_reenable.sql
-- Riattiva il criterio BUILD del final_gate nel motore agentico NATIVO (Rust).
--
-- Contesto: il criterio build (mig 0423) verifica che il codice COMPILI prima di
-- chiudere il turno "completed", non solo che i file esistano. Era cablato nel
-- brain Python via _resolve_build_command. Col cutover del grafo a Rust,
-- native_engine::load_final_gate_config lasciava build_command=None (risoluzione
-- non portata) -> il criterio build NON veniva mai costruito -> un'app con import
-- non risolti / errori TS passava il gate. Incidente Beauty-Book: App.tsx
-- importava "./components/ui/sonner" inesistente (boilerplate shadcn non estratto
-- dal Figma Make), `vite build` non eseguito dal gate, turno chiuso "completed".
--
-- Il setting agent.final_gate.build_command (script auto-detect npm/cargo, no-op
-- se nessun target) e' ancora presente; mancava il flag di gate
-- build_check_enabled, cancellato come "orfano del brain" dalla mig 0463. Lo
-- ripristiniamo: ora e' letto dal motore NATIVO Rust
-- (native_engine::load_final_gate_config), non piu' dal brain eliminato.
-- Idempotente.
INSERT INTO settings (key, value) VALUES
  ('agent.final_gate.build_check_enabled', 'true')
ON CONFLICT (key) DO NOTHING;
