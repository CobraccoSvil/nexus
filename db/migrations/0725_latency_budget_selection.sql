-- 0725_latency_budget_selection.sql
-- I tre parametri del criterio "la latenza osservata entra nella selezione"
-- (Fase 3, Lotto 3 — F3-05).
--
-- Il difetto che accompagna: la selezione non sapeva quanto e' lento un
-- fornitore. L'unica fonte per-modello (`ai_model_health_history.latency_ms`,
-- probe ~30m) non aveva lettori sulla strada della scelta: il gate duale
-- convocava validatori col p95 osservato sopra il proprio timeout per
-- validatore (`orchestrator.critical_step_gate_timeout_s`), e ogni
-- convocazione bruciava un'astensione `timeout` per costruzione (misurato il
-- 13/08/2026: kimi p95 22-26s). Il rimedio NON e' alzare il timeout per
-- inseguire il lento (regola H): e' che la selezione segua la configurazione
-- — chi dichiara un budget non riceve chi, ai fatti, non arriva in tempo.
--
-- Il criterio (PURO, nexus-agent-graph::decisions::latency_budget):
-- percentile osservato oltre il budget -> escluso dal pool; IGNOTO (nessuna
-- osservazione, o campioni sotto soglia) NON esclude (regola Q); pool
-- svuotato -> pool INTERO servito col segnale `latency=overbudget_fallback`
-- nel rationale, mai fail-closed. Percentile e non media: un outlier singolo
-- sposta la media di un fattore che il percentile assorbe.
--
-- Consumatori (regola G, niente numeri nel codice):
--   mcp-core::latency_telemetry -> load_latency_policy / load_latency_observations
--   mcp-core::orchestrator::model_service -> ModelRequest.latency_budget_ms
--
-- Senza budget dichiarato (`latency_budget_ms = None`) il percorso e'
-- bit-identico allo storico: queste chiavi governano COME si misura, non SE.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'routing.latency.window_hours', '72', 'routing',
    'Finestra (ore) dello storico probe (ai_model_health_history) su cui si calcola il percentile di latenza per coppia (provider, model) quando un chiamante dichiara un budget di latenza alla selezione (ModelRequest.latency_budget_ms). Al ritmo del probe (~30m) 72h valgono ~144 campioni per modello. Contano i soli probe sani (healthy AND latency_ms IS NOT NULL).'
),
(
    'routing.latency.min_samples', '5', 'routing',
    'Campioni minimi in finestra perche'' il percentile di latenza sia una misura e non rumore: sotto questa soglia il candidato e'' Unknown e NON viene escluso dal budget (regola Q: non si decide al buio, un modello appena entrato a catalogo resta convocabile).'
),
(
    'routing.latency.percentile', '0.95', 'routing',
    'Il percentile (0..1] della latenza osservata confrontato col budget dichiarato (percentile_cont sui probe sani in finestra). 0.95 = si esclude chi nel 5% peggiore dei casi non arriva comunque: la domanda del budget e'' "entro quanto arriva DI SOLITO", e la media sarebbe spostata da un singolo outlier.'
)
ON CONFLICT (key) DO NOTHING;
