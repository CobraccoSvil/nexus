-- Migrazione 0331 — Wiki code-docs enricher.
--
-- Problema risolto (causa radice): i wiki_docs con kind='code' venivano creati
-- come PLACEHOLDER inerti da `wiki::code_graph::ensure_code_doc` (body fisso
-- "Placeholder doc per il file di codice ...") al solo scopo di ancorare le
-- triple `imports` nel code-graph. Nessuno stadio successivo li arricchiva con
-- una descrizione reale ne' calcolava l'embedding (qdrant_point_id restava
-- NULL). Risultato: la knowledge base non conteneva conoscenza utilizzabile sui
-- file e l'agente rianalizzava i sorgenti ad ogni turno.
--
-- Questa migrazione introduce lo stadio mancante di ARRICCHIMENTO: un worker
-- (`wiki::code_docs_enricher`) genera via LLM una scheda descrittiva per file e
-- ne calcola l'embedding in wiki_content. Pattern allineato a chat_note_worker
-- (mig 0305) e title_gen (mig 0306): settings DB-driven con cache 60s, niente
-- fallback hardcoded sul modello (regola G).
--
-- Modello come CATEGORIA configurabile da admin: il purpose usa il `tier`
-- (mig 0203, valori light|medium|heavy) come selezione primaria. L'admin
-- cambia la categoria dalla dashboard (routing config). provider/model_id
-- restano come fallback statico esplicito (NOT NULL), usati solo se il catalog
-- non ha candidati per il tier.
--
-- Idempotente: ON CONFLICT (key/purpose) DO NOTHING; ADD COLUMN IF NOT EXISTS.

BEGIN;

-- ── Colonne marker su wiki_docs ──────────────────────────────────────────────
-- code_source_hash: sha256 del CONTENUTO del file sorgente al momento
-- dell'arricchimento. Permette l'idempotenza reale: il worker ri-arricchisce un
-- doc solo se il sorgente e' cambiato (hash diverso), senza sprecare chiamate
-- LLM. code_docs_enriched_at: timestamp dell'ultimo arricchimento riuscito,
-- usato dal cap diurno e per distinguere i placeholder mai arricchiti
-- (code_source_hash IS NULL) dal backfill gia' processato.
ALTER TABLE wiki_docs
    ADD COLUMN IF NOT EXISTS code_source_hash TEXT;
ALTER TABLE wiki_docs
    ADD COLUMN IF NOT EXISTS code_docs_enriched_at TIMESTAMPTZ;

-- ── Settings: pipeline di arricchimento code-docs ────────────────────────────
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.wiki.code_docs_enricher_enabled', 'true', 'wiki',
     'Se true, il worker wiki::code_docs_enricher arricchisce i wiki_docs kind=code (placeholder) con una scheda descrittiva LLM e ne calcola l''embedding. Mettere a false per pausa globale.',
     NOW()),
    ('agent.wiki.code_docs_enricher_interval_secs', '45', 'wiki',
     'Intervallo (secondi) tra i batch del worker di arricchimento code-docs. Minimo applicato lato Rust: 5s.',
     NOW()),
    ('agent.wiki.code_docs_enricher_batch_max', '20', 'wiki',
     'Numero massimo di doc code arricchiti per singolo batch (rate limit costo/latenza).',
     NOW()),
    ('agent.wiki.code_docs_enricher_daily_cap', '500', 'wiki',
     'Numero massimo di arricchimenti LLM in 24h (conta i doc con code_docs_enriched_at popolato nelle ultime 24h). Protegge il costo durante il backfill iniziale.',
     NOW()),
    ('agent.wiki.code_docs_enricher_max_source_chars', '12000', 'wiki',
     'Budget massimo di caratteri del sorgente iniettati nel prompt LLM (troncamento difensivo). File piu'' grandi vengono troncati.',
     NOW()),
    ('agent.wiki.code_docs_enricher_min_source_chars', '40', 'wiki',
     'Soglia minima di caratteri del sorgente: sotto questa lunghezza il file e'' considerato banale e l''arricchimento viene saltato.',
     NOW())
ON CONFLICT (key) DO NOTHING;

-- ── Purpose model: chi genera la scheda descrittiva del file ─────────────────
-- Il default e' una CATEGORIA (tier='medium'), non un modello fisso: l'admin la
-- cambia dalla dashboard routing config. provider/model_id sono il fallback
-- statico esplicito (NOT NULL), usati solo se il catalog non offre candidati per
-- il tier (es. tutti in cooldown). required_capability='code': preferisce
-- modelli con buona comprensione del codice. requires_tool_use=false: e' pura
-- generazione di testo, nessun tool.
INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES
    ('wiki_code_docs_enricher', 'google', 'gemini-2.5-flash-lite', 'medium', 'code', false,
     'Arricchimento wiki_docs kind=code: genera una scheda descrittiva del file (scopo, simboli esportati, dipendenze) per la knowledge base. Modello come categoria (tier) configurabile da admin; provider/model_id sono fallback statico.')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();

COMMIT;
