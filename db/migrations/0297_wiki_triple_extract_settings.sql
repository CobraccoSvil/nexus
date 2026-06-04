-- Migrazione 0297 — ADR 0017 v2 Fase 5: settings DB-driven per il LLM-assisted
-- triple extractor della wiki unificata + purpose model dedicato.
--
-- Niente fallback hardcoded nel codice Rust: queste chiavi sono la fonte unica
-- di verita' per cadenza, soglie di confidence e cap diurno del worker.
-- Il worker `wiki::triple_extractor` le legge con cache 60s; se un setting
-- manca il modulo logga WARN una volta e applica i safe_defaults allineati a
-- questa migrazione.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.

BEGIN;

-- ── Settings: tutti i parametri della pipeline LLM triple extraction ──────
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.wiki.triple_extract_enabled', 'true', 'agent',
     'Se true, il worker periodico `wiki::triple_extractor` estrae triple semantiche dai wiki_docs via LLM. Mettere a false per pausa globale.',
     NOW()),
    ('agent.wiki.triple_extract_interval_secs', '1800', 'agent',
     'Intervallo (s) fra due esecuzioni del worker periodico. Default 1800 = 30 min. Min effettivo: 60s.',
     NOW()),
    ('agent.wiki.triple_extract_cap_per_day_meta', '50', 'agent',
     'Numero massimo di meta-doc processati al giorno via LLM (rate limit costo). Conta i doc con almeno una tripla source=llm creata nelle ultime 24h.',
     NOW()),
    ('agent.wiki.triple_extract_cap_per_day_project', '200', 'agent',
     'Numero massimo di doc progetto processati al giorno via LLM per ogni progetto registrato.',
     NOW()),
    ('agent.wiki.triple_extract_min_confidence', '0.55', 'agent',
     'Soglia minima [0..1] di confidence per accettare una tripla emessa dal modello. Sotto soglia la tripla viene scartata silenziosamente.',
     NOW()),
    ('agent.wiki.triple_extract_max_triples_per_doc', '20', 'agent',
     'Numero massimo di triple richieste al modello per singolo documento. Iniettato nel prompt come {{max_triples}}.',
     NOW())
ON CONFLICT (key) DO NOTHING;

-- ── Purpose model: chi fa l'extraction ────────────────────────────────────
-- Schema reale (mig 0102): colonna `purpose` (PK), `provider`, `model_id`.
-- Gemini 2.5 Flash Lite e' adatto: economico, JSON output stabile,
-- buon recall su task di information extraction.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes) VALUES
    ('wiki_triple_extract', 'google', 'gemini-2.5-flash-lite',
     'ADR 0017 v2 Fase 5: extraction triple semantiche da wiki_docs. JSON strict, max 20 triple/doc.')
ON CONFLICT (purpose) DO NOTHING;

COMMIT;
