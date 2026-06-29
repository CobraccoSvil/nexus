-- Mig 0493 — Seed dei settings "fantasma" (letti nel codice ma privi di
-- migrazione versionata che li definisse).
--
-- Il gate ratchet `scripts/audit-settings.sh --gate` segnalava 2 settings
-- FANTASMA (letti via get_setting nel codice ma assenti dalle migrazioni; su un
-- DB fresco non esisterebbero, violando la regola G "il DB e' l'unica fonte"):
--   - agent.fs.read_full_max_lines       (crates/mcp-core/src/agent_tools/files.rs)
--   - agent.attachment.audio_max_bytes   (crates/nexus-agent-tools/src/audio_tools.rs)
--
-- Fix definitivo (regola G/H, NON alzare la baseline del ratchet): veicolare i due
-- settings con una migrazione versionata, coi valori-default gia' usati dal codice
-- come fallback. Idempotente: ON CONFLICT DO NOTHING (sui DB live dove erano gia'
-- stati inseriti a mano, no-op; su DB fresco li crea).

INSERT INTO settings (key, value, category, description, updated_at)
VALUES
    (
        'agent.fs.read_full_max_lines',
        '1200',
        'agent',
        'read_file: oltre questo numero di righe il file non viene riversato integralmente nel contesto ma si rimanda a read_file_lines + mappa (anti-overflow). 0 = nessun limite. Default 1200.',
        NOW()
    ),
    (
        'agent.attachment.audio_max_bytes',
        '26214400',
        'agent',
        'Dimensione massima (byte) di un allegato audio processabile dai tool audio (trascrizione). Default 26214400 (25 MB). Gemello di agent.attachment.image_max_bytes.',
        NOW()
    )
ON CONFLICT (key) DO NOTHING;
