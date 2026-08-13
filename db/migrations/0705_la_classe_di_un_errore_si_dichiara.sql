-- 0705 — La classe di un errore fornitore si DICHIARA, non si indovina da una
-- sottostringa.
--
-- IL DIFETTO (misurato il 13/08/2026 su nexus_provider_health_history):
--   openai risponde al credito esaurito con
--     429 {"type":"insufficient_quota","code":"credit_balance_exhausted"}
--   L'estrattore prendeva il PRIMO campo presente (`/error/code`) e buttava via
--   gli altri cinque; il punto di decisione confrontava quell'unico valore con
--   un vocabolario di SOTTOSTRINGHE (quota|billing|payment_required|
--   account_deactivated). `credit_balance_exhausted` non contiene "quota",
--   quindi cadeva sulla tabella per status: 429 -> Transient.
--   4439 righe dal 30/07 al 13/08, contro 1960 righe CORRETTE (code =
--   insufficient_quota) dal 06/07 al 30/07: gli intervalli sono DISGIUNTI, non
--   e' oscillazione. openai non ha smesso di dichiarare il credito — `type` e'
--   rimasto giusto, e' il `code` NUOVO, che ha la precedenza, a oscurarlo.
--   Conseguenza: ri-provato ogni ~62s per 14 giorni, mai in cooldown billing.
--
-- SECONDO CASO, stessa classe, segno invertito: mistral manda
--   400 {"type":"invalid_model","code":"1500"}
--   e `is_invalid_model_error("1500", 400)` e' FALSE, quindi il ramo di
--   auto-disable del modello deprecato non e' MAI scattato: 160 righe il
--   12/08 nei log del gateway.
--
-- LA DIVISIONE, e il perche':
--   - il VOCABOLARIO delle cause e la loro proiezione sulle 4 classi vivono in
--     CODICE (enum chiuso `CausaErrore`, funzione totale `classe()`): il
--     significato e' nostro e una UPDATE non deve poterlo cambiare;
--   - l'ASSEGNAZIONE (fornitore, valore) -> causa vive QUI: e' la parte che
--     cambia quando un fornitore cambia idea, e cambia senza preavviso.
--     Costo di un codice nuovo: una riga in una migrazione versionata piu' il
--     TTL di 60s. Nessun redeploy — che e' la ragione per cui l'incidente
--     openai e' durato 14 giorni.
--
-- POLARITA' (perche' il DB e' il posto giusto): questo elenco ASSEGNA, non
-- assolve. La sua incompletezza non spegne un presidio: manda la decisione
-- all'anello successivo (lo status), che e' esattamente il comportamento di
-- oggi. Costa denaro e latenza, che si misurano — non sicurezza silenziosa.
--
-- Il canale di scoperta NON puo' essere il log: MISURATO, `code=` esce solo dal
-- ramo ClientError, e delle 4439 chiamate sbagliate non e' rimasta UNA riga.
-- Per questo la seconda tabella.

CREATE TABLE IF NOT EXISTS nexus_provider_error_code (
  -- '*' = identificatore di CONVENZIONE (OpenAI-compat / google.rpc.Code),
  -- non del singolo fornitore. Vedi il vincolo `jolly_senza_status`.
  provider     text NOT NULL,
  -- Valore ESATTO, normalizzato. MAI un pattern: e' il pattern che ha prodotto
  -- l'incidente Moonshot (`exceeded_current_quota_error` riconosciuto perche'
  -- conteneva "quota" per caso).
  valore       text NOT NULL,
  -- NULL = qualunque status. Valorizzato dove la STESSA stringa vale due cose:
  -- deepseek manda `invalid_request_error` sia sul 402 ("Insufficient Balance")
  -- sia sul 400 di formato history (incidente 26/07, test routes.rs).
  http_status  smallint NULL,
  -- NULL = dichiarato AMBIGUO: non riconoscere, e non contarlo fra i codici da
  -- dichiarare. Serve per i valori che non sono identificatori (groq `tokens`
  -- e' la CATEGORIA della quota, non l'errore).
  causa        text NULL,
  -- Documentazione: dove il fornitore lo mette. Non partecipa alla ricerca.
  campo        text NULL,
  -- 'measured' = l'abbiamo visto noi; 'spec' = enum in una specifica
  -- versionata e SCARICABILE (quindi diffabile: un valore nuovo si scopre il
  -- giorno stesso); 'doc' = prosa o tabella HTML, che un job non puo' leggere.
  origine      text NOT NULL,
  occorrenze_al_seed bigint NULL,
  nota         text NULL,
  created_at   timestamptz NOT NULL DEFAULT now(),
  updated_at   timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT valore_normalizzato   CHECK (valore   = lower(btrim(valore))),
  CONSTRAINT provider_normalizzato CHECK (provider = lower(btrim(provider))),
  -- Replica in SQL del vocabolario chiuso di `CausaErrore`. Una riga fuori
  -- vocabolario non entra, invece di entrare e non combaciare mai.
  CONSTRAINT causa_nel_vocabolario CHECK (causa IS NULL OR causa IN (
    'credit_exhausted','rate_limit','overloaded','provider_fault',
    'model_not_found','malformed_request','auth_denied','request_too_large')),
  CONSTRAINT ambiguo_dichiara_il_motivo CHECK (causa IS NOT NULL OR nota IS NOT NULL),
  CONSTRAINT origine_nel_vocabolario   CHECK (origine IN ('measured','doc','spec')),
  -- Il jolly e' ammesso solo per gli identificatori di CONVENZIONE, che sono
  -- privi di status proprio: una riga jolly con status sarebbe una regola
  -- cross-fornitore su un codice HTTP, cioe' la tabella per status travestita.
  CONSTRAINT jolly_senza_status CHECK (provider <> '*' OR http_status IS NULL)
);

-- La chiave e' (provider, valore, status), con NULL = "qualunque status".
-- Indice e non PRIMARY KEY perche' Postgres non ammette espressioni in una PK.
CREATE UNIQUE INDEX IF NOT EXISTS nexus_provider_error_code_chiave
  ON nexus_provider_error_code (provider, valore, COALESCE(http_status, -1));

-- Cio' che il catalogo NON sa. Una riga per (fornitore, campo, valore) con
-- contatore, primo/ultimo visto ed esempio: alla prima occorrenza la riga
-- nasce, alla quattromillesima e' la stessa riga con occorrenze = 4439.
CREATE TABLE IF NOT EXISTS nexus_provider_error_code_unknown (
  provider text NOT NULL,
  campo    text NOT NULL,
  valore   text NOT NULL,
  status_ultimo     smallint NULL,
  -- La classe con cui si e' proceduto intanto: dichiara che si e' deciso
  -- SENZA sapere, invece di lasciarlo indistinguibile da un verdetto informato.
  classe_di_ripiego text NOT NULL,
  occorrenze   bigint      NOT NULL DEFAULT 1,
  primo_visto  timestamptz NOT NULL DEFAULT now(),
  ultimo_visto timestamptz NOT NULL DEFAULT now(),
  -- `error.message` STRUTTURATO, troncato: serve a chi dovra' classificarlo.
  esempio      text NULL,
  PRIMARY KEY (provider, campo, valore)
);

INSERT INTO settings (key, value, category, description) VALUES
 ('gateway.error_catalog.refresh_seconds','60','gateway',
  'Ricarica del catalogo dei codici errore fornitore (nexus_provider_error_code)'),
 ('gateway.error_catalog.unknown_dedup_seconds','60','gateway',
  'Finestra di dedup delle scritture di codice ignoto: 4439 occorrenze non sono 4439 UPSERT'),
 ('gateway.error_catalog.unknown_alert_occurrences','20','gateway',
  'Occorrenze oltre cui error-code-census --gate esce 1')
ON CONFLICT (key) DO NOTHING;

-- ─────────────────────────────────────────────────────────────────────────────
-- Il seed. DISCIPLINA: si semina cio' che e' MISURATO oppure inequivocabile in
-- doc. L'ambiguo si lascia scoprire, perche' una riga sbagliata e' peggio di un
-- ignoto: l'ignoto e' contato, la riga sbagliata no.
--
-- NON si semina ('google','failed_precondition','credit_exhausted'): ZERO
-- occorrenze nel nostro storico, doc ambigua ("a prerequisite is not met, e.g.
-- disabled billing"), e oggi quella stringa e' in
-- settings.routing.client_error_failover_codes, cioe' un client error
-- cross-provider-RECUPERABILE. Seminarla billing metterebbe google in cooldown
-- di credito su un errore mai osservato.
-- ─────────────────────────────────────────────────────────────────────────────

INSERT INTO nexus_provider_error_code
  (provider, valore, http_status, causa, campo, origine, occorrenze_al_seed, nota) VALUES

-- ── IL DIFETTO ──────────────────────────────────────────────────────────────
('openai','credit_balance_exhausted',NULL,'credit_exhausted','/error/code','measured',4439,
 'openai ha CAMBIATO error.code il 2026-07-30; error.type e'' rimasto insufficient_quota. 14 giorni di retry ogni ~62s, zero cooldown billing'),

-- ── Identificatori di CONVENZIONE (jolly) ───────────────────────────────────
-- La ricerca ESATTA sul fornitore vince SEMPRE su queste righe: una riga
-- specifica non puo' essere sovrascritta da un jolly.
('*','insufficient_quota',NULL,'credit_exhausted','/error/type','measured',2127,
 'convenzione OpenAI adottata dai cloni: misurata su openai (1960 fino al 30/07) e perplexity (167)'),
('*','rate_limit_exceeded',NULL,'rate_limit','/error/code','measured',408,
 'misurata su groq: emessa sia su 429 (239) sia su 413 (169). Sul 413 lo status da solo direbbe ContextTooLong e il motore farebbe failover invece di attendere (fix 16-17/07)'),
('*','invalid_request_error',NULL,'malformed_request','/error/type','measured',NULL,
 'convenzione OpenAI-compat. CONSERVA is_history_related_client_error, che oggi non guarda il fornitore: senza questa riga un 400 di formato smetterebbe di innescare la sanificazione aggressiva. deepseek la SCAVALCA sul 402 (riga esatta sotto)'),
('*','invalid_request_message_order',NULL,'malformed_request','/error/code','doc',NULL,
 'conserva is_history_related_client_error'),
('*','malformed_function_call',NULL,'malformed_request','/error/status','doc',NULL,
 'google: tool-call malformata. Conserva is_history_related_client_error'),
('*','invalid_argument',NULL,'malformed_request','/error/status','measured',23,
 'google.rpc.Code, sempre a 400. Conserva is_history_related_client_error; e'' anche in settings.routing.client_error_failover_codes'),
('*','invalid_model',NULL,'model_not_found','/error/type','measured',160,
 'conserva is_invalid_model_error, che oggi non guarda il fornitore'),
('*','model_not_found',NULL,'model_not_found','/error/code','doc',NULL,
 'conserva is_invalid_model_error'),
('*','not_found',NULL,'model_not_found','/error/status','measured',92,
 'google.rpc.Code, sempre a 404: la regola di status lo copriva gia'', la riga lo rende dichiarato'),

-- ── CREDENZIALE RIFIUTATA ───────────────────────────────────────────────────
-- MISURATO il 13/08/2026 inviando a tutti e nove i fornitori una chiave non
-- valida (errore provocabile a costo zero: nessun token consumato), e riprodotto
-- due volte in modo indipendente. Stesso evento semantico, nove forme diverse.
--
-- Perche' vale la pena dichiararlo: la CLASSE non cambia (un 401 e' ClientError
-- con o senza queste righe), ma la CAUSA si': senza, il `invalid_request_error`
-- di deepseek e l'`invalid_argument` di google diventano `malformed_request`,
-- che innesca una sanificazione aggressiva della history — un rimedio che non
-- puo' riparare una credenziale rifiutata, e che costa una chiamata in piu'.
('*','invalid_api_key',NULL,'auth_denied','/error/code','measured',NULL,
 'openai e groq lo mettono in error.code; perplexity lo mette in error.type. Stesso valore, stesso significato'),
('*','authentication_error',NULL,'auth_denied','/error/type','measured',NULL,
 'deepseek e anthropic'),
('*','invalid_authentication_error',NULL,'auth_denied','/error/type','measured',NULL,'moonshot/kimi'),
('*','incorrect_api_key_error',NULL,'auth_denied','/error/type','doc',NULL,'tabella errori Moonshot'),
('*','permission_denied_error',NULL,'auth_denied','/error/type','doc',NULL,'tabella errori Moonshot, 403'),
-- deepseek INVERTE i ruoli rispetto a openai: mette il generico in `code` e lo
-- specifico in `type`. La riga per STATUS e' cio' che rende inutile una tabella
-- di precedenze per fornitore: qui `invalid_request_error` significa credenziale
-- rifiutata, sul 402 credito e sul 400 formato history. Tre righe, un solo
-- meccanismo.
('deepseek','invalid_request_error',401,'auth_denied','/error/code','measured',NULL,
 'MISURATO: a chiave non valida deepseek risponde 401 con type=authentication_error e code=invalid_request_error'),
-- google risponde alla chiave non valida con 400 INVALID_ARGUMENT, non 401: lo
-- status MENTE sulla causa, e l'unico campo che la dice e' details[].reason.
('google','api_key_invalid',NULL,'auth_denied','/error/details/reason','measured',NULL,
 'ErrorInfo.reason: identificatore stabile con enum VERSIONATO (google/api/error_reason.proto, 46 valori), mentre /error/status porta la sola CATEGORIA gRPC (INVALID_ARGUMENT, 17 valori). Misurato da noi, e riscontrabile nella spec'),

-- ── anthropic (l'unico quirk sulla prosa, isolato nell'adapter) ─────────────
('anthropic','billing_error',400,'credit_exhausted','quirk','measured',1806,
 'SINTETICO ma NON inventato: `billing_error` e'' uno dei 9 valori dell''enum ErrorType di anthropic (spec Stainless) — il fornitore lo dichiara, semplicemente non lo emette su questo 400, dove manda invalid_request_error, lo STESSO identificatore di una richiesta malformata. Vedi quirk_del_fornitore'),
('anthropic','api_error',NULL,'provider_fault','/error/type','measured',21,'misurate a status 500'),
('anthropic','overloaded_error',NULL,'provider_fault','/error/type','measured',8,'misurate a status 529'),
('anthropic','rate_limit_error',NULL,'rate_limit','/error/type','spec',NULL,
 'enum ErrorType della spec Stainless, versionata insieme all''SDK (.stats.yml di anthropic-sdk-python)'),
('anthropic','request_too_large',NULL,'request_too_large','/error/type','doc',NULL,
 'resta doc E NON spec: la tabella HTML lo elenca per il 413, l''enum ErrorType della spec (9 valori) NO. Le due fonti dello stesso fornitore divergono, e la riga dichiara quale delle due l''ha prodotta'),
('anthropic','not_found_error',NULL,'model_not_found','/error/type','spec',NULL,'enum ErrorType'),
('anthropic','authentication_error',NULL,'auth_denied','/error/type','spec',NULL,'enum ErrorType'),
('anthropic','permission_error',NULL,'auth_denied','/error/type','spec',NULL,'enum ErrorType'),
('anthropic','timeout_error',NULL,'provider_fault','/error/type','spec',NULL,
 'enum ErrorType, HTTP 504: il fornitore dichiara di non aver risposto in tempo'),

-- ── groq ────────────────────────────────────────────────────────────────────
('groq','tokens',NULL,NULL,'/error/type','measured',408,
 'AMBIGUO: e'' la CATEGORIA della quota, non l''errore. Non riconoscere, non contare come debito. Decide il code (rate_limit_exceeded)'),

-- ── deepseek: la STESSA stringa vale due cose a due status ──────────────────
('deepseek','invalid_request_error',402,'credit_exhausted','/error/code','measured',30,
 'il 402 di deepseek significa "Insufficient Balance": il code porta un type e non dice nulla, e'' lo status a distinguere'),
('deepseek','invalid_request_error',400,'malformed_request','/error/code','measured',NULL,
 'MISURATO 26/07: BODY_400_DEEPSEEK e'' un errore di formato history e DEVE innescare la sanificazione aggressiva (test routes.rs). Ridondante col jolly, e volutamente: rende leggibile in UNA query che la stessa stringa vale due cose'),
('deepseek','unknown_error',NULL,NULL,'/error/type','measured',30,'AMBIGUO: non dice nulla'),

-- ── mistral: il type semantico oscurato dal code numerico ───────────────────
('mistral','1500',NULL,'model_not_found','code','measured',160,
 'code top-level numerico; il type dice invalid_model. is_invalid_model_error("1500",400) era false: il ramo di auto-disable non e'' MAI scattato'),
('mistral','3810',NULL,'overloaded','code','measured',2,'"Capacity exceeded for this model"'),
('mistral','engine_max_pending_tokens',NULL,'overloaded','type','measured',2,NULL),
('mistral','3800',NULL,'provider_fault','code','measured',1,'"Service unavailable"'),
('mistral','internal_server_error',NULL,'provider_fault','type','measured',1,NULL),

-- ── google ──────────────────────────────────────────────────────────────────
('google','resource_exhausted',NULL,'rate_limit','/error/status','measured',54,
 'quota per minuto; il /error/code di google e'' NUMERICO e viene scartato'),
('google','permission_denied',NULL,'auth_denied','/error/status','measured',3,
 'misurato anche su "Your API key was reported as leaked"'),

-- ── openrouter: il campo MISURATO, non quello documentato ───────────────────
('openrouter','openrouter_credits',NULL,'credit_exhausted','/error/metadata/limit_source','measured',127,
 'stessa classe che lo status 402 gia'' dava: la riga la rende DICHIARATA, non la cambia'),
('openrouter','upstream_provider_shared_pool',NULL,'rate_limit','/error/metadata/limit_source','measured',4,
 'MISURATO sul 429: "temporarily rate-limited upstream". Stessa classe dello status; senza la riga sarebbe un ignoto permanente'),
('openrouter','payment_required',NULL,'credit_exhausted','/error/code','doc',NULL,NULL),
('openrouter','context_length_exceeded',NULL,'request_too_large','/error/code','doc',NULL,NULL),

-- openrouter pubblica un enum di 27 valori (`ApiErrorType` in openapi.json, che
-- la spec dichiara «canonical [...] stable across all API formats» ed e' marcato
-- APERTO in coda: x-speakeasy-unknown-values allow, e contiene perfino un valore
-- `unmapped`). NON si semina: nei nostri corpi reali openrouter non emette alcun
-- `error.type`, quindi non sappiamo in quale CAMPO quei valori viaggino — e una
-- riga di cui non si sa dove cercare il valore non e' verificabile. Il passo
-- successivo e' misurarlo, non indovinarlo. (Che l'enum sia dichiarato aperto
-- conferma pero' una scelta: il codice ignoto come variante DICHIARATA non e'
-- nostra prudenza, e' richiesto dal contratto del fornitore.)

-- ── kimi: i tre 429 con rimedi opposti ──────────────────────────────────────
('kimi','engine_overloaded_error',NULL,'overloaded','/error/type','measured',3,NULL),
('kimi','exceeded_current_quota_error',NULL,'credit_exhausted','/error/type','doc',NULL,
 'misurato sull''API Moonshot il 09/08/2026 (account a saldo zero), mai nel nostro storico. Oggi passa per la sottostringa "quota": domani per dichiarazione'),
('kimi','rate_limit_reached_error',NULL,'rate_limit','/error/type','doc',NULL,NULL),

-- ── openai, documentati e mai visti ─────────────────────────────────────────
('openai','organization_spend_limit_exceeded',NULL,'credit_exhausted','/error/code','doc',NULL,NULL),
('openai','project_spend_limit_exceeded',NULL,'credit_exhausted','/error/code','doc',NULL,NULL),
('openai','organization_usage_limit_exceeded',NULL,'credit_exhausted','/error/code','doc',NULL,NULL)
ON CONFLICT DO NOTHING;
