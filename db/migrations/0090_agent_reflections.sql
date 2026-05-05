-- Migrazione 0090: tabella nexus_agent_reflections per self-reflection runtime (Fase 2)
--
-- Ogni record corrisponde a un'esecuzione reflection_node su un singolo run agente.
-- Il campo score (0.0-1.0) e' il punteggio aggregato usato per calcolare il reward
-- finale del Q-learner: final_reward = 0.7 * heuristic + 0.3 * reflection_score.
--
-- Dipendenze: nexus_prompt_templates (0035), nessun FK su agent_runs per evitare
-- dipendenza ciclica tra servizi.

CREATE TABLE IF NOT EXISTS nexus_agent_reflections (
    id             BIGSERIAL PRIMARY KEY,
    run_id         UUID,                                    -- thread_id del run agente (no FK cross-service)
    prompt_key     TEXT        NOT NULL,
    prompt_version INT         NOT NULL DEFAULT 1,
    score          NUMERIC(3,2) CHECK (score BETWEEN 0.0 AND 1.0),
    dimensions     JSONB,                                   -- {correctness, completeness, efficiency, safety}
    weaknesses     TEXT[]      NOT NULL DEFAULT '{}',
    suggestions    TEXT[]      NOT NULL DEFAULT '{}',
    model_used     TEXT,                                    -- modello usato per la reflection
    latency_ms     INT,                                     -- latenza della chiamata reflection
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indici per query aggregate del PromptOptimizerWorker (Fase 3)
CREATE INDEX IF NOT EXISTS idx_nexus_agent_reflections_key_ts
    ON nexus_agent_reflections (prompt_key, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_nexus_agent_reflections_key_score
    ON nexus_agent_reflections (prompt_key, prompt_version, score DESC)
    WHERE score IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_nexus_agent_reflections_run_id
    ON nexus_agent_reflections (run_id)
    WHERE run_id IS NOT NULL;

-- Commento descrittivo sulla tabella
COMMENT ON TABLE nexus_agent_reflections IS
    'Record di self-reflection post-esecuzione agente. '
    'Alimenta il reward Q-learning e il PromptOptimizerWorker (Fase 3).';
