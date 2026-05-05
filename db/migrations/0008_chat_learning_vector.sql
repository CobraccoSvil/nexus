ALTER TABLE chat_sessions
ADD COLUMN IF NOT EXISTS profile_id TEXT,
ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

ALTER TABLE chat_messages
ADD COLUMN IF NOT EXISTS project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
ADD COLUMN IF NOT EXISTS request_message_id UUID REFERENCES chat_messages(id) ON DELETE SET NULL,
ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS deleted_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

UPDATE chat_messages cm
SET project_id = cs.project_id
FROM chat_sessions cs
WHERE cm.session_id = cs.id
  AND cm.project_id IS NULL;

ALTER TABLE chat_messages
ALTER COLUMN project_id SET NOT NULL;

CREATE INDEX IF NOT EXISTS idx_chat_sessions_user_project_updated
ON chat_sessions(user_id, project_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_chat_messages_session_project_created
ON chat_messages(session_id, project_id, created_at);

CREATE INDEX IF NOT EXISTS idx_chat_messages_request_message_id
ON chat_messages(request_message_id);

CREATE TABLE IF NOT EXISTS ai_response_feedback (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id UUID NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    message_id UUID NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    orchestrator_run_id UUID REFERENCES orchestrator_runs(id) ON DELETE SET NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    feedback_type TEXT NOT NULL DEFAULT 'error',
    intent TEXT,
    provider TEXT,
    model TEXT,
    error_comment TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open',
    review_note TEXT,
    reviewed_by UUID REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_project_status_created
ON ai_response_feedback(project_id, status, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_ai_response_feedback_intent_provider_created
ON ai_response_feedback(project_id, intent, provider, created_at DESC);

CREATE TABLE IF NOT EXISTS prompt_corrections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    feedback_id UUID REFERENCES ai_response_feedback(id) ON DELETE SET NULL,
    session_id UUID REFERENCES chat_sessions(id) ON DELETE SET NULL,
    message_id UUID REFERENCES chat_messages(id) ON DELETE SET NULL,
    orchestrator_run_id UUID REFERENCES orchestrator_runs(id) ON DELETE SET NULL,
    intent TEXT,
    provider TEXT,
    model TEXT,
    correction_text TEXT NOT NULL,
    normalized_hint_hash TEXT NOT NULL,
    qdrant_point_id TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    status TEXT NOT NULL DEFAULT 'open',
    retrieved_count BIGINT NOT NULL DEFAULT 0,
    last_retrieved_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_prompt_corrections_qdrant_point_id
ON prompt_corrections(qdrant_point_id);

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_project_active_created
ON prompt_corrections(project_id, active, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_prompt_corrections_project_hash
ON prompt_corrections(project_id, normalized_hint_hash);

CREATE TABLE IF NOT EXISTS project_learning_config (
    project_id UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    prompt_corrections_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    auto_apply_max_changes_per_day INTEGER NOT NULL DEFAULT 2,
    feedback_threshold INTEGER NOT NULL DEFAULT 5,
    feedback_window_days INTEGER NOT NULL DEFAULT 7,
    min_confidence NUMERIC(6,4) NOT NULL DEFAULT 0.6500,
    rollback_window_hours INTEGER NOT NULL DEFAULT 24,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS learning_policy_snapshots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    intent TEXT NOT NULL,
    previous_chain TEXT NOT NULL,
    next_chain TEXT NOT NULL,
    baseline_error_count BIGINT NOT NULL DEFAULT 0,
    snapshot_reason TEXT NOT NULL DEFAULT 'auto_apply',
    created_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_learning_policy_snapshots_project_created
ON learning_policy_snapshots(project_id, created_at DESC);

CREATE TABLE IF NOT EXISTS learning_decisions_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    intent TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    confidence NUMERIC(6,4) NOT NULL,
    feedback_count INTEGER NOT NULL DEFAULT 0,
    window_days INTEGER NOT NULL DEFAULT 7,
    action TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'applied',
    snapshot_id UUID REFERENCES learning_policy_snapshots(id) ON DELETE SET NULL,
    details JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    rolled_back_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_learning_decisions_log_project_applied
ON learning_decisions_log(project_id, applied_at DESC);

CREATE TABLE IF NOT EXISTS vector_compaction_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    trigger_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'started',
    before_count BIGINT NOT NULL DEFAULT 0,
    after_count BIGINT NOT NULL DEFAULT 0,
    dedup_count BIGINT NOT NULL DEFAULT 0,
    deleted_count BIGINT NOT NULL DEFAULT 0,
    qdrant_deleted_count BIGINT NOT NULL DEFAULT 0,
    details JSONB NOT NULL DEFAULT '{}'::JSONB,
    requested_by UUID REFERENCES users(id) ON DELETE SET NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_vector_compaction_runs_started
ON vector_compaction_runs(started_at DESC);

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
    ('learning_prompt_corrections_enabled', 'true', 'learning', 'Enable runtime prompt corrections retrieval from vector memory', FALSE),
    ('vector_compaction_schedule_cron', '0 2 * * *', 'learning', 'Daily vector compaction schedule (server local time)', FALSE)
ON CONFLICT (key) DO NOTHING;
