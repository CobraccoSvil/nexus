-- 0119_conversation_summaries.sql
-- BP4 piano riduzione token: rolling conversation summarization.
-- Audit table per tracciare quando il summarizer interviene, quanti
-- messaggi vengono compressi, quale modello e' stato usato e con che latenza.
-- Il riassunto stesso e' inline nel messaggio in memoria; qui salviamo per
-- replay/audit (non e' usato per ricostruire il contesto a runtime).

CREATE TABLE IF NOT EXISTS nexus_conversation_summaries (
    id BIGSERIAL PRIMARY KEY,
    thread_id TEXT NOT NULL,
    replaced_msg_count INT NOT NULL,
    summary_text TEXT NOT NULL,
    model_used TEXT NOT NULL,
    latency_ms INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_conv_summaries_thread_created
    ON nexus_conversation_summaries (thread_id, created_at DESC);

-- Aggiunge il purpose 'conversation_summary' al catalogo dei purpose
-- consentiti (la tabella nexus_purpose_model e' definita in 0102).
-- Usiamo Haiku per default: economico, veloce, sufficiente per il task.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes)
VALUES (
    'conversation_summary',
    'anthropic',
    'claude-haiku-4-5-20251001',
    'Modello small/fast usato dal summarizer (BP4 piano riduzione token).'
)
ON CONFLICT (purpose) DO NOTHING;
