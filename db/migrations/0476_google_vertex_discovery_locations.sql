-- 0476_google_vertex_discovery_locations.sql
-- Discovery multi-region per il backend Vertex + fallback di region in inference.
--
-- Causa radice: il provider Google (backend Vertex) usava UNA sola location
-- (settings.google_vertex_location = 'europe-west4') sia per il DISCOVERY
-- (list_models, GET publisherModels) sia per l'INFERENCE
-- (generateContent/streamGenerateContent). La region europe-west4 NON espone i
-- modelli gemini-3.x: nel catalog risultano missing_from_api, mentre la region
-- 'global' li espone e alcuni (es. gemini-3.5-flash) rispondono 200. Verificato
-- live. Una sola location non puo' soddisfare entrambi i requisiti
-- (data-residency UE come prima scelta + accesso ai 3.x oggi solo su 'global').
--
-- Fix strutturale (DB-driven, regola G; punto unico del bivio gemini/vertex,
-- regola L):
--   - DISCOVERY: il provider itera su una LISTA ordinata di region (questo
--     setting) e unisce/deduplica i modelli trovati, cosi' il catalog_sync vede
--     anche i gemini-3.x esposti solo su 'global'.
--   - INFERENCE: per ogni modello prova le region NELL'ORDINE; la PRIMA che
--     risponde non-404 vince ed e' cachata (TTL ~300s) per il modello. I modelli
--     presenti in UE (europe-west4, prima region) NON producono 404 -> nessun
--     fallback, zero regressione su gemini-2.5. Il fallback a 'global' scatta
--     SOLO per i 3.x assenti in UE.
--
-- DATA-RESIDENCY: l'ordine del CSV e' l'ordine di preferenza. La prima region
-- (europe-west4) e' UE: il fallback fuori UE (global) avviene SOLO se la UE da
-- 404. Un deploy che non deve MAI uscire dall'UE imposta questo setting a
-- 'europe-west4' (senza 'global'): cosi' non c'e' alcun fallback fuori UE e i
-- modelli 3.x semplicemente non saranno disponibili finche' la UE non li espone.
--
-- NB: la sola fonte di verita' resta il DB (regola G). google_vertex_location
-- (mig 0183) continua a definire la region di prima scelta per l'inference;
-- questo setting estende SOLO le region candidate di discovery e fallback.

INSERT INTO settings (key, value, category, description)
VALUES (
    'google_vertex_discovery_locations',
    'europe-west4,global',
    'providers',
    'CSV ordinato per preferenza delle region Vertex usate per discovery (list_models) e fallback di region in inference. La prima region che risponde non-404 vince. La prima e'' UE (data-residency): per restare sempre in UE impostare a ''europe-west4'' (senza ''global'').'
)
ON CONFLICT (key) DO NOTHING;
