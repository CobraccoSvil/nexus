-- 0704 — Il pensiero di kimi e' un FATTO del fornitore, e il catalogo lo negava.
--
-- ROOT CAUSE. `ai_price_catalog.uses_thinking_mode` vale `false` su tutti e
-- quattro i modelli kimi (k2.6, k2.7-code, k2.7-code-highspeed, k3), e da li'
-- la vista `v_model_capabilities.thinking` propaga il falso a chiunque la
-- interroghi. Il dato e' misurabilmente sbagliato, e il codice del gateway lo
-- dichiara gia' nella direzione opposta in due punti:
--
--   - `openai_compat.rs:55-81` (doc di `ReasoningDialect::Kimi`) e
--     `kimi.rs:67-84`: «il pensiero e' SEMPRE acceso e non si spegne»,
--     dichiarato come CONTRATTO del fornitore, non come preferenza nostra.
--   - `step_validation.rs:395-399`: il 400 di Moonshot che ha motivato il fix
--     del 09-10/08 dice testualmente «incompatible with thinking enabled».
--
-- E la misura lo conferma: le risposte degeneri di kimi spendono l'intero tetto
-- in `reasoning_content` — 8 righe `degenerate_hollow` da 1024 token esatti nel
-- ledger del 11-12/08, tutte fatturate ($0,0795), tutte con `content` vuoto.
-- Un modello che «non ragiona» non produce 8.192 token di ragionamento.
--
-- PERCHE' CONTA ADESSO. Il nuovo punto unico del tetto di output
-- (`decisions::tetto_output`) legge proprio questa colonna per decidere quanto
-- spazio lasciare: con `thinking = false` classifica kimi fra i modelli che non
-- ragionano e gli concede il solo visibile piu' la chiusura — 512 token, cioe'
-- META' del tetto che gia' produceva il vuoto. Il fix del chiamante, da solo,
-- avrebbe peggiorato il caso che nasce per chiudere: il dato falso rendeva il
-- criterio giusto una trappola.
--
-- PORTATA. Solo kimi, e solo perche' per kimi il pensiero e' NON DISATTIVABILE:
-- e' il caso in cui un `false` non e' un'imprecisione ma un'affermazione
-- contraria al contratto del fornitore. Gli altri provider non si toccano — un
-- allineamento generale di questa colonna e' un lavoro a se', che va fatto
-- MISURANDO ciascun fornitore e non per analogia.
--
-- NON tocca `is_enabled`: i modelli auto-disabilitati dal difetto (k2.6 alle
-- 16:56 e k3 alle 16:58 del 12/08, `auto_disabled_reason='empty_completion'`)
-- restano spenti, e li riaccendera' il ciclo di re-probe quando li avra'
-- verificati. Riabilitarli qui sarebbe la toppa (regola H): renderebbe verde
-- una riga senza che nessuno abbia riprovato la chiamata.

UPDATE ai_price_catalog
   SET uses_thinking_mode = true,
       updated_at = NOW()
 WHERE provider = 'kimi'
   AND uses_thinking_mode IS DISTINCT FROM true;
