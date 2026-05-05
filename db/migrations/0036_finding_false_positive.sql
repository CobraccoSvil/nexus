ALTER TABLE project_quality_findings
  ADD COLUMN IF NOT EXISTS is_false_positive BOOLEAN NOT NULL DEFAULT FALSE,
  ADD COLUMN IF NOT EXISTS false_positive_reason TEXT,
  ADD COLUMN IF NOT EXISTS false_positive_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS false_positive_rule_key TEXT;
