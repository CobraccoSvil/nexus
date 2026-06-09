-- 0373: provider instradabili dall'endpoint /vision/describe come PUNTO UNICO
-- DB-driven (regola G/L). Elimina la lista hardcoded duplicata tra
-- classify_capabilities (Rust) e brain/grpc_server/routes/vision.py (Python).
--
-- classify_capabilities marca supports_vision=true SOLO per i provider in questa
-- lista (con l'euristica/metadata per-modello); vision.py instrada solo questi e
-- usa la lista anche nel messaggio 501. Aggiungere un provider alla descrizione
-- immagini = aggiungere il suo nome QUI + il ramo corrispondente in vision.py.
-- Formato: CSV di provider lowercase.

INSERT INTO settings (key, value, category, description) VALUES
    ('vision.routable_providers', 'google,anthropic,openai', 'routing',
     'Provider con un ramo implementato nell''endpoint /vision/describe (brain/grpc_server/routes/vision.py). Punto unico DB-driven: classify_capabilities (Rust) marca supports_vision=true SOLO per questi provider; vision.py instrada e segnala solo questi. CSV di provider lowercase. Aggiungere un provider richiede anche il suo ramo vision.py.')
ON CONFLICT (key) DO NOTHING;
