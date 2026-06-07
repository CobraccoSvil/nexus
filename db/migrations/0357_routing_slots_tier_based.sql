-- 0357_routing_slots_tier_based.sql
--
-- Porta la matrice slot-based (nexus_routing_slots_matrix, mig 0133) sotto il
-- meccanismo di routing per TIER, eliminando i modelli pinnati staticamente.
--
-- DIAGNOSI (verificata sul DB):
--   La filosofia del sistema (mig 0353, regola G/H/L) e': il solo dato fisso
--   per una richiesta e' il TIER (+ capability); provider e modello concreti li
--   sceglie dinamicamente il punto unico tier-based dal catalog
--   (disponibilita', cooldown, costo).
--
--   MA nexus_routing_slots_matrix - che e' il percorso PRIMARIO di routing
--   quando il classifier estrae slot affidabili (confidence >= 0.60) - pinnava
--   provider+model_id statici (mig 0133). Questi pin:
--     - bypassavano il governo per tier (un SECONDO punto di controllo per
--       "quale modello", viola regola L);
--     - marcivano: al momento del fix 3 regole puntavano a modelli morti o
--       disabilitati (mistral-large-2411 missing_from_api, deepseek-chat x2) e
--       le versioni Claude erano disallineate dalla routing matrix per intent
--       (claude-opus-4-6 vs claude-opus-4-8);
--     - degradavano la qualita': la regola read+code+multi_file era pinnata su
--       mistral-small-latest (tier light), per cui i run agentici che leggono
--       molti file finivano su un modello debole invece di mistral-large-latest
--       (tier medium, sano) governato per tier.
--
-- FIX (regola G/H/L):
--   La slot-matrix esprime ORA solo (preferred_tier, required_capabilities,
--   requires_tool_use, cost_direction) per ogni chiave (action, target,
--   framework, scope). La scelta provider+modello e' delegata al punto unico
--   select_models_for_requirement() in routing_matrix_auto_promoter.rs - lo
--   STESSO scoring che governa nexus_routing_matrix per intent. Niente piu'
--   modelli pinnati: la slot-matrix non puo' piu' marcire.
--
-- DERIVAZIONE TIER: per ogni chiave il tier riprende il performance_tier del
--   modello che era pinnato a priority massima (fotografia del comportamento
--   approvato), con due correzioni principled documentate:
--     - read + scope multi_file/cross_service -> almeno 'medium' (leggere o
--       attraversare molti file richiede un modello capace con context ampio;
--       corregge il pin mistral-small troppo debole osservato in produzione);
--     - azioni generative (write) su scope multi_file -> almeno 'medium'
--       (coordinare scritture su piu' file non e' meno impegnativo che
--       risolvere bug multi-file, gia' medium).
--   Le capability si limitano a quelle realmente presenti nel catalog
--   (code, reasoning, fix): long-context NON e' usata come requisito perche'
--   etichetta un solo modello e restringerebbe ingiustamente la selezione.
--
-- Idempotente: ADD/DROP COLUMN IF [NOT] EXISTS; il seed e' DELETE-then-INSERT
-- (config seed-only, nessuna scrittura runtime su questa tabella).

BEGIN;

-- 1) Nuove colonne tier+capability (gemelle di nexus_intent_routing_requirements).
ALTER TABLE nexus_routing_slots_matrix
    ADD COLUMN IF NOT EXISTS preferred_tier        TEXT,
    ADD COLUMN IF NOT EXISTS required_capabilities TEXT[]  NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS requires_tool_use     BOOLEAN NOT NULL DEFAULT TRUE,
    ADD COLUMN IF NOT EXISTS cost_direction        TEXT    NOT NULL DEFAULT 'asc';

-- 2) Svuota il seed vecchio basato su pin provider+model.
DELETE FROM nexus_routing_slots_matrix;

-- 3) Rimuove le colonne dei pin. DROP COLUMN provider rimuove automaticamente
--    la vecchia UNIQUE (action, target, framework, scope, provider) che ne
--    dipende: la scelta del modello passa al punto unico tier-based.
ALTER TABLE nexus_routing_slots_matrix
    DROP COLUMN IF EXISTS provider,
    DROP COLUMN IF EXISTS model_id,
    DROP COLUMN IF EXISTS priority;

-- 4) Re-seed: una riga per chiave (action, target, framework, scope) -> tier.
INSERT INTO nexus_routing_slots_matrix
    (action_verb, target_type, framework, scope,
     preferred_tier, required_capabilities, requires_tool_use, cost_direction, rationale)
VALUES
    -- ── RESOLVE ──────────────────────────────────────────────────────────
    ('resolve','tests','*','single',        'light',  ARRAY['code','fix'],             TRUE,'asc',
     'Risoluzione test single-file: modello leggero sufficiente'),
    ('resolve','tests','*','multi_file',     'medium', ARRAY['code','fix'],             TRUE,'asc',
     'Risoluzione fail test multi-file: debug + edit coordinato'),
    ('resolve','tests','playwright','multi_file','medium',ARRAY['code','fix'],          TRUE,'asc',
     'Playwright test multi-file (override framework, caso Redemptor)'),
    ('resolve','tests','*','cross_service',  'heavy',  ARRAY['code','reasoning','fix'], TRUE,'desc',
     'Test cross-service: ragionamento architetturale'),
    ('resolve','code','*','single',          'light',  ARRAY['code','fix'],             TRUE,'asc',
     'Bug fix single-file'),
    ('resolve','code','*','multi_file',      'medium', ARRAY['code','fix'],             TRUE,'asc',
     'Bug fix multi-file'),
    ('resolve','code','*','cross_service',   'heavy',  ARRAY['code','reasoning','fix'], TRUE,'desc',
     'Bug fix cross-service: ragionamento esteso'),
    ('resolve','config','*','single',        'light',  ARRAY['code'],                   TRUE,'asc',''),
    ('resolve','config','*','multi_file',    'medium', ARRAY['code'],                   TRUE,'asc',''),
    ('resolve','service','*','cross_service','medium', ARRAY['code','reasoning'],       TRUE,'desc',
     'Risoluzione problema servizio cross-service'),

    -- ── WRITE ────────────────────────────────────────────────────────────
    ('write','tests','*','single',           'light',  ARRAY['code'],                   TRUE,'asc',
     'Scrittura test single-file'),
    ('write','tests','*','multi_file',        'medium', ARRAY['code'],                   TRUE,'asc',
     'Scrittura test multi-file: coordinamento'),
    ('write','tests','cargo','*',             'medium', ARRAY['code'],                   TRUE,'asc',
     'Test Rust: expertise specifica'),
    ('write','code','*','single',             'light',  ARRAY['code'],                   TRUE,'asc',
     'Scrittura codice single-file'),
    ('write','code','*','multi_file',         'medium', ARRAY['code'],                   TRUE,'asc',
     'Scrittura codice multi-file: coordinamento (corregge pin light)'),
    ('write','code','*','cross_service',      'medium', ARRAY['code','reasoning'],       TRUE,'desc',
     'Scrittura cross-service'),
    ('write','docs','*','*',                  'medium', ARRAY[]::TEXT[],                 TRUE,'asc',
     'Documentazione: testo strutturato'),

    -- ── READ ─────────────────────────────────────────────────────────────
    ('read','code','*','single',              'light',  ARRAY['code'],                   TRUE,'asc',
     'Lettura singolo file: modello veloce ed economico'),
    ('read','code','*','multi_file',          'medium', ARRAY['code'],                   TRUE,'asc',
     'Lettura multi-file: serve context ampio (corregge il pin mistral-small)'),
    ('read','code','*','cross_service',       'medium', ARRAY['code'],                   TRUE,'asc',
     'Ispezione cross-service: context ampio'),
    ('read','config','*','*',                 'light',  ARRAY['code'],                   TRUE,'asc',
     'Lettura config'),

    -- ── ANALYZE ──────────────────────────────────────────────────────────
    ('analyze','code','*','single',           'light',  ARRAY['code'],                   TRUE,'asc',''),
    ('analyze','code','*','multi_file',        'medium', ARRAY['code','reasoning'],       TRUE,'asc',
     'Root cause analysis multi-file: ragionamento profondo'),
    ('analyze','code','*','cross_service',     'heavy',  ARRAY['code','reasoning'],       TRUE,'desc',
     'Root cause cross-service'),
    ('analyze','service','*','cross_service',  'medium', ARRAY['code','reasoning'],       TRUE,'desc',''),
    ('analyze','tests','*','*',                'medium', ARRAY['code','reasoning'],       TRUE,'asc',
     'Analisi fallimenti test'),

    -- ── REFACTOR ─────────────────────────────────────────────────────────
    ('refactor','code','*','single',          'light',  ARRAY['code'],                   TRUE,'asc',''),
    ('refactor','code','*','multi_file',       'medium', ARRAY['code'],                   TRUE,'asc',''),
    ('refactor','code','*','cross_service',    'heavy',  ARRAY['code','reasoning'],       TRUE,'desc',''),

    -- ── CONFIGURE / DEPLOY ───────────────────────────────────────────────
    ('configure','service','*','*',           'medium', ARRAY['code'],                   TRUE,'desc',''),
    ('configure','infrastructure','*','*',     'medium', ARRAY['code'],                   TRUE,'desc',''),
    ('deploy','service','*','*',               'medium', ARRAY['code','reasoning'],       TRUE,'desc',''),
    ('deploy','infrastructure','*','system_wide','heavy',ARRAY['code','reasoning'],       TRUE,'desc',
     'Deploy system-wide: coordinamento'),

    -- ── DELETE (safety: sempre modello capace) ───────────────────────────
    ('delete','*','*','*',                     'medium', ARRAY['code'],                   TRUE,'desc',
     'Operazioni distruttive: modello capace per ridurre errori');

-- 5) Vincoli su preferred_tier (ora valorizzato) e cost_direction.
ALTER TABLE nexus_routing_slots_matrix
    ALTER COLUMN preferred_tier SET NOT NULL;

ALTER TABLE nexus_routing_slots_matrix
    DROP CONSTRAINT IF EXISTS slots_preferred_tier_valid;
ALTER TABLE nexus_routing_slots_matrix
    ADD CONSTRAINT slots_preferred_tier_valid
    CHECK (preferred_tier IN ('light','medium','heavy'));

ALTER TABLE nexus_routing_slots_matrix
    DROP CONSTRAINT IF EXISTS slots_cost_direction_valid;
ALTER TABLE nexus_routing_slots_matrix
    ADD CONSTRAINT slots_cost_direction_valid
    CHECK (cost_direction IN ('asc','desc'));

-- 6) Nuova UNIQUE per chiave (senza provider): una riga = un tier per chiave.
ALTER TABLE nexus_routing_slots_matrix
    DROP CONSTRAINT IF EXISTS slots_key_unique;
ALTER TABLE nexus_routing_slots_matrix
    ADD CONSTRAINT slots_key_unique
    UNIQUE (action_verb, target_type, framework, scope);

COMMIT;
