-- 0514: flag di rendering chat "activity stream" (ADR 0037).
--
-- Registra la chiave `chat.activity_stream_enabled` nella tabella `settings` in
-- modo VERSIONATO (regola G/H): il flag compare nella UI admin ed e' toggleabile
-- senza patch codice ne' env var. Sopravvive a wipe + re-migrazione.
--
-- Default 'false': con flag OFF la chat mantiene il rendering odierno,
-- bit-identico (ADR 0037 sezione "Flag e sicurezza"). L'attivazione (UPDATE a
-- 'true') e' letta a runtime dal frontend via `GET /api/ui-flags`, che espone la
-- whitelist dei flag UI non sensibili a QUALUNQUE utente autenticato (non solo
-- admin) -> la feature non resta muta per i non admin (regola H).
--
-- Colonne allineate allo schema reale di `settings` (mig 0002:
-- key/value/category/description/is_secret/updated_at). Non sensibile:
-- is_secret resta al default FALSE.

INSERT INTO settings (key, value, category, description) VALUES
  ('chat.activity_stream_enabled', 'false', 'chat',
   'Abilita il rendering "activity stream" della chat (timeline per-provider, ADR 0037). OFF = rendering odierno (bit-identico). Letto dal frontend via /api/ui-flags.')
ON CONFLICT (key) DO NOTHING;
