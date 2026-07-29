-- 0657_affinita_fornitore_instradatore.sql
-- Quale FORNITORE A VALLE preferire, su un instradatore, perche' il prefisso
-- venga davvero riusato.
--
-- Il difetto che chiude. Il commit 16e3175a manda a OpenRouter sia `session_id`
-- (che dovrebbe fissare il fornitore dentro l'instradatore) sia
-- `prompt_cache_key` (inoltrato a valle, fissa il server interno di quel
-- fornitore). Su `x-ai/grok-4.5` ha funzionato: da 1% a 98% stabile. Sugli altri
-- modelli OpenRouter no, con un'alternanza 99%/0% fra chiamate consecutive a
-- prefisso IDENTICO.
--
-- La causa, misurata il 29/07/2026 chiamando DIRETTAMENTE l'API OpenRouter
-- (bypassando il gateway, l'unico modo per leggere il campo `provider` della
-- risposta e vedere CHI ha servito), 8 chiamate consecutive per sequenza:
--
--   qwen/qwen3-235b-a22b-2507, senza vincolo   0/8 con cache
--     fornitori visti nella stessa sequenza: DeepInfra, Alibaba, Novita
--   z-ai/glm-4.7-flash, senza vincolo          6/8, fornitori: Cloudflare, DeepInfra
--
-- `session_id` NON fissa il fornitore: cambia fra una chiamata e la successiva.
-- E i fornitori dello stesso modello non si equivalgono -- alcuni la cache non
-- la servono affatto:
--
--   qwen3-235b   DeepInfra 0/8    Alibaba 0/8    Novita 0/6    Google 6/6 (99%)
--   glm-4.7-flash  Cloudflare 0%              DeepInfra 8/8 (99%)
--   minimax-m2     Minimax 0/4    Novita 4/4*  Google 99%
--
-- Fissato il fornitore, l'intermittenza sparisce: la causa e' l'affinita', non
-- il fornitore che "cachea male". `x-ai/grok-4.5` non compare in tabella perche'
-- non ne ha bisogno: OpenRouter lo serve da un fornitore solo (xAI), ed e'
-- esattamente il motivo per cui li' il fix precedente era bastato.
--
-- (*) minimax-m2 su Novita e' un caso a parte: riporta `prompt_tokens` 20.011
-- dove Minimax e Google, con lo STESSO prefisso, riportano 10.160/10.162 -- il
-- doppio esatto. Non e' un errore di lettura nostro: OpenRouter fattura su quel
-- numero (`cost` $0,00341 contro $0,00266 di Minimax, nonostante il 49% di cache
-- dichiarato). Qui la preferenza per Google lo evita, ma la contabilita' che
-- ricalcola il costo dal listino invece di leggere quello dichiarato
-- dall'instradatore resta un difetto suo, tracciato a parte.
--
-- PREFERENZA, non vincolo (`allow_fallbacks` resta true lato codice): misurato
-- che l'ordine da solo tiene fermo il fornitore -- 8/8 sullo stesso fornitore
-- con i ripieghi ATTIVI, su entrambi i modelli -- quindi non c'e' ragione di
-- pagare la perdita del ripiego di OpenRouter per averlo. Se il fornitore
-- preferito e' giu', la chiamata riesce comunque su un altro e si perde solo il
-- riuso del prefisso.
--
-- Regola G: il vocabolario dei fornitori sta qui, non nel codice. Il criterio
-- "su un instradatore va fissato anche il fornitore" sta invece nel punto unico
-- del concern (`PromptCacheKeying::requires_upstream_pinning`, regola L).
--
-- Consumatore: nexus-gateway::providers::openai_compat (cache TTL 60s).

CREATE TABLE IF NOT EXISTS nexus_router_upstream_affinity (
    -- L'instradatore, come si chiama nel registry provider ('openrouter').
    provider        TEXT NOT NULL,
    -- Il modello COMPLETO come lo conosce l'instradatore ('qwen/qwen3-235b-a22b-2507').
    model_id        TEXT NOT NULL,
    -- Fornitori a valle in ordine di preferenza, come li nomina l'instradatore
    -- (campo `provider` della risposta OpenRouter). CSV: il primo che risponde
    -- serve la chiamata.
    upstream_order  TEXT NOT NULL,
    -- Quando la preferenza e' stata misurata sul campo, e con che esito. Serve a
    -- sapere quando una riga e' vecchia: i fornitori di un modello cambiano.
    misurato_il     DATE,
    nota            TEXT,
    is_active       BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, model_id)
);

COMMENT ON TABLE nexus_router_upstream_affinity IS
    'Fornitore a valle preferito, per modello di un instradatore, perche il prefisso venga riusato. Vedi 0657.';
COMMENT ON COLUMN nexus_router_upstream_affinity.upstream_order IS
    'CSV di fornitori in ordine di preferenza, nel vocabolario dell instradatore. Preferenza, non vincolo: i ripieghi restano attivi.';

INSERT INTO nexus_router_upstream_affinity
    (provider, model_id, upstream_order, misurato_il, nota)
VALUES
    ('openrouter', 'qwen/qwen3-235b-a22b-2507', 'Google', DATE '2026-07-29',
     'Senza preferenza 0/8: la sequenza girava fra DeepInfra, Alibaba e Novita, che su questo modello non servono cache. Google 6/6 al 99%.'),
    ('openrouter', 'z-ai/glm-4.7-flash', 'DeepInfra', DATE '2026-07-29',
     'Senza preferenza 6/8 con il fornitore che alternava Cloudflare e DeepInfra. DeepInfra 8/8 al 99%.'),
    ('openrouter', 'minimax/minimax-m2', 'Google,Minimax', DATE '2026-07-29',
     'Google serve la cache al 99%. Novita e escluso di proposito: riporta e fattura prompt_tokens doppio (20.011 contro 10.162 sullo stesso prefisso).')
ON CONFLICT (provider, model_id) DO NOTHING;
