-- Affidabilita' consegna comandi terminale: claim + ack esplicito
ALTER TABLE terminal_commands
    ADD COLUMN IF NOT EXISTS claimed_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS claimed_by TEXT,
    ADD COLUMN IF NOT EXISTS failed_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS fail_reason TEXT,
    ADD COLUMN IF NOT EXISTS output_preview TEXT;

CREATE INDEX IF NOT EXISTS idx_terminal_commands_claim
    ON terminal_commands(project_id, status, created_at, claimed_at);

