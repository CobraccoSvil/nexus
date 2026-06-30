-- Migration 0051: Nexus routing observability on agent_runs.
--
-- Fase 9C: aggiunge metadata per osservare gli effetti del routing A/B attivo
-- (gestito da `nexus_routing.rs` + `NexusBridge`).
--
-- Le colonne sono tutte nullable / con default perché l'override è opt-in via
-- settings.nexus_active_routing_pct e viene applicato solo su una frazione
-- dei run. I run non interessati avranno NULL/FALSE.
--
-- Uso previsto nelle query di analisi canary:
--   SELECT
--     AVG(CASE WHEN nexus_override_applied THEN 1 ELSE 0 END) AS override_rate,
--     nexus_agent_type,
--     COUNT(*) AS runs
--   FROM agent_runs
--   WHERE created_at > NOW() - INTERVAL '1 day'
--   GROUP BY nexus_agent_type
--   ORDER BY runs DESC;
--
-- E per confrontare qualità override vs baseline:
--   SELECT
--     nexus_override_applied,
--     AVG(iteration_count) AS avg_iterations,
--     AVG(EXTRACT(EPOCH FROM (completed_at - created_at))) AS avg_duration_sec
--   FROM agent_runs
--   WHERE created_at > NOW() - INTERVAL '1 day'
--     AND status = 'completed'
--   GROUP BY nexus_override_applied;

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS nexus_override_applied BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS nexus_agent_type TEXT,
    ADD COLUMN IF NOT EXISTS nexus_q_value REAL,
    ADD COLUMN IF NOT EXISTS nexus_task_type TEXT;

COMMENT ON COLUMN agent_runs.nexus_override_applied IS
    'TRUE se il run ha usato provider/model override dal NexusBridge Q-Learning router.';
COMMENT ON COLUMN agent_runs.nexus_agent_type IS
    'AgentType suggerito dal router (es. Coder, Tester, Reviewer). NULL se il bridge non ha prodotto una decisione.';
COMMENT ON COLUMN agent_runs.nexus_q_value IS
    'Q-value della decisione del router nell''intervallo [0, 1.5].';
COMMENT ON COLUMN agent_runs.nexus_task_type IS
    'Classe di task classificata dall''agent_loop (coding/testing/review/design).';

-- Indice parziale per accelerare le query canary che filtrano solo gli
-- override applicati (ci aspettiamo 5-25% del totale nei primi rollout).
CREATE INDEX IF NOT EXISTS idx_agent_runs_nexus_override
    ON agent_runs (created_at DESC)
    WHERE nexus_override_applied = TRUE;
