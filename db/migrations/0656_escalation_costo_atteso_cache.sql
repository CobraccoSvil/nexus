-- 0656_escalation_costo_atteso_cache.sql
-- Finestra e soglia con cui si osserva l'hit-rate di prompt-cache, usato dalla
-- catena di escalation per ordinare i modelli sul costo ATTESO invece che sul
-- solo listino.
--
-- Il difetto che accompagna: la vista `v_model_escalation_chain` (mig 0471)
-- ordina su `blended_cost = input*0.75 + output*0.25`, che e' il prezzo PIENO
-- dell'input. Ma in un loop agentico il prefisso -- system prompt, tool schemas,
-- primi messaggi -- e' identico a ogni iterazione, quindi una quota grande e
-- sistematica del prompt viene servita da cache a una frazione del prezzo
-- (DeepSeek ~1/10, OpenAI ~1/2). Il blended_cost non ha modo di vederla, e
-- l'escalation sceglie il fornitore piu' caro proprio quando il contesto e' al
-- massimo -- cioe' quando la cache conta di piu'.
--
-- Misurato il 29/07/2026 su `ai_usage_ledger` (7 giorni, colonne cache
-- attendibili dal commit 587595b9 in poi; prima erano zero per costruzione):
--   deepseek    403 chiamate  9.889.456 token in  67,0% da cache
--   mistral     393 chiamate  6.444.280 token in   5,2% da cache
--   openrouter  290 chiamate  2.411.734 token in   9,2% da cache
-- Sullo stesso task (app gestione-spese) deepseek e' costato $0,14-$0,19 e
-- mistral $3,08-$0,77: la differenza e' quasi tutta hit-rate.
--
-- La granularita' e' (provider, model) e non provider: dentro mistral,
-- `mistral-small-latest` sta al 17,1% e `mistral-medium-latest` allo 0,0%;
-- dentro openrouter, `z-ai/glm-4.7-flash` al 43,1% e `qwen3-235b` allo 0,0%.
-- La media per provider cancellerebbe proprio il segnale da cui si decide.
--
-- Consumatori (regola G, nessun numero hardcoded nel codice):
--   nexus-ledger::observed_cache_hit_rate  -> legge entrambe le chiavi
--   nexus-pricing::expected_call_cost      -> applica la frazione al listino
--   mcp-core::agent_graph_adapter::escalation_port -> ordina la catena
--
-- Campioni sotto la soglia, o finestra vuota (modello nuovo, mai chiamato):
-- l'hit-rate NON viene indovinato. Si dichiara ignoto (`CacheHitRate::Unknown`)
-- e il costo atteso resta quello di listino, cioe' esattamente il comportamento
-- di oggi. Un default prudenziale inventato sarebbe il magic fallback che la
-- regola G vieta.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'escalation.cache_hitrate_window_hours', '168', 'routing',
    'Ampiezza in ore della finestra su `ai_usage_ledger` da cui si misura l''hit-rate di prompt-cache per (provider, model), usato dalla catena di escalation per stimare il costo atteso di una chiamata. Default 168 = 7 giorni: abbastanza da coprire i modelli usati di rado, abbastanza corto da seguire un cambio di listino o di comportamento del provider. Attenzione alle serie storiche: le colonne cache del ledger sono attendibili solo dal 28/07/2026 (commit 587595b9); prima riportano zero per costruzione, e uno zero li'' non significa "nessun hit" ma "nessuno guardava". Allargare la finestra oltre quella data non aggiunge informazione, la diluisce.'
),
(
    'escalation.cache_hitrate_min_samples', '20', 'routing',
    'Numero minimo di chiamate con prompt non vuoto che (provider, model) deve avere nella finestra perche'' il suo hit-rate sia considerato misurato. Sotto la soglia l''hit-rate e'' dichiarato IGNOTO e il costo atteso ricade sul listino pieno, mai su un valore di ripiego inventato (regola G): poche chiamate producono un rapporto instabile, e un modello appena entrato in catalogo avrebbe altrimenti un hit-rate apparente di 0% che lo penalizzerebbe a vita, impedendogli di accumulare le chiamate con cui smentirlo.'
)
ON CONFLICT (key) DO NOTHING;
