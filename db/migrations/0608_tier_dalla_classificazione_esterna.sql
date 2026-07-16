-- 0608: il tier viene dalla classificazione esterna, non dal prezzo.
--
-- Contesto (indagine 2026-07-16): la classificazione dei modelli era sparsa su
-- 4 fonti stratificate (nome -> prezzo -> indice -> batteria -> curatela) con
-- dati legacy mai normalizzati. Effetti misurati sul catalogo vivo:
--   - il filtro capability testuale `capabilities @> '["reasoning"]'`
--     selezionava groq/gpt-oss-20b (agentic_index 3.1, il peggiore del parco)
--     ed escludeva claude-opus-4-7 (44.4, il formato legacy a OGGETTO
--     {"thinking": true} delle mig 0170/0240 non matcha la stringa);
--   - il prior sul prezzo produceva tier opposti per lo stesso modello
--     (mistral-medium-2505 -> light, mistral-medium-2604 -> heavy);
--   - whisper-1/tts-1/gpt-image-1 risultavano tool-capable con capability
--     "reasoning": modelli audio/immagine candidabili come consiglieri.
--
-- Decisione (piramide, dall'alto): manual > measured (batteria) >
-- synced (agentic_index da OpenRouter/Artificial Analysis) > NULL.
-- Il PREZZO esce dalla classificazione; il NOME era gia' uscito (mig 0599).

-- (1) `capabilities` e' SEMPRE un array JSON di stringhe. Il default storico
--     '{}' (mig 0170) e gli elementi-oggetto {"thinking": true} sono residui
--     legacy: il thinking vive nelle colonne canoniche uses_thinking_mode /
--     agentic_thinking_policy (ADR 0024/0025), non nel jsonb.
--
--     Il trigger `trg_invalidate_qualification` (mig 0591) va disattivato per
--     questi due UPDATE, ed e' l'unico punto del file in cui serve. Il trigger
--     squalifica un modello quando le capability DICHIARATE cambiano — una
--     difesa giusta, ma qui sarebbe un falso positivo: cambia la
--     RAPPRESENTAZIONE, non cio' che il modello dichiara di saper fare (il
--     thinking sta nelle colonne canoniche, come dice il commento sopra).
--     Senza questo, verificato eseguendo la migrazione in BEGIN/ROLLBACK sul
--     DB reale, i tre Claude piu' capaci (opus-4-6, opus-4-7, sonnet-4-6)
--     uscivano 'unqualified' con reason 'declared_capabilities_changed' e
--     lasciavano il pool routabile: la stessa migrazione che al punto (3)
--     promuove opus-4-7 a 'heavy' lo toglieva dal pool venti righe prima.
--     Il DISABLE vale per questa transazione (sqlx esegue ogni migrazione in
--     una transazione), non lascia il trigger spento.
ALTER TABLE ai_price_catalog DISABLE TRIGGER trg_invalidate_qualification;

UPDATE ai_price_catalog
   SET capabilities = '[]'::jsonb, updated_at = NOW()
 WHERE jsonb_typeof(capabilities) <> 'array';

UPDATE ai_price_catalog
   SET capabilities = COALESCE(
         (SELECT jsonb_agg(elem)
            FROM jsonb_array_elements(capabilities) AS elem
           WHERE jsonb_typeof(elem) = 'string'),
         '[]'::jsonb),
       updated_at = NOW()
 WHERE jsonb_typeof(capabilities) = 'array'
   AND EXISTS (SELECT 1 FROM jsonb_array_elements(capabilities) AS e
                WHERE jsonb_typeof(e) <> 'string');

ALTER TABLE ai_price_catalog ENABLE TRIGGER trg_invalidate_qualification;

-- (2) Un modello MEDIA (image/audio/video, criterio STRUTTURALE sui flag mig
--     0478) non e' un modello agentico di testo: niente tool_use, niente
--     capability testuali, niente stato "qualified" ereditato da una batteria
--     che non lo riguarda. Copre whisper-1, tts-1, tts-1-hd, gpt-image-1,
--     gpt-4o-transcribe e ogni futuro caso gia' in tabella.
UPDATE ai_price_catalog
   SET supports_tool_use = false,
       capabilities = '[]'::jsonb,
       qualified_capabilities = '[]'::jsonb,
       qualification_state = 'unqualified',
       qualification_reason = 'media_model_not_agentic (mig 0608)',
       updated_at = NOW()
 WHERE (supports_image_gen OR supports_audio_in OR supports_audio_out
        OR supports_video_gen)
   AND (supports_tool_use
        OR capabilities <> '[]'::jsonb
        OR qualification_state = 'qualified');

-- (3) IL DIFETTO ORIGINALE, TRAVESTITO: nessuno dei 49 `tier_source='manual'`
--     e' curatela umana. La mig 0599 (righe 63-64) li marco' cosi' deducendo
--     "il TIER e' curato" da "le CAPABILITY sono curate":
--       UPDATE ... SET tier_source='manual' WHERE capability_source='manual'
--     ma sono due concern diversi. Quelle righe avevano vision/media/tool_use
--     curati a mano (mig 0318, 0479-0482, 0566, 0582); il loro TIER veniva da
--     `infer_tier_from_name`. Le UNICHE due migrazioni che abbiano mai scritto
--     un tier esplicito lo fanno per pattern sul NOME e lo dichiarano:
--       0354 "qui usiamo solo il match per nome, sicuro" (LIKE 'ministral-%',
--            '%small%', '%nemo%')
--       0488 "l'euristica ora produce gli stessi [valori]" (LIKE '%coder%', '%pro%')
--
--     Effetto misurato: `manual` batte indice e batteria, quindi il tier dedotto
--     dal nome e' diventato ETERNO. deepseek-r1 e' 'medium' perche' si chiama
--     "r1" (indice reale 3.1); gpt-oss-120b e' 'high' perche' si chiama "120b"
--     (13.2); claude-opus-4-7 e' 'medium' con 44.4 — il piu' capace del parco,
--     declassato per sempre. La 0599 dichiarava morto "il tier dal nome": era
--     vivo, protetto dalla colonna che doveva certificarne la provenienza.
--
--     Il tier NON viene azzerato (niente buco nel routing): resta il valore
--     attuale con `tier_source = NULL`, che nella semantica della 0599 significa
--     gia' "nessuna fonte si e' espressa" — la verita'. Da qui indice e batteria
--     possono finalmente correggerlo (entrambi scrivono su NULL/'synced').
--     La curatela VERA nascera' dall'UI di amministrazione, dove un umano
--     decide davvero: quella sara' 'manual' a ragione.
UPDATE ai_price_catalog
   SET tier_source = NULL, updated_at = NOW()
 WHERE tier_source = 'manual';

-- (4) Vocabolario canonico delle fonti del tier (regola N): 'facts_prior'
--     diventa 'synced' — il nome dice DA DOVE viene il valore (il sync
--     dell'indice esterno), non che cosa era ("un prior sui fatti" copriva
--     due fonti diverse, prezzo e indice; il prezzo non esiste piu').
--
--     Ma la rinomina NON e' un UPDATE cieco: proprio perche' 'facts_prior'
--     copriva due fonti, solo le righe con un `agentic_index` vengono davvero
--     dal sync. Marcare 'synced' anche le altre direbbe "l'ha classificato il
--     servizio esterno" di un tier dedotto dal PREZZO — la stessa bugia sulla
--     provenienza che il punto (3) rimuove. Chi non ha indice tiene il tier
--     (niente buchi nel routing) con `tier_source = NULL`: "non so da dove
--     viene", finche' l'indice arriva o la batteria misura.
--
-- IF EXISTS: il CHECK nasce come clausola inline di un `ADD COLUMN IF NOT
-- EXISTS` (0599:53), quindi su un DB dove la colonna esisteva gia' Postgres
-- salta l'intero sottocomando e il vincolo non c'e'. Senza IF EXISTS la
-- migrazione abortirebbe qui e rollbackerebbe anche il DROP COLUMN sotto.
ALTER TABLE ai_price_catalog DROP CONSTRAINT IF EXISTS ai_price_catalog_tier_source_check;

UPDATE ai_price_catalog SET tier_source = 'synced', updated_at = NOW()
 WHERE tier_source = 'facts_prior' AND agentic_index IS NOT NULL;

UPDATE ai_price_catalog SET tier_source = NULL, updated_at = NOW()
 WHERE tier_source = 'facts_prior';

ALTER TABLE ai_price_catalog
  ADD CONSTRAINT ai_price_catalog_tier_source_check
  CHECK (tier_source IN ('synced', 'measured', 'manual'));

-- (5) La colonna is_thinking (mig 0275) e' morta a runtime: nessuna SELECT
--     decisionale la legge. Il suo concern ("escludi i reasoning-only dal
--     routing agentico") e' espresso da agentic_thinking_policy (ADR 0025,
--     mig 0319) che la SUPERSEDE dichiaratamente. Le due colonne si
--     contraddicevano perfino: la o-series era is_thinking=true ("escludi")
--     ma policy='native' ("dentro, con tool nativi").
DROP INDEX IF EXISTS idx_ai_price_catalog_is_thinking;
ALTER TABLE ai_price_catalog DROP COLUMN IF EXISTS is_thinking;

-- (6) Il prezzo esce dalla classificazione: via le soglie del prior sul
--     costo e il gradino long-context (il contesto lungo lo CERTIFICA la
--     batteria col profilo agentic_longctx, mig 0599). Restano le soglie
--     dell'indice (agentic_index_*_min, mig 0600).
DELETE FROM settings WHERE key IN (
  'catalog.tier_prior.frontier_min_input_cost',
  'catalog.tier_prior.heavy_min_input_cost',
  'catalog.tier_prior.high_min_input_cost',
  'catalog.tier_prior.long_context_tokens'
);

-- (7) La capability 'reasoning' esce dai requisiti dei purpose: era un
--     confronto testuale su un campo auto-dichiarato (regola M) che di fatto
--     restringeva il consiglio a 2 provider. La capacita' la esprime il TIER
--     (+ pavimento, fase successiva); la capability resta per i concern
--     binari veri (vision, media, tool_use — quest'ultimo su colonna).
UPDATE nexus_purpose_model
   SET required_capability = NULL
 WHERE required_capability = 'reasoning';
