-- Fix M59: rinforza la regola Postgres applicativi in mode_automatic_instruction.
--
-- Sintomo iter_8 (run badd7d4f, step 23): l'agente ha generato src/index.ts
-- con fallback hardcoded a "postgres://postgres:postgres@localhost:5432/..."
-- e ECONNREFUSED. Il prompt SCAFFOLDING APP gia' diceva di usare :5433
-- nexus/nexus ma l'agente ha messo un fallback alla porta Postgres standard
-- :5432 (che non esiste in questo host).
--
-- Fix: prepend un avviso esplicito che vieta hardcoding di porte Postgres
-- diverse da 5433 e vieta fallback a localhost:5432.

UPDATE nexus_prompt_templates
SET content = $$REGOLA POSTGRES APPLICATIVI (mandatoria per qualunque app generata):
- L'unico Postgres disponibile e' localhost:5433 user=nexus password=nexus (container ideai-postgres-nexus-1).
- VIETATO hardcodare in qualsiasi sorgente o config: "localhost:5432", "127.0.0.1:5432", "postgres://postgres:postgres", "5432" come default connessione applicativa. La porta 5432 NON esiste in questo host: usare 5432 produce ECONNREFUSED al runtime.
- L'unica connection string ammessa nei sorgenti applicativi: postgres://nexus:nexus@localhost:5433/<slug> (e relativa variante postgresql://). Caricarla da process.env.DATABASE_URL, NON inlinarla.
- Niente fallback a sqlite ("type":"sqlite", file db.sqlite, ecc.) anche solo come "se DATABASE_URL mancante usa SQLite". Se DATABASE_URL manca, scrivilo SUBITO in .env e poi avvia.

$$ || content,
    updated_at = NOW()
WHERE key = 'automation.mode_automatic_instruction';
