-- 0244_regression_gate_hard_block.sql
--
-- M13.5 del piano "Impact analysis": regression gate HARD block, default-OFF.
--
-- Estende il gate SOFT (M13.4, mig 0243) con la modalita' di blocco hard. Quando
-- attivo, se i test dell'impact set falliscono il run viene marcato come bloccato
-- (project_impact_runs.gate_status IN ('blocked','blocked_capped')) e l'auto-commit
-- a fine run NON deve committare il codice rotto (controllo in
-- crates/mcp-core/src/agent_types.rs auto_commit_project_changes).
--
-- Policy di rollout (regola della chat + regola G CLAUDE.md): default-OFF. Il flag
-- vive solo nel DB, nessun fallback hardcoded lato codice.
--
-- Aggiunge anche un vincolo UNIQUE su project_impact_runs.run_id: la chiave logica
-- di un record di impact e' il run_id (un record per run), cosi' l'endpoint
-- /api/internal/impact/record-run puo' fare un UPSERT atomico (ON CONFLICT (run_id)).
-- La tabella e' vuota (creata in 0242), nessun rischio di violazione su dati esistenti.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING per i settings (stile 0240);
-- IF NOT EXISTS / nome esplicito per il vincolo unico.

CREATE UNIQUE INDEX IF NOT EXISTS uq_pir_run_id ON project_impact_runs (run_id);

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('regression_gate.hard_block', 'false', 'regression_gate', 'Abilita il blocco HARD del regression gate (M13.5): se i test dell impact set falliscono il run e bloccato e l auto-commit non committa. Default-OFF (rollout).', FALSE),
    ('regression_gate.max_cycles', '1', 'regression_gate', 'Numero massimo di cicli fix-and-retest che il gate hard concede prima di bloccare definitivamente il run.', FALSE)
ON CONFLICT (key) DO NOTHING;
