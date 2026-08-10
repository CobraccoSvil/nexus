-- 0695_data_routing_kind_dal_vocabolario.sql
--
-- La CHECK di `nexus_data_routing` ammetteva i soli due kind che esistevano
-- quando la tabella e' nata (mig 0496, 30/06/2026): 'session' e 'run'. Da
-- allora il codice ne ha aggiunti TRE — 'message', 'correction', 'feedback' —
-- e ogni loro scrittura e' stata respinta dal DB con SQLSTATE 23514, mentre
-- `register_entity_routing` ne ingoiava l'errore in un WARN indistinguibile da
-- un guasto di rete.
--
-- MISURATO il 10/08/2026 sul meta-DB: 893 righe in tabella, 706 'run' e 187
-- 'session'. Zero 'message', zero 'correction', zero 'feedback' — non "poche":
-- nessuna, mai, in 41 giorni. Nei log di mcp-core il rifiuto compare 11 volte
-- nei soli 4 file di log conservati.
--
-- La conseguenza e' di comportamento, non di igiene: la directory serve a
-- rispondere in O(1) alla domanda "di quale progetto e' questa entita'".
-- `project_data_pool_by_message_from`, `_by_correction_from` e
-- `_by_feedback_from` non trovavano mai la riga, quindi cadevano SEMPRE sul
-- fallback che itera tutti i DB-progetto con una SELECT ciascuno; e il
-- self-healing che avrebbe dovuto registrare la mappa al primo passaggio
-- riscriveva ogni volta la stessa riga rifiutata. Il fast-path era inerte per
-- costruzione, e piu' progetti esistono piu' costava.
--
-- L'elenco di questa CHECK non e' scritto a mano: DISCENDE dal vocabolario
-- `nexus_project_pools::EntityKind::TUTTI` (regole L + N), che e' anche il tipo
-- con cui il codice passa il kind da questa migrazione in poi. Un test in
-- quel crate confronta le due liste: aggiungere una variante senza toccare lo
-- schema fa fallire il test, invece di produrre scritture respinte in silenzio.

ALTER TABLE nexus_data_routing
    DROP CONSTRAINT IF EXISTS nexus_data_routing_entity_kind_check;

ALTER TABLE nexus_data_routing
    ADD CONSTRAINT nexus_data_routing_entity_kind_check
    CHECK (entity_kind IN ('session', 'run', 'message', 'correction', 'feedback'));

-- Niente backfill per i tre kind nuovi: la directory e' una CACHE di
-- instradamento e si ripopola da sola al primo accesso di ogni entita'
-- (self-healing di `project_data_pool_by_search_from`), che da adesso puo'
-- finalmente scrivere. Un backfill dovrebbe iterare i DB-progetto per
-- materializzare righe che nessuno ha ancora chiesto: costo certo, beneficio
-- solo per le entita' che verranno davvero interrogate.
