-- Mig 0184: Vertex DB-only credentials.
--
-- Allinea le descrizioni dei settings Vertex alla regola G del CLAUDE.md:
-- "TUTTA la configurazione viene dal DB. Niente env var, niente fallback
-- hardcoded, niente magic defaults."
--
-- Rimuove dalle descrizioni qualsiasi riferimento a GOOGLE_APPLICATION_CREDENTIALS
-- o ADC come fallback: il brain ora REJECTA queste sorgenti e usa solo il DB.
--
-- Vedi mig 0183 per la creazione iniziale delle chiavi.

BEGIN;

UPDATE settings
SET description = 'Service Account JSON per Vertex AI (sensitive, contenuto del file di chiavi GCP). OBBLIGATORIO se backend=vertex: il brain NON eredita credenziali da env GOOGLE_APPLICATION_CREDENTIALS o ADC. Incolla qui l''intero contenuto del file JSON SA.'
WHERE key = 'google_vertex_credentials_json';

UPDATE settings
SET description = 'Region GCP per Vertex AI (es. europe-west4, europe-west8, us-central1). OBBLIGATORIO se backend=vertex. Consigliato europe-west4/europe-west8 per compliance EU/GDPR. Default: europe-west4.'
WHERE key = 'google_vertex_location';

UPDATE settings
SET description = 'ID progetto GCP per Vertex AI (es. nexus-prod-123456). OBBLIGATORIO se backend=vertex.'
WHERE key = 'google_vertex_project';

UPDATE settings
SET description = 'Backend Google provider: ''gemini'' (Gemini API direct, API key) oppure ''vertex'' (Vertex AI, Service Account dal DB). Default ''gemini''. Tutte le credenziali Vertex devono essere nel DB — niente env var.'
WHERE key = 'google_provider_backend';

COMMIT;
