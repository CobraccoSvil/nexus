-- Migrazione 0296 — Settings DB-driven per il link worker della wiki unificata
-- (ADR 0017 v2, Fase 4). Niente fallback hardcoded nel codice Rust: queste
-- chiavi sono la fonte unica di verita' per soglie e cadenza del worker.
--
-- Le chiavi rispettano la regola G (modelli AI / parametri operativi mai
-- hardcoded). Il worker `wiki::links_worker` le legge con cache 60s; se il
-- setting manca dopo questa migrazione, logga WARN e salta lo step.

BEGIN;

INSERT INTO settings (key, value)
VALUES
    ('agent.wiki.semantic_link_threshold', '0.60'),
    ('agent.wiki.semantic_link_top_k',     '10'),
    ('agent.wiki.link_worker_enabled',     'true'),
    ('agent.wiki.link_worker_interval_secs', '1800')
ON CONFLICT (key) DO NOTHING;

COMMIT;
