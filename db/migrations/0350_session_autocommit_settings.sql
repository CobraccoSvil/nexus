-- 0350_session_autocommit_settings.sql
--
-- Seed dei settings di configurazione del modulo `session_autocommit` (auto-commit
-- per sessione su branch nexus/session/<short_id>). Regola G: tutta la
-- configurazione vive nel DB, niente env var ne' default hardcoded.
--
-- - agent.autocommit.enabled (bool, default true): kill switch globale.
-- - agent.autocommit.branch_prefix (text, default 'nexus/session/'): namespace
--   dei branch creati. Cambiare il prefisso non migra i branch esistenti (il
--   modulo continua a leggere il nuovo prefisso per i commit futuri).
--
-- Idempotente: ON CONFLICT DO NOTHING. Non sovrascrive eventuali override admin.

INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.autocommit.enabled', 'true', 'agent',
    'Se true, ogni mutazione file dell''agente produce un commit su un branch dedicato '
    || '`<branch_prefix><short_session>` (rete di sicurezza secondaria sopra file_mutations). '
    || 'No-op silenzioso se il progetto non e'' un repo git. Niente push remoto.'
),
(
    'agent.autocommit.branch_prefix', 'nexus/session/', 'agent',
    'Prefisso dei branch creati dal session_autocommit. Il branch finale e'' '
    || '`<prefix><short_session_id>` (es. `nexus/session/a1b2c3d4`). Lascia il / finale.'
)
ON CONFLICT (key) DO NOTHING;
