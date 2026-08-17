-- 0732_kimi_reasoning_effort.sql
--
-- `reasoning_effort` sul dialetto Kimi (fase 4, lotto 7).
--
-- Il campo esisteva gia' nel corpo OpenAI-compat, emesso pero' dal solo dialetto
-- o-series: il commento della variante Kimi dichiarava esplicitamente perche' non
-- ci fosse un ramo — mancavano il PRODUTTORE e il DATO. Questa migrazione
-- fornisce entrambi, e li fornisce SPENTI.
--
-- MISURATO il 17/08/2026 sull'API Moonshot reale (stesso prompt, stesso tetto di
-- 2048 token, una chiamata per riga):
--
--   kimi-k3    senza il campo   -> 200,  80 token di completion, 191 char di pensiero
--   kimi-k3    effort=low       -> 200,  24 token,                12 char
--   kimi-k3    effort=high      -> 200,  96 token,               244 char
--   kimi-k2.6  senza il campo   -> 200, 156 token,               418 char
--   kimi-k2.6  effort=low       -> 200,  70 token,               189 char
--   kimi-k2.6  effort=high      -> 200, 134 token,               407 char
--
-- L'effetto e' reale e va nella direzione utile: su k3 `low` porta il completion
-- da 80 a 24 token (-70%), su k2.6 da 156 a 70 (-55%). Su un fornitore il cui
-- output e' la voce cara del listino, e' la stessa posta della corsia
-- differibile.
--
-- IL CAMPO NON E' DEL SOLO k3, e il design lo dava per tale: MISURATI 200 anche
-- su `kimi-k2.7-code` e `kimi-k2.7-code-highspeed`. Tutti e quattro i modelli a
-- catalogo lo accettano, e la colonna li dichiara tutti e quattro — si semina
-- cio' che si e' misurato, non cio' che si era supposto.
--
-- L'API NON VALIDA IL VALORE, ed e' la ragione per cui la colonna serve lo
-- stesso: `reasoning_effort: "assurdo"` su kimi-k3 risponde 200 (117 char di
-- pensiero, in mezzo fra low e high). Non esiste quindi un 400 che ci avverta di
-- aver mandato qualcosa di insensato — ne' un valore inventato, ne' il campo a un
-- modello che non lo interpreta. La colonna e il vocabolario chiuso nel driver
-- non evitano un errore: evitano un EFFETTO CHE NESSUNO HA DICHIARATO, che e' la
-- forma peggiore perche' non si vede. Percio' NULL = non emettere, e resta la
-- direzione sicura per ogni modello che nessuno ha provato.
--
-- (Osservato ma NON messo a vocabolario: `minimal` su k3 risponde 200 con 10 char
-- di pensiero, ancora meno di `low`. La doc Moonshot dichiara low|high|max e su
-- un valore non documentato non si spedisce.)
--
-- IL SETTING NASCE VUOTO: il meccanismo e' INERTE al deploy. Il flip a 'low' e'
-- una decisione da prendere sui numeri qui sopra, che sono la baseline PRIMA del
-- flip, e non un effetto collaterale di questa migrazione.
--
-- ROLLBACK: setting a stringa vuota (TTL 60s, senza riavvio) oppure colonna a
-- NULL. Due leve indipendenti: la prima spegne tutto il fornitore, la seconda un
-- modello solo.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS accepts_reasoning_effort BOOLEAN NULL;

COMMENT ON COLUMN ai_price_catalog.accepts_reasoning_effort IS
    'Il modello interpreta il campo top-level reasoning_effort. NULL = non dichiarato -> NON emettere: l''API Moonshot accetta con 200 anche un valore insensato, quindi mandarlo a un modello non misurato produce un effetto non dichiarato invece di un errore visibile. Misurato 17/08/2026 su tutti e quattro i modelli kimi a catalogo.';

UPDATE ai_price_catalog
   SET accepts_reasoning_effort = TRUE,
       updated_at               = NOW()
 WHERE provider = 'kimi'
   AND model IN ('kimi-k3', 'kimi-k2.6', 'kimi-k2.7-code', 'kimi-k2.7-code-highspeed');

INSERT INTO settings (key, value, category, description) VALUES
(
    'providers.kimi.reasoning_effort', '', 'providers',
    'Valore di reasoning_effort emesso ai modelli kimi che lo dichiarano (ai_price_catalog.accepts_reasoning_effort). Vocabolario chiuso low|high|max (doc Moonshot); VUOTO = non emettere, ed e'' il seed. Un valore fuori vocabolario non parte e produce un WARN: l''API lo accetterebbe in silenzio con un effetto non dichiarato. Non ha effetto quando il pensiero viene spento sulla stessa richiesta. Cache 60s lato driver.'
)
ON CONFLICT (key) DO NOTHING;
