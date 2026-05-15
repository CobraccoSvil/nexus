-- PR-1 Plan/Act/Verify orchestrator: schema per planner + TodoList + verifier runs.
--
-- Le 3 tabelle introdotte sono il core persistente del nuovo flusso Plan/Act/Verify
-- (analogo pattern Claude Code). Tutto e' filtrato per project_id per garantire
-- isolamento multi-tenant.
--
-- Le tabelle restano vuote finche' orchestrator.plan_phase_enabled = false
-- (default OFF), quindi questa migrazione e' safe da applicare in qualunque
-- momento senza impatti sui flussi esistenti.

-- nexus_agent_plans: un piano per run (1:1 con un agent run quando il planner
-- e' attivo). acceptance_criteria sono i criteri DoD globali del plan; i
-- criteri specifici per todo vivono in nexus_agent_todos.acceptance_criteria.
CREATE TABLE IF NOT EXISTS nexus_agent_plans (
    run_id              UUID PRIMARY KEY,
    project_id          UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    thread_id           TEXT NOT NULL,
    acceptance_criteria JSONB NOT NULL DEFAULT '[]'::jsonb,
    planner_model       TEXT NOT NULL,
    approved_at         TIMESTAMPTZ,
    approved_by         UUID,
    score               DOUBLE PRECISION,
    plan_revisions      INTEGER NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_plans_project ON nexus_agent_plans(project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_plans_thread ON nexus_agent_plans(thread_id);

-- nexus_agent_todos: items della checklist del plan. seq garantisce ordinamento
-- stabile. acceptance_criteria locale al todo per check granulari del verifier.
CREATE TABLE IF NOT EXISTS nexus_agent_todos (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id              UUID NOT NULL REFERENCES nexus_agent_plans(run_id) ON DELETE CASCADE,
    project_id          UUID NOT NULL,
    seq                 INTEGER NOT NULL,
    content             TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('pending','in_progress','completed','blocked','skipped')),
    priority            TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('high','normal','low')),
    acceptance_criteria JSONB NOT NULL DEFAULT '[]'::jsonb,
    verify_failures     INTEGER NOT NULL DEFAULT 0,
    iteration_seen      INTEGER NOT NULL DEFAULT 0,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_todos_run_seq ON nexus_agent_todos(run_id, seq);
CREATE INDEX IF NOT EXISTS idx_todos_status ON nexus_agent_todos(run_id, status);
CREATE INDEX IF NOT EXISTS idx_todos_project ON nexus_agent_todos(project_id);

-- nexus_agent_verifier_runs: log dei cicli di verifica. Popolata solo quando
-- verifier_node sara' attivo (PR-2), ma lo schema vive in PR-1 per evitare
-- migrazioni successive cross-PR.
CREATE TABLE IF NOT EXISTS nexus_agent_verifier_runs (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id           UUID NOT NULL REFERENCES nexus_agent_plans(run_id) ON DELETE CASCADE,
    todo_id          UUID REFERENCES nexus_agent_todos(id) ON DELETE CASCADE,
    cycle            INTEGER NOT NULL,
    criteria_results JSONB NOT NULL,
    passed           BOOLEAN NOT NULL,
    duration_ms      INTEGER,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_verifier_runs_run ON nexus_agent_verifier_runs(run_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_verifier_runs_todo ON nexus_agent_verifier_runs(todo_id);
