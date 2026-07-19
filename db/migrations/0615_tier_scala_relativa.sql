-- 0614: la scala RELATIVA dei tier (Fase A del piano "scala relativa").
--
-- Le soglie ASSOLUTE sull'agentic_index (mig 0600) erano fossili del parco di
-- un giorno preciso: a ogni rilascio forte andavano riviste a mano, e i due
-- fallimenti opposti misurati (banda heavy VUOTA per un'asticella
-- irraggiungibile; high satura, superata da un 8B) hanno mostrato che
-- un'asticella fissa non misura un parco che si muove. Da qui: il piu' forte
-- trovato E' frontier, le bande sono PERCENTUALI del leader
-- (model_service::tier_from_leader, punto unico regola L), e quando arriva uno
-- piu' forte si ri-scala tutto senza ri-misurare nessuno.
--
-- L'ANCORA (il leader) e' persistita e si muove solo oltre una deadband
-- (anti-flapping): la aggiorna refresh_tiers_from_index dopo ogni sync
-- dell'indice, via update_setting_value (le chiavi NASCONO qui, regola G).

-- (1) La scala: percentuali del leader. Ad ancora 54.0 (il leader attuale,
--     openai/gpt-5.6-sol) riproducono quasi esattamente le vecchie soglie
--     assolute: 45.9/35.1/24.3/10.8 contro 45/35/25/10. Quantificato sul
--     catalogo vivo (19/07): 5 modelli su 79 cambiano banda, tutti nelle
--     finestre di bordo.
INSERT INTO settings (key, value, category, description) VALUES
  ('catalog.tier_relative.frontier_pct', '0.85', 'routing',
   'Frazione del leader (ancora) da cui parte la banda frontier. La scala dei tier e'' RELATIVA al parco: il piu'' forte trovato e'' frontier (mig 0615).'),
  ('catalog.tier_relative.heavy_pct', '0.65', 'routing',
   'Frazione del leader da cui parte heavy. Vedi frontier_pct.'),
  ('catalog.tier_relative.high_pct', '0.45', 'routing',
   'Frazione del leader da cui parte high.'),
  ('catalog.tier_relative.medium_pct', '0.20', 'routing',
   'Frazione del leader da cui parte medium. Sotto: light.'),
  ('catalog.tier_relative.anchor_deadband_pct', '0.03', 'routing',
   'Scarto relativo minimo perche'' l''ancora segua un nuovo massimo (anti-flapping): entro la deadband la scala non si muove.')
ON CONFLICT (key) DO NOTHING;

-- (2) L'ancora del prior, seminata dal massimo indice FRESCO del parco enabled
--     (finestra = catalog.agentic_index_sync.max_age_hours, come a runtime).
--     Su un DB nuovo (wipe + re-apply) il catalogo e' vuoto: l'ancora nasce
--     vuota e la fissa il primo giro di refresh_tiers_from_index — fino ad
--     allora il prior per-modello tace, che e' la verita'.
INSERT INTO settings (key, value, category, description)
SELECT 'catalog.tier_relative.anchor',
       COALESCE((
         SELECT max(agentic_index)::text FROM ai_price_catalog
          WHERE is_enabled AND agentic_index IS NOT NULL
            AND agentic_index_at >= now() - make_interval(hours =>
                COALESCE((SELECT value::int FROM settings
                           WHERE key = 'catalog.agentic_index_sync.max_age_hours'), 168))
       ), ''),
       'routing',
       'L''ANCORA della scala relativa del prior: l''agentic_index del leader del parco. Aggiornata da refresh_tiers_from_index con la deadband; vuota = scala non ancora ancorata.'
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
SELECT 'catalog.tier_relative.anchor_model',
       COALESCE((
         SELECT provider || '/' || model FROM ai_price_catalog
          WHERE is_enabled AND agentic_index IS NOT NULL
            AND agentic_index_at >= now() - make_interval(hours =>
                COALESCE((SELECT value::int FROM settings
                           WHERE key = 'catalog.agentic_index_sync.max_age_hours'), 168))
          ORDER BY agentic_index DESC LIMIT 1
       ), ''),
       'routing',
       'Il modello leader dell''ancora del prior (diagnostica: un numero senza la sua premessa e'' un''opinione).'
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description) VALUES
  ('catalog.tier_relative.anchor_at', now()::text, 'routing',
   'Quando l''ancora del prior e'' stata fissata l''ultima volta.')
ON CONFLICT (key) DO NOTHING;

-- (3) Via le soglie ASSOLUTE (mig 0600) e, per idempotenza sul wipe, i fossili
--     prezzo della 0599 (gia' rimossi dai DB vivi dalla 0608).
DELETE FROM settings WHERE key IN (
  'catalog.tier_prior.agentic_index_frontier_min',
  'catalog.tier_prior.agentic_index_heavy_min',
  'catalog.tier_prior.agentic_index_high_min',
  'catalog.tier_prior.agentic_index_medium_min',
  'catalog.tier_prior.frontier_min_input_cost',
  'catalog.tier_prior.heavy_min_input_cost',
  'catalog.tier_prior.high_min_input_cost',
  'catalog.tier_prior.long_context_tokens'
);
