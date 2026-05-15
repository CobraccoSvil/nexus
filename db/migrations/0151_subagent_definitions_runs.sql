-- PR-3 sub-agents pattern: tabelle definitions + runs.
--
-- Pattern Claude Code / Cursor: main agent persistente + sub-agents
-- spawnabili runtime con context isolato. La definition dichiara prompt,
-- tool whitelist, modello purpose, max_iterations, timeout, is_background.
-- Ogni esecuzione e' tracciata in nexus_subagent_runs con parent_run_id.

CREATE TABLE IF NOT EXISTS nexus_subagent_definitions (
    kind            TEXT PRIMARY KEY,
    description     TEXT NOT NULL,                     -- usato per auto-delegation by description (Cursor style)
    prompt_key      TEXT NOT NULL,                     -- riferimento a nexus_prompt_templates
    tool_whitelist  TEXT[] NOT NULL,                   -- nomi tool ammessi
    model_purpose   TEXT NOT NULL,                     -- chiave in nexus_purpose_model
    max_iterations  INTEGER NOT NULL DEFAULT 25,
    timeout_s       INTEGER NOT NULL DEFAULT 300,
    is_background   BOOLEAN NOT NULL DEFAULT false,
    is_enabled      BOOLEAN NOT NULL DEFAULT true,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS nexus_subagent_runs (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_run_id     UUID NOT NULL,                   -- FK a agent_runs.id (no constraint per legacy compat)
    project_id        UUID NOT NULL,
    kind              TEXT NOT NULL,
    task_description  TEXT NOT NULL,
    context_blob      TEXT,
    expected_format   TEXT,
    status            TEXT NOT NULL CHECK (status IN ('pending','running','completed','failed','timeout','cancelled','paused')),
    is_background     BOOLEAN NOT NULL DEFAULT false,
    resumable_token   TEXT,
    final_summary     TEXT,
    artifacts         TEXT[] DEFAULT '{}',
    iterations        INTEGER DEFAULT 0,
    tokens_prompt     INTEGER DEFAULT 0,
    tokens_completion INTEGER DEFAULT 0,
    cost_usd          NUMERIC(12, 6) DEFAULT 0,
    depth             INTEGER NOT NULL DEFAULT 1,
    source            TEXT NOT NULL DEFAULT 'db' CHECK (source IN ('db','project_override')),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at      TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_subagent_runs_parent ON nexus_subagent_runs(parent_run_id);
CREATE INDEX IF NOT EXISTS idx_subagent_runs_project ON nexus_subagent_runs(project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_subagent_runs_kind_status ON nexus_subagent_runs(kind, status);
CREATE INDEX IF NOT EXISTS idx_subagent_runs_bg
    ON nexus_subagent_runs(parent_run_id, is_background)
    WHERE status IN ('running','paused');

-- Seed dei 5 kind base (Claude Code analog: Plan/Explore/Task + Verify/Review).
-- I prompt_key referenziati saranno seedati in mig 0152.
INSERT INTO nexus_subagent_definitions (kind, description, prompt_key, tool_whitelist, model_purpose, max_iterations, timeout_s, is_background) VALUES
    ('plan',      'Pianifica task complessi in context isolato. Use proactively per task multi-step.', 'subagent.plan.base',      ARRAY['list_files','read_file','search_in_files','recall_context','nexus_todo_write'], 'planner', 12, 180, false),
    ('explore',   'Esplora il codebase e ritorna un summary di 200-600 char. Use proactively per analisi precedenti modifiche.', 'subagent.explore.base',   ARRAY['list_files','read_file','search_in_files','recall_context','search_codebase_semantic'], 'explorer', 20, 240, false),
    ('implement', 'Esegue un sotto-task implementativo isolato (single feature/file).', 'subagent.implement.base', ARRAY['read_file','write_file','edit_file','run_command','list_files','search_in_files'], 'planner', 30, 600, false),
    ('verify',    'Verifica deterministica con LLM-assist per interpretare output test/build.', 'subagent.verify.base',    ARRAY['read_file','run_command','list_files'], 'verifier', 10, 180, false),
    ('review',    'Code review post-implementazione. Output: lista issue con severity.', 'subagent.review.base',    ARRAY['list_files','read_file','search_in_files','run_command'], 'reviewer', 15, 240, false)
ON CONFLICT (kind) DO UPDATE SET
    description = EXCLUDED.description,
    prompt_key = EXCLUDED.prompt_key,
    tool_whitelist = EXCLUDED.tool_whitelist,
    model_purpose = EXCLUDED.model_purpose,
    max_iterations = EXCLUDED.max_iterations,
    timeout_s = EXCLUDED.timeout_s,
    is_background = EXCLUDED.is_background,
    updated_at = NOW();

-- nexus_purpose_model entries per i sub-agent kinds (modelli cheap/fast).
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes, updated_at) VALUES
    ('explorer', 'anthropic', 'claude-haiku-4-5-20251001', 'Modello veloce per sub-agent explore: ricerca + summary breve.', NOW()),
    ('reviewer', 'anthropic', 'claude-sonnet-4-6',         'Modello capable per code review post-implementazione.', NOW())
ON CONFLICT (purpose) DO UPDATE SET
    provider = EXCLUDED.provider,
    model_id = EXCLUDED.model_id,
    notes = EXCLUDED.notes,
    updated_at = NOW();
