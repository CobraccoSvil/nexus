-- Migrazione 0112: nexus_routing_decisions (telemetria audit del routing)
--
-- Tabella append-only che registra ogni decisione di routing del Rust mcp-core.
-- Permette:
--   1. Audit ex-post: "perche' Nexus ha scelto questo modello per questo prompt?"
--   2. Calibration di Fase 4: ricavare la confidence threshold ottimale dai dati
--   3. Drift detection: notare quando il classifier degrada o quando un modello
--      viene scelto male in modo sistematico
--   4. Cost analytics: aggregare per (intent, provider, model) e vedere il mix
--
-- Pattern: fire-and-forget INSERT da resolve_agent_provider_detailed (vedi
-- crates/mcp-core/src/orchestrator.rs).
--
-- Stima volume: ~5-50k record/giorno per setup mono-utente sviluppo.
-- Retention: nessuna ora (Fase 4 introdurra' pg_cron cleanup quando volume > 1M).

CREATE TABLE IF NOT EXISTS nexus_routing_decisions (
    id BIGSERIAL PRIMARY KEY,
    decided_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Input
    prompt_hash             TEXT NOT NULL,                 -- sha256(message[:1000]) — non PII
    estimated_tokens        INT,
    behavior_mode           TEXT,                          -- veloce|economica|bilanciata|approfondita|dinamico|manuale
    -- Classification (output del classifier intent)
    intent                  TEXT,                          -- chat|debug|fix|refactor|test|docs|architecture|file_ops|system_admin
    classifier_source       TEXT,                          -- 'llm' | 'keyword' | 'agentic_promotion' | 'fallback'
    classifier_confidence   REAL,
    classifier_cached       BOOLEAN,
    -- Decision (output di route_model_with_mode / route_model_from_catalog)
    selected_provider       TEXT NOT NULL,
    selected_model          TEXT NOT NULL,
    decision_source         TEXT,                          -- 'matrix' | 'catalog' | 'override' | 'cooldown_fallback' | 'no_capable'
    rationale               TEXT,
    -- Health flags
    no_capable_provider     BOOLEAN NOT NULL DEFAULT false,
    providers_in_cooldown   TEXT[],
    fallback_triggered      BOOLEAN NOT NULL DEFAULT false,
    -- Performance (latenza scelta del modello + classifier, NON dell'inferenza)
    latency_ms              INT,
    -- Quality (popolato in Fase 4 da analytics offline su feedback utente,
    -- retry rate, ecc. Per ora sempre NULL).
    actual_quality_score    REAL
);

COMMENT ON TABLE nexus_routing_decisions IS
'Telemetria append-only di ogni decisione di routing. Una riga per ogni resolve_agent_provider_detailed (mcp-core orchestrator.rs). Pattern fire-and-forget (tokio::spawn) per non aggiungere latenza al path caldo.';

-- Indici per query analytics frequenti
CREATE INDEX IF NOT EXISTS idx_routing_decisions_decided_at
    ON nexus_routing_decisions(decided_at DESC);
CREATE INDEX IF NOT EXISTS idx_routing_decisions_intent
    ON nexus_routing_decisions(intent, behavior_mode);
CREATE INDEX IF NOT EXISTS idx_routing_decisions_prompt_hash
    ON nexus_routing_decisions(prompt_hash);
CREATE INDEX IF NOT EXISTS idx_routing_decisions_selected_model
    ON nexus_routing_decisions(selected_provider, selected_model);
