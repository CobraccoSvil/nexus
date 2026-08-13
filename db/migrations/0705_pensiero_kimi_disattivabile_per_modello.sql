-- 0705 — Se il pensiero si possa SPEGNERE e' un fatto del fornitore, per MODELLO.
--
-- ROOT CAUSE. Il driver kimi non emette mai il controllo `thinking`, e la ragione
-- scritta nel codice e' una premessa generalizzata: `kimi.rs:76-80` dichiara che
-- «il pensiero su k3/k2.7-code non e' disattivabile» e ne deduce che il campo
-- `ResolvedReasoning.enabled` non abbia produttore. La 0704 ha ripetuto la stessa
-- premessa («solo perche' per kimi il pensiero e' NON DISATTIVABILE»).
--
-- MISURATO il 13/08/2026 chiamando l'API vera (`https://api.moonshot.ai/v1`,
-- chiave da `settings.kimi_api_key`), con `thinking: {"type":"disabled"}`:
--
--     kimi-k2.6                  ACCETTATO   (reasoning da 575 token a 1)
--     kimi-k3                    ACCETTATO
--     kimi-k2.7-code             HTTP 400    «only type=enabled is allowed»
--     kimi-k2.7-code-highspeed   HTTP 400    idem
--
-- La premessa era vera per META' del parco e falsa per l'altra meta'. Non e'
-- un'accettazione per distrazione dell'API: `thinking: {"type":"banana"}` da'
-- 400 (il campo e' validato davvero) mentre un campo INVENTATO passa con 200 (i
-- campi ignoti sono ignorati) — quindi il 200 su `disabled` e' una risposta al
-- campo, non un silenzio su di esso.
--
-- IL DANNO. Su kimi `max_completion_tokens` limita l'output TOTALE, pensiero
-- compreso. Stesso prompt, stesso tetto di 1024, due esiti:
--
--     senza `thinking` (cio' che il driver manda oggi):
--         reasoning 1023/1024, `content` VUOTO, finish=length -> `degenerate_hollow`
--     con `thinking: disabled`:
--         reasoning 1, risposta reale
--
-- E i due sintomi stanno IN SEQUENZA, non insieme: nel ledger `degenerate_hollow`
-- finisce il 12/08 alle 16:58 — chiuso dal commit 3eafec9d, che alza il tetto
-- leggendolo dal catalogo — e `attempt_timeout` comincia alle 20:54 dello stesso
-- giorno. Col tetto a 8192 kimi riempie 8191 token di PENSIERO e impiega 214,8
-- secondi contro un cap per tentativo di 72. Alzare il tetto non ha chiuso il
-- difetto: gli ha fatto cambiare forma, perche' il tetto non e' la causa.
--
-- COSA DICHIARA QUESTA COLONNA, E COSA NO. `uses_thinking_mode` (0704) dice che
-- il modello RAGIONA, e resta vera per tutti e quattro. Questa dice se quel
-- ragionamento si possa SPEGNERE, ed e' una domanda diversa con un'altra
-- risposta: sono ortogonali, e fonderle e' esattamente l'errore che ha prodotto
-- la premessa generalizzata di sopra.
--
-- NULL = NON DICHIARATO, e vale «non si spegne». La cautela non e' teorica: e'
-- l'immagine speculare del difetto della temperatura che il driver kimi gia'
-- evita — spegnere dove il fornitore non lo consente e' un HTTP 400 su OGNI
-- chiamata a quel modello. Percio' il codice spegne solo su un `true` esplicito,
-- e un modello nuovo — che nasce senza riga qui — si comporta come oggi finche'
-- qualcuno non lo MISURA. Nessun default a tabella: un default `false` sarebbe
-- indistinguibile da una misura, un default `true` aprirebbe il 400.
--
-- PERCHE' NON `agentic_thinking_policy`. Sembra la sede naturale del «quando
-- spegnere», ed e' una trappola: quella colonna la RISCRIVE il catalog sync
-- (`model_catalog_sync.rs:2495` e `models.rs:328`, ramo `capability_source='auto'`,
-- che e' il valore delle quattro righe kimi) partendo da un'euristica sul NOME
-- (`classify_caps`), la quale per «kimi-k2.6» non trova alcun marcatore di
-- reasoning e conclude 'none'. Un UPDATE qui verrebbe cancellato dal primo giro
-- di sync che tocca la riga: un fix che il sistema annulla da solo non e' un fix
-- (regola H). Questa colonna e' al sicuro dallo stesso destino perche' non ha —
-- e non deve avere — un'euristica sul nome che la produca: la si sa solo
-- chiamando l'API. Allineare `agentic_thinking_policy` per kimi richiede prima
-- di insegnare al classificatore che kimi e' thinking, ed e' un lavoro a se' che
-- va fatto MISURANDO ogni fornitore, non per analogia — la stessa portata che la
-- 0704 si e' data.
--
-- PORTATA. Solo kimi: e' l'unico fornitore per cui la disattivabilita' e' stata
-- misurata. Gli altri restano NULL, cioe' «nessuno ha ancora guardato», che e'
-- il vero stato delle cose e non un'affermazione su di loro.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS thinking_can_be_disabled BOOLEAN;

COMMENT ON COLUMN ai_price_catalog.thinking_can_be_disabled IS
    'Il fornitore accetta di SPEGNERE il ragionamento su questo modello? '
    'true/false = MISURATO chiamando l''API; NULL = non dichiarato, e chi legge '
    'non spegne (spegnere dove non e'' consentito e'' un HTTP 400 su ogni '
    'chiamata). Ortogonale a uses_thinking_mode, che dice se il modello ragiona. '
    'Non deducibile dal nome: nessuna euristica la scrive, quindi il catalog '
    'sync non la sovrascrive.';

UPDATE ai_price_catalog
   SET thinking_can_be_disabled = true,
       updated_at = NOW()
 WHERE provider = 'kimi'
   AND model IN ('kimi-k2.6', 'kimi-k3')
   AND thinking_can_be_disabled IS DISTINCT FROM true;

UPDATE ai_price_catalog
   SET thinking_can_be_disabled = false,
       updated_at = NOW()
 WHERE provider = 'kimi'
   AND model IN ('kimi-k2.7-code', 'kimi-k2.7-code-highspeed')
   AND thinking_can_be_disabled IS DISTINCT FROM false;
