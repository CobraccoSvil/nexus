-- Migrazione 0289 — Sudo Manager Livello 1 (whitelist).
--
-- Risolve la classe di bug "Nexus non puo' fare X perche' richiede sudo"
-- (es. apt-get install librerie sistema per Playwright, vedi incident chat 6).
-- Nessuna password sudo viene mai salvata: la sicurezza viene da
-- /etc/sudoers.d/nexus-runner che concede NOPASSWD SOLO al binary
-- /usr/local/bin/nexus-sudo-runner. Il runner consulta nexus_sudo_purposes
-- per validare il purpose richiesto contro una whitelist DB + pattern
-- hardcoded nel binary stesso (defense-in-depth).
--
-- Setup (one-time, manuale): bash deploy/install-sudo-manager.sh
-- Esecuzione da mcp-core: sudo_manager::execute("playwright-install-deps")
--
-- Regola H: niente storage credenziali, niente sudo arbitrario. Whitelist + audit.
-- Regola G: tutti i comandi sono in DB (modificabili da Admin UI), niente hardcode.

BEGIN;

-- ─────────────────────────── Whitelist purposes ────────────────────────────
CREATE TABLE IF NOT EXISTS nexus_sudo_purposes (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                TEXT NOT NULL UNIQUE,         -- es. "playwright-install-deps"
    description         TEXT NOT NULL,                -- mostrato in UI admin
    command_template    TEXT NOT NULL,                -- es. "apt-get install -y libnspr4 libnss3 ..."
    requires_confirm    BOOLEAN NOT NULL DEFAULT TRUE,
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    category            TEXT NOT NULL DEFAULT 'general',  -- general|playwright|service|filesystem
    created_by          TEXT NOT NULL DEFAULT 'system',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- CHECK: name kebab-case, niente shell metacharacters
    CONSTRAINT nexus_sudo_purposes_name_format
        CHECK (name ~ '^[a-z][a-z0-9-]{2,63}$'),
    -- CHECK: command_template niente metacaratteri pericolosi
    -- (sicurezza secondaria: il binary runner ha la sua allowlist piu' stretta)
    CONSTRAINT nexus_sudo_purposes_command_safe
        CHECK (command_template !~ '[;&|`$<>]')
);

CREATE INDEX IF NOT EXISTS idx_nexus_sudo_purposes_category_enabled
    ON nexus_sudo_purposes (category, enabled);

-- ─────────────────────────── Audit log immutabile ──────────────────────────
CREATE TABLE IF NOT EXISTS nexus_sudo_audit_log (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    purpose_name        TEXT NOT NULL,                -- nome purpose al momento dell'esecuzione
    full_command        TEXT NOT NULL,                -- comando finale eseguito
    requested_by_service TEXT,                        -- "mcp-core", "admin-ui", ...
    requested_by_user   TEXT,                         -- user_id o email
    exit_code           INTEGER,
    stdout_excerpt      TEXT,                         -- max 4KB (troncato)
    stderr_excerpt      TEXT,                         -- max 4KB (troncato)
    duration_ms         INTEGER,
    executed_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_nexus_sudo_audit_log_executed_at
    ON nexus_sudo_audit_log (executed_at DESC);
CREATE INDEX IF NOT EXISTS idx_nexus_sudo_audit_log_purpose
    ON nexus_sudo_audit_log (purpose_name, executed_at DESC);

-- ─────────────────────────── Seed purposes iniziali ────────────────────────
-- Quelli che servono SUBITO per sbloccare chat 6 (Playwright deps).
-- Aggiunti con requires_confirm=TRUE: la UI admin deve mostrare conferma
-- esplicita prima di eseguire (impedisce ripetizione automatica involontaria).
INSERT INTO nexus_sudo_purposes (name, description, command_template, category) VALUES
    (
        'playwright-install-deps',
        'Installa le librerie di sistema necessarie a chromium-headless-shell (Playwright). Risolve l''errore "Target page, context or browser has been closed" quando il binary del browser non puo'' avviarsi per assenza di libnspr4/libnss3/libasound. Da eseguire una volta dopo setup nuovo del WSL/container.',
        'apt-get install -y libnspr4 libnss3 libnssutil3 libasound2t64 libxss1 libgbm1 libgtk-3-0 libpangocairo-1.0-0 libatk1.0-0t64 libatk-bridge2.0-0t64 libcups2t64 libxshmfence1',
        'playwright'
    ),
    (
        'apt-update',
        'Aggiorna l''indice dei pacchetti APT (apt-get update). Prerequisito per qualsiasi installazione di librerie sistema.',
        'apt-get update',
        'general'
    )
ON CONFLICT (name) DO NOTHING;

-- ─────────────────────────── Settings sudo manager ─────────────────────────
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.sudo.manager_enabled', 'true', 'agent',
     'Se true, mcp-core puo'' invocare sudo_manager::execute per i purpose nella whitelist. Disattiva (false) per smoke test o ambienti dove sudoers.d non e'' configurato.',
     NOW()),
    ('agent.sudo.runner_path', '/usr/local/bin/nexus-sudo-runner', 'agent',
     'Path assoluto del binary nexus-sudo-runner. Modificabile se installato in path diverso (utili per multi-tenant). Configurato in /etc/sudoers.d/nexus-runner.',
     NOW()),
    ('agent.sudo.audit_excerpt_max_bytes', '4096', 'agent',
     'Limite (bytes) di stdout/stderr troncati salvati in nexus_sudo_audit_log. Default 4096.',
     NOW())
ON CONFLICT (key) DO NOTHING;

COMMIT;
