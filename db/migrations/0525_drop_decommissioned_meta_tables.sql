-- 0525_drop_decommissioned_meta_tables.sql
-- Fase 2 (finale) del cutover db-separation per-progetto: rimozione DEFINITIVA
-- delle tabelle META decommissionate.
--
-- Contesto: il cutover ha spostato chat/run/step/plan/todo/ecc. nei DB di
-- progetto. Le copie META sono state RINOMINATE `zz_decommissioned_*` (mig 0507)
-- come "rename fail-fast" — rete di rollback + trappola per i call-site rimasti
-- per errore sul pool meta (una query sulla tabella rinominata fallisce subito e
-- rende il bug auto-rilevabile dai log). Il cutover e' confermato E2E e stabile:
-- le copie meta sono VUOTE (0 righe su tutte e 21, verificato) e NESSUN oggetto
-- vivo dipende da loro (nessuna vista, nessuna FK in entrata; solo i loro indici
-- e sequence, droppati in cascata). Questa migrazione chiude la fase 2 ed elimina
-- i vestigi, risolvendo "una volta per tutte" la duplicazione di schema nel meta.
--
-- Sicurezza:
--   - `DROP TABLE IF EXISTS` -> idempotente; safe anche dove una tabella non fu
--     mai creata (regola: la migrazione deve poter girare su ogni ambiente).
--   - `CASCADE` -> rimuove solo oggetti DI PROPRIETA' della tabella (indici,
--     sequence). Verificato: niente di vivo dipende dalle zz_.
--   - Dinamico: droppa OGNI tabella `zz_decommissioned_%` presente al momento,
--     senza elencare 21 nomi a mano (un solo punto di verita', copre anche
--     eventuali rinomini futuri con lo stesso prefisso).

DO $$
DECLARE
    t text;
    n int := 0;
BEGIN
    FOR t IN
        SELECT tablename FROM pg_tables
        WHERE schemaname = 'public' AND tablename LIKE 'zz_decommissioned_%'
        ORDER BY tablename
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS public.%I CASCADE', t);
        n := n + 1;
    END LOOP;
    RAISE NOTICE 'mig 0525: droppate % tabelle zz_decommissioned_*', n;
END $$;
