-- ADR 0017 v2 TODO 6 + 7 — settings DB-driven per chat-note e run-summary worker.
--
-- I worker leggono questi setting con cache 60s (pattern allineato a
-- `wiki::triple_extractor`). Niente fallback hardcoded nel codice (regola G):
-- se i setting mancano si applicano i `safe_defaults` Rust segnalando WARN.

BEGIN;

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    -- ── chat-note worker (TODO 6) ────────────────────────────────────────
    ('agent.wiki.chat_note_worker_enabled', 'true', 'wiki',
     'Abilita il worker che crea wiki_docs (kind=chat_note) dai messaggi user', FALSE),
    ('agent.wiki.chat_note_worker_interval_secs', '30', 'wiki',
     'Intervallo di scan del worker chat-note (secondi)', FALSE),
    ('agent.wiki.chat_note_min_body_chars', '100', 'wiki',
     'Lunghezza minima del body utente per essere ingestito come chat_note', FALSE),
    ('agent.wiki.chat_note_skip_patterns', '^(ok|si|sì|no|grazie|ciao|bene|perfetto|ottimo)[.!?\s]*$', 'wiki',
     'Regex (case-insensitive) per scartare messaggi banali. Pattern separati da | nella stessa regex', FALSE),
    ('agent.wiki.chat_note_max_per_minute', '50', 'wiki',
     'Cap di sicurezza: numero massimo di chat-note create al minuto', FALSE),
    -- ── run-summary worker (TODO 7) ──────────────────────────────────────
    ('agent.wiki.run_summary_worker_enabled', 'true', 'wiki',
     'Abilita il worker che crea wiki_docs (kind=run_summary) dai run agent terminati', FALSE),
    ('agent.wiki.run_summary_worker_interval_secs', '60', 'wiki',
     'Intervallo di scan del worker run-summary (secondi)', FALSE),
    ('agent.wiki.run_summary_max_per_minute', '30', 'wiki',
     'Cap di sicurezza: numero massimo di run-summary create al minuto', FALSE)
ON CONFLICT (key) DO NOTHING;

COMMIT;
