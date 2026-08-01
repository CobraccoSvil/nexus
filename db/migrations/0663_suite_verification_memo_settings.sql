-- 0663_suite_verification_memo_settings.sql
-- Configurazione della verifica a suite: memoria degli esiti e classificazione
-- del rosso non riprodotto.
--
-- Il difetto che accompagna (misurato il 31/07/2026, progetto
-- bacheca-attivita): 53 esecuzioni della stessa suite Playwright in una serata
-- sulla stessa app, 31 fallite e 21 passate, perche' la domanda "com'e' andata
-- la suite?" veniva posta da tre attori senza memoria condivisa -- il
-- final_gate, l'agente, il ciclo review -- e ognuno rispondeva rieseguendo. I
-- rossi erano i due test sensibili al cold-start di Vite; il ciclo di
-- correzione li trattava come difetti reali e il correttore modificava codice
-- sano, introducendo due volte un difetto vero.
--
-- Consumatore (regola G): mcp-core::suite_verification::SuitePolicy::dal_db.
-- I default nel codice valgono solo a chiave assente e sono identici ai valori
-- qui sotto.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
(
    'agent.testing.suite_memo_enabled', 'true', 'agent',
    'Se l''esito di una suite di test puo'' essere RIUSATO quando la suite e'' gia'' stata eseguita sullo stesso identico stato del codice (digest dei sorgenti + generazione dei servizi vivi). A false ogni richiesta di verifica riesegue la suite, come prima: gli esiti continuano a essere registrati con la loro chiave, ma nessuno li rilegge.',
    NOW()
),
(
    'agent.testing.suite_memo_ttl_seconds', '900', 'agent',
    'Eta'' massima di un esito di suite riusabile. La chiave copre il codice e la generazione dei servizi, non i dati di un DB ne'' il mondo attorno: questo tetto e'' il limite dichiarato di cio'' che la chiave non puo'' vedere. Oltre, si riesegue.',
    NOW()
),
(
    'agent.testing.flaky_reclassify_enabled', 'true', 'agent',
    'Se un esito fallito va sottoposto a UNA riesecuzione mirata dei soli test falliti (playwright --last-failed) per classificarlo: se ripassano a stato del codice IDENTICO l''esito diventa flaky, che non apre il ciclo di correzione e non boccia il final gate, ma resta scritto e conteggiato come debito di test. Non e'' un ritenta-finche''-verde: la riesecuzione e'' una sola, un fallimento riprodotto resta fallito e un caso non classificabile resta fallito. A false l''esito resta failed come prima.',
    NOW()
)
ON CONFLICT (key) DO NOTHING;
