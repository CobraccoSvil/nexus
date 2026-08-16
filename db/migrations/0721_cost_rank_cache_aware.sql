-- 0721_cost_rank_cache_aware.sql
-- Flag di rollout del riordino cache-aware di Rank::CostFirst (Fase 3, Lotto 1).
--
-- Il difetto che accompagna: la selezione del servizio unico (`model_service`,
-- Rank::CostFirst) ordina i candidati sul prezzo NOMINALE dell'input
-- (`AGENTIC_COST_FIRST_ORDER` = input_cost_per_million_tokens ASC), che ignora
-- due fatti gia' misurati e gia' usati ALTROVE nel sistema:
--
--   1. l'hit-rate di prompt-cache osservato sul ledger (mig 0656): in un loop
--      agentico il prefisso e' identico a ogni iterazione e una quota grande del
--      prompt costa una frazione. Misurato il 29/07/2026: deepseek 67% di hit
--      contro mistral 5,2%, e sullo stesso task mistral e' costato 18 volte
--      deepseek. La catena di escalation ordina GIA' sul costo atteso
--      (escalation_port); la selezione primaria — che decide la maggioranza
--      delle chiamate — no, ed e' il motivo per cui mistral fa il 62-65% delle
--      chiamate del parco;
--   2. le finestre orarie di prezzo (mig 0715): deepseek in fascia peak vale 2x,
--      e un ORDER BY sul prezzo base non lo vede mai.
--
-- Il criterio nuovo: costo ATTESO del milione di token di prompt =
-- expected_call_cost(prezzo vigente, CallShape{1M, 0}, hit osservato). Stessa
-- formula dell'escalation (regola L, nessuna seconda formula), stesso asse
-- dell'ORDER BY di oggi (input per milione), finestre incluse perche' il
-- moltiplicatore si applica DENTRO resolve_active_prices_at.
--
-- Consumatore (regola G, nessun default hardcoded nel codice):
--   mcp-core::orchestrator::cost_rank -> rerank_expected_cost
--
-- A flag OFF (default) il percorso e' bit-identico a oggi: nessun pool esteso,
-- nessun riordino. Flip a ON via UPDATE settings dopo la prima finestra di
-- osservazione; rollback = flip a OFF, zero deploy.
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'routing.cost_rank_cache_aware', 'false', 'routing',
    'Se ''true'', la selezione dei modelli con Rank::CostFirst (model_service) riordina i candidati del tier sul costo ATTESO del prompt — listino vigente (finestre orarie di ai_price_window incluse) scontato dall''hit-rate di prompt-cache osservato sul ledger (finestra e soglia di escalation.cache_hitrate_*) — invece che sul solo prezzo nominale di input. Il tier resta il criterio primario: il riordino avviene dentro ogni gruppo di tier, mai fra gruppi. A ''false'' (default) il percorso e'' bit-identico allo storico: nessun pool esteso, nessun riordino. Rollback = flip a false, zero deploy.'
)
ON CONFLICT (key) DO NOTHING;
