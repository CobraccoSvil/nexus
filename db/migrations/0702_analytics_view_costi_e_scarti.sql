-- 0702: la vista analitica espone i COSTI per direzione e la spesa SCARTATA.
--
-- Due assenze, misurate durante la valutazione token del 12/08/2026:
--
-- 1. La vista non selezionava input_cost/output_cost: non esisteva NESSUNA
--    query pronta che dicesse quanto pesa l'OUTPUT sul costo di un modello, che
--    e' il primo numero da guardare per dimensionare max_tokens e thinking
--    budget (l'output domina i tempi: -50% output ~ -50% latenza).
--
-- 2. Le righe 'discarded' (mig 0701) non vi comparivano: la spesa dei tentativi
--    consumati-e-buttati (risposte degeneri, cap per-tentativo scaduti) restava
--    fuori dall'unico punto di lettura analitica.
--
-- Le migrazioni applicate sono immutabili: la 0644 resta com'e' e questa la
-- ridefinisce (stesso pattern con cui la 0644 ridefini' la 0405). DROP+CREATE
-- perche' cambiano le colonne.
--
-- Le colonne ESISTENTI mantengono il significato della 0644 (aggregati delle
-- sole righe 'finalized', via FILTER): nessun consumatore vede un gradino.
-- Le colonne NUOVE:
--   - input_cost / output_cost      costo per direzione (finalized);
--   - discarded_calls               tentativi consumati e scartati nel bucket;
--   - discarded_tokens              token fatturati di quei tentativi (le righe
--                                   attempt_timeout valgono 0 per costruzione:
--                                   nessun usage osservato);
--   - discarded_cost                costo di quei tentativi.

DROP VIEW IF EXISTS ai_usage_analytics_view;

CREATE VIEW ai_usage_analytics_view AS
WITH catalog_price AS (
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
    COUNT(*) FILTER (WHERE l.status = 'finalized') AS calls,
    -- input a TARIFFA PIENA (vedi 0644 per il razionale del CASE: quando il
    -- listino non ha la tariffa di cache, calculate_cost_breakdown fattura i
    -- token cached a prezzo pieno e lo dichiara in details.cache_price_state).
    SUM(
        CASE
            WHEN l.details->>'cache_price_state' = 'cache_price_missing'
                THEN l.prompt_tokens
            ELSE GREATEST(
                l.prompt_tokens - l.cache_read_tokens - l.cache_creation_tokens,
                0
            )
        END
    ) FILTER (WHERE l.status = 'finalized')::bigint AS prompt_tokens_net,
    SUM(l.completion_tokens) FILTER (WHERE l.status = 'finalized') AS completion_tokens,
    SUM(l.total_tokens) FILTER (WHERE l.status = 'finalized')      AS total_tokens,
    SUM(l.cache_read_tokens) FILTER (WHERE l.status = 'finalized') AS cache_read_tokens,
    SUM(l.cache_creation_tokens) FILTER (WHERE l.status = 'finalized')
                                                  AS cache_creation_tokens,
    SUM(l.prompt_tokens) FILTER (WHERE l.status = 'finalized')::bigint
                                                  AS input_tokens_gross,
    ROUND(
        SUM(l.cache_read_tokens) FILTER (WHERE l.status = 'finalized')::numeric
        / NULLIF(SUM(l.prompt_tokens) FILTER (WHERE l.status = 'finalized'), 0),
        4
    )                                             AS cache_hit_rate,
    SUM(l.total_cost) FILTER (WHERE l.status = 'finalized')        AS total_cost,
    -- costo per DIREZIONE: il rapporto output/input per modello e' il primo
    -- numero della valutazione token (docs/token-optimization.md).
    SUM(l.input_cost) FILTER (WHERE l.status = 'finalized')        AS input_cost,
    SUM(l.output_cost) FILTER (WHERE l.status = 'finalized')       AS output_cost,
    SUM(l.cache_read_cost) FILTER (WHERE l.status = 'finalized')   AS cache_read_cost,
    SUM(l.cache_creation_cost) FILTER (WHERE l.status = 'finalized')
                                                  AS cache_creation_cost,
    ROUND(
        SUM(l.total_cost) FILTER (WHERE l.status = 'finalized')
        / NULLIF(COUNT(*) FILTER (WHERE l.status = 'finalized'), 0),
        6
    )                                             AS avg_cost_per_call,
    ROUND(
        SUM(l.cache_read_tokens) FILTER (WHERE l.status = 'finalized')::numeric
        / 1000000.0
        * (cp.input_cost_per_million_tokens - cp.cache_read_cost_per_million_tokens),
        6
    )                                             AS cache_savings_est,
    -- la spesa NASCOSTA (mig 0701): tentativi consumati la cui risposta e'
    -- stata buttata. Zero e' un risultato: significa che nel bucket la chain
    -- non ha scartato nulla.
    COUNT(*) FILTER (WHERE l.status = 'discarded')                 AS discarded_calls,
    COALESCE(SUM(l.total_tokens) FILTER (WHERE l.status = 'discarded'), 0)
                                                  AS discarded_tokens,
    COALESCE(SUM(l.total_cost) FILTER (WHERE l.status = 'discarded'), 0)
                                                  AS discarded_cost
FROM ai_usage_ledger l
LEFT JOIN catalog_price cp
    ON cp.provider = l.provider AND cp.model = l.model
WHERE l.status IN ('finalized', 'discarded')
GROUP BY
    l.provider,
    l.model,
    date_trunc('hour', l.created_at),
    cp.input_cost_per_million_tokens,
    cp.cache_read_cost_per_million_tokens;

COMMENT ON VIEW ai_usage_analytics_view IS
    'Punto unico di lettura analitica uso AI per (provider, model, ora) da ai_usage_ledger. Aggregati finalized (colonne storiche della 0644, invariate nel significato) piu'' costo per direzione (input_cost/output_cost) e spesa scartata (discarded_* dalla mig 0701). Mig 0702, ridefinisce la 0644.';
