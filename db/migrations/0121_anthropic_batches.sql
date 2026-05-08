-- 0121_anthropic_batches.sql
-- BP9 piano riduzione token: Batch API per worker non interattivi.
-- La Batch API Anthropic e' asincrona (submit -> 24h -> fetch) e applica
-- uno sconto del 50% sui token. Adatta per worker che non hanno bisogno
-- di una risposta immediata: prompt_optimizer (varianti), learner_node
-- (riassunti embedding).
--
-- Questa migrazione crea solo l'infrastruttura DB. L'attivazione effettiva
-- del flusso batch e' gated dal flag prompt_optimizer_use_batch_api in
-- nexus_admin_settings (default false). Vedi follow-up nel modulo
-- crates/nexus-orchestrator/src/workers/prompt_optimizer.rs.

CREATE TABLE IF NOT EXISTS nexus_anthropic_batches (
    id BIGSERIAL PRIMARY KEY,
    -- ID restituito dall'API Anthropic (batch_*)
    anthropic_batch_id TEXT NOT NULL UNIQUE,
    -- Worker che ha sottomesso il batch (es. 'prompt_optimizer', 'learner')
    worker_name TEXT NOT NULL,
    -- Numero di richieste contenute nel batch
    request_count INTEGER NOT NULL,
    -- Stato: 'in_progress' | 'ended' | 'expired' | 'canceled' | 'failed'
    status TEXT NOT NULL DEFAULT 'in_progress',
    -- Payload originale (per replay/audit). Compresso JSON.
    request_payload JSONB,
    -- Risposte recuperate (popolato dal poller quando status='ended').
    response_payload JSONB,
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    -- Cost saving stimato in USD (prezzo full - prezzo batch).
    estimated_savings_usd NUMERIC(12, 6)
);

CREATE INDEX IF NOT EXISTS idx_anthropic_batches_status_submitted
    ON nexus_anthropic_batches (status, submitted_at DESC);

CREATE INDEX IF NOT EXISTS idx_anthropic_batches_worker
    ON nexus_anthropic_batches (worker_name, submitted_at DESC);

-- Feature flag in settings (default OFF: l'infrastruttura e' pronta ma il
-- flusso resta sincrono finche' un admin non lo attiva).
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'prompt_optimizer_use_batch_api',
    'false',
    'optimizer',
    'BP9: usa Batch API Anthropic per le varianti del prompt_optimizer (50% sconto token, latenza fino a 24h).',
    false
)
ON CONFLICT (key) DO NOTHING;
