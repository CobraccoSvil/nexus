-- 0644: la vista analitica dell'uso AI torna a dire il vero sui token di cache.
--
-- La 0405 ha creato `ai_usage_analytics_view` su questa premessa, scritta nel suo
-- commento: "dopo compute_turn_cost il campo prompt_tokens nel ledger e' NETTO
-- (gia' scorporato dei cached)". Da quella premessa discendevano due formule:
--
--     input_tokens_gross = SUM(prompt_tokens + cache_read_tokens)
--     cache_hit_rate     = SUM(cache_read) / SUM(cache_read + prompt_tokens)
--
-- La premessa non e' piu' vera. `prompt_tokens` nel ledger e' il LORDO: lo
-- scrivono cosi' entrambi gli scrittori (`nexus-gateway/src/server/billing.rs`
-- alla INSERT e `mcp-core/src/billing.rs` alla UPDATE di finalizzazione), perche'
-- il lordo e' la convenzione unica del sistema (`nexus_gateway::LlmUsage`) e i
-- due conteggi di cache ne sono un DETTAGLIO, mai addendi. Lo scorporo avviene in
-- un posto solo, `nexus_pricing::calculate_cost_breakdown` (regola L), che e'
-- l'unico a chiedersi a quanti token si applichi la tariffa piena.
--
-- Con il prompt lordo le due formule della 0405 sbagliano in modo sistematico:
--   - `input_tokens_gross` conta i cache_read DUE volte (sono gia' dentro
--     prompt_tokens), gonfiando l'input di ogni riga che usa la cache;
--   - `cache_hit_rate` ha lo stesso denominatore gonfiato, quindi SOTTOSTIMA il
--     riuso, cioe' l'esatta metrica per cui la vista era stata scritta.
--
-- Le migrazioni applicate sono immutabili: la 0405 resta com'e' e questa la
-- ridefinisce. Serve un DROP perche' `CREATE OR REPLACE VIEW` non puo' cambiare
-- il significato di una colonna gia' esistente ne' il suo tipo: si ricrea.
--
-- Le colonne restano le stesse (nessun consumatore applicativo legge la vista:
-- e' interrogata da query analitiche ad-hoc). Cambia cosa CALCOLANO:
--   - prompt_tokens_net   input DAVVERO pagato a tariffa piena: il lordo meno le
--                         due quantita' di cache, MA il lordo INTERO sulle righe
--                         con details->>'cache_price_state' =
--                         'cache_price_missing', dove il listino non aveva la
--                         tariffa di cache e nexus-pricing ha fatturato quei
--                         token a prezzo pieno invece di regalarli. Cosi' la
--                         colonna resta divisibile per il costo scritto accanto.
--   - input_tokens_gross  SUM(prompt_tokens), senza piu' addizioni.
--   - cache_hit_rate      cache_read / prompt_tokens (lordo), che e' la frazione
--                         di contesto servita dalla cache.
--
-- NB serie storica di Anthropic (stacco: 2026-07-27). Fino a questo lavoro
-- l'adapter Anthropic scriveva nel ledger `usage.input_tokens` verbatim, che per
-- Anthropic e' il NETTO (il wire riporta cache_read e cache_creation come campi
-- separati, non compresi). Da qui in avanti l'adapter SOMMA le due quantita' per
-- arrivare al lordo, come ogni altro provider. Conseguenze da tenere presenti
-- leggendo trend che attraversano la data:
--   - le righe Anthropic PRECEDENTI hanno prompt_tokens e total_tokens piu'
--     BASSI a parita' di chiamata (sottostimavano il contesto);
--   - su quelle righe questa vista calcola un input lordo sottostimato e un
--     hit-rate sovrastimato (denominatore piccolo). Il salto e' nei dati, non
--     nella formula: non e' un regime da correggere qui, perche' i token veri di
--     quelle chiamate non sono ricostruibili dal ledger;
--   - lo stesso salto tocca le quote (`ai_quota_policies`), che dal deploy
--     misurano per Anthropic il consumo reale invece di quello sottostimato.

DROP VIEW IF EXISTS ai_usage_analytics_view;

CREATE VIEW ai_usage_analytics_view AS
WITH catalog_price AS (
    -- Un prezzo per (provider, model). Il catalog non versiona storicamente i
    -- prezzi per coppia, quindi DISTINCT ON estrae la riga corrente; se in
    -- futuro il catalog avesse piu' righe per coppia, qui si aggiungera' il
    -- criterio di selezione (es. validita' temporale).
    SELECT DISTINCT ON (provider, model)
        provider,
        model,
        input_cost_per_million_tokens,
        cache_read_cost_per_million_tokens
    FROM ai_price_catalog
    ORDER BY provider, model
)
SELECT
    l.provider,
    l.model,
    date_trunc('hour', l.created_at)              AS bucket_hour,
    COUNT(*)                                      AS calls,
    -- input a TARIFFA PIENA: quanti token sono stati DAVVERO pagati al prezzo
    -- pieno di input. Non e' sempre "lordo meno la cache", e la differenza non e'
    -- un caso di confine: quando il listino non ha la tariffa di cache,
    -- calculate_cost_breakdown (nexus-pricing) RIMETTE quei token nel monte a
    -- tariffa piena invece di regalarli, e lo dichiara nel ledger con
    -- details->>'cache_price_state' = 'cache_price_missing'. Oggi e' il regime di
    -- maggioranza: la tariffa cache_read manca su 157/170 modelli google, 55/55
    -- openrouter, 9/9 groq, 73/146 openai, 50/101 mistral, e cache_creation su
    -- tutti tranne anthropic.
    -- Sottrarre comunque farebbe divergere questa colonna dal costo scritto
    -- accanto: chi divide input_cost per prompt_tokens_net otterrebbe una tariffa
    -- ~3.5x quella di catalog e concluderebbe che il calcolo del costo e' rotto,
    -- mentre a mentire sarebbe la colonna. E' lo stesso errore della 0405
    -- (premessa dichiarata piu' larga del vero), rifatto in piccolo.
    -- Il segnale per dire il vero c'e' gia' e lo scrivono ENTRAMBI gli scrittori
    -- del ledger (INSERT del gateway e UPDATE di finalize_usage).
    -- Righe storiche senza quella chiave: details->>... e' NULL, cade nell'ELSE,
    -- e li' cache_read/cache_creation sono 0, quindi la sottrazione e' identita'.
    -- Il cast tiene il tipo a BIGINT come nella 0405: senza, la somma di una
    -- espressione bigint uscirebbe numeric e cambierebbe il tipo della colonna.
    SUM(
        CASE
            WHEN l.details->>'cache_price_state' = 'cache_price_missing'
                THEN l.prompt_tokens
            ELSE GREATEST(
                l.prompt_tokens - l.cache_read_tokens - l.cache_creation_tokens,
                0
            )
        END
    )::bigint                                     AS prompt_tokens_net,
    SUM(l.completion_tokens)                      AS completion_tokens,
    SUM(l.total_tokens)                           AS total_tokens,
    SUM(l.cache_read_tokens)                      AS cache_read_tokens,
    SUM(l.cache_creation_tokens)                  AS cache_creation_tokens,
    -- input lordo = prompt_tokens, che i due conteggi di cache li comprende gia'
    SUM(l.prompt_tokens)::bigint                  AS input_tokens_gross,
    -- cache hit-rate sull'input lordo: 0 = nessun riuso, 1 = tutto dalla cache
    ROUND(
        SUM(l.cache_read_tokens)::numeric
        / NULLIF(SUM(l.prompt_tokens), 0),
        4
    )                                             AS cache_hit_rate,
    SUM(l.total_cost)                             AS total_cost,
    SUM(l.cache_read_cost)                        AS cache_read_cost,
    SUM(l.cache_creation_cost)                    AS cache_creation_cost,
    ROUND(SUM(l.total_cost) / NULLIF(COUNT(*), 0), 6) AS avg_cost_per_call,
    -- risparmio stimato dalla cache: token cached pagati a tariffa cache invece
    -- che a tariffa input piena. NULL se il prezzo del modello non e' a catalog.
    ROUND(
        SUM(l.cache_read_tokens)::numeric / 1000000.0
        * (cp.input_cost_per_million_tokens - cp.cache_read_cost_per_million_tokens),
        6
    )                                             AS cache_savings_est
FROM ai_usage_ledger l
LEFT JOIN catalog_price cp
    ON cp.provider = l.provider AND cp.model = l.model
WHERE l.status = 'finalized'
GROUP BY
    l.provider,
    l.model,
    date_trunc('hour', l.created_at),
    cp.input_cost_per_million_tokens,
    cp.cache_read_cost_per_million_tokens;

COMMENT ON VIEW ai_usage_analytics_view IS
    'Punto unico di lettura analitica uso AI (cache hit-rate, risparmio cache, costo/chiamata, trend) per (provider, model, ora) da ai_usage_ledger finalized. prompt_tokens del ledger e'' il LORDO: prompt_tokens_net e'' l''input a tariffa piena (lordo meno le due quantita'' di cache) e cache_hit_rate ha il lordo a denominatore. Mig 0644, ridefinisce la 0405. Per Anthropic le righe anteriori al 2026-07-27 hanno il prompt sottostimato (vedi la nota nella migrazione).';
