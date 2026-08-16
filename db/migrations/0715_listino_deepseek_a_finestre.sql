-- ─────────────────────────────────────────────────────────────────────────────
-- 0715 — il listino deepseek ha due prezzi al giorno, il catalogo ne portava uno
--
-- Dal 16/08/2026 16:00 UTC deepseek fattura per FASCE ORARIE (fonte:
-- api-docs.deepseek.com/quick_start/pricing, verificata il 16/08): PEAK =
-- 01:00-04:00 e 06:00-10:00 UTC, tutte le altre ore OFF-PEAK, e il peak vale
-- 2x l'off-peak su OGNI voce del listino. Il catalogo ha UNA riga per
-- (provider, model): qualunque numero singolo — il peak, l'off-peak di prima,
-- una media — e' un prezzo che il fornitore non pratica in almeno meta' della
-- giornata, e il ledger fatturerebbe con quello (regola G: non si inventa).
--
-- Forma scelta: il catalogo porta il prezzo BASE (= off-peak per deepseek);
-- una finestra attiva MOLTIPLICA TUTTE le voci (input, output, cache read,
-- cache creation) — e' la forma del listino deepseek (peak = 2x ogni voce) e
-- resta esprimibile per qualunque fornitore futuro a fasce. Il moltiplicatore
-- lo applica il punto unico del listino (`nexus-pricing`,
-- `moltiplicatore_finestra` + `resolve_active_price*`), all'istante della
-- risoluzione. NB per le stime DIFFERITE (batch): vanno risolte con l'ora di
-- esecuzione prevista, non con l'ora della stima — oggi nessun chiamante lo
-- fa, annotato nella doc del crate.
-- ─────────────────────────────────────────────────────────────────────────────

-- (1) Le finestre orarie di prezzo.
CREATE TABLE IF NOT EXISTS ai_price_window (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  provider    text NOT NULL,
  -- NULL = la finestra vale per TUTTE le coppie del provider (jolly). Una
  -- finestra con model valorizzato vale per quel solo modello e VINCE sul
  -- jolly: decide la piu' specifica (nexus_pricing::moltiplicatore_finestra).
  model       text NULL,
  -- Orari UTC di parete, intervallo SEMIAPERTO [start, end): alle 04:00 il
  -- peak 01:00-04:00 e' gia' finito, come nel listino del fornitore.
  -- start > end = la finestra scavalca la mezzanotte (es. 23:00-01:00).
  start_utc   time NOT NULL,
  end_utc     time NOT NULL,
  multiplier  numeric(8,4) NOT NULL,
  label       text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT moltiplicatore_positivo CHECK (multiplier > 0),
  -- start = end sarebbe ambiguo (finestra vuota o giornata intera?): non entra.
  CONSTRAINT finestra_non_degenere   CHECK (start_utc <> end_utc),
  CONSTRAINT provider_normalizzato   CHECK (provider = lower(btrim(provider)))
);

COMMENT ON TABLE ai_price_window IS
  'Finestre orarie di prezzo: il catalogo porta il prezzo BASE (= off-peak per deepseek); una finestra attiva moltiplica TUTTE le voci (input, output, cache read, cache creation) — e'' la forma del listino deepseek (peak = 2x ogni voce). Consumata dal punto unico nexus-pricing. Mig 0715.';

-- Una finestra e' identificata da cio' che dichiara: stesso provider, stesso
-- bersaglio, stessi estremi. Indice e non PRIMARY KEY perche' COALESCE e'
-- un'espressione (stesso motivo della 0707).
CREATE UNIQUE INDEX IF NOT EXISTS ai_price_window_chiave
  ON ai_price_window (provider, COALESCE(model, '*'), start_utc, end_utc);

-- (2) Seed: le due fasce peak di deepseek, PER MODELLO e non a jolly. La
-- finestra vale solo dove il prezzo BASE e' stato ri-basato all'off-peak (le
-- due righe curate al punto 4): un jolly di provider moltiplicherebbe x2 anche
-- le righe legacy (deepseek-chat, r1, v3...), che portano il prezzo flat del
-- sync LiteLLM e NON l'off-peak — in fascia peak il ledger le fatturerebbe
-- il doppio di una base sbagliata, e senza lucchetto il sync potrebbe
-- riscriverle mentre la finestra continua a moltiplicarle. L'invariante
-- «catalogo = base off-peak» vale solo dove il lock lo custodisce, e la
-- finestra deve coprire esattamente quel perimetro (review avversaria 16/08).
INSERT INTO ai_price_window (provider, model, start_utc, end_utc, multiplier, label) VALUES
  ('deepseek', 'deepseek-v4-flash', TIME '01:00', TIME '04:00', 2.0, 'peak'),
  ('deepseek', 'deepseek-v4-flash', TIME '06:00', TIME '10:00', 2.0, 'peak'),
  ('deepseek', 'deepseek-v4-pro',   TIME '01:00', TIME '04:00', 2.0, 'peak'),
  ('deepseek', 'deepseek-v4-pro',   TIME '06:00', TIME '10:00', 2.0, 'peak')
ON CONFLICT (provider, COALESCE(model, '*'), start_utc, end_utc) DO NOTHING;

-- (3) Il lucchetto sui prezzi curati. Protegge le righe qui sotto dal giorno in
-- cui il sync LiteLLM matchasse deepseek/deepseek-v4-* (oggi non le matcha):
-- l'upsert del sync (SQL_UPSERT_VOCE_CATALOG, mcp-core/src/models.rs) conserva
-- i 4 campi prezzo dove price_locked e' true, con lo stesso pattern CASE WHEN
-- di capability_source='manual' per i flag di capability (ADR 0024).
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS price_locked boolean NOT NULL DEFAULT false;

COMMENT ON COLUMN ai_price_catalog.price_locked IS
  'true = i 4 campi prezzo sono curati da migrazione: il sync LiteLLM non li sovrascrive (CASE WHEN in SQL_UPSERT_VOCE_CATALOG). Mig 0715.';

-- (4) I prezzi BASE deepseek v4 al listino OFF-PEAK ufficiale (il peak lo
-- produce la finestra, non una seconda riga). cache_creation = 0 e' un prezzo
-- REALE: il context caching di deepseek e' automatico e la scrittura non si
-- paga — l'ignoto sarebbe NULL, e qui ignoto non e'.
--
-- UPSERT e non UPDATE: sul DB vivo le due righe esistono (arrivate dal
-- discovery) e ricevono i prezzi nuovi + il lucchetto; su un DB migrato da
-- zero nascono qui, curate e is_enabled=false (il probe-before-enable della
-- 0629 governa l'abilitazione, non questa migrazione). Senza il ramo INSERT
-- l'UPDATE sarebbe un no-op su ogni DB nuovo e i test sullo schema reale non
-- avrebbero il loro oggetto (regola O). Il DO UPDATE tocca SOLO prezzi e
-- lucchetto: is_enabled, capability e finestra di contesto restano del DB vivo.
-- pricing_state non si tocca: il trigger della 0583 promuove 'unknown' ->
-- 'priced' sui costi > 0 e non degrada mai.
INSERT INTO ai_price_catalog
  (provider, model, display_name,
   input_cost_per_million_tokens, output_cost_per_million_tokens,
   cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens,
   currency, is_enabled, price_locked, effective_from)
VALUES
  ('deepseek', 'deepseek-v4-flash', 'DeepSeek V4 Flash',
   0.22, 0.66, 0.007, 0, 'USD', false, true, now()),
  ('deepseek', 'deepseek-v4-pro', 'DeepSeek V4 Pro',
   0.66, 1.98, 0.022, 0, 'USD', false, true, now())
ON CONFLICT (provider, model) DO UPDATE SET
  input_cost_per_million_tokens          = EXCLUDED.input_cost_per_million_tokens,
  output_cost_per_million_tokens         = EXCLUDED.output_cost_per_million_tokens,
  cache_read_cost_per_million_tokens     = EXCLUDED.cache_read_cost_per_million_tokens,
  cache_creation_cost_per_million_tokens = EXCLUDED.cache_creation_cost_per_million_tokens,
  price_locked                           = true,
  updated_at                             = now();
