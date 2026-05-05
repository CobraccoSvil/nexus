-- Migrazione 0093: tabella prompt_eval_runs per l'eval harness (Fase 4)
--
-- Ogni riga corrisponde all'esecuzione di un caso di test dell'eval harness
-- (`pnpm eval:prompts`). Le metriche JSON permettono confronti tra versioni
-- di prompt e tra varianti A/B.

CREATE TABLE IF NOT EXISTS prompt_eval_runs (
    id              BIGSERIAL   PRIMARY KEY,
    -- Identificazione del caso
    eval_case_name  TEXT        NOT NULL,
    agent_type      TEXT        NOT NULL,
    prompt_key      TEXT        NOT NULL,
    prompt_version  INT         NOT NULL DEFAULT 1,
    -- Se questo run fa parte di un esperimento A/B
    experiment_id   UUID        REFERENCES prompt_ab_experiments(id) ON DELETE SET NULL,
    is_baseline     BOOLEAN     NOT NULL DEFAULT TRUE,
    -- Metriche raccolte dal runner
    metrics         JSONB       NOT NULL DEFAULT '{}',
    -- Struttura attesa di metrics:
    -- {
    --   "passed": bool,
    --   "residual_placeholders": bool,
    --   "xml_tags_present": [str],
    --   "forbidden_strings_found": [str],
    --   "render_latency_ms": int,
    --   "reflection_score": float | null,
    --   "rubric_checks": { check_name: bool }
    -- }
    passed          BOOLEAN     NOT NULL DEFAULT FALSE,
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_prompt_eval_runs_key_version
    ON prompt_eval_runs (prompt_key, prompt_version, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_prompt_eval_runs_case
    ON prompt_eval_runs (eval_case_name, passed, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_prompt_eval_runs_experiment
    ON prompt_eval_runs (experiment_id)
    WHERE experiment_id IS NOT NULL;

COMMENT ON TABLE prompt_eval_runs IS
    'Risultati dell''eval harness (pnpm eval:prompts). '
    'Usato dalla dashboard A/B per confrontare baseline vs variante.';

-- Vista utile per la dashboard: success rate per prompt_key negli ultimi 7 giorni
CREATE OR REPLACE VIEW prompt_eval_summary_7d AS
SELECT
    prompt_key,
    prompt_version,
    COUNT(*)                                          AS total_runs,
    SUM(CASE WHEN passed THEN 1 ELSE 0 END)           AS passed_runs,
    ROUND(
        SUM(CASE WHEN passed THEN 1 ELSE 0 END)::numeric / NULLIF(COUNT(*), 0),
        3
    )                                                 AS pass_rate,
    MAX(created_at)                                   AS last_run_at
FROM prompt_eval_runs
WHERE created_at >= NOW() - INTERVAL '7 days'
GROUP BY prompt_key, prompt_version
ORDER BY prompt_key, prompt_version DESC;

COMMENT ON VIEW prompt_eval_summary_7d IS
    'Riepilogo eval harness ultimi 7 giorni per prompt_key e versione.';
