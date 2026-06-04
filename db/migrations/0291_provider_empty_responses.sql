-- Migrazione 0291 — Diagnostica empty/hollow completion provider (QW2).
--
-- Quando un provider chiude un turno agente con risposta vuota o "RESIGNED"
-- (es. gemini-2.5-pro che ritorna content vuoto dopo cascade fallback,
-- osservato in chat 6 Beauty-Book 12:08:33), salviamo qui un record
-- diagnostico per poter capire la CAUSA (safety filter Google? token budget
-- zero? tool definitions troppo grandi?) senza dover ricreare il run.
--
-- Insert-only (audit immutabile). TTL di pulizia 30 giorni via worker.

CREATE TABLE IF NOT EXISTS nexus_provider_empty_responses (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Identificazione run / chat (per join futuri)
    agent_run_id    UUID,
    chat_session_id UUID,
    project_id      UUID,
    -- Routing al momento dell'incident
    provider        TEXT NOT NULL,
    model           TEXT NOT NULL,
    intent          TEXT,
    -- Stato turno
    kind            TEXT NOT NULL,             -- EMPTY_ANSWER / NO_TOOLS / EMPTY_ANSWER+NO_TOOLS / RESIGNED
    iteration       INTEGER,
    steps_count     INTEGER,
    final_answer_chars INTEGER,
    -- Stima context al momento della call
    est_input_tokens   INTEGER,
    est_output_tokens  INTEGER,
    -- Raw response del provider (troncato a 8KB per evitare blob enormi)
    raw_response_excerpt TEXT,                 -- es. response JSON di Anthropic / Gemini
    -- Possibile causa (best-guess derivata da raw_response_excerpt)
    suspected_cause TEXT                       -- safety_filter / max_tokens / unknown
);

CREATE INDEX IF NOT EXISTS idx_nexus_provider_empty_responses_occurred
    ON nexus_provider_empty_responses (occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_nexus_provider_empty_responses_model
    ON nexus_provider_empty_responses (provider, model, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_nexus_provider_empty_responses_run
    ON nexus_provider_empty_responses (agent_run_id);

-- Settings (regola G).
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.diagnostics.empty_response_log_enabled', 'true', 'agent',
     'Se true, brain salva una riga in nexus_provider_empty_responses ogni volta che un provider chiude un turno con content vuoto o RESIGNED. Utile per diagnostica provider-side. Default true.',
     NOW()),
    ('agent.diagnostics.empty_response_excerpt_max_bytes', '8192', 'agent',
     'Massima dimensione (bytes) del raw response salvato. Default 8192.',
     NOW()),
    ('agent.diagnostics.empty_response_retention_days', '30', 'agent',
     'Retention (giorni) per le righe in nexus_provider_empty_responses. Worker pulizia nightly.',
     NOW())
ON CONFLICT (key) DO NOTHING;
