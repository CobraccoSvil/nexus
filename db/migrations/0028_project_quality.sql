CREATE TABLE IF NOT EXISTS project_quality_findings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    scanned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    file_path TEXT NOT NULL,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    title TEXT NOT NULL,
    detail TEXT NOT NULL,
    line_number INTEGER,
    fixed_at TIMESTAMPTZ,
    fixed_by_run_id UUID
);
CREATE INDEX IF NOT EXISTS idx_quality_project ON project_quality_findings(project_id, scanned_at DESC);
