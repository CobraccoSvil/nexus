-- Migrazione 0100: aggiorna nomi modello AI obsoleti.
--
-- Contesto: Mistral ha rinominato 'mistral-small-4' (alias usato in 0032)
-- a 'mistral-small-latest'. L'API restituisce 400 invalid_model per il vecchio
-- nome. Stessa cosa potrebbe succedere per altri modelli in futuro.
--
-- IMPORTANTE: questa e' una soluzione TAMPONE. La soluzione strutturale e' il
-- task spawnato "rimuovere TUTTI i model name hardcoded → registry DB" che
-- consente di aggiornare i modelli da admin UI senza migrazione.

-- 1. ai_price_catalog: rinomina riga mistral-small-4 (preserva costi storici).
UPDATE ai_price_catalog
   SET model = 'mistral-small-latest'
 WHERE provider = 'mistral'
   AND model = 'mistral-small-4';

-- 2. settings: aggiorna il default Mistral.
UPDATE settings
   SET value = 'mistral-small-latest'
 WHERE key = 'provider_model_mistral'
   AND value = 'mistral-small-4';

-- 3. nexus_routing_history non contiene una colonna 'model' separata
--    (selected_agent contiene gia' info aggregata). Skip.
