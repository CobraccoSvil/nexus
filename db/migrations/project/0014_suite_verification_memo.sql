-- 0014_suite_verification_memo.sql
-- Memoria degli esiti di suite: a quale STATO del codice appartiene un esito.
--
-- Causa radice (corretta nel codice insieme a questa migrazione,
-- mcp-core/src/suite_verification): la stessa suite veniva eseguita da TRE
-- attori che non si riconoscevano a vicenda -- il final_gate come criterio di
-- chiusura, l'agente con run_playwright_tests, il ciclo review dopo ogni
-- rimando in correzione -- perche' l'esito non era legato a NIENTE: la riga
-- `jobs` diceva "passata" o "fallita", non "su quale codice".
--
-- MISURATO il 31/07/2026 sul progetto bacheca-attivita: 53 esecuzioni della
-- stessa suite in una serata sulla stessa app, 31 fallite e 21 passate. I rossi
-- erano i due test sensibili al cold-start di Vite (falliscono nei ~20 secondi
-- dopo un riavvio del servizio, passano a caldo). Ogni rosso apriva un ciclo di
-- correzione su codice sano, e due volte il correttore ha introdotto un difetto
-- vero (un css_syntax_error che ha fatto crashare il frontend, un TS2322 in
-- useActivitiesApi.ts). La flakiness non ritardava la chiusura: la impediva
-- fabbricando regressioni.
--
-- Le due colonne sono la chiave di quella memoria:
--   state_key -- digest dei sorgenti dell'albero del run + generazione dei
--                servizi vivi (pid e istante d'avvio). I servizi vi entrano
--                perche' una suite E2E li interroga: senza, un `passed`
--                sopravviverebbe allo spegnimento del servizio che lo aveva
--                reso vero, cioe' un fail-open.
--   suite_key -- identita' della suite (comando normalizzato + directory): due
--                invocazioni con filtri diversi sono suite diverse e non devono
--                rispondersi a vicenda.
--
-- Non nasce una tabella nuova di proposito: il registro degli esiti esisteva
-- gia' (`jobs`, kind 'playwright_test'), gli mancava la chiave. Una seconda
-- tabella avrebbe creato un secondo posto in cui leggere la stessa cosa
-- (regola L).
--
-- `jobs.status` acquisisce il valore 'flaky' accanto a 'passed'/'failed'
-- (regola N, identificatori canonici): un fallimento i cui test ripassano alla
-- riesecuzione mirata a codice invariato non e' ne' l'uno ne' l'altro. La
-- colonna non ha CHECK, quindi il valore non richiede DDL: la nota resta qui
-- perche' chi legge lo schema sappia che i valori attesi sono tre.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS + CREATE INDEX IF NOT EXISTS.

ALTER TABLE public.jobs
    ADD COLUMN IF NOT EXISTS state_key TEXT,
    ADD COLUMN IF NOT EXISTS suite_key TEXT;

COMMENT ON COLUMN public.jobs.state_key IS
    'Stato del codice + generazione dei servizi su cui questo esito e'' stato misurato (mcp-core::suite_verification::state_key). NULL = esito non riusabile.';
COMMENT ON COLUMN public.jobs.suite_key IS
    'Identita'' della suite (comando normalizzato + directory): due suite diverse non si rispondono a vicenda dalla memoria.';

-- Lookup della memoria: (progetto, kind, suite, stato) con il piu' recente in
-- testa. Parziale sulle righe che hanno la chiave: le altre non sono riusabili
-- e non appesantiscono l'indice.
CREATE INDEX IF NOT EXISTS idx_jobs_suite_memo
    ON public.jobs (project_id, kind, suite_key, state_key, updated_at DESC)
    WHERE state_key IS NOT NULL;
