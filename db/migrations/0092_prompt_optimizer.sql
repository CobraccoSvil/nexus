-- Migrazione 0092: infrastruttura PromptOptimizerWorker (Fase 3)
--
-- Estende nexus_prompt_templates con flag `experimental` per le varianti
-- generate automaticamente. Aggiunge tabelle per gli esperimenti A/B canary
-- e per il feedback utente esplicito sui prompt.

-- ─── Colonna experimental su nexus_prompt_templates ─────────────────────────
-- Già aggiunta dalla migrazione 0087, qui aggiungiamo ONLY IF NOT EXISTS
-- tramite controllo information_schema (idempotente).
DO $$ BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'nexus_prompt_templates'
          AND column_name = 'experimental'
    ) THEN
        ALTER TABLE nexus_prompt_templates
            ADD COLUMN experimental BOOLEAN NOT NULL DEFAULT FALSE;
    END IF;
END $$;

-- ─── Tabella: prompt_ab_experiments ─────────────────────────────────────────
-- Registra ogni esperimento canary: baseline vs variante sperimentale.
-- Ciclo di vita: running → promoted | discarded | rolled_back.
CREATE TABLE IF NOT EXISTS prompt_ab_experiments (
    id                      UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    prompt_key              TEXT        NOT NULL,
    baseline_version        INT         NOT NULL,
    variant_version         INT         NOT NULL,
    -- Percentuale traffico verso la variante (default 10%)
    traffic_pct             INT         NOT NULL DEFAULT 10 CHECK (traffic_pct BETWEEN 1 AND 50),
    status                  TEXT        NOT NULL DEFAULT 'running'
        CHECK (status IN ('running', 'promoted', 'discarded', 'rolled_back')),
    started_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at                TIMESTAMPTZ,
    -- Metriche finali calcolate alla chiusura
    baseline_success_rate   NUMERIC(4,3),
    variant_success_rate    NUMERIC(4,3),
    baseline_reflection_avg NUMERIC(4,3),
    variant_reflection_avg  NUMERIC(4,3),
    p_value                 NUMERIC(5,4),
    -- Motivazione della decisione automatica
    decision_reason         TEXT,
    -- Numero minimo di run per considerare l'esperimento statisticamente valido
    min_runs_required       INT         NOT NULL DEFAULT 30,
    -- Flag: se TRUE il worker puo' promuovere automaticamente
    auto_promote_enabled    BOOLEAN     NOT NULL DEFAULT FALSE
);

CREATE INDEX IF NOT EXISTS idx_prompt_ab_experiments_status
    ON prompt_ab_experiments (status, prompt_key);

CREATE INDEX IF NOT EXISTS idx_prompt_ab_experiments_key_running
    ON prompt_ab_experiments (prompt_key)
    WHERE status = 'running';

COMMENT ON TABLE prompt_ab_experiments IS
    'Esperimenti canary A/B tra prompt baseline e varianti sperimentali. '
    'Gestiti dal PromptOptimizerWorker (Fase 3).';

-- ─── Tabella: prompt_feedback ────────────────────────────────────────────────
-- Feedback esplicito dell'utente su singoli run (thumbs up/down + commento).
-- Alimenta il success_rate nella dashboard e la decisione di promozione.
CREATE TABLE IF NOT EXISTS prompt_feedback (
    id              BIGSERIAL   PRIMARY KEY,
    run_id          UUID,
    prompt_key      TEXT        NOT NULL,
    prompt_version  INT         NOT NULL DEFAULT 1,
    -- +1 = positivo, -1 = negativo, 0 = neutro
    user_thumbs     SMALLINT    CHECK (user_thumbs IN (-1, 0, 1)),
    user_comment    TEXT,
    session_id      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_prompt_feedback_key_version
    ON prompt_feedback (prompt_key, prompt_version, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_prompt_feedback_run_id
    ON prompt_feedback (run_id)
    WHERE run_id IS NOT NULL;

COMMENT ON TABLE prompt_feedback IS
    'Feedback esplicito utente su run agente. Alimenta la dashboard e le '
    'decisioni di promozione del PromptOptimizerWorker.';

-- ─── Settings Fase 3 ─────────────────────────────────────────────────────────
INSERT INTO settings (key, value, category, description, is_secret) VALUES

    ('optimizer_enabled',
     'true',
     'optimizer',
     'Abilita il PromptOptimizerWorker. Con auto_promote_enabled=false '
     'genera varianti ma non le promuove (dry-run). Kill switch globale.',
     FALSE),

    ('optimizer_auto_promote',
     'false',
     'optimizer',
     'Se true, il worker promuove automaticamente le varianti che superano '
     'il test statistico (Wilson score, p<0.05). Default false = dry-run.',
     FALSE),

    ('optimizer_min_runs',
     '30',
     'optimizer',
     'Numero minimo di run per cohort prima di considerare un esperimento '
     'statisticamente valido. Cohort con meno run vengono ignorati.',
     FALSE),

    ('optimizer_success_rate_threshold',
     '0.60',
     'optimizer',
     'Soglia di success_rate sotto cui un prompt e'' candidato '
     'all''ottimizzazione automatica.',
     FALSE),

    ('optimizer_reflection_threshold',
     '0.65',
     'optimizer',
     'Soglia di avg_reflection_score sotto cui un prompt e'' candidato '
     'all''ottimizzazione. Richiede Fase 2 (reflection) attiva.',
     FALSE),

    ('optimizer_canary_traffic_pct',
     '10',
     'optimizer',
     'Percentuale di traffico inviata alla variante sperimentale durante '
     'il canary test (1-50). Default 10%.',
     FALSE),

    ('optimizer_max_concurrent_experiments',
     '3',
     'optimizer',
     'Numero massimo di esperimenti running in contemporanea. Evita '
     'instabilita'' globale da troppi canary simultanei.',
     FALSE),

    ('optimizer_rollback_threshold',
     '0.15',
     'optimizer',
     'Se dopo la promozione il success_rate scende di piu'' di questa '
     'percentuale rispetto alla baseline, scatta il rollback automatico.',
     FALSE)

ON CONFLICT (key) DO NOTHING;
