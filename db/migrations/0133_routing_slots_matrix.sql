-- 0133: Slot-filling routing matrix (Livello 4 disambiguation framework)
--
-- Aggiunge una matrice di routing indicizzata su slot canonici estratti dal
-- task dell'utente: (action_verb, target_type, scope) → (provider, model).
-- E' piu' precisa della classica (intent, behavior_mode) perche':
--   - distingue "write tests" (light model) da "resolve test failures" (capable)
--   - distingue scope single-file (light) da multi-file (capable)
--   - permette override per framework specifici (es. cargo richiede Rust expertise)
--
-- Schema (vedi piano sezione Livello 4):
--   action_verb : read|write|resolve|analyze|refactor|configure|deploy|delete
--   target_type : code|tests|config|service|docs|data|infrastructure
--   framework   : stringa libera, '*' = wildcard (playwright, pytest, cargo, ...)
--   scope       : single|multi_file|cross_service|system_wide
--
-- Lookup: gerarchico con fallback wildcard sui campi piu' specifici.
-- Vedi `SlotsRoutingMatrix::lookup()` in crates/mcp-core/src/routing_slots.rs.

BEGIN;

CREATE TABLE IF NOT EXISTS nexus_routing_slots_matrix (
    id              BIGSERIAL PRIMARY KEY,
    action_verb     TEXT NOT NULL,
    target_type     TEXT NOT NULL,
    framework       TEXT NOT NULL DEFAULT '*',
    scope           TEXT NOT NULL,
    provider        TEXT NOT NULL,
    model_id        TEXT NOT NULL,
    priority        INT  NOT NULL DEFAULT 100,
    is_active       BOOLEAN NOT NULL DEFAULT TRUE,
    rationale       TEXT NOT NULL DEFAULT '',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT slots_action_verb_valid CHECK (
        action_verb IN ('read','write','resolve','analyze','refactor',
                        'configure','deploy','delete')
    ),
    CONSTRAINT slots_target_type_valid CHECK (
        target_type IN ('code','tests','config','service','docs','data',
                        'infrastructure','*')
    ),
    CONSTRAINT slots_scope_valid CHECK (
        scope IN ('single','multi_file','cross_service','system_wide','*')
    ),

    -- Una sola entry per (action, target, framework, scope, provider).
    -- Piu' provider sulla stessa chiave = chain di fallback ordinata per priority DESC.
    UNIQUE (action_verb, target_type, framework, scope, provider)
);

CREATE INDEX IF NOT EXISTS idx_slots_lookup
    ON nexus_routing_slots_matrix (action_verb, target_type, scope)
    WHERE is_active = TRUE;

CREATE INDEX IF NOT EXISTS idx_slots_framework
    ON nexus_routing_slots_matrix (framework)
    WHERE framework != '*' AND is_active = TRUE;

-- ─────────────────────────────────────────────────────────────────────
-- SEED: combinazioni piu' frequenti, ordinate per priority DESC.
-- ─────────────────────────────────────────────────────────────────────
-- I seed coprono i casi reali piu' visti, NON tutte le 256 combinazioni:
-- il lookup fa fallback gerarchico (specifico → wildcard → intent classico).

-- RESOLVE: il caso paradigmatico Redemptor — "esegui test e risolvi fail"
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    -- Resolve test failures: serve modello capable, multi-file editing.
    ('resolve', 'tests', '*', 'multi_file', 'anthropic', 'claude-sonnet-4-6', 100,
     'Risoluzione fail test multi-file: capable model per debug + edit coordinato'),
    ('resolve', 'tests', '*', 'multi_file', 'mistral', 'mistral-large-2411', 90,
     'Fallback: Mistral Large per multi-file edit'),
    ('resolve', 'tests', 'playwright', 'multi_file', 'anthropic', 'claude-sonnet-4-6', 110,
     'Playwright tests + multi-file: override esplicito (caso Redemptor)'),
    ('resolve', 'tests', '*', 'single', 'anthropic', 'claude-haiku-4-5-20251001', 100,
     'Risoluzione test single-file: Haiku basta'),
    ('resolve', 'tests', '*', 'cross_service', 'anthropic', 'claude-opus-4-6', 100,
     'Test cross-service: Opus per ragionamento architetturale'),

    -- Resolve code bugs
    ('resolve', 'code', '*', 'single', 'anthropic', 'claude-haiku-4-5-20251001', 100,
     'Bug fix single-file: Haiku'),
    ('resolve', 'code', '*', 'single', 'deepseek', 'deepseek-chat', 80,
     'Fallback: DeepSeek per bug semplici'),
    ('resolve', 'code', '*', 'multi_file', 'anthropic', 'claude-sonnet-4-6', 100,
     'Bug fix multi-file: Sonnet'),
    ('resolve', 'code', '*', 'cross_service', 'anthropic', 'claude-opus-4-6', 100,
     'Bug fix cross-service: Opus'),

    -- Resolve config/service issues
    ('resolve', 'config', '*', 'single', 'anthropic', 'claude-haiku-4-5-20251001', 100, ''),
    ('resolve', 'config', '*', 'multi_file', 'anthropic', 'claude-sonnet-4-6', 100, ''),
    ('resolve', 'service', '*', 'cross_service', 'anthropic', 'claude-sonnet-4-6', 100,
     'Risoluzione problema servizio cross: Sonnet')
ON CONFLICT DO NOTHING;

-- WRITE: scrittura nuovo codice/test/docs
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    ('write', 'tests', '*', 'single', 'openai', 'gpt-4.1-mini', 100,
     'Scrittura test single-file: light model basta'),
    ('write', 'tests', '*', 'single', 'deepseek', 'deepseek-chat', 80, ''),
    ('write', 'tests', '*', 'multi_file', 'anthropic', 'claude-haiku-4-5-20251001', 100, ''),
    ('write', 'tests', 'cargo', '*', 'anthropic', 'claude-sonnet-4-6', 110,
     'Test Rust: serve expertise specifica, Sonnet'),
    ('write', 'code', '*', 'single', 'openai', 'gpt-4.1-mini', 100, ''),
    ('write', 'code', '*', 'multi_file', 'anthropic', 'claude-haiku-4-5-20251001', 100, ''),
    ('write', 'code', '*', 'cross_service', 'anthropic', 'claude-sonnet-4-6', 100, ''),
    ('write', 'docs', '*', '*', 'openai', 'gpt-4.1', 100,
     'Documentazione: GPT-4.1 ottimo per produrre testo strutturato')
ON CONFLICT DO NOTHING;

-- READ: lettura/ispezione codebase
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    ('read', 'code', '*', 'single', 'google', 'gemini-2.5-flash', 100,
     'Lettura singolo file: Flash veloce ed economico'),
    ('read', 'code', '*', 'multi_file', 'mistral', 'mistral-small-latest', 100,
     'Lettura multi-file: Small basta'),
    ('read', 'code', '*', 'cross_service', 'anthropic', 'claude-haiku-4-5-20251001', 100,
     'Ispezione cross-service: Haiku'),
    ('read', 'config', '*', '*', 'google', 'gemini-2.5-flash', 100, '')
ON CONFLICT DO NOTHING;

-- ANALYZE: debug, root cause analysis (legge molto, modifica poco)
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    ('analyze', 'code', '*', 'single', 'anthropic', 'claude-haiku-4-5-20251001', 100, ''),
    ('analyze', 'code', '*', 'multi_file', 'anthropic', 'claude-sonnet-4-6', 100,
     'Root cause analysis multi-file: Sonnet per ragionamento profondo'),
    ('analyze', 'code', '*', 'cross_service', 'anthropic', 'claude-opus-4-6', 100, ''),
    ('analyze', 'tests', '*', '*', 'anthropic', 'claude-sonnet-4-6', 100,
     'Analisi fallimenti test: Sonnet')
ON CONFLICT DO NOTHING;

-- REFACTOR: ristruttura codice senza cambiare behavior
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    ('refactor', 'code', '*', 'single', 'anthropic', 'claude-haiku-4-5-20251001', 100, ''),
    ('refactor', 'code', '*', 'multi_file', 'anthropic', 'claude-sonnet-4-6', 100, ''),
    ('refactor', 'code', '*', 'cross_service', 'anthropic', 'claude-opus-4-6', 100, '')
ON CONFLICT DO NOTHING;

-- CONFIGURE/DEPLOY: cambi su infrastruttura
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    ('configure', 'service', '*', '*', 'anthropic', 'claude-sonnet-4-6', 100, ''),
    ('configure', 'infrastructure', '*', '*', 'anthropic', 'claude-sonnet-4-6', 100, ''),
    ('deploy', 'service', '*', '*', 'anthropic', 'claude-sonnet-4-6', 100, ''),
    ('deploy', 'infrastructure', '*', 'system_wide', 'anthropic', 'claude-opus-4-6', 100,
     'Deploy system-wide: Opus per coordinamento')
ON CONFLICT DO NOTHING;

-- DELETE: operazioni distruttive (sempre capable per minimizzare errori)
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope, provider, model_id, priority, rationale) VALUES
    ('delete', '*', '*', '*', 'anthropic', 'claude-sonnet-4-6', 100,
     'Operazioni distruttive: sempre capable model per ridurre errori')
ON CONFLICT DO NOTHING;

COMMIT;
