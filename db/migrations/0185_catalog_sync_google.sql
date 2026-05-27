-- Mig 0185: aggiungi 'google' al default catalog_sync.providers.
--
-- Ora che google_provider.py supporta Vertex SDK con Service Account dal DB
-- (mig 0183/0184), il worker model_catalog_sync puo' interrogare anche Google
-- via brain REST `/providers/google/models/live`. Senza questa modifica il
-- catalog Google restava stale (gemini-1.5-flash deprecato, gemini-3.5-flash
-- inesistente, ecc.).

BEGIN;

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  ('catalog_sync.providers',
   'anthropic,openai,mistral,deepseek,google',
   'catalog_sync',
   'Provider per cui eseguire l''auto-discovery dei modelli (CSV). Google passa per brain REST /providers/google/models/live (Vertex SDK).',
   false)
ON CONFLICT (key) DO UPDATE
  SET value = CASE
    -- Solo se il valore corrente NON contiene gia' 'google', aggiungilo
    WHEN settings.value LIKE '%google%' THEN settings.value
    ELSE settings.value || ',google'
  END,
  description = EXCLUDED.description;

SELECT key, value FROM settings WHERE key = 'catalog_sync.providers';

COMMIT;
