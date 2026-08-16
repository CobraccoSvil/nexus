-- 0714_openrouter_header_attribuzione.sql
--
-- Header di attribuzione verso OpenRouter. La doc del fornitore RACCOMANDA
-- `HTTP-Referer` e `X-Title` su ogni richiesta (ranking e attribuzione della
-- app chiamante); non sono obbligatori, l'API funziona col solo Bearer. La
-- mig 0567 lo annotava gia' come completamento previsto ("campo extra_headers
-- nel client OpenAiCompatClient + registry"): questa migrazione e' la meta'
-- dati di quel completamento.
--
-- La colonna sta nel REGISTRY e non nel codice (regola G): un header nuovo o
-- un valore diverso e' una UPDATE con TTL di riavvio del provider, non un
-- redeploy. Forma: oggetto JSONB piatto {nome: valore}, solo valori stringa.
-- Il bootstrap lo legge, lo passa al provider generico e il client
-- OpenAI-compat lo applica a TUTTE le richieste HTTP (chat, stream, lista
-- modelli, healthcheck, immagini, audio): l'attribuzione non dipende dal
-- verbo. Gli adapter dedicati (openai, anthropic, ...) non leggono il campo:
-- compongono le proprie richieste e nessuno di loro ha un fornitore che
-- chieda header di attribuzione.
--
-- NULL = nessun header extra: e' il default di tutti i fornitori diretti, e
-- la colonna nuova non cambia il comportamento di nessuna riga esistente
-- oltre a openrouter.

ALTER TABLE nexus_provider_registry
    ADD COLUMN IF NOT EXISTS extra_headers JSONB NULL;

COMMENT ON COLUMN nexus_provider_registry.extra_headers IS
    'Header HTTP aggiuntivi applicati a OGNI richiesta verso il fornitore (oggetto piatto nome->valore, solo stringhe). NULL = nessuno. Consumato dal solo provider generico openai_compat; openrouter lo usa per gli header di attribuzione HTTP-Referer / X-Title (mig 0714).';

UPDATE nexus_provider_registry
   SET extra_headers = '{"HTTP-Referer": "https://cobracco.it/nexus", "X-Title": "Nexus"}'::jsonb,
       updated_at    = now()
 WHERE name = 'openrouter';
