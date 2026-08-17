-- 0731_google_thinking_dialect.sql
--
-- Il dialetto con cui un modello Gemini dichiara il proprio pensiero (fase 4,
-- lotto 6).
--
-- I due dialetti non sono due modi di scrivere la stessa cosa, e L'ASIMMETRIA E'
-- A SENSO UNICO. MISURATO il 17/08/2026 sull'API Vertex reale (backend di
-- produzione, location `global`), turno con una tool definition e tetto 1024:
--
--   gemini-2.5-flash   + thinkingBudget 0   -> HTTP 200, nessun pensiero
--   gemini-2.5-flash   + thinkingLevel low  -> HTTP 400 INVALID_ARGUMENT
--                                              «thinking_level is not supported
--                                              by this model»
--   gemini-3-flash-preview  + thinkingLevel minimal|low|high -> HTTP 200
--   gemini-3.1-pro-preview  + thinkingLevel low -> HTTP 200, 126 tok di pensiero
--   gemini-3.5-flash        + thinkingLevel low -> HTTP 200,  91 tok di pensiero
--   gemini-3.1-flash-lite   + thinkingLevel low -> HTTP 200, 133 tok di pensiero
--
-- IL 400 DELLA 0578 NON E' PIU' RIPRODUCIBILE: quella migrazione dichiarava che
-- i 3.x rifiutano `thinkingBudget=0`, e oggi tutti e quattro i modelli provati
-- lo ACCETTANO (HTTP 200, pensiero soppresso). Il verso opposto invece e' vivo,
-- ed e' quello che questa colonna deve proteggere: marcare un 2.5 come `level`
-- e' un 400 su OGNI chiamata a quel modello. Percio' la precisione del seed qui
-- sotto non e' cosmetica, ed e' coperta da un test che legge questo file.
--
-- IL VALORE non e' quindi evitare un errore, e' emettere il dialetto NATIVO:
-- sul turno con tool il livello da' un ragionamento contenuto e MISURATO (~90-130
-- token) al posto di un budget in token che `build_generation_config` deve
-- compensare alzando `maxOutputTokens`. Meno maneggio del tetto, e un controllo
-- graduato che lo zero non offre.
--
-- PERCHE' UNA COLONNA DEDICATA e non `agentic_thinking_policy`, che pure ha nel
-- proprio vocabolario il valore giusto: quella colonna la RISCRIVE il catalog
-- sync da un'euristica sul NOME (stessa ragione documentata nella mig 0705 per
-- kimi). Non e' un timore teorico ed e' MISURATO sul catalogo vivo: delle 17
-- righe `gemini-3%` NON UNA porta oggi il valore `'native'` che la 0578 vi
-- scrisse — 6 sono tornate a `disable_for_tools` e 11 sono `none`, con
-- `capability_source='probe'` su tutte le abilitate. Che il sync le abbia
-- riscritte o che i modelli siano stati scoperti dopo, la conclusione e' la
-- stessa: quel rimedio non e' in vigore da nessuna parte, e nessuno se n'e'
-- accorto perche' il 400 che doveva evitare nel frattempo era sparito. Questa
-- colonna il sync non la tocca.
--
-- NULL = NON DICHIARATO, e vale il comportamento di ieri (dialetto budget). E'
-- la direzione sicura in entrambi i sensi: sui 2.5 e' la forma giusta, e sui 3.x
-- e' una forma che oggi risponde 200. La direzione opposta — dedurre `level` da
-- un nome — e' esattamente l'euristica che questa colonna esiste per evitare, e
-- su un 2.5 sarebbe il 400 misurato qui sopra.
--
-- LIMITE DICHIARATO: i modelli arrivano anche dal discovery a runtime, che
-- inserisce in `ai_price_catalog` senza sapere nulla di questo campo. Un
-- gemini-3.x scoperto dopo questa migrazione nasce percio' NULL, cioe' col
-- dialetto storico. E' lo stesso limite di `nexus_provider_capabilities`: la
-- dichiarazione e' umana, non e' un'osservazione sincronizzabile. Il degrado
-- resta quello sicuro (il budget bounded che la 0578 gia' produce via
-- `thinking.mandatory`), non un 400.
--
-- IL SEED ESCLUDE le varianti non testuali della stessa famiglia (image, tts,
-- live, audio, embedding): non passano da `generateContent` e non accettano
-- alcun `thinkingConfig`, in nessuno dei due dialetti. Stessa distinzione che
-- la 0578 faceva col filtro sulla policy, che qui non e' usabile — e' proprio
-- il valore che non c'e' piu'.
--
-- ROLLBACK: `UPDATE ai_price_catalog SET thinking_dialect = NULL WHERE provider
-- = 'google'` — comportamento storico integrale entro la TTL di 60s, senza
-- riavvio e senza revert di codice.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS thinking_dialect TEXT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_thinking_dialect'
    ) THEN
        ALTER TABLE ai_price_catalog
            ADD CONSTRAINT chk_thinking_dialect
            CHECK (thinking_dialect IS NULL OR thinking_dialect IN ('budget', 'level'));
    END IF;
END $$;

COMMENT ON COLUMN ai_price_catalog.thinking_dialect IS
    'Dialetto del thinkingConfig di questo modello: budget (thinkingBudget in token, famiglia 2.5, lo zero SPEGNE) | level (thinkingLevel, famiglia 3.x, nessuno spegnimento e lo zero e'' HTTP 400). NULL = non dichiarato -> comportamento storico (budget). Provenienza doc del fornitore; il catalog sync NON la riscrive, a differenza di agentic_thinking_policy.';

UPDATE ai_price_catalog
   SET thinking_dialect = 'level',
       updated_at       = NOW()
 WHERE provider = 'google'
   AND model LIKE 'gemini-3%'
   AND model NOT LIKE '%image%'
   AND model NOT LIKE '%tts%'
   AND model NOT LIKE '%live%'
   AND model NOT LIKE '%audio%'
   AND model NOT LIKE '%embedding%';

UPDATE ai_price_catalog
   SET thinking_dialect = 'budget',
       updated_at       = NOW()
 WHERE provider = 'google'
   AND model LIKE 'gemini-2.5%'
   AND model NOT LIKE '%image%'
   AND model NOT LIKE '%tts%'
   AND model NOT LIKE '%live%'
   AND model NOT LIKE '%audio%'
   AND model NOT LIKE '%embedding%';

-- Il livello di default sui turni dei 3.x. 'low' e non 'high': il livello
-- governa quanto il modello pensa PRIMA di rispondere, e su un turno agentico
-- con tool cio' che serve e' l'azione — e' la stessa posizione che il gateway
-- prende gia' su kimi e deepseek, dove il pensiero si spegne quando ci sono
-- tool. Qui non si puo' spegnere, quindi si sceglie il minimo utile.
--
-- Vocabolario chiuso minimal|low|medium|high, validato nel driver: un valore
-- fuori vocabolario non parte (sarebbe un 400 su OGNI chiamata) e ricade su
-- 'low' con un WARN.
INSERT INTO settings (key, value, category, description) VALUES
(
    'providers.google.thinking_level', 'low', 'providers',
    'Livello di pensiero emesso come thinkingConfig.thinkingLevel sui modelli Gemini con thinking_dialect=''level'' (famiglia 3.x). Vocabolario chiuso: minimal|low|medium|high. Un valore fuori vocabolario ricade su ''low'' con WARN. Non ha effetto sui modelli a dialetto budget (2.5), che continuano a usare providers.google.thinking_budget. Cache 60s lato driver.'
)
ON CONFLICT (key) DO NOTHING;
