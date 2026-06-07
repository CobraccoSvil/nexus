-- 0352_routing_heal_orphan_pins.sql
--
-- Fix definitivo del bug "routing su modello morto" (regola H).
--
-- DIAGNOSI (verificata sul DB):
--   - catalog_sync ha correttamente disabilitato mistral-large-2411
--     (auto_disabled_reason='missing_from_api') perche' Mistral non lo offre piu'.
--   - MA tutte le ~106 righe agentiche di nexus_routing_matrix hanno
--     manual_override=true (pin tecnici delle migrazioni 0260/0268/0270/0274/
--     0337). Sia il promote sia il cleanup di routing_matrix_auto_promoter
--     RISPETTANO manual_override e non toccano quelle righe.
--   - Risultato: la matrix resta a puntare a mistral-large-2411 (inesistente);
--     il routing lo sceglie, fallisce e degrada su mistral-small-latest (light).
--
-- FIX in 3 parti:
--   1) Riconciliazione dati immediata: sostituisce il modello morto col
--      successore sano dello stesso provider+tier in TUTTE le righe attive.
--      (Lo stesso esito che heal_orphan_pinned_models produrra' a regime.)
--   2) Flag DB del nuovo auto-heal (regola G), default abilitato.
--   3) Corregge il default_model di mistral: era mistral-small-latest (light),
--      causa del fallback degradato. Lo portiamo a un modello capace (medium).
--
-- Idempotente: gli UPDATE sono ripetibili; ON CONFLICT DO UPDATE sul settings.

-- 1) Riconciliazione dati: ogni riga matrix attiva il cui model_id e'
--    missing_from_api viene aggiornata al miglior sostituto sano dello stesso
--    provider+performance_tier (featured > context grande > costo basso).
--    Preserva manual_override (cambia solo il model_id morto).
WITH dead AS (
    SELECT DISTINCT m.provider, m.model_id AS dead_model, c.performance_tier AS tier
      FROM nexus_routing_matrix m
      JOIN ai_price_catalog c
        ON LOWER(c.provider) = LOWER(m.provider) AND c.model = m.model_id
     WHERE m.is_active = true
       AND c.auto_disabled_reason = 'missing_from_api'
),
repl AS (
    SELECT d.provider, d.dead_model,
           (SELECT c2.model
              FROM ai_price_catalog c2
             WHERE LOWER(c2.provider) = LOWER(d.provider)
               AND c2.performance_tier = d.tier
               AND c2.is_enabled = true
               AND c2.consecutive_failures = 0
               AND c2.model <> d.dead_model
             -- Il tier di un provider mescola modelli di capacita' diverse
             -- (es. Mistral 'medium' ha sia ministral-3b sia mistral-large).
             -- Si PREFERISCE il successore della stessa famiglia: stesso prefisso
             -- (model senza l'ultimo segmento versione). regexp_replace toglie
             -- l'ultimo "-<token>": mistral-large-2411 -> mistral-large.
             ORDER BY (regexp_replace(c2.model, '-[^-]+$', '')
                       = regexp_replace(d.dead_model, '-[^-]+$', '')) DESC,
                      COALESCE(c2.is_featured, false) DESC,
                      COALESCE(c2.context_window, 8192) DESC,
                      c2.input_cost_per_million_tokens ASC
             LIMIT 1) AS new_model
      FROM dead d
)
UPDATE nexus_routing_matrix m
   SET model_id = r.new_model,
       notes = COALESCE(m.notes, '') ||
               ' [0352 heal: ' || r.dead_model || ' -> ' || r.new_model || ']',
       updated_at = NOW()
  FROM repl r
 WHERE LOWER(m.provider) = LOWER(r.provider)
   AND m.model_id = r.dead_model
   AND m.is_active = true
   AND r.new_model IS NOT NULL;

-- 2) Flag del nuovo auto-heal (consumato da heal_orphan_pinned_models, regola G).
INSERT INTO settings (key, value, category, description) VALUES
(
    'agent.routing_matrix_heal_orphan_enabled', 'true', 'agent',
    'Se true, l''auto-promoter sostituisce nei record routing i modelli '
    || '''missing_from_api'' (deprecati dal provider) col miglior modello sano '
    || 'dello stesso provider+tier, ANCHE su righe manual_override (pin orfani).'
)
ON CONFLICT (key) DO NOTHING;

-- 3) default_model mistral capace: era mistral-small-latest (light), causa del
--    fallback degradato sui task agentici. Lo portiamo a mistral-large-latest
--    (medium, tool-robust) se presente e sano; altrimenti resta invariato.
UPDATE nexus_provider_default_model
   SET model_id = 'mistral-large-latest', updated_at = NOW()
 WHERE provider = 'mistral'
   AND model_id = 'mistral-small-latest'
   AND EXISTS (
        SELECT 1 FROM ai_price_catalog
         WHERE provider = 'mistral' AND model = 'mistral-large-latest'
           AND is_enabled = true
   );
