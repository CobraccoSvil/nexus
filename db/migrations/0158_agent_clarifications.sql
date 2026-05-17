-- PR-3 (Codex pattern) — Clarifying questions pre-flight del planner.
-- Quando il task utente e' ambiguo:
--   * Confirm mode → planner emette requires_clarification, loop HITL
--   * Automatico/Continuo → planner applica `applied_defaults` trasparenti
-- Storicizziamo entrambi per audit + few-shot futuri.
CREATE TABLE IF NOT EXISTS nexus_agent_clarifications (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id           UUID NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    project_id       UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- [{id, question, suggested_default}, ...]
    questions        JSONB NOT NULL,
    -- {qid: answer, ...} null se in attesa
    user_answers     JSONB,
    -- Ipotesi automatiche applicate in Automatico/Continuo
    applied_defaults JSONB,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    answered_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_clarifications_run ON nexus_agent_clarifications(run_id);
CREATE INDEX IF NOT EXISTS idx_clarifications_project ON nexus_agent_clarifications(project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_clarifications_pending
    ON nexus_agent_clarifications(run_id) WHERE user_answers IS NULL AND applied_defaults IS NULL;

-- Setting per controllo runtime.
INSERT INTO settings (key, value, updated_at) VALUES
    ('orchestrator.clarifying_questions_enabled', 'true', NOW()),
    ('orchestrator.clarifying_questions_max', '3', NOW())
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value, updated_at = NOW()
WHERE settings.value IS DISTINCT FROM EXCLUDED.value;
