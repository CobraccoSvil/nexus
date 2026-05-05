-- Migrazione 0110: tabella nexus_intent_capability
--
-- Sostituisce i match Rust statici in crates/mcp-core/src/orchestrator.rs:
--   - righe 444-461: match intent { ... } per required_tier
--   - righe 478-490: match intent { ... } per required_capability
--   - righe 589-594: match intent_key { ... } per preferred_provider
--
-- Cambiare il tier/capability di un intent diventa un UPDATE sulla tabella,
-- niente patch+redeploy. Cache lato Rust 60s con pattern RoutingMatrixCache
-- (vedi crates/mcp-core/src/routing_matrix.rs).
--
-- Regola G del CLAUDE.md: nessun match hardcoded di decisioni di routing
-- nel codice Rust.

CREATE TABLE IF NOT EXISTS nexus_intent_capability (
    intent                 TEXT PRIMARY KEY,
    base_tier              TEXT NOT NULL CHECK (base_tier IN ('light','medium','heavy')),
    base_capability        TEXT NOT NULL CHECK (base_capability IN ('chat','code','reasoning','docs')),
    preferred_provider     TEXT,
    medium_token_threshold INT,
    heavy_token_threshold  INT,
    notes                  TEXT,
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE nexus_intent_capability IS
'Mappa intent -> (base_tier, base_capability, preferred_provider) per routing dinamico DB-driven. Letta dal Rust orchestrator con cache 60s. Sostituisce match statici in orchestrator.rs (mig 0110).';

COMMENT ON COLUMN nexus_intent_capability.medium_token_threshold IS
'Se non NULL, sopra questo numero di token l''intent passa da light a medium (es. fix passa medium se >3000 token).';

COMMENT ON COLUMN nexus_intent_capability.heavy_token_threshold IS
'Se non NULL, sopra questo numero di token l''intent passa a heavy.';

-- Seed: replica esatta dei match Rust attuali (orchestrator.rs:444-490)
INSERT INTO nexus_intent_capability
    (intent, base_tier, base_capability, preferred_provider, medium_token_threshold, heavy_token_threshold, notes) VALUES
    ('debug',        'heavy',  'reasoning', 'anthropic', NULL, NULL,
     'Tool call multi-step (read_file -> str_replace -> restart_service)'),
    ('architecture', 'heavy',  'reasoning', 'anthropic', NULL, NULL,
     'Decisioni strutturali ad alto impatto'),
    ('system_admin', 'heavy',  'reasoning', 'anthropic', NULL, NULL,
     'Configura servizi, crea utenti, deploy: tool use solido necessario'),
    ('file_ops',     'medium', 'reasoning', 'anthropic', NULL, NULL,
     'Creare/eliminare/spostare file: tool use solido necessario'),
    ('refactor',     'light',  'reasoning', 'anthropic', 3000, NULL,
     'Up-tier a medium se piu di 3k token'),
    ('fix',          'light',  'code',      NULL,        3000, NULL,
     'Up-tier a medium se piu di 3k token (fix complessi)'),
    ('test',         'light',  'code',      NULL,        NULL, NULL,
     'Test unitari: modelli light bastano'),
    ('docs',         'medium', 'docs',      'openai',    NULL, NULL,
     'Documentazione: capability dedicata docs'),
    ('chat',         'light',  'chat',      'openai',    NULL, 6000,
     'Chat conversazionale; up-tier heavy solo per context lunghi (>6k token)')
ON CONFLICT (intent) DO NOTHING;
