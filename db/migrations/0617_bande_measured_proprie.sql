-- 0617 - Le bande measured hanno soglie PROPRIE, e il vertice torna stretto
--
-- Misurato il 2026-07-20, dopo il primo giro completo della scala relativa
-- (35 modelli con measured_score a suite 5, ancora = deepseek-v4-pro 85.0):
--
--   medium     2 modelli   27.5-35.0
--   high       7 modelli   39.4-53.8
--   heavy     11 modelli   53.8-69.3
--   frontier  15 modelli   71.3-85.0   <- il 43% del parco
--
-- Una banda "frontier" che tiene quasi meta' dei modelli non identifica il
-- vertice: fra i 15 c'erano ministral-8b-2512 (75.0) e ministral-8b-latest
-- (71.3), modelli 8B con agentic_index 1.2. Non e' un errore di calcolo — quei
-- modelli passano DAVVERO chain 4/4, real 3/3, latent 3/4 — ma il soffitto si e'
-- spostato dai gate allo score: chi supera i profili facili ha gia' la
-- maggioranza dei punti, e solo il recupero separa (ministral batte minimax-m2.1
-- unicamente per un recovery 1/4 contro 0/4).
--
-- PERCHE' SOGLIE SEPARATE E NON UN VALORE CONDIVISO PIU' STRETTO
-- Le percentuali erano UNA sola serie (catalog.tier_relative.*) per entrambe le
-- ancore. Ma le due scale misurano grandezze diverse e si distribuiscono in modo
-- diverso: allo stesso 85%, il prior esterno da' 6 frontier su ~80 (7,5%: sano)
-- e il measured 15 su 35 (43%: rotto). Stringere il valore condiviso avrebbe
-- curato il measured ammalando il prior, che sarebbe sceso a 2 frontier senza
-- alcun dato che lo giustificasse. Ogni ancora prende le sue soglie
-- (`relative_bands(db, prefisso)`, gemella di `persist_anchor`).
--
-- IL VALORE 0.92, scelto sui dati e non a occhio. Simulazione sui 35 misurati:
--   pct    soglia   frontier
--   0.85     72.2      14      <- oggi
--   0.88     74.8       9
--   0.90     76.5       5
--   0.92     78.2       3      <- scelto
--   0.95     80.8       2      (fragile: due soli modelli)
-- A 0.92 restano deepseek-v4-pro (85.0), qwen/qwen3.6-plus (82.5) e
-- mistral-medium-latest (80.0); ministral-8b esce dal vertice. Le bande basse
-- non si toccano: heavy/high/medium restano ai valori del prior, che sui dati
-- attuali distribuiscono in modo ragionevole.
--
-- E' un taglio, non una cura: la compressione degli score in alto resta, e si
-- affronta alzando la difficolta' dei profili facili (links_target, profondita'
-- della catena, distrattori del latent). Questa migrazione rende il vertice
-- utilizzabile SUBITO; la taratura dei test e' il lavoro che segue.

INSERT INTO settings (key, value, description) VALUES
  ('catalog.measured_band.frontier_pct', '0.92',
   'Bande measured: soglia frontier come frazione del leader misurato. 0.92 (non 0.85 del prior) perche' ||
   ' la distribuzione degli score interni e'' compressa in alto: a 0.85 il 43% del parco era frontier (mig 0617).'),
  ('catalog.measured_band.heavy_pct', '0.65',
   'Bande measured: soglia heavy come frazione del leader misurato.'),
  ('catalog.measured_band.high_pct', '0.45',
   'Bande measured: soglia high come frazione del leader misurato.'),
  ('catalog.measured_band.medium_pct', '0.20',
   'Bande measured: soglia medium come frazione del leader misurato.')
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, description = EXCLUDED.description;
