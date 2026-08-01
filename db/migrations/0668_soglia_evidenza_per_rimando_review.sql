-- Migrazione 0668 — Un rimando in correzione vuole l'evidenza che lo giustifica.
--
-- `orchestrator.review_min_severity_per_rimando`: gravita' MINIMA che almeno un
-- finding deve raggiungere perche' un voto `needs_changes` del panel di review
-- faccia girare il ciclo di correzione. Vocabolario chiuso: alta|media|bassa.
--
-- Trigger: misurato il 01/08/2026 sul run 397c0824 (bacheca-attivita, chat in
-- modalita' automatica). Due revisori: uno vota `pass` con zero finding, l'altro
-- `needs_changes` con UN finding di gravita' `bassa` il cui testo dice "Not a
-- blocker" e "Codice accettabile", su uno scenario che il revisore stesso
-- dichiara impossibile ("SELECT COUNT(*) never returns NULL"). Il panel ha
-- prodotto NeedsChanges; il ciclo di correzione e' girato a vuoto due volte
-- (progress: no_writes) e il run ha chiuso failed_diagnosed con l'applicazione
-- funzionante end-to-end.
--
-- Prima la gravita' entrava nella decisione SOLO per il veto
-- (`orchestrator.review_fail_on_high_severity`, mig 0571/0572): un
-- `needs_changes` faceva girare il ciclo qualunque cosa portasse a sostegno,
-- incluso nulla.
--
-- Default `media`: una `bassa` non giustifica da sola un giro di correzione — il
-- revisore che la trova scrive tipicamente "not a blocker" — una `media` si'.
-- Alzando a `alta` solo l'evidenza grave rimanda; abbassando a `bassa` si torna
-- al comportamento precedente.

INSERT INTO settings (key, value, description)
VALUES (
    'orchestrator.review_min_severity_per_rimando',
    'media',
    'Gravita minima (alta|media|bassa) che un finding deve raggiungere perche un voto needs_changes del panel di review valga come rimando in correzione. Sotto soglia il voto conta come approvazione e i finding restano annotati nel resoconto.'
)
ON CONFLICT (key) DO NOTHING;
