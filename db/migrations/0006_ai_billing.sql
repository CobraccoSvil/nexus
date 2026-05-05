CREATE TABLE IF NOT EXISTS ai_price_catalog (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_cost_per_million_tokens NUMERIC(18, 6) NOT NULL,
    output_cost_per_million_tokens NUMERIC(18, 6) NOT NULL,
    currency TEXT NOT NULL,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to TIMESTAMPTZ,
    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ai_price_catalog_lookup
    ON ai_price_catalog(provider, model, effective_from DESC)
    WHERE is_enabled = TRUE;

CREATE TABLE IF NOT EXISTS ai_quota_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope_type TEXT NOT NULL CHECK (scope_type IN ('user', 'project', 'user_project')),
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
    token_limit BIGINT,
    cost_limit NUMERIC(18, 6),
    currency TEXT,
    valid_from TIMESTAMPTZ NOT NULL,
    valid_to TIMESTAMPTZ NOT NULL,
    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (valid_to > valid_from),
    CHECK (
        (scope_type = 'user' AND user_id IS NOT NULL AND project_id IS NULL) OR
        (scope_type = 'project' AND user_id IS NULL AND project_id IS NOT NULL) OR
        (scope_type = 'user_project' AND user_id IS NOT NULL AND project_id IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_ai_quota_policies_scope
    ON ai_quota_policies(scope_type, user_id, project_id, valid_from, valid_to)
    WHERE is_enabled = TRUE;

CREATE TABLE IF NOT EXISTS ai_usage_ledger (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id UUID REFERENCES orchestrator_runs(id) ON DELETE SET NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    input_cost NUMERIC(18, 6) NOT NULL DEFAULT 0,
    output_cost NUMERIC(18, 6) NOT NULL DEFAULT 0,
    total_cost NUMERIC(18, 6) NOT NULL DEFAULT 0,
    currency TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('reserved', 'finalized', 'rejected', 'failed', 'released')),
    rejection_reason TEXT,
    details JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finalized_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_ai_usage_ledger_user_time
    ON ai_usage_ledger(user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_ai_usage_ledger_project_time
    ON ai_usage_ledger(project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_ai_usage_ledger_status_time
    ON ai_usage_ledger(status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_ai_usage_ledger_provider_model
    ON ai_usage_ledger(provider, model, created_at DESC);

ALTER TABLE orchestrator_runs
ADD COLUMN IF NOT EXISTS user_id UUID REFERENCES users(id) ON DELETE SET NULL;

ALTER TABLE orchestrator_runs
ADD COLUMN IF NOT EXISTS audit_json JSONB;

UPDATE orchestrator_runs
SET audit_json = audit
WHERE audit_json IS NULL AND audit IS NOT NULL;

INSERT INTO settings (key, value, category, description, is_secret)
VALUES ('billing_base_currency', 'EUR', 'routing', 'Base currency used for AI accounting and quotas', FALSE)
ON CONFLICT (key) DO NOTHING;
