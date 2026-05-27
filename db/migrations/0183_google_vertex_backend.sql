-- Mig 0183: Backend dual per Google provider.
--
-- Permette di switchare il provider Google tra Gemini API direct (default,
-- semplice, API key) e Vertex AI (enterprise, Service Account, region GCP).
-- Stessi modelli, stesso SDK (google-genai), backend differente.
--
-- Default backend = "gemini" per backward compatibility: gli utenti con
-- google_api_key gia' configurata continuano a funzionare senza modifiche.
--
-- Per attivare Vertex:
--   1. UPDATE settings SET value='vertex' WHERE key='google_provider_backend';
--   2. UPDATE settings SET value='nexus-prod' WHERE key='google_vertex_project';
--   3. UPDATE settings SET value='europe-west4' WHERE key='google_vertex_location';
--   4. UPDATE settings SET value='{...service-account-json...}' WHERE key='google_vertex_credentials_json';
--   5. Riavvio brain (nexus-neural-wsl) per ricreare il client.
--
-- Region consigliate (privacy GDPR-friendly):
--   europe-west4 (Eemshaven, NL) - default consigliato EU
--   europe-west1 (St. Ghislain, BE)
--   europe-west8 (Milano, IT)
--   europe-southwest1 (Madrid, ES)

BEGIN;

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  ('google_provider_backend',
   'gemini',
   'providers',
   'Backend Google: ''gemini'' (API key, direct API) o ''vertex'' (Service Account, Vertex AI). Default gemini per backward compat.',
   false),
  ('google_vertex_project',
   '',
   'providers',
   'ID progetto GCP per Vertex AI (es. nexus-prod-123456). Vuoto se backend=gemini.',
   false),
  ('google_vertex_location',
   'europe-west4',
   'providers',
   'Region GCP per Vertex AI. Consigliato europe-west4/europe-west8 per compliance EU. Vuoto se backend=gemini.',
   false),
  ('google_vertex_credentials_json',
   '',
   'providers',
   'Service Account JSON per Vertex AI (sensitive, contenuto del file di chiavi GCP). Vuoto se backend=gemini o se si usa GOOGLE_APPLICATION_CREDENTIALS env.',
   true)
ON CONFLICT (key) DO NOTHING;

COMMIT;
