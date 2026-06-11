-- 0405_ai_usage_analytics_view.sql
--
-- Telemetria di LETTURA analitica sopra ai_usage_ledger (gap rilevato
-- dall'audit telemetria token). La raccolta era gia' completa (il ledger
-- cattura token, costi e la dimensione cache read/creation per ogni chiamata,
-- mig 0006 + 0129 + 0403), ma l'unico aggregato pronto era per-run
-- (usageBreakdown in chat_agent.rs). Le domande di ottimizzazione continuative
-- - cache hit-rate per provider, risparmio da cache, costo medio per chiamata,
-- trend nel tempo - richiedevano una query SQL scritta a mano ogni volta.
--
-- Questa vista e' il PUNTO UNICO di lettura analitica (regola L): un solo posto
-- che risponde a quelle domande. Regola G: nessun valore hardcoded, prezzi e
-- token vengono da ai_price_catalog e ai_usage_ledger. Regola H: vive in una
-- migrazione versionata, sopravvive a deploy e re-applicazione delle migrazioni.
--
-- Granularita': una riga per (provider, model, ora). L'aggregazione a giorno o
-- settimana si ottiene a valle con date_trunc sul bucket. Sfrutta l'indice
-- esistente idx_ledger_cache_provider_time (mig 0129): nessun indice nuovo.
--
-- NB contabilita': dopo compute_turn_cost (brain) il campo prompt_tokens nel
-- ledger e' NETTO (gia' scorporato dei cached, per non doppio-contarli). Quindi
-- l'input LORDO di una chiamata e' (prompt_tokens + cache_read_tokens) e il
-- cache hit-rate si calcola su quel denominatore.

CREATE OR REPLACE VIEW ai_usage_analytics_view AS
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
    SUM(l.prompt_tokens)                          AS prompt_tokens_net,
    SUM(l.completion_tokens)                      AS completion_tokens,
    SUM(l.total_tokens)                           AS total_tokens,
    SUM(l.cache_read_tokens)                      AS cache_read_tokens,
    SUM(l.cache_creation_tokens)                  AS cache_creation_tokens,
    -- input lordo = prompt netto + cached (vedi NB contabilita' sopra)
    SUM(l.prompt_tokens + l.cache_read_tokens)    AS input_tokens_gross,
    -- cache hit-rate sull'input lordo: 0 = nessun riuso, 1 = tutto dalla cache
    ROUND(
        SUM(l.cache_read_tokens)::numeric
        / NULLIF(SUM(l.cache_read_tokens + l.prompt_tokens), 0),
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
    'Punto unico di lettura analitica uso AI (cache hit-rate, risparmio cache, costo/chiamata, trend) per (provider, model, ora) da ai_usage_ledger finalized. Mig 0405.';
