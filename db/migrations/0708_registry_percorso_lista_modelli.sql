-- 0705_registry_percorso_lista_modelli.sql
--
-- «Dove sta la LISTA MODELLI di questo fornitore?» non e' la stessa domanda di
-- «dove stanno le sue COMPLETION», e Perplexity lo dimostra: le due risposte
-- vivono su prefissi di versione DIVERSI dello stesso host.
--
-- MISURATO il 13/08/2026 contro l'API reale, con la chiave in `perplexity_api_key`:
--   POST https://api.perplexity.ai/chat/completions     -> 400 (endpoint c'e', il
--                                                          400 e' sul parametro:
--                                                          "max_tokens must be at
--                                                          least 16")
--   POST https://api.perplexity.ai/v1/chat/completions  -> 404
--   GET  https://api.perplexity.ai/models               -> 404   <- quello nostro
--   GET  https://api.perplexity.ai/v1/models            -> 200 (42 modelli)
--
-- Il registry porta UNA sola base (`base_url_default` = 'https://api.perplexity.ai',
-- mig 0568) e il client OpenAI-compat vi appendeva `/models` sia per la discovery
-- sia per lo HEALTHCHECK. Conseguenza doppia, e la seconda e' la piu' grave:
--   1. il catalog sync di perplexity fallisce a OGNI giro
--      («catalog_sync[perplexity] skip: Nexus Gateway 502 ... perplexity HTTP 404»);
--   2. `healthcheck()` e' lo stesso GET: per il re-probe del gateway perplexity
--      non torna sano MAI, qualunque cosa faccia il fornitore.
--
-- PERCHE' UN PERCORSO E NON UNA SECONDA URL: la divergenza e' nel PATH dell'API,
-- non nell'host. Con una seconda URL assoluta, un override operativo di
-- `perplexity_base_url` (un proxy) verrebbe applicato alle sole completion e la
-- discovery continuerebbe a uscire dal proxy — due destinazioni per lo stesso
-- fornitore, che e' una versione piu' silenziosa del difetto di partenza.
--
-- Il DEFAULT resta '/models': e' il dialetto OpenAI che tutti gli altri parlano,
-- e nessuna riga esistente cambia comportamento.

ALTER TABLE nexus_provider_registry
    ADD COLUMN IF NOT EXISTS models_path TEXT NOT NULL DEFAULT '/models';

COMMENT ON COLUMN nexus_provider_registry.models_path IS
    'Percorso della lista modelli RELATIVO a base_url (default ''/models'', dialetto OpenAI). Perplexity espone le completion sulla radice e i modelli sotto /v1: vedi mig 0705.';

UPDATE nexus_provider_registry
   SET models_path = '/v1/models',
       updated_at  = now()
 WHERE name = 'perplexity'
   AND models_path <> '/v1/models';
