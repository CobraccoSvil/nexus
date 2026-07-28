-- 0646_file_mutations_scope_audit.sql
--
-- MISURA (non enforcement) delle scritture che cadono fuori dal `write_scope`
-- dichiarato dal pianificatore.
--
-- Perche' misurare invece di vincolare. Oggi `write_scope`
-- (`nexus_agent_todos.write_scope`, mig project 0006) alimenta UNA sola
-- decisione: se una wave di todo si puo' parallelizzare
-- (`subtasks_are_disjoint` -> `parallel_writers_allowed`). Non arriva ai tool, e
-- nessuno ha mai contato quanto spesso un sub-run scriva fuori da cio' che il
-- piano aveva dichiarato. Senza quel numero, mettere un gate sarebbe costruire su
-- un caso singolo: gli scope li dichiara il pianificatore PRIMA che qualcuno abbia
-- letto il codice, quindi un enforcement duro rischia di convertire lavoro sporco
-- ma RIUSCITO in todo bloccati. Prima il dato, poi la decisione fra enforcement
-- duro, estensione su richiesta (`extend_write_scope`) e correzione del
-- pianificatore.
--
-- Perche' nel set META e non in `db/migrations/project`: `file_mutations` e' una
-- tabella del META (mig 0349) e il suo unico scrittore,
-- `mcp-core::file_mutations::record_mutation`, riceve il pool `ToolContextCore.db`
-- — che e' il meta, non `run_db`. Mettere le colonne nel set project le avrebbe
-- lasciate inesistenti per lo scrittore reale: l'INSERT sarebbe fallito INTERO,
-- il chiamante lo assorbe con un `tracing::warn!`, e il risultato sarebbe stato
-- zero righe misurate PIU' la perdita del tracking ripristinabile esistente.
--
-- `scope_verdict` e' un identificatore canonico (regola N) prodotto dal punto
-- unico `nexus_agent_graph::decisions::classify_write`, non una frase:
--   'no_scope_declared' -> il task non ha dichiarato scope: NON misurabile.
--                          E' il valore che rende visibile una propagazione rotta,
--                          invece di far leggere zeri rassicuranti.
--   'in_scope'          -> path scritto dentro almeno un path dichiarato.
--   'out_of_scope'      -> scope dichiarato, path scritto fuori da tutti.
-- NULL = riga scritta prima di questa migrazione (storico), distinta da
-- 'no_scope_declared', che invece e' una misura effettuata.
--
-- `declared_write_scope` conserva CIO' CHE ERA STATO DICHIARATO, non solo il
-- verdetto: senza, si saprebbe che il pianificatore ha sbagliato ma non COME
-- (ha dimenticato la rotta accanto al modello? ha dichiarato una cartella troppo
-- stretta?), e la scelta fra i tre esiti resterebbe cieca.
--
-- Idempotente (ADD COLUMN IF NOT EXISTS + CREATE OR REPLACE VIEW).

-- `run_id` e' il run che ha eseguito la scrittura. Per un sub-run e' l'id del
-- sub-run stesso, e siccome ogni sub-run esegue UN todo, e' il proxy del todo:
-- senza questa colonna la domanda "su quanti TODO si sbaglia" non e' rispondibile
-- — `session_id` confonde in un solo valore tutti i sub-run di una sessione, e una
-- violazione ripetuta 30 volte dallo stesso todo sarebbe indistinguibile da 30
-- todo che sbagliano una volta ciascuno. Sono due diagnosi opposte: la prima dice
-- "un piano fatto male", la seconda "il pianificatore non sa dichiarare".
--
-- Nessuna FK: `agent_runs`/`nexus_subagent_runs` vivono nei DB-PROGETTO mentre
-- `file_mutations` sta nel meta (separazione DB, mig 0527). Un vincolo cross-DB
-- non e' esprimibile; la correlazione si fa in lettura, per id.
ALTER TABLE public.file_mutations
    ADD COLUMN IF NOT EXISTS scope_verdict TEXT,
    ADD COLUMN IF NOT EXISTS declared_write_scope TEXT[],
    ADD COLUMN IF NOT EXISTS run_id UUID;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'file_mutations_scope_verdict_check'
    ) THEN
        ALTER TABLE public.file_mutations
            ADD CONSTRAINT file_mutations_scope_verdict_check
            CHECK (scope_verdict IS NULL OR scope_verdict IN
                   ('no_scope_declared', 'in_scope', 'out_of_scope'));
    END IF;
END $$;

-- Indice parziale sulle sole violazioni: sono la minoranza attesa e sono l'unica
-- riga che si interroga per path/progetto. Un indice pieno sarebbe peso morto
-- (le altre query di questa tabella vanno per progetto/path/sessione e hanno gia'
-- i loro indici in mig 0349).
CREATE INDEX IF NOT EXISTS idx_file_mutations_out_of_scope
    ON file_mutations (project_id, created_at DESC)
    WHERE scope_verdict = 'out_of_scope';

COMMENT ON COLUMN file_mutations.scope_verdict IS
    'Misura scope (mig 0646): no_scope_declared|in_scope|out_of_scope. NULL = riga precedente alla misura.';
COMMENT ON COLUMN file_mutations.declared_write_scope IS
    'Scope dichiarato dal pianificatore al momento della scrittura: dice COME sbagliava, non solo che sbagliava.';
COMMENT ON COLUMN file_mutations.run_id IS
    'Run che ha scritto (per un sub-run e'' il proxy del todo). Cross-DB con agent_runs: nessuna FK.';

-- Aggregazione: il deliverable e' un numero interrogabile, non un log da
-- spulciare. La vista e' il punto unico della domanda "quanto scrive fuori
-- scope?", cosi' chi analizza non ricopia la stessa GROUP BY con filtri diversi
-- (regola L: due copie divergono, e la seconda misura un'altra cosa).
--
-- `measured` esclude di proposito NULL e 'no_scope_declared': la percentuale ha
-- senso solo sulle scritture MISURABILI. Le altre due colonne restano in vista
-- perche' un `not_measurable` che domina e' il segnale che la propagazione dello
-- scope e' rotta — non che il pianificatore sia preciso.
CREATE OR REPLACE VIEW file_mutations_scope_audit AS
SELECT
    project_id,
    session_id,
    COUNT(*)                                                    AS mutations_total,
    COUNT(*) FILTER (WHERE scope_verdict IS NULL)               AS unmeasured_legacy,
    COUNT(*) FILTER (WHERE scope_verdict = 'no_scope_declared') AS not_measurable,
    COUNT(*) FILTER (WHERE scope_verdict IN ('in_scope', 'out_of_scope')) AS measured,
    COUNT(*) FILTER (WHERE scope_verdict = 'in_scope')          AS in_scope,
    COUNT(*) FILTER (WHERE scope_verdict = 'out_of_scope')      AS out_of_scope,
    -- "su quanti TODO": ogni sub-run esegue un todo, quindi i run DISTINTI che
    -- hanno violato sono i todo che hanno violato. Distinguere questo dal numero
    -- di scritture e' cio' che separa "un piano fatto male" da "il pianificatore
    -- non sa dichiarare lo scope".
    COUNT(DISTINCT run_id) FILTER (WHERE scope_verdict = 'out_of_scope') AS runs_out_of_scope,
    COUNT(DISTINCT run_id) FILTER (WHERE scope_verdict IN ('in_scope', 'out_of_scope'))
                                                                AS runs_measured,
    ROUND(
        100.0 * COUNT(*) FILTER (WHERE scope_verdict = 'out_of_scope')
        / NULLIF(COUNT(*) FILTER (WHERE scope_verdict IN ('in_scope', 'out_of_scope')), 0),
        2
    )                                                           AS out_of_scope_pct,
    MIN(created_at)                                             AS first_at,
    MAX(created_at)                                             AS last_at
FROM file_mutations
GROUP BY project_id, session_id;

-- Quali PATH ricorrono nelle violazioni. E' la seconda meta' del dato che serve a
-- decidere: se le violazioni si concentrano su pochi file adiacenti a quelli
-- dichiarati (la rotta accanto al modello), la risposta e' far raffinare lo scope
-- a valle da chi ha letto il codice; se sono sparse ovunque, e' il pianificatore
-- che non sa dichiarare. Due conclusioni opposte a partire dallo stesso totale.
--
-- Niente ORDER BY nella vista (lo impone chi interroga) e niente unnest dello
-- scope dichiarato: aggregare array di lunghezze diverse moltiplicherebbe le righe
-- e falserebbe proprio i conteggi per cui la vista esiste.
CREATE OR REPLACE VIEW file_mutations_out_of_scope_paths AS
SELECT
    project_id,
    file_path,
    COUNT(*)               AS scritture,
    COUNT(DISTINCT run_id) AS run_distinti,
    MIN(created_at)        AS prima,
    MAX(created_at)        AS ultima
FROM file_mutations
WHERE scope_verdict = 'out_of_scope'
GROUP BY project_id, file_path;

COMMENT ON VIEW file_mutations_out_of_scope_paths IS
    'Path ricorrenti nelle violazioni di scope (mig 0646): distingue poche aree adiacenti (raffinare lo scope a valle) da violazioni sparse (correggere il pianificatore).';

COMMENT ON VIEW file_mutations_scope_audit IS
    'Punto unico dell''aggregazione scope (mig 0646). out_of_scope_pct e'' calcolata sulle sole scritture misurabili; un not_measurable dominante indica propagazione rotta, non un pianificatore preciso.';
