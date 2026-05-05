-- Migration 0055: Reasoning Bank — storage pattern cognitivi.
--
-- Il ReasoningBank accumula pattern estratti dai LearningWorkers
-- (UltralearnWorker, MemoryConsolidationWorker) e li usa per:
--   - Suggerire approcci a task simili
--   - Migliorare la qualità dell'output degli agenti
--   - Identificare anti-pattern da evitare
--
-- Tabelle:
--   reasoning_patterns    → pattern cognitivi estratti
--   reasoning_examples    → esempi concreti per ogni pattern
--   reasoning_antipatterns → pattern negativi da evitare

-- ---------------------------------------------------------------------------
-- Enum: pattern type
-- ---------------------------------------------------------------------------
DO $$ BEGIN
    CREATE TYPE reasoning_pattern_type AS ENUM (
        'code_structure',    -- Pattern strutturali (design patterns)
        'problem_solving',   -- Approcci di risoluzione problemi
        'optimization',      -- Tecniche di ottimizzazione
        'debugging',         -- Pattern di debug e root cause
        'testing',           -- Strategie di testing
        'architecture',      -- Pattern architetturali
        'security',          -- Pattern di sicurezza
        'performance',       -- Pattern di performance
        'documentation',     -- Pattern documentazione
        'refactoring',       -- Pattern di refactoring
        'antipattern'        -- Antipattern (da evitare)
    );
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- ---------------------------------------------------------------------------
-- Tabella: reasoning_patterns
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS reasoning_patterns (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Classificazione
    pattern_type    reasoning_pattern_type NOT NULL,
    name            TEXT        NOT NULL,
    description     TEXT        NOT NULL DEFAULT '',
    -- Embedding semantico (per similarity search)
    embedding       float4[],
    -- Confidenza del pattern (0-1): aumenta con validazioni positive
    confidence      REAL        NOT NULL DEFAULT 0.5,
    -- Utilizzo
    use_count       BIGINT      NOT NULL DEFAULT 0,
    success_count   BIGINT      NOT NULL DEFAULT 0,
    -- Contesto applicabilità
    applicable_languages TEXT[] NOT NULL DEFAULT '{}',
    applicable_frameworks TEXT[] NOT NULL DEFAULT '{}',
    applicable_tasks     TEXT[] NOT NULL DEFAULT '{}',
    -- Metadati
    source_agent    TEXT,       -- Agent type che ha estratto il pattern
    tags            TEXT[]      NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE reasoning_patterns IS
    'Pattern cognitivi estratti dall''esecuzione degli agenti. Base del ReasoningBank.';

CREATE INDEX IF NOT EXISTS idx_reasoning_patterns_type
    ON reasoning_patterns (pattern_type);

CREATE INDEX IF NOT EXISTS idx_reasoning_patterns_confidence
    ON reasoning_patterns (confidence DESC)
    WHERE confidence > 0.3;

CREATE INDEX IF NOT EXISTS idx_reasoning_patterns_source_agent
    ON reasoning_patterns (source_agent);

-- Full-text search su name + description
CREATE INDEX IF NOT EXISTS idx_reasoning_patterns_fts
    ON reasoning_patterns
    USING GIN (to_tsvector('english', name || ' ' || description));

-- ---------------------------------------------------------------------------
-- Tabella: reasoning_examples
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS reasoning_examples (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    pattern_id      UUID        NOT NULL REFERENCES reasoning_patterns(id) ON DELETE CASCADE,
    -- Input che ha triggerato il pattern
    input_summary   TEXT        NOT NULL,
    -- Output prodotto applicando il pattern
    output_summary  TEXT        NOT NULL,
    -- Contesto (linguaggio, framework, ecc.)
    context         JSONB       NOT NULL DEFAULT '{}',
    -- Feedback sull'esempio
    quality_score   REAL        NOT NULL DEFAULT 0.5,
    validated       BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_reasoning_examples_pattern
    ON reasoning_examples (pattern_id, quality_score DESC);

-- ---------------------------------------------------------------------------
-- Tabella: reasoning_antipatterns
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS reasoning_antipatterns (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT        NOT NULL UNIQUE,
    description     TEXT        NOT NULL DEFAULT '',
    -- Pattern positivo che dovrebbe essere usato invece
    suggested_pattern_id UUID   REFERENCES reasoning_patterns(id) ON DELETE SET NULL,
    embedding       float4[],
    severity        TEXT        NOT NULL DEFAULT 'warning' CHECK (severity IN ('info', 'warning', 'error', 'critical')),
    detection_count BIGINT      NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_reasoning_antipatterns_severity
    ON reasoning_antipatterns (severity);

-- ---------------------------------------------------------------------------
-- View: top patterns by confidence and usage
-- ---------------------------------------------------------------------------
CREATE OR REPLACE VIEW top_reasoning_patterns AS
SELECT
    id,
    pattern_type::TEXT,
    name,
    description,
    confidence,
    use_count,
    CASE WHEN use_count > 0
         THEN ROUND((success_count::REAL / use_count * 100)::NUMERIC, 1)
         ELSE NULL
    END AS success_rate_pct,
    source_agent,
    tags,
    updated_at
FROM reasoning_patterns
WHERE confidence > 0.3
ORDER BY (confidence * 0.6 + LEAST(use_count::REAL / 100, 0.4)) DESC
LIMIT 1000;
