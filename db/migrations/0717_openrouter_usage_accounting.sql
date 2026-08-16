-- 0717_openrouter_usage_accounting.sql
--
-- Opt-in di usage accounting verso i fornitori che lo richiedono nel body.
-- OpenRouter dichiara il costo ESATTO della chiamata in `usage.cost` (USD),
-- ma solo se la richiesta porta `usage: {"include": true}`. Oggi ogni chiamata
-- openrouter su modello scoperto dalla discovery entra nel ledger a costo 0
-- (`pricing_state='unknown'`, il listino non conosce il prezzo): il costo
-- dichiarato dal wire e' l'unico costo vero disponibile per quelle righe.
--
-- La colonna sta nel REGISTRY e non nel codice (regola G, stesso pattern di
-- extra_headers mig 0714): un fornitore nuovo che adotti lo stesso opt-in e'
-- una UPDATE, non un redeploy. Il bootstrap la legge, la passa al provider
-- generico e il client OpenAI-compat aggiunge il campo al body nel punto unico
-- `corpo_della_richiesta` (entrambi i percorsi complete/stream lo ereditano).
--
-- false = il campo non parte: e' il default di tutti i fornitori diretti, e la
-- colonna nuova non cambia il comportamento di nessuna riga oltre a openrouter.

ALTER TABLE nexus_provider_registry
    ADD COLUMN IF NOT EXISTS usage_accounting boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN nexus_provider_registry.usage_accounting IS
    'true = il client OpenAI-compat aggiunge usage:{include:true} al body per ricevere usage.cost dichiarato dal fornitore (USD). Consumato dal solo provider generico openai_compat. Oggi solo openrouter (mig 0717).';

UPDATE nexus_provider_registry
   SET usage_accounting = true,
       updated_at       = now()
 WHERE name = 'openrouter';
