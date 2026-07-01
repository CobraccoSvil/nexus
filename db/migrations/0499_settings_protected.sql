-- 0499_settings_protected.sql
-- Guard sui setting di INFRASTRUTTURA critici: non devono essere modificabili con un
-- toggle qualsiasi dagli endpoint admin generici (update_setting / bulk_update, sia in
-- admin-service sia in mcp-core -- oggi duplicati). Il rischio, emerso dall'audit del
-- go-live separazione DB: un flip accidentale di `db.project_separation.enabled` dalla
-- UI cambierebbe dove vivono i dati per-progetto.
--
-- Punto unico (regola L): invece di replicare il check in ogni handler HTTP, il guard
-- vive nel DB come trigger BEFORE UPDATE -> copre QUALSIASI vettore (qualunque servizio
-- che scriva su settings). DB-driven (regola G): la lista dei protetti e' la colonna
-- `is_protected`, non codice.
--
-- Bypass per le modifiche LEGITTIME (procedura documentata): settare la variabile di
-- sessione nella STESSA transazione ->
--   BEGIN; SET LOCAL app.allow_protected_write = 'on';
--   UPDATE settings SET value = '...' WHERE key = 'db.project_separation.enabled'; COMMIT;
-- Gli endpoint HTTP non la settano, quindi un loro UPDATE di un protetto fallisce con
-- errore chiaro (insufficient_privilege) e l'handler ritorna status=error.
--
-- Idempotente: ADD COLUMN IF NOT EXISTS + CREATE OR REPLACE + DROP/CREATE TRIGGER.

ALTER TABLE settings ADD COLUMN IF NOT EXISTS is_protected BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN settings.is_protected IS
  'Setting di infrastruttura critico: la modifica del value e'' bloccata dal trigger '
  'trg_settings_guard_protected salvo bypass esplicito (SET LOCAL app.allow_protected_write=''on'').';

CREATE OR REPLACE FUNCTION settings_guard_protected() RETURNS trigger AS $$
BEGIN
  IF OLD.is_protected
     AND NEW.value IS DISTINCT FROM OLD.value
     AND current_setting('app.allow_protected_write', true) IS DISTINCT FROM 'on' THEN
    RAISE EXCEPTION
      'setting protetto "%": modifica del value non consentita dagli endpoint generici. Usa la procedura dedicata (SET LOCAL app.allow_protected_write=''on'' nella stessa transazione).',
      OLD.key
      USING ERRCODE = 'insufficient_privilege';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_settings_guard_protected ON settings;
CREATE TRIGGER trg_settings_guard_protected
  BEFORE UPDATE ON settings
  FOR EACH ROW EXECUTE FUNCTION settings_guard_protected();

-- Marca i setting strutturalmente critici. Estendibile: aggiungere qui le chiavi che
-- rompono il sistema se cambiate per errore dalla UI.
UPDATE settings SET is_protected = TRUE
 WHERE key IN ('db.project_separation.enabled');
