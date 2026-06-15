-- 0423_final_gate_build_check.sql
-- Fix qualita' intervento agentico: il final_gate (gate di chiusura) deve
-- verificare che il codice COMPILI prima di marcare il turno "completed", non
-- solo che i file esistano (outputs_exist) o che i log runtime siano puliti.
-- Senza questo l'agente, dopo aver editato del codice che NON compila (build
-- EXIT != 0), chiudeva comunque "completed" (incidente Beauty-Book: interfaccia
-- Customer + ~20 errori TS, login mai partito).
--
-- Il criterio e' un `run_command` (gia' supportato da criteria_runner) eseguito
-- nel project_root: deve ritornare exit 0. Il comando di default e' un
-- auto-detect generico (npm/cargo) eseguito in shell dal tool run_command;
-- per-progetto ha precedenza una run_config con label/role 'build'
-- (run_configurations), e l'admin puo' sovrascrivere il default qui. Se nessun
-- target di build e' rilevato, l'echo+exit 0 rende il criterio un no-op (N/A):
-- i progetti senza build non vengono bloccati.
-- Idempotente.
INSERT INTO settings (key, value) VALUES
  ('agent.final_gate.build_check_enabled', 'true'),
  ('agent.final_gate.build_timeout_s', '180'),
  ('agent.final_gate.build_command',
   'if [ -f package.json ]; then npm run build; elif [ -f app/package.json ]; then cd app && npm run build; elif [ -f frontend/package.json ]; then cd frontend && npm run build; elif [ -f Cargo.toml ]; then cargo build; else echo "[final_gate] nessun target di build rilevato"; fi')
ON CONFLICT (key) DO NOTHING;
