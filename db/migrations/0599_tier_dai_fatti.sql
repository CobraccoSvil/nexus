-- 0599: il performance_tier smette di essere indovinato dal NOME.
--
-- IL DIFETTO (misurato il 15/07). `infer_tier_from_name` deduce la fascia dal
-- nome del modello con euristiche per-provider scritte a mano. Conseguenze:
--   - `_ => "medium"` cattura groq, openrouter, perplexity e OGNI provider
--     futuro: non potranno MAI essere heavy, per costruzione;
--   - ogni modello nuovo cade in un ramo `else` e viene declassato IN SILENZIO:
--     `gpt-5.6-sol` (il piu' capace del parco, agentic_index 54) e' 'high'
--     perche' nessuno ha aggiunto "5.6" alla lista di if; `claude-sonnet-5`
--     (46.7, batte OGNI heavy) e' 'medium' perche' "non contiene opus";
--   - 68 coppie invertite sul parco reale: il tier NON ordina i modelli come la
--     loro capacita'.
-- Il tier 'heavy' e' cosi' diventato un FOSSILE: contiene cio' che l'euristica
-- sapeva il giorno in cui fu scritta. Il 15/07 openai+anthropic sono finiti
-- insieme in cooldown billing e il consiglio e' morto con 19 modelli sani a un
-- gradino di distanza.
--
-- LA CURA: il tier viene dai FATTI, in ordine di autorita'
--   1. manual      -> curatela umana, vince sempre
--   2. measured    -> banda certificata dalla batteria di probe REALI
--   3. facts_prior -> derivato dai fatti gia' nel catalog (prezzo, finestra,
--                     capability PROVATE)
--   4. (nessuna)   -> NULL. Il nome non compare in nessun ramo.
--
-- DROP DEFAULT 'medium' (regola G). Quel default E' il fallback magico da cui
-- nasce `_ => "medium"`: finche' la colonna ha un default, "non lo so" e "e'
-- medium" restano indistinguibili. Con NULL il sistema puo' DIRE che non lo sa.
--
-- Il tier NULL e' SICURO, e non e' nemmeno un cambiamento: con
-- enforce_routing_gate=true (mig 0595) un modello unqualified e' GIA' fuori dal
-- pool agentico. In piu' `agentic_failover_candidates` non filtra per tier
-- (AnyTier), quindi un modello a tier NULL resta candidato di ULTIMA ISTANZA del
-- failover ma non e' mai una scelta primaria: esattamente la semantica giusta.
-- L'unita' che deve fallire visibilmente non e' il modello, e' il POOL — e per
-- quello c'e' gia' il WARN sul pool qualificato vuoto piu' la degradazione, che
-- garantisce che finche' UN tier ha un modello sano il pool non si svuota.

ALTER TABLE ai_price_catalog ALTER COLUMN performance_tier DROP DEFAULT;
ALTER TABLE ai_price_catalog ALTER COLUMN performance_tier DROP NOT NULL;
-- Il CHECK esistente resta valido con NULL: in SQL `NULL = ANY(...)` e' UNKNOWN,
-- e un CHECK passa se non e' FALSE. Non va toccato.

-- Provenienza del tier. NULL = nessuna fonte si e' espressa (il modello non ha
-- tier). E' una colonna NUOVA e non un riuso di `capability_source` perche'
-- quella cumula gia' 3 concern (provenienza flag, gate chk_qualified_implies_probe,
-- guard dell'UPDATE del tier) e ha una regressione latente: model_catalog_sync e
-- models.rs aggiornano il tier solo WHERE capability_source='auto', mentre la
-- batteria scrive 'probe' -> appena un modello si qualifica il suo tier si
-- CONGELA per sempre al valore indovinato dal nome. Sovraccaricare la quarta
-- semantica sulla stessa colonna sarebbe una violazione di regola L nel punto
-- esatto che stiamo curando.
ALTER TABLE ai_price_catalog
  ADD COLUMN IF NOT EXISTS tier_source TEXT
    CHECK (tier_source IN ('facts_prior', 'measured', 'manual'));

COMMENT ON COLUMN ai_price_catalog.tier_source IS
  'Provenienza di performance_tier: manual (curatela) > measured (banda '
  'certificata dalla batteria) > facts_prior (derivato dai fatti del catalog). '
  'NULL = nessuna fonte, il tier e'' ignoto. Il NOME del modello non e'' una fonte.';

-- I 52 tier scritti a mano dall'admin sono curatela: vanno preservati e non
-- devono essere sovrascritti da una derivazione automatica.
UPDATE ai_price_catalog SET tier_source = 'manual'
 WHERE capability_source = 'manual' AND performance_tier IS NOT NULL;

-- Tutto il resto e' stato indovinato dal nome: NON lo marchiamo come derivato
-- dai fatti (sarebbe una bugia). Resta col tier attuale e tier_source NULL
-- finche' derive_tier_prior/measured non si esprime. Il routing intanto funziona
-- come oggi: nessun cambio di comportamento all'applicazione della migrazione.

-- ── La banda che un profilo di probe CERTIFICA ──────────────────────────────
-- Il profilo dice "questo modello sa fare X"; la banda e' la CONSEGUENZA.
-- NULL = il profilo non certifica un tier (serve solo a qualificare).
ALTER TABLE ai_model_probe_profile
  ADD COLUMN IF NOT EXISTS certifies_tier TEXT
    CHECK (certifies_tier IN ('light', 'medium', 'high', 'heavy', 'frontier'));

COMMENT ON COLUMN ai_model_probe_profile.certifies_tier IS
  'La banda che questo profilo certifica se superato. Criterion-referenced: un '
  '''heavy'' ha DIMOSTRATO qualcosa che un ''medium'' non ha fatto, e l''evidenza '
  'sta in ai_model_probe_evidence. NULL = il profilo non certifica un tier.';

-- Le bande dei profili esistenti (suite v2).
UPDATE ai_model_probe_profile SET certifies_tier = 'light'  WHERE profile_key = 'chat_smoke';
UPDATE ai_model_probe_profile SET certifies_tier = 'medium' WHERE profile_key = 'agentic_real';

-- Il vocabolario dei `kind` e' un contratto dello schema: i 3 nuovi profili
-- introducono 3 tipi di prova che il CHECK non conosce ancora. Il CHECK viene
-- esteso QUI e non allargato a piacere nel codice: e' la stessa ragione per cui
-- il tier ha un CHECK: un kind sconosciuto deve essere rifiutato dal DB, non
-- ignorato in silenzio da un `match` con un ramo `_`.
ALTER TABLE ai_model_probe_profile DROP CONSTRAINT IF EXISTS ai_model_probe_profile_kind_check;
ALTER TABLE ai_model_probe_profile ADD CONSTRAINT ai_model_probe_profile_kind_check
  CHECK (kind IN ('chat', 'tool_minimal', 'tool_realistic', 'thinking_matrix',
                  'tool_chain', 'tool_recovery', 'long_context'));

-- ── I 3 profili GRADUATI: le bande alte si guadagnano ───────────────────────
-- Ogni banda chiede una capacita' che la precedente non ha dimostrato. Il
-- checker e' DETERMINISTICO (pass_predicate + verifica programmatica), mai un
-- giudizio sul testo (regola M).
--
-- max_tokens contenuti: il design misura lo sweep completo a ~$80-95 e i 5
-- openai/*-pro da soli farebbero $17 dei $37 della batteria attuale. Il cap e'
-- una mitigazione dai dati, non dal buonsenso.
INSERT INTO ai_model_probe_profile
  (profile_key, suite_version, ord, kind, is_blocking, applies_when, grants,
   payload, pass_predicate, enabled, certifies_tier)
VALUES
  -- HIGH: concatenare tool con DIPENDENZA. Non basta chiamarne 3: l'input della
  -- seconda deve venire dall'output della prima. E' la differenza fra "sa usare
  -- i tool" (medium) e "sa costruire un piano" (high).
  ('agentic_chain', 2, 50, 'tool_chain', false, NULL, '[]'::jsonb,
   '{"repeat": 4, "timeout_s": 120, "max_tokens": 2048,
     "tool_names": ["read_file", "list_files", "search_in_files", "write_file", "run_command"],
     "history_chars": 8000, "system_template_key": "system.nexus_base"}'::jsonb,
   '{"min_chained_calls": 3, "max_latency_ms": 120000,
     "promote_min_passes": 3, "hold_min_passes": 2}'::jsonb,
   true, 'high'),

  -- HEAVY: RECUPERARE da un errore strutturato. Il tool ritorna is_error; il
  -- modello deve cambiare strategia senza ripetere l'azione fallita. E' la
  -- capacita' che distingue un agente da un esecutore — ed e' esattamente cio'
  -- che serviva alle figure del consiglio.
  ('agentic_recovery', 2, 60, 'tool_recovery', false, NULL, '[]'::jsonb,
   '{"repeat": 4, "timeout_s": 120, "max_tokens": 2048,
     "tool_names": ["read_file", "list_files", "run_command"],
     "history_chars": 8000, "system_template_key": "system.nexus_base"}'::jsonb,
   '{"requires_recovery": true, "forbids_repeat_of_failed": true,
     "max_latency_ms": 120000, "promote_min_passes": 3, "hold_min_passes": 2}'::jsonb,
   true, 'heavy'),

  -- FRONTIER: reggere il contesto lungo con retrieval VERIFICABILE. Un fatto
  -- piantato a meta' di 100k di history: il modello deve ritrovarlo. Non e'
  -- "quanto e' grande la finestra dichiarata" (quello e' un numero del
  -- fornitore): e' se la USA davvero.
  ('agentic_longctx', 2, 70, 'long_context', false, NULL, '[]'::jsonb,
   '{"repeat": 4, "timeout_s": 180, "max_tokens": 1024,
     "tool_names": ["read_file", "search_in_files"],
     "history_chars": 100000, "needle_required": true,
     "system_template_key": "system.nexus_base"}'::jsonb,
   '{"requires_needle": true, "max_latency_ms": 180000,
     "promote_min_passes": 3, "hold_min_passes": 2}'::jsonb,
   true, 'frontier')
ON CONFLICT (profile_key) DO UPDATE
  SET certifies_tier = EXCLUDED.certifies_tier,
      suite_version  = EXCLUDED.suite_version,
      payload        = EXCLUDED.payload,
      pass_predicate = EXCLUDED.pass_predicate,
      kind           = EXCLUDED.kind,
      ord            = EXCLUDED.ord,
      enabled        = EXCLUDED.enabled;

-- ── Soglie del facts_prior (regola G: nel DB, non nel codice) ───────────────
-- Il prior si esprime SOLO dai fatti dichiarati dal fornitore + le capability
-- gia' PROVATE dalla batteria. E' un ripiego onesto in attesa della misura, non
-- una verita': appena una banda e' certificata, `measured` lo sostituisce.
INSERT INTO settings (key, value, category, description) VALUES
  ('catalog.tier_prior.frontier_min_input_cost', '8.0', 'routing',
   'Prezzo input $/M oltre il quale il fornitore stesso posiziona il modello in cima al listino. Soglia del facts_prior, sostituita dalla misura appena la batteria certifica una banda.'),
  ('catalog.tier_prior.heavy_min_input_cost', '2.0', 'routing',
   'Prezzo input $/M della fascia alta. Vedi catalog.tier_prior.frontier_min_input_cost.'),
  ('catalog.tier_prior.high_min_input_cost', '0.5', 'routing',
   'Prezzo input $/M della fascia medio-alta.'),
  ('catalog.tier_prior.long_context_tokens', '200000', 'routing',
   'Finestra oltre la quale il modello e'' considerato long-context dal prior (alza di UN gradino, mai oltre heavy: la finestra dichiarata non prova la capacita'' agentica).'),
  ('catalog.tier_prior.enabled', 'true', 'routing',
   'Se il facts_prior puo'' esprimersi. A false restano solo manual e measured: un modello mai misurato ha tier NULL.')
ON CONFLICT (key) DO NOTHING;
