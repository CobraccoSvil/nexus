-- Fix M10: tabella per errori runtime dei tool agente (run_command, browser-check, ecc.)
-- Popolata dai hook post-exec in agent_tools/exec.rs e da endpoint browser-check.
-- Il pannello Console Debug / Problemi del web-ide legge queste righe e mostra
-- per ciascuna il bottone "Risolvi con Nexus".

CREATE TABLE IF NOT EXISTS project_runtime_issues (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Sorgente dell'errore: 'run_command' / 'browser_check' / 'pnpm_install' / 'cargo_build' / etc.
    source      TEXT NOT NULL,
    -- Severita: 'error' / 'warning' / 'info'
    severity    TEXT NOT NULL DEFAULT 'error' CHECK (severity IN ('error', 'warning', 'info')),
    -- Messaggio breve (max 500 char)
    message     TEXT NOT NULL,
    -- Dettagli completi (stack trace, output stderr, ecc.)
    details     TEXT,
    -- Riferimento opzionale al run/step agente che ha causato l'errore
    run_id      UUID REFERENCES agent_runs(id) ON DELETE SET NULL,
    step_id     UUID REFERENCES agent_steps(id) ON DELETE SET NULL,
    -- Tool name che ha generato l'errore (utile per filtri UI)
    tool_name   TEXT,
    -- Comando eseguito (per run_command)
    command     TEXT,
    -- Exit code (per run_command)
    exit_code   INTEGER,
    -- Status: 'open' / 'in_progress' (un agente sta lavorandoci) / 'resolved'
    status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'in_progress', 'resolved')),
    -- Hash del message+command per deduplicare errori ricorrenti
    fingerprint TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_runtime_issues_project_status
    ON project_runtime_issues(project_id, status, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_runtime_issues_fingerprint
    ON project_runtime_issues(project_id, fingerprint)
    WHERE fingerprint IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_runtime_issues_run
    ON project_runtime_issues(run_id)
    WHERE run_id IS NOT NULL;
