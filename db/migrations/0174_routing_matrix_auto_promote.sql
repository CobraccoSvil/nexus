-- Migrazione 0174: routing matrix auto-promoter.
--
-- Aggiunge supporto per il worker `routing_matrix_auto_promoter` che
-- ricostruisce periodicamente le righe della routing matrix dal catalog
-- modelli in base a regole stabili (tier, cost, capability, health).
--
-- Cosi':
--   - quando esce un nuovo modello su LiteLLM, `catalog_sync_worker` lo
--     porta in `ai_price_catalog`, poi `routing_matrix_auto_promoter` lo
--     promuove automaticamente nelle righe della routing matrix dove e' il
--     "best fit" per (intent, behavior_mode);
--   - quando un modello viene auto-disabled dal `model_health_probe`,
--     viene sostituito al prossimo run dell'auto-promoter;
--   - l'admin puo' fissare manualmente una riga: setta `manual_override=true`
--     e l'auto-promoter NON la tocca piu'.

ALTER TABLE nexus_routing_matrix
    ADD COLUMN IF NOT EXISTS manual_override BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE nexus_routing_matrix
    ADD COLUMN IF NOT EXISTS last_auto_promote_at TIMESTAMPTZ;

ALTER TABLE nexus_routing_matrix
    ADD COLUMN IF NOT EXISTS auto_promote_score REAL;

COMMENT ON COLUMN nexus_routing_matrix.manual_override IS
'true = riga gestita dall admin (auto-promoter non la sovrascrive). false = riga gestita automaticamente.';
COMMENT ON COLUMN nexus_routing_matrix.last_auto_promote_at IS
'Timestamp dell ultimo ricalcolo automatico. NULL se mai promossa.';
COMMENT ON COLUMN nexus_routing_matrix.auto_promote_score IS
'Score 0..1 dell auto-promoter, mostrato in UI come "fitness" della scelta.';

-- Settings per il worker.
INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('routing_matrix_auto_promote_enabled', 'true', 'ai',
     'Abilita il worker che ricostruisce la routing matrix dal catalog.', false),
    ('routing_matrix_auto_promote_interval_s', '21600', 'ai',
     'Cadenza ricalcolo routing matrix (default 6h, minimo 600).', false)
ON CONFLICT (key) DO NOTHING;

-- Tabella di mapping intent -> requirement (capability + tier preferito).
-- Usata dall'auto-promoter per filtrare/ordinare i candidati.
CREATE TABLE IF NOT EXISTS nexus_intent_routing_requirements (
    intent TEXT NOT NULL,
    behavior_mode TEXT NOT NULL,
    -- Capabilities che il modello DEVE avere (array of capability strings).
    -- Confronto: (catalog.capabilities @> required_capabilities) OR cardinality(required_capabilities)=0
    required_capabilities TEXT[] NOT NULL DEFAULT '{}',
    -- Filtro tool support (necessario per intent agente).
    requires_tool_use BOOLEAN NOT NULL DEFAULT false,
    -- Tier preferito: 'light'|'medium'|'heavy'.
    preferred_tier TEXT NOT NULL DEFAULT 'medium',
    -- Pesi dello scoring: somma a 1.0.
    weight_tier REAL NOT NULL DEFAULT 0.35,
    weight_cost REAL NOT NULL DEFAULT 0.25,
    weight_context REAL NOT NULL DEFAULT 0.20,
    weight_capabilities REAL NOT NULL DEFAULT 0.20,
    -- 'asc'|'desc' sul costo (economica=asc, approfondita=desc).
    cost_direction TEXT NOT NULL DEFAULT 'asc',
    PRIMARY KEY (intent, behavior_mode)
);

COMMENT ON TABLE nexus_intent_routing_requirements IS
'Regole di scoring per il routing_matrix_auto_promoter: per ogni (intent, behavior_mode) definisce capability richieste, tier preferito e pesi scoring.';

-- Seed: regole iniziali derivate da quello che gia c'e' in routing matrix.
INSERT INTO nexus_intent_routing_requirements
    (intent, behavior_mode, required_capabilities, requires_tool_use, preferred_tier, cost_direction) VALUES
    -- Chat: niente tool, modelli light/medium veloci.
    ('chat_breve',    'veloce',       '{"chat"}',                        false, 'light',  'asc'),
    ('chat_breve',    'economica',    '{"chat"}',                        false, 'light',  'asc'),
    ('chat_breve',    'bilanciata',   '{"chat"}',                        false, 'medium', 'asc'),
    ('chat_breve',    'approfondita', '{"chat"}',                        false, 'medium', 'desc'),
    ('chat_media',    'veloce',       '{"chat"}',                        false, 'medium', 'asc'),
    ('chat_media',    'economica',    '{"chat"}',                        false, 'light',  'asc'),
    ('chat_media',    'bilanciata',   '{"chat"}',                        false, 'medium', 'asc'),
    ('chat_media',    'approfondita', '{"chat","reasoning"}',            false, 'heavy',  'desc'),
    ('chat_lunga',    'veloce',       '{"chat","long-context"}',         false, 'medium', 'asc'),
    ('chat_lunga',    'economica',    '{"chat","long-context"}',         false, 'medium', 'asc'),
    ('chat_lunga',    'bilanciata',   '{"chat","long-context"}',         false, 'medium', 'asc'),
    ('chat_lunga',    'approfondita', '{"chat","reasoning","long-context"}', false, 'heavy',  'desc'),
    -- Fix / refactor: tool use obbligatorio.
    ('fix_semplice',  'veloce',       '{"code","fix"}',                  true,  'light',  'asc'),
    ('fix_semplice',  'economica',    '{"code","fix"}',                  true,  'light',  'asc'),
    ('fix_semplice',  'bilanciata',   '{"code","fix"}',                  true,  'medium', 'asc'),
    ('fix_semplice',  'approfondita', '{"code","fix"}',                  true,  'medium', 'desc'),
    ('fix_complesso', 'veloce',       '{"code","fix"}',                  true,  'medium', 'asc'),
    ('fix_complesso', 'economica',    '{"code","fix"}',                  true,  'medium', 'asc'),
    ('fix_complesso', 'bilanciata',   '{"code","fix","reasoning"}',      true,  'heavy',  'desc'),
    ('fix_complesso', 'approfondita', '{"code","fix","reasoning"}',      true,  'heavy',  'desc'),
    ('refactor',      'veloce',       '{"code"}',                        true,  'medium', 'asc'),
    ('refactor',      'economica',    '{"code"}',                        true,  'medium', 'asc'),
    ('refactor',      'bilanciata',   '{"code","long-context"}',         true,  'medium', 'desc'),
    ('refactor',      'approfondita', '{"code","reasoning","long-context"}', true, 'heavy', 'desc'),
    -- Test / docs.
    ('test',          'veloce',       '{"code","test"}',                 true,  'medium', 'asc'),
    ('test',          'economica',    '{"code","test"}',                 true,  'light',  'asc'),
    ('test',          'bilanciata',   '{"code","test"}',                 true,  'medium', 'asc'),
    ('test',          'approfondita', '{"code","test","reasoning"}',     true,  'heavy',  'desc'),
    ('docs',          'veloce',       '{"chat"}',                        false, 'medium', 'asc'),
    ('docs',          'economica',    '{"chat"}',                        false, 'light',  'asc'),
    ('docs',          'bilanciata',   '{"chat"}',                        false, 'medium', 'asc'),
    ('docs',          'approfondita', '{"chat","reasoning"}',            false, 'heavy',  'desc'),
    -- Architecture: ragionamento, contesto lungo, tier alto.
    ('architecture',  'veloce',       '{"reasoning"}',                   false, 'medium', 'asc'),
    ('architecture',  'economica',    '{"reasoning"}',                   false, 'medium', 'asc'),
    ('architecture',  'bilanciata',   '{"reasoning","long-context"}',    false, 'heavy',  'desc'),
    ('architecture',  'approfondita', '{"reasoning","long-context"}',    false, 'heavy',  'desc'),
    -- File ops: tool use essenziale, costo basso.
    ('file_ops',      'veloce',       '{"code"}',                        true,  'light',  'asc'),
    ('file_ops',      'economica',    '{"code"}',                        true,  'light',  'asc'),
    ('file_ops',      'bilanciata',   '{"code"}',                        true,  'medium', 'asc'),
    ('file_ops',      'approfondita', '{"code"}',                        true,  'medium', 'desc')
ON CONFLICT (intent, behavior_mode) DO NOTHING;

-- Endpoint admin per la lista (futuro).
COMMENT ON TABLE nexus_intent_routing_requirements IS
'Regole di scoring auto-promoter. Modificabile da admin via UI o SQL. Il worker rilegge ogni run senza restart.';
