-- 0600: l'agentic_index come SEME del tier, sopra il prezzo.
--
-- PERCHE' (misurato sui 110 modelli attivi+tool, prima di spendere i probe).
-- Il `facts_prior` sul PREZZO (mig 0599) dimezza le inversioni rispetto al nome
-- (64 -> 31) ma sbaglia in modo grossolano, perche' il prezzo e' il posizionamento
-- COMMERCIALE del fornitore, non la capacita' del modello:
--   agentic  PRIOR      oggi       modello
--      30.2  heavy      light      gpt-5.4-mini    <- un MINI promosso a heavy
--      47.2  heavy      frontier   claude-opus-4-8 <- DECLASSATO
--      54.0  heavy      high       gpt-5.6-sol     <- il migliore, non arriva a frontier
-- Un mini caro e un frontier economico rompono la scala. Il prezzo resta come
-- RIPIEGO per i modelli non coperti, non come sorgente principale.
--
-- L'`agentic_index` (Artificial Analysis, via OpenRouter) MISURA invece cio' che
-- ci serve: Agents 34% + Coding 24% dell'Intelligence Index v4.1, eseguito su un
-- harness agentico con tool. E' esattamente il nostro uso.
--
-- L'ORDINE diventa:
--   manual > measured (batteria) > agentic_index > prezzo (ripiego) > NULL
--
-- LIMITI DICHIARATI (per questo e' un SEME e non l'autorita'):
--   1. il campo `benchmarks.artificial_analysis` di OpenRouter e' UNDOCUMENTED
--      (le doc citano solo Design Arena): puo' sparire senza preavviso. Se sparisce
--      l'indice resta l'ultimo sincronizzato, invecchia, e `agentic_index_at` lo
--      rende visibile — poi si ricade sul prezzo.
--   2. l'indice e' VERSIONATO (v4.0 -> v4.1 ha cambiato 3 benchmark su 9): le
--      soglie assolute vanno riviste a ogni cambio di versione. Per questo sono in
--      settings (regola G) e non nel codice.
--   3. copre 43/110 dei nostri modelli (39%) — ma e' il 39% che il routing sceglie
--      davvero. Per gli altri il prezzo batte comunque il nome.
--   4. l'harness domina il modello (arXiv 2605.23950: "harness-induced variance can
--      substantially exceed model-induced variance, including model ranking
--      reversal"): la misura NOSTRA (batteria) resta superiore e sostituisce questa.

ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS agentic_index DOUBLE PRECISION,
  ADD COLUMN IF NOT EXISTS agentic_index_at TIMESTAMPTZ;

COMMENT ON COLUMN ai_price_catalog.agentic_index IS
  'Artificial Analysis agentic_index (0-100) via OpenRouter, sincronizzato da '
  'sync_agentic_index. SEME del tier sopra il prezzo, sotto la misura della '
  'batteria. NULL = non coperto (39% del parco lo e''): si ricade sul prezzo.';
COMMENT ON COLUMN ai_price_catalog.agentic_index_at IS
  'Quando l''indice e'' stato sincronizzato. La fonte e'' UNDOCUMENTED e puo'' '
  'sparire: questa colonna rende visibile un indice che invecchia invece di '
  'lasciarlo passare per fresco.';

-- Le soglie, dai dati reali misurati il 16/07 (distribuzione del parco):
--   54.0 gpt-5.6-sol | 52.8 fable-5 | 47.2 opus-4-8 | 46.7 sonnet-5 | 41.1 gpt-5.4
--   36.4 deepseek-v4-pro | 30.2 gpt-5.4-mini | 21.4 gemini-3.1-pro | 16.4 haiku-4.5
--   5.5 mistral-large | 1.0 gpt-4o-mini
INSERT INTO settings (key, value, category, description) VALUES
  ('catalog.tier_prior.agentic_index_frontier_min', '45', 'routing',
   'agentic_index minimo per la fascia frontier. Soglia ASSOLUTA su un indice VERSIONATO (AA v4.1): rivedere a ogni cambio di versione della metodologia.'),
  ('catalog.tier_prior.agentic_index_heavy_min', '35', 'routing',
   'agentic_index minimo per heavy. Vedi agentic_index_frontier_min.'),
  ('catalog.tier_prior.agentic_index_high_min', '25', 'routing',
   'agentic_index minimo per high.'),
  ('catalog.tier_prior.agentic_index_medium_min', '10', 'routing',
   'agentic_index minimo per medium. Sotto questa soglia: light.'),
  ('catalog.agentic_index_sync.enabled', 'true', 'routing',
   'Se sincronizzare l''agentic_index da OpenRouter (API pubblica, senza auth). A false l''indice non si aggiorna e il prior ricade sul prezzo.'),
  ('catalog.agentic_index_sync.url', 'https://openrouter.ai/api/v1/models', 'routing',
   'Endpoint del catalogo OpenRouter da cui leggere benchmarks.artificial_analysis.agentic_index. Nel DB (regola G): se cambia non serve un redeploy.'),
  ('catalog.agentic_index_sync.max_age_hours', '168', 'routing',
   'Oltre questa eta'' l''indice e'' considerato STANTIO e il prior lo ignora, ricadendo sul prezzo. La fonte e'' undocumented: se sparisce, l''indice invecchia e il sistema deve accorgersene invece di fidarsi per sempre.')
ON CONFLICT (key) DO NOTHING;
