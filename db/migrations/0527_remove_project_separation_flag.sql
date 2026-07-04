-- 0527: rimozione del flag db.project_separation.enabled. Il cutover e' chiuso:
-- le tabelle meta zz_decommissioned_* sono droppate (mig 0525) e la separazione
-- per-progetto e' ora SEMPRE attiva nel codice (rami OFF rimossi). Il flag non ha
-- piu' semantica: il suo ramo OFF (rollback al meta) leggerebbe tabelle inesistenti.
-- Protetto dal trigger 0499 -> serve il bypass di sessione nella stessa transazione.
SET LOCAL app.allow_protected_write = 'on';
DELETE FROM settings WHERE key = 'db.project_separation.enabled';
