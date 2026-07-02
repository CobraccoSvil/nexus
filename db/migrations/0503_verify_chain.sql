-- 0503_verify_chain.sql
-- ADR 0019 L3 (tool nexus_verify_change) + ADR 0018 leva 3 (criteri
-- strutturali del final_gate). Seed dei default DB-driven (regola G: nessun
-- comando/flag hardcoded nella business logic; il codice ha solo i safe-default
-- inerti se il DB e' irraggiungibile).
--
-- TOOL nexus_verify_change: catena di verifica post-modifica
-- typecheck -> build -> lint -> test con fail-fast al primo step rosso ed esito
-- STRUTTURATO (VerifyReport JSON: exit_code + build_errors per step, regola M).
-- Risoluzione comando per step (punto unico resolve_verify_command):
--   1. run_configurations del progetto (role = nome step)     [override locale]
--   2. settings agent.verify.<lang>.<step>                    [default globale]
--   3. nessuno dei due -> step SKIPPATO con skipped_reason     [mai hardcode]
--
--   - agent.verify.enabled            kill-switch del tool
--   - agent.verify.step_timeout_s     timeout per singolo step (secondi)
--   - agent.verify.output_max_chars   troncamento output per step nel report
--   - agent.verify.<lang>.<step>      matrice comandi default per linguaggio
--     (i linguaggi sono quelli dei resolver di nexus-build-graph: typescript,
--      rust, python, go; uno step senza chiave viene saltato, non inventato)
--
-- CRITERI STRUTTURALI final_gate/verifier (ADR 0018 leva 3):
--   - agent.final_gate.structural_criteria_enabled  kill-switch dei 3 criteri
--     action_requested / tool_capability / completion_confirmed
--
-- Idempotente: i valori restano se gia' presenti.

INSERT INTO settings (key, value) VALUES
  ('agent.verify.enabled', 'true'),
  ('agent.verify.step_timeout_s', '180'),
  ('agent.verify.output_max_chars', '4000'),
  -- TypeScript/JavaScript: npm run con --if-present cosi' un progetto senza lo
  -- script esce 0 (lo step "passa vuoto") invece di fallire per assenza script.
  ('agent.verify.typescript.typecheck', 'npx tsc --noEmit'),
  ('agent.verify.typescript.build', 'npm run build --if-present'),
  ('agent.verify.typescript.lint', 'npm run lint --if-present'),
  ('agent.verify.typescript.test', 'npm test --if-present'),
  -- Rust: stessa catena del gate di repo (cargo check/clippy/test).
  ('agent.verify.rust.typecheck', 'cargo check --workspace'),
  ('agent.verify.rust.build', 'cargo build --workspace'),
  ('agent.verify.rust.lint', 'cargo clippy --workspace --all-targets -- -D warnings'),
  ('agent.verify.rust.test', 'cargo test --workspace --no-fail-fast'),
  -- Python: niente step build (non esiste una build canonica: chiave assente
  -- = step saltato per design, non un buco).
  ('agent.verify.python.typecheck', 'python -m mypy .'),
  ('agent.verify.python.lint', 'python -m ruff check .'),
  ('agent.verify.python.test', 'python -m pytest'),
  -- Go: vet come typecheck/lint di base.
  ('agent.verify.go.typecheck', 'go vet ./...'),
  ('agent.verify.go.build', 'go build ./...'),
  ('agent.verify.go.test', 'go test ./...'),
  -- ADR 0018 leva 3: criteri strutturali del final_gate/verifier.
  ('agent.final_gate.structural_criteria_enabled', 'true')
ON CONFLICT (key) DO NOTHING;

-- Esposizione del tool nexus_verify_change nelle whitelist DB-driven che
-- filtrano il catalogo (stesso append idempotente della mig 0502). NON in
-- automation.study_mode_readonly_tools: la verify ESEGUE comandi di build/test
-- (side-effect su target/, node_modules/, ecc.), non e' read-only.
UPDATE settings
   SET value = value || ',nexus_verify_change'
 WHERE key IN (
         'agent.tools.discovery_first_whitelist',
         'agent.tools.core_whitelist',
         'agent.tools.inline_core_whitelist',
         'automation.o_series_essential_tools'
       )
   AND value NOT LIKE '%nexus_verify_change%';
