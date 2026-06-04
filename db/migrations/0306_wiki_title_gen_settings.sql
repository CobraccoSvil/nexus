-- Migrazione 0306 — ADR 0017 v2: generazione titoli descrittivi LLM per i
-- wiki_docs con titoli-artefatto (chat_note, run_summary, other), il cui titolo
-- corrente e' un frammento del primo messaggio o un placeholder ("Run agent
-- del ...", "chrome-error://...", ecc.) e quindi poco parlante nella KB.
--
-- Pattern allineato a `wiki::triple_extractor` (mig 0297): settings DB-driven
-- con cache 60s lato Rust + purpose model dedicato. Niente fallback hardcoded
-- sul nome modello nel codice (regola G): la fonte unica e' questa tabella.
--
-- Idempotente: ON CONFLICT (key/purpose) DO NOTHING.

BEGIN;

-- ── Settings: pipeline di rigenerazione titoli ────────────────────────────
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.wiki.title_gen_enabled', 'true', 'wiki',
     'Se true, l''endpoint POST /api/wiki/recompute-titles puo'' rigenerare i titoli descrittivi via LLM per i doc con titolo-artefatto (kind chat_note/run_summary/other). Mettere a false per pausa globale.',
     NOW()),
    ('agent.wiki.title_gen_daily_cap', '100', 'wiki',
     'Numero massimo di doc per cui rigenerare il titolo via LLM in 24h, per scope (rate limit costo). Conta i doc con title_generated_at popolato nelle ultime 24h.',
     NOW()),
    ('agent.wiki.title_gen_max_words', '10', 'wiki',
     'Numero massimo di parole del titolo generato. Iniettato nel prompt come {{max_words}} e applicato come cap difensivo lato server.',
     NOW())
ON CONFLICT (key) DO NOTHING;

-- ── Colonna marker: quando un title e' stato (ri)generato via LLM ─────────
-- Serve al cap diurno (COUNT ultime 24h) e a non rigenerare ciclicamente lo
-- stesso doc. NON e' `manually_edited`: la rigenerazione automatica NON marca
-- il doc come modificato a mano.
ALTER TABLE wiki_docs
    ADD COLUMN IF NOT EXISTS title_generated_at TIMESTAMPTZ;

-- ── Purpose model: chi genera il titolo ──────────────────────────────────
-- Stesso modello economico del triple extractor (mig 0297): adatto a un task
-- di sintesi breve, basso costo. NON hardcodare il nome nel codice Rust:
-- il modulo lo legge via routing_matrix.purpose_model("wiki_title_gen").
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('wiki_title_gen', 'google', 'gemini-2.5-flash-lite',
     'ADR 0017 v2: generazione titolo descrittivo conciso per wiki_docs con titolo-artefatto. Output: solo il titolo, max parole da settings.')
ON CONFLICT (purpose) DO NOTHING;

COMMIT;
