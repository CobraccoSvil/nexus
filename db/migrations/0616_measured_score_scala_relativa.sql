-- 0615: lo SCORE MISURATO della batteria + bande measured RELATIVE (Fase B del
-- piano "scala relativa") + bump della suite a 5.
--
-- La scala measured aveva un solo gradino utile: recovery/heavy irraggiungibile
-- (0 pass su tutti i modelli) e chain/high satura (80% pass, incluso un 8B).
-- La batteria ora produce UNO SCORE 0-100 (derive_measured_score, punto unico):
-- media pesata sui tentativi CONCLUSIVI di 5 componenti (catena CONTINUA
-- min(links/5,1), recovery rate, pass rate di real/latent/longctx), meno un
-- malus per ripetizioni/sintassi rotta (cap -5). Le bande measured si derivano
-- dallo score con la STESSA scala relativa della mig 0615 (tier_from_leader),
-- ancorata al leader MISURATO a suite corrente, in un pass di ri-ancoraggio a
-- fine giro (riancora_bande_measured).
--
-- SEMANTICA NUOVA, dichiarata: il tier measured di un modello puo' muoversi
-- senza che quel modello sia stato ri-provato — si e' mosso il leader.

-- (1) Le colonne dello score. measured_score_suite e' OBBLIGATORIA nel
--     confronto: score di suite diverse non sono confrontabili, e il leader si
--     calcola solo fra righe alla suite corrente.
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS measured_score DOUBLE PRECISION,
  ADD COLUMN IF NOT EXISTS measured_score_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS measured_score_suite INT;

COMMENT ON COLUMN ai_price_catalog.measured_score IS
  'Score 0-100 della batteria (derive_measured_score, mig 0616). Scritto SOLO '
  'da model_service::apply_measured_score nella transazione del verdetto '
  'Qualified. NULL = mai misurato con la formula.';
COMMENT ON COLUMN ai_price_catalog.measured_score_suite IS
  'La suite dei profili con cui lo score e'' stato misurato: score di suite '
  'diverse non sono confrontabili, il leader measured si calcola solo a suite '
  'corrente.';

-- (2) I pesi della formula (regola G: nel DB, mai hardcoded). w_recovery=30 e''
--     deliberato: un gemello del leader senza recovery resta FUORI da frontier
--     senza dipendere dal malus. Un profilo non applicabile per struttura
--     (applies_when) vale 0 punti SENZA rinormalizzare.
INSERT INTO settings (key, value, category, description) VALUES
  ('catalog.measured_score.w_chain', '25', 'routing',
   'Peso della componente catena (media CONTINUA di min(chained_links/5, 1) sui tentativi conclusivi di tool_chain).'),
  ('catalog.measured_score.w_recovery', '30', 'routing',
   'Peso del recovery rate (fatto ''recovered'' sui conclusivi di tool_recovery). 30 e'' deliberato: senza recovery non si arriva a frontier.'),
  ('catalog.measured_score.w_real', '15', 'routing',
   'Peso del pass rate conclusivo di tool_realistic.'),
  ('catalog.measured_score.w_latent', '15', 'routing',
   'Peso del pass rate conclusivo di latent_state.'),
  ('catalog.measured_score.w_longctx', '15', 'routing',
   'Peso del pass rate conclusivo di long_context. Profilo assente dalla suite o non applicabile = 0 punti, senza rinormalizzare.')
ON CONFLICT (key) DO NOTHING;

-- (3) Le bande measured: ancora + isteresi + popolazione minima.
INSERT INTO settings (key, value, category, description) VALUES
  ('catalog.measured_band.anchor', '', 'routing',
   'L''ANCORA delle bande measured: lo score del leader misurato a suite corrente. Aggiornata dal pass di ri-ancoraggio con la deadband; vuota = mai ancorata.'),
  ('catalog.measured_band.anchor_model', '', 'routing',
   'Il modello leader dell''ancora measured.'),
  ('catalog.measured_band.anchor_at', '', 'routing',
   'Quando l''ancora measured e'' stata fissata l''ultima volta.'),
  ('catalog.measured_band.anchor_deadband_pct', '0.03', 'routing',
   'Deadband relativa dell''ancora measured (anti-flapping), come catalog.tier_relative.anchor_deadband_pct per il prior.'),
  ('catalog.measured_band.demote_margin', '3', 'routing',
   'Isteresi di demozione in PUNTI score: si sale superando la soglia della banda, si scende solo sotto (soglia - margine). Solo per bande GUADAGNATE dalla batteria (tier_source=measured).'),
  ('catalog.measured_band.min_population', '3', 'routing',
   'Sotto questo numero di modelli misurati a suite corrente le bande measured NON si applicano (il tier resta synced): senza, il primo misurato di ogni suite sarebbe frontier per definizione.')
ON CONFLICT (key) DO NOTHING;

-- (4) Bump della suite a 5 per i profili ENABLED: elimina i vintage misti del
--     recovery (3 versioni del test dentro la suite 4) e ripopola il parco
--     misurato sotto il test corrente — necessario comunque, perche' gli score
--     richiedono evidenza omogenea. NESSUN backfill dello score da verdetti
--     storici: measured_score parte NULL e lo riempiono i giri.
UPDATE ai_model_probe_profile SET suite_version = 5 WHERE enabled = TRUE;
