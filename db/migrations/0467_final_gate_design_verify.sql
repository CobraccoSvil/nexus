-- 0467_final_gate_design_verify.sql
-- Gate design_verify (P5): il final_gate non chiude un task figma se la resa visiva
-- e' sotto soglia. Il criterio e' DETERMINISTICO: legge l'ultimo similarity_score
-- prodotto dall'agente con nexus_visual_compare nella history (niente vision nel
-- gate) e lo confronta con la soglia. Si applica SOLO se un confronto e' stato fatto
-- (task figma); per i task non-figma il criterio non viene nemmeno costruito.
--
-- Contesto (incidente Beauty-Book): l'agente leggeva il design Figma ma chiudeva
-- "completed" con un layout non conforme, perche' il final_gate verifica build/
-- endpoint, non il design. Questo gate chiude il buco: se l'agente HA misurato una
-- similarity sotto soglia, non puo' chiudere -> re-executor (continua ad allineare).
--
-- Tutto DB-driven (regola G): niente soglia hardcoded nel codice.
--   - design_verify_enabled: gate attivo (default true; non blocca i non-figma).
--   - design_verify_min_score: soglia 0-100 (default 70).
-- Letti da native_engine::load_final_gate_config. Idempotente.
INSERT INTO settings (key, value) VALUES
  ('agent.final_gate.design_verify_enabled', 'true'),
  ('agent.final_gate.design_verify_min_score', '70')
ON CONFLICT (key) DO NOTHING;
