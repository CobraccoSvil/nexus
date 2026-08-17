-- ─────────────────────────────────────────────────────────────────────────────
-- 0729 — openai: la corsia DIFFERIBILE (service_tier flex), meccanismo SPENTO
--
-- Il tier `flex` di openai costa la META' del listino standard e in cambio non
-- promette latenza: e' la corsia giusta per il lavoro che nessuno sta
-- guardando — titoli di conversazione, riassunti, note di documentazione,
-- estrazioni wiki. Chi lo decide non e' il fornitore e non e' un setting: e' il
-- CHIAMANTE, con il campo di contratto `LlmRequest.deferrable`, perche' lo
-- stesso modello serve un turno di chat e un titolo, e solo chi ha chiesto il
-- lavoro sa quale dei due sia.
--
-- Questa migrazione porta i TRE fatti che il codice non puo' sapere da se':
-- l'interruttore dell'installazione, la platea del fornitore, e cosa significa
-- il rifiuto quando la corsia e' piena. Nasce SPENTA: `flex_enabled` e' 'false',
-- quindi al deploy non cambia una sola richiesta.
--
-- ─────────────────────────────────────────────────────────────────────────────
-- LA PLATEA E' MISURATA, NON DEDOTTA DALLA DOC (17/08/2026, API reale)
--
-- La doc openai elenca «o3, o4-mini, gpt-5»: una famiglia, non un elenco di
-- modelli, e il catalogo ne ha 65 abilitati. Interrogare l'API era possibile a
-- COSTO ZERO, e per una ragione strutturale: la validazione del parametro
-- PRECEDE il controllo del credito. Su un account senza credito residuo — il
-- nostro — la stessa chiamata risponde
--
--   * 400 «Invalid service_tier argument»        -> il modello non ha la corsia
--   * 400 «Flex is not available for this model» -> idem, con messaggio proprio
--   * 429 «You have no credits remaining»        -> il parametro E' PASSATO
--
-- e quel 429 e' la prova che cercavamo: il rifiuto arriva DOPO la validazione,
-- quindi il modello la corsia ce l'ha. Che il controllo sia per-modello e non
-- globale lo dimostra `gpt-5-pro`, che ha un 400 tutto suo mentre `gpt-5` passa.
--
-- 57 modelli chat abilitati interrogati, nessun token consumato:
--
--   AMMESSI (29)   o3, o4-mini (+ snapshot datati), gpt-5 / -mini / -nano
--                  (+ snapshot), gpt-5.1, gpt-5.2 (+ -pro), gpt-5.4 (+ -mini,
--                  -nano, -pro e snapshot), gpt-5.5 (+ -pro), gpt-5.6-{luna,
--                  sol,terra}
--   NON AMMESSI    tutta la famiglia gpt-4o e gpt-4.1, o1, o3-mini, gpt-5-pro
--   DEPRECATI 404  gpt-5-chat-latest, gpt-5-codex, gpt-5.1-chat-latest,
--                  gpt-5.1-codex, gpt-5.1-codex-mini, gpt-5.2-chat-latest,
--                  gpt-5.2-codex, gpt-5.3-chat-latest; gpt-5.3-codex non e'
--                  servito da /chat/completions. Non li tocca questa migrazione
--                  (sono is_enabled=true a catalogo: e' un fronte suo)
--
-- I modelli NON interrogati restano NULL, che non e' un permesso: e' «nessuno
-- ha dichiarato niente», e il driver non manda la corsia (regola Q). Un modello
-- nuovo nasce cosi', e costa il prezzo pieno finche' qualcuno non lo misura —
-- il verso giusto, perche' l'errore opposto e' un 400 su ogni sua chiamata.
--
-- ─────────────────────────────────────────────────────────────────────────────
-- IL RIFIUTO PER CAPACITA': 429 CHE NON E' UN RATE LIMIT
--
-- MISURATO sullo stesso giro (`gpt-5.2-pro`, due volte di fila):
--
--   HTTP 429  Retry-After: 300
--   {"error":{"message":"Flex tier does not have sufficient resources available
--     to fulfill your request. You can try again later ... or change
--     service_tier=default","type":"resource_unavailable",
--     "code":"flex_unavailable"}}
--
-- Lo status e' quello di un tetto di frequenza e il rimedio e' l'opposto: il
-- fornitore stesso scrive «change service_tier=default». Senza dichiararlo, la
-- tabella per status ricadrebbe su Transient, il gateway onorerebbe quel
-- `Retry-After: 300` e metterebbe la coppia in cooldown per cinque minuti —
-- togliendo dalla selezione un modello che al tier standard rispondeva subito.
--
-- DUE RIGHE perche' il fornitore emette DUE valori nello stesso body, e la
-- classificazione decide sul primo candidato RICONOSCIUTO: dichiarandoli
-- entrambi il caso si chiude anche se domani uno dei due cambia nome.
--
-- La causa `flex_capacity` e' NUOVA nel vocabolario chiuso di `CausaErrore` (e
-- percio' il CHECK si riallarga, pattern della 0709). Proietta su `transient`
-- sul WIRE — la capacita' opportunistica torna da se', ed e' cio' che la classe
-- descrive del fornitore — ma la CONSEGUENZA nel gateway la decide la CAUSA:
-- `complete_with_retry` non ritenta e non marca cooldown. Nessuna nuova
-- `ClasseErrore` in nexus-types: allargare il vocabolario condiviso per una
-- distinzione che il solo gateway usa sarebbe attrito senza consumatori.
--
-- Nel caso NORMALE nulla di tutto questo si vede: il driver openai consuma il
-- rifiuto da se', rimandando UNA volta la stessa richiesta senza il campo. Le
-- righe qui sotto coprono i due percorsi che il driver non governa — un
-- `service_tier` PINNATO dal chiamante (che il driver non scavalca di
-- proposito) e un endpoint compat che lo emetta per conto suo.
--
-- ─────────────────────────────────────────────────────────────────────────────
-- I BUDGET: perche' una richiesta differibile ha numeri PROPRI
--
-- I budget ordinari nascono da «quanti turni deve poter completare il run che
-- contiene questa chiamata». Una richiesta differibile non appartiene a nessun
-- run — non c'e' nessun `min_turns` da garantire — quindi quella domanda per lei
-- non si pone, e dimensionarla su un run la farebbe scadere prima di servire (la
-- doc flex parla di 10-15 minuti). Il RUN vince dove e' dichiarato: chi manda
-- `run_timeout_secs` sta dentro un run vivo, e un flag non puo' allungarglielo.
--
-- Il cap per-tentativo e' comunque limitato dal TETTO DI TRASPORTO del client
-- reqwest (oggi 300s, da `gateway.stream_timeout_seconds`): una deadline logica
-- oltre il tetto non allunga la chiamata, la fa morire con un errore di
-- trasporto opaco al posto dell'`attempt_timeout` strutturato su cui il motore
-- fa failover. Il taglio non e' silenzioso — un WARN all'avvio lo dichiara e
-- indica la chiave da alzare per ottenerlo davvero.
--
-- ROLLBACK: `providers.openai.flex_enabled` a 'false' (TTL 60s, senza riavvio).
-- ─────────────────────────────────────────────────────────────────────────────

-- 1. L'interruttore e i budget. Il seed 'false' e' il punto: il meccanismo
--    arriva in produzione SPENTO e si accende dal DB quando un probe con
--    credito conferma il risparmio, non al deploy del codice.
INSERT INTO settings (key, value, category, description) VALUES
(
    'providers.openai.flex_enabled', 'false', 'providers',
    'Interruttore della corsia DIFFERIBILE di openai (service_tier=flex, meta'' prezzo, nessuna garanzia di latenza). Vale solo per le richieste che il CHIAMANTE dichiara differibili (LlmRequest.deferrable) su modelli che il catalogo dichiara ammessi (ai_price_catalog.supports_flex): le tre condizioni sono in congiunzione. Nasce ''false'': il meccanismo e'' inerte finche'' qualcuno non lo accende. Cache 60s lato driver.'
),
(
    'gateway.flex.request_budget_seconds', '900', 'gateway',
    'Budget end-to-end (chain e retry inclusi) di una richiesta DIFFERIBILE. Numero proprio e non derivato dal run: una richiesta differibile non appartiene a un run con turni da garantire, e il tier flex non promette latenza (la doc openai parla di 10-15 minuti). Non puo'' ACCORCIARE i budget ordinari. Vale solo dove il chiamante NON dichiara run_timeout_secs: dove lo dichiara, e'' il run a vincere.'
),
(
    'gateway.flex.per_attempt_seconds', '900', 'gateway',
    'Cap su un singolo tentativo di una richiesta DIFFERIBILE. Limitato al tetto di trasporto del client reqwest (max fra per_attempt e gateway.stream_timeout_seconds, oggi 300s): oltre quel tetto la deadline logica non allunga la chiamata, la fa morire con un errore di trasporto opaco invece dell''attempt_timeout strutturato. Per allungarlo davvero si alza gateway.stream_timeout_seconds; il taglio e'' dichiarato da un WARN all''avvio.'
)
ON CONFLICT (key) DO NOTHING;

-- 2. La platea, dal FORNITORE e per MODELLO (regola G: non un elenco di nomi
--    nel codice Rust, che resterebbe fermo e servirebbe un redeploy per
--    seguirlo).
ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS supports_flex BOOLEAN NULL;

COMMENT ON COLUMN ai_price_catalog.supports_flex IS
    'Il modello accetta service_tier=flex (corsia differibile, meta'' prezzo). NULL = non dichiarato -> NON si emette il campo: l''ignoto non e'' un permesso, e mandarlo dove non e'' ammesso e'' un 400 su ogni chiamata. Provenienza del seed: misura diretta sull''API del 17/08/2026 (la validazione del parametro precede il controllo del credito, quindi 400 = non ammesso e 429-credito = ammesso). Il catalog sync NON la riscrive.';

UPDATE ai_price_catalog SET supports_flex = true, updated_at = now()
 WHERE provider = 'openai'
   AND model IN (
     'o3','o3-2025-04-16',
     'o4-mini','o4-mini-2025-04-16',
     'gpt-5','gpt-5-2025-08-07',
     'gpt-5-mini','gpt-5-mini-2025-08-07',
     'gpt-5-nano','gpt-5-nano-2025-08-07',
     'gpt-5.1','gpt-5.1-2025-11-13',
     'gpt-5.2','gpt-5.2-2025-12-11','gpt-5.2-pro',
     'gpt-5.4','gpt-5.4-2026-03-05',
     'gpt-5.4-mini','gpt-5.4-mini-2026-03-17',
     'gpt-5.4-nano','gpt-5.4-nano-2026-03-17',
     'gpt-5.4-pro','gpt-5.4-pro-2026-03-05',
     'gpt-5.5','gpt-5.5-2026-04-23',
     'gpt-5.5-pro','gpt-5.5-pro-2026-04-23',
     'gpt-5.6-luna','gpt-5.6-sol','gpt-5.6-terra'
   );

-- Il `false` e' una MISURA come il `true`, e vale la pena scriverlo: distingue
-- «interrogato, non ce l'ha» da «mai interrogato», che e' il NULL. Senza,
-- ri-misurare domani vorrebbe dire ripetere tutte e 57 le chiamate.
UPDATE ai_price_catalog SET supports_flex = false, updated_at = now()
 WHERE provider = 'openai'
   AND model IN (
     'gpt-4.1','gpt-4.1-2025-04-14',
     'gpt-4.1-mini','gpt-4.1-mini-2025-04-14',
     'gpt-4.1-nano','gpt-4.1-nano-2025-04-14',
     'gpt-4o','gpt-4o-2024-05-13','gpt-4o-2024-08-06','gpt-4o-2024-11-20',
     'gpt-4o-mini','gpt-4o-mini-2024-07-18',
     'o1','o1-2024-12-17',
     'o3-mini','o3-mini-2025-01-31',
     'gpt-5-pro','gpt-5-pro-2025-10-06'
   );

-- 3. Il vocabolario chiuso si allarga di una causa. Il CHECK lo replica in SQL,
--    o le righe qui sotto non entrerebbero (pattern della 0709).
ALTER TABLE nexus_provider_error_code
    DROP CONSTRAINT IF EXISTS causa_nel_vocabolario;

ALTER TABLE nexus_provider_error_code
    ADD CONSTRAINT causa_nel_vocabolario CHECK (causa IS NULL OR causa IN (
      'credit_exhausted','rate_limit','overloaded','provider_fault',
      'model_not_found','malformed_request','auth_denied','request_too_large',
      'request_exceeds_credit','flex_capacity'));

INSERT INTO nexus_provider_error_code
  (provider, valore, http_status, causa, campo, origine, occorrenze_al_seed, nota) VALUES
('openai','flex_unavailable',429,'flex_capacity','/error/code','measured',NULL,
 'la corsia differibile non ha capacita'' ADESSO. Misurato il 17/08/2026 su gpt-5.2-pro con service_tier=flex, due volte: 429 con Retry-After 300 e messaggio che indica come rimedio service_tier=default. Senza questa riga il 429 ricadrebbe su Transient, il Retry-After sarebbe onorato e la coppia entrerebbe in cooldown per 5 minuti - mentre al tier standard rispondeva subito'),
('openai','resource_unavailable',429,'flex_capacity','/error/type','measured',NULL,
 'lo stesso rifiuto, nell''altro campo che il fornitore valorizza nello STESSO body (type accanto a code=flex_unavailable). Dichiarati entrambi: la classificazione decide sul primo candidato riconosciuto, e cosi'' il caso resta chiuso anche se uno dei due nomi cambia')
ON CONFLICT DO NOTHING;
