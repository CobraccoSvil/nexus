-- 0607_orchestration_paradigm_enable.sql
-- ACCENSIONE del paradigma di orchestrazione dimensionata dal problema.
--
-- Le fasi 1-8 sono state costruite tutte a flag OFF, cosi' ogni commit fosse
-- bit-identico al comportamento precedente e il rischio restasse zero fino a
-- qui. Ma un paradigma che resta spento non e' un paradigma: e' codice morto.
-- La lezione delle mig 0571/0572 e' esattamente questa — la direttiva della
-- review adversariale esisteva da settimane e aveva prodotto 0 esecuzioni,
-- perche' nessuno l'aveva accesa e nessun innesco la convocava.
--
-- Cosa cambia da adesso:
--   * `sizing_enabled` -> il numero di consiglieri, revisori, provider e
--     avvocati lo decide il PROBLEMA (classe del classificatore + profilo
--     admin) entro il budget di costo e tempo, non piu' un cap fisso uguale
--     per tutti. I cap storici restano come backstop.
--   * `debate_enabled` -> quando il consiglio dichiara una decisione
--     architetturale contesa, avvocati indipendenti difendono UNA posizione
--     ciascuno e il coordinatore decide sul merito del confronto. Prima il
--     motore misurava il dissenso; ora sa provocarlo.
--   * `advisory_overlap_enabled` -> il run parte SUBITO: la ricognizione
--     read-only non attende il consiglio (non ne ha bisogno), la prima
--     SCRITTURA si'. Un veto ferma il run prima della prima modifica.
--
-- Cosa NON si accende qui:
--   * `agent.run_time_budget_s` resta 0 (deadline disattivata): e' una scelta
--     di POLICY dell'utente, non parte del paradigma. Il doppio vincolo
--     costo+tempo funziona con la sola dimensione configurata; il resolver
--     dichiara in `sized_by` quale ha deciso.
--
-- Reversibile: `UPDATE settings SET value='false' WHERE key IN (...)` e il
-- sistema torna al comportamento precedente senza deploy (regola G: la
-- configurazione ha un solo posto, il DB).
--
-- Idempotente: l'UPDATE e' per chiave, ri-eseguirlo non cambia nulla.

UPDATE settings SET value = 'true', updated_at = NOW()
 WHERE key IN (
        'orchestrator.sizing_enabled',
        'orchestrator.debate_enabled',
        'orchestrator.advisory_overlap_enabled'
       )
   AND value <> 'true';
