-- 0460_classifier_engine_cutover_rust.sql
-- CUTOVER del classifier intent da Python (endpoint brain /classify-intent-agentic)
-- a Rust in-process (crate::intent_classifier::classify). Ultima dipendenza viva
-- Rust->Python del path di classificazione: con classifier_engine='rust' mcp-core
-- NON chiama piu' /classify-intent-agentic ad ogni turno.
--
-- Razionale (regola H: cutover di config in migrazione versionata, non UPDATE a
-- mano): il valore deve sopravvivere a un wipe+migrate. La 0458 inserisce il
-- setting con default 'python' (ON CONFLICT DO NOTHING); questa migrazione lo
-- promuove a 'rust' DOPO che la 0458 lo ha creato. L'ordine numerico garantisce
-- che giri sempre dopo l'INSERT della 0458, sia su DB esistente sia su DB nuovo.
--
-- Validazione che precede il cutover (parita' Rust vs Python su 12 messaggi
-- rappresentativi, gateway+DB reali):
--   - action_oriented derivato: identico 12/12
--   - report_only derivato:     identico 12/12
--   - authorizes_changes:       identico 12/12 (segnale report-vs-act)
--   - intent:                   identico 11/12 (l'unico diff, 'verifica...compili'
--                               debug vs code_read, e' un borderline read-only con
--                               authorizes_changes=false e action_oriented/report_only
--                               identici -> comportamento downstream invariato)
--   - agentic_score gap <= 0.05 salvo il borderline (irrilevante per i derivati)
--
-- Punto unico (regola L): la scelta del motore vive nel selettore
-- orchestrator::intent::select_classifier_engine, che legge questa stessa chiave.
-- Regola G rispettata: nessun nome modello qui; il classifier Rust risolve il
-- modello via purpose 'intent_classifier' (tier-aware, mig 0102/0338).
--
-- ROLLBACK (reversibile, nessuna perdita dati):
--   UPDATE settings SET value = 'python' WHERE key = 'routing.classifier_engine';
--   (attendere <=60s per il refresh cache lato Rust)
--
-- Idempotente: la WHERE limita l'effetto al solo caso in cui il setting esiste.
UPDATE settings
SET value = 'rust',
    description = 'Motore di classificazione intent: ''rust'' (default dopo cutover mig 0460, crate::intent_classifier::classify in-process) oppure ''python'' (rollback, endpoint brain /classify-intent-agentic). Punto unico di scelta in orchestrator::intent (regola L). Cache 60s. Valore ignoto -> ''python'' (motore stabile).'
WHERE key = 'routing.classifier_engine';
