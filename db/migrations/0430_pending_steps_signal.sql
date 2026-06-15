-- 0430_pending_steps_signal.sql
--
-- Segnale strutturale "report con passi pendenti" — fix "Continuo non riprende
-- dopo report" (regola H: causa radice, non toppa lessicale).
--
-- Causa radice: quando un agente in modalita' continuous/automatic chiude il
-- turno con un resoconto del tipo
--
--    Stato attuale: ...
--    Prossimi passi necessari:
--    1. Verificare X
--    2. Eseguire Y
--
-- ...la risposta DICHIARA il task come ancora APERTO (elenca passi da fare) ma
-- sfugge a tutti i segnali esistenti di "non compiuto":
--   - `declared_outcome` (task_complete) assente;
--   - `_detect_unfulfilled_intent` cerca verbi 1a persona ("creero", "sto
--     procedendo") o futuri/gerundi morfologici: gli infiniti/imperativi degli
--     item ("Verificare", "Eseguire") NON matchano;
--   - il closure_judge LLM (mig 0422) ha la liberta' di classificare il report
--     come "lavoro svolto con limiti dichiarati" -> fulfilled=true.
--
-- Risultato: `route_after_executor` chiude come "resoconto finale legittimo"
-- (guard `has_productive_action_in_history AND NOT _unfulfilled_signal`), e
-- in continuous/automatic non c'e' trigger autonomo per ri-eseguire -> la
-- chat NON riprende.
--
-- Fix (de-lessicalizzazione strutturale, regola H + L):
--   1. Nuova funzione PURA `detect_pending_steps_report` in helpers.py:
--      identifica la STRUTTURA "etichetta-trigger (Prossimi passi, Next steps,
--      TODO, Da fare, ...) + >=N item numerati/puntati". Niente blacklist di
--      frasi, niente analisi dei verbi.
--   2. Integrata nel punto unico `_unfulfilled_signal` (routing.py) fra il
--      verdetto del closure_judge e il fallback lessicale. Cosi' il guard
--      "resoconto finale legittimo" NON chiude piu' un report-con-TODO, e
--      in continuous/automatic `_unfulfilled_triggers` ri-attiva l'executor.
--   3. Clausola esplicita nel prompt del closure_judge: "una lista di passi
--      ancora da svolgere = non compiuto" (allineamento decisore LLM ↔ segnale
--      strutturale).
--
-- DB-driven (regola G): due setting con cache 60s lato brain. Disattivabile
-- senza redeploy.
-- Idempotente.

BEGIN;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.closure.pending_steps_detection_enabled',
    'true',
    'agent',
    'Mig 0430: se true (default), il segnale strutturale detect_pending_steps_report e'' valutato in _unfulfilled_signal fra il closure_judge LLM e il fallback lessicale. Riconosce report del tipo "Prossimi passi necessari: 1. ... 2. ..." come task NON compiuto, indipendentemente da lingua/verbi. Risolve il caso "Continuo non riprende dopo report". false = disattivato (skip totale, fallback diretto alla blacklist lessicale). Cache 60s lato brain.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.closure.pending_steps_min_items',
    '2',
    'agent',
    'Mig 0430: numero minimo di item (numerati 1./2. o bullet -/*/•) dopo l''etichetta-trigger ("Prossimi passi", "Next steps", "TODO", "Da fare", ...) perche'' il testo sia classificato come "report con passi pendenti". Default 2 (un solo punto opzionale non basta). Aumentare per essere piu'' conservativi sui falsi positivi.'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
