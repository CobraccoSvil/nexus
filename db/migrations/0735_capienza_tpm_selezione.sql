-- 0735_capienza_tpm_selezione.sql
-- I due parametri del criterio "il tetto di token al minuto dichiarato dal
-- fornitore entra nella selezione".
--
-- NUMERO: 0733 e' occupata; 0734 la sta prendendo un altro cantiere in volo,
-- che al momento della scrittura non ha ancora committato il file. Al merge il
-- numero va RIVERIFICATO: due file con lo stesso numero e sqlx ne applica UNO
-- SOLO, in silenzio (gia' successo con la 0690).
--
-- Il difetto che accompagna, MISURATO il 17/08/2026 in esercizio. Run reale
-- dalla UI, contesto ~180.000 token: la selezione ha scelto
-- groq/openai/gpt-oss-20b e ha preso HTTP 429 «Rate limit reached ... on
-- tokens per minute (TPM): Limit 8000, Used 5503». Il dato che avrebbe evitato
-- il tentativo era GIA' IN CASA, scritto dal sensore della mig 0718 un minuto
-- prima:
--
--   groq | openai/gpt-oss-20b | tokens_limit 8000 | tokens_remaining 120
--        | tokens_reset_at 14:59:18 | observed_at 14:58:19
--
-- Sono DUE fatti con rimedi opposti: il residuo istantaneo (120 su 8000) e'
-- una congestione che passa da sola al reset; il limite di 8000 TPM contro
-- 180.000 token di contesto e' STRUTTURALE — per i turni grossi quella coppia
-- non e' una scelta valida MAI. Il sensore della 0718 nacque dichiarando «solo
-- telemetria, nessuna decisione automatica»: la scelta era giusta allora
-- (prima si osserva, poi si decide), e questa e' la migrazione che chiude il
-- giro.
--
-- Il criterio (PURO, nexus-agent-graph::decisions::capienza_tpm):
--   richiesta > tokens_limit  -> OltreIlLimite, ESCLUDE dal pool (429 certo);
--   richiesta > tokens_remaining -> ResiduoInsufficiente, RETROCEDE in coda
--     (al reset torna valido, e se e' l'unico rimasto e' meglio provarlo);
--   nessuna osservazione / troppo vecchia / tetto o residuo non dichiarati
--     -> Ignota, NON tocca nulla (regola Q: «non ho guardato» non e' «non ci
--     sta»; deepseek, openrouter e perplexity non mandano quegli header e non
--     vanno penalizzati per questo);
--   pool svuotato dall'esclusione -> pool INTERO servito col segnale
--     `tpm=oltre_limite_ricaduta` nel rationale, mai fail-closed.
--
-- Consumatori (regola G, niente numeri nel codice):
--   mcp-core::tpm_telemetry -> load_tpm_policy / load_tpm_observations
--   mcp-core::orchestrator::model_service -> ModelRequest.richiesta_token_stimati
--
-- Senza dimensione dichiarata (`richiesta_token_stimati = None`) il percorso
-- e' bit-identico allo storico: queste chiavi governano COME si misura, non
-- SE.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'routing.tpm_guard_enabled', 'true', 'routing',
    'Se il tetto di token al minuto dichiarato dal fornitore (nexus_rate_limit_observations, mig 0718) entra nella selezione del modello quando il chiamante dichiara la dimensione della richiesta (ModelRequest.richiesta_token_stimati). Nasce ACCESO, al contrario degli altri criteri opt-in: a fatti ignoti non fa nulla (nessuna osservazione = nessuna esclusione), e dove i fatti ci sono evita un 429 certo. Spegnerlo riporta la selezione a quella del 17/08/2026, che mandava 180.000 token a un fornitore da 8000 TPM.'
),
(
    'routing.tpm_observation_max_age_s', '120', 'routing',
    'Oltre questa eta'' (secondi) un''osservazione di rate limit non descrive piu'' il minuto corrente e il criterio la dichiara SCADUTA, senza escludere ne'' retrocedere nessuno. 120s = due giri dello snapshot del gateway (gateway.rate_limit_snapshot_interval_s, default 30s) piu'' margine. Non e'' un filtro SQL: la lettura porta su anche le righe vecchie, perche'' «osservata e non piu'' fresca» e «mai osservata» sono due cose diverse da dire a chi legge (regola Q).'
)
ON CONFLICT (key) DO NOTHING;
