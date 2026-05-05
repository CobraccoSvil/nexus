CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE projects
ADD COLUMN IF NOT EXISTS owner_user_id UUID REFERENCES users(id),
ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'private',
ADD COLUMN IF NOT EXISTS last_opened_by_user_id UUID REFERENCES users(id);

UPDATE projects
SET owner_user_id = (
    SELECT pm.user_id
    FROM project_members pm
    WHERE pm.project_id = projects.id
    ORDER BY pm.created_at ASC
    LIMIT 1
)
WHERE owner_user_id IS NULL;

ALTER TABLE projects
ALTER COLUMN owner_user_id SET NOT NULL;

ALTER TABLE repositories
ADD COLUMN IF NOT EXISTS root_path TEXT,
ADD COLUMN IF NOT EXISTS is_git_repo BOOLEAN NOT NULL DEFAULT FALSE,
ADD COLUMN IF NOT EXISTS current_branch TEXT;

CREATE TABLE IF NOT EXISTS user_project_preferences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    preferences JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, project_id)
);

CREATE TABLE IF NOT EXISTS project_open_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    workspace_id UUID REFERENCES workspaces(id) ON DELETE SET NULL,
    active_file_paths JSONB NOT NULL DEFAULT '[]'::JSONB,
    terminal_cwd TEXT,
    last_opened_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, project_id)
);

CREATE TABLE IF NOT EXISTS git_operations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    workspace_id UUID REFERENCES workspaces(id) ON DELETE SET NULL,
    branch TEXT,
    operation TEXT NOT NULL,
    status TEXT NOT NULL,
    stdout TEXT NOT NULL DEFAULT '',
    stderr TEXT NOT NULL DEFAULT '',
    metadata JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS git_status_snapshots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    workspace_id UUID REFERENCES workspaces(id) ON DELETE SET NULL,
    branch TEXT,
    status_json JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS git_remotes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    fetch_url TEXT NOT NULL,
    push_url TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(repository_id, name)
);

CREATE INDEX IF NOT EXISTS idx_projects_owner_user_id ON projects(owner_user_id);
CREATE INDEX IF NOT EXISTS idx_project_open_sessions_user_id ON project_open_sessions(user_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_operations_project_id ON git_operations(project_id, created_at DESC);
