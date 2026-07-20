-- 0625: cap dei rimandi in correzione del ReviewGate.
--
-- La review adversariale programmatica diventa un NODO del grafo (ReviewGate),
-- interposto sul funnel di chiusura: su verdetto Fail/NeedsChanges rimanda il
-- run all'executor con i findings come consegna di correzione (stesso
-- meccanismo del ramo FAIL del final_gate), invece di annotare un run gia'
-- chiuso. Questo setting governa QUANTI rimandi sono ammessi prima che la
-- bocciatura diventi definitiva (RejectedFinal -> failed_diagnosed).
--
-- Le altre chiavi della review (autoconvene_enabled, quorum_min_valid,
-- fail_on_high_severity, panel_size) esistono gia' (mig 0571/0572) e vengono
-- riusate invariate dal gate.
--
-- Panel convocati al piu' N+1 (l'ultima ri-review verifica l'ultima correzione).
INSERT INTO settings (key, value, description)
VALUES (
    'orchestrator.review_max_correction_cycles',
    '1',
    'ReviewGate: numero massimo di rimandi in correzione dopo una review adversariale bocciata (0 = mai rimandare, la bocciatura e'' subito definitiva). I panel convocati per run sono al piu'' N+1.'
)
ON CONFLICT (key) DO NOTHING;
