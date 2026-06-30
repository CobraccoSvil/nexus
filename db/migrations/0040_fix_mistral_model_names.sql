-- Migration 0040: corregge i nomi modello Mistral nel catalogo
-- "codestral" non esiste nell'API Mistral → "codestral-latest"
-- "mistral-nemo" non esiste nell'API Mistral → "open-mistral-nemo"

UPDATE ai_price_catalog
SET model = 'codestral-latest'
WHERE provider = 'mistral' AND model = 'codestral';

UPDATE ai_price_catalog
SET model = 'open-mistral-nemo'
WHERE provider = 'mistral' AND model = 'mistral-nemo';
