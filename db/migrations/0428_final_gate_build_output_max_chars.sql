-- 0428_final_gate_build_output_max_chars.sql
-- Hardening qualita' agentico (fix 2026-06-15): quando il criterio BUILD del
-- final_gate fallisce, l'agente correggeva SOLO il primo errore percepito e
-- ri-provava, lasciando gli altri irrisolti (incidente Beauty-Book: errori TS
-- residui ignorati). Causa concorrente: l'output_excerpt iniettato all'executor
-- era troncato a ~600/900 char, sotto i quali tipicamente compare solo il
-- PRIMO errore TS/cargo; gli altri restavano invisibili.
--
-- Setting:
--   agent.final_gate.build_output_max_chars
--     Limite (caratteri) dell'output_excerpt esposto all'agente quando il
--     criterio `build` del final_gate fallisce. Default 4000 (sufficiente per
--     ~15-30 errori TS o ~20 errori Rust). Configurabile dall'admin senza
--     redeploy; cache 60s lato brain.
--     NON tocca gli altri usi di `run_command` nel criteria_runner (che
--     restano a 600), per non gonfiare i prompt dei criteri brevi.
--
-- Idempotente.
INSERT INTO settings (key, value) VALUES
  ('agent.final_gate.build_output_max_chars', '4000')
ON CONFLICT (key) DO NOTHING;
