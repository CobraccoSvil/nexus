-- ─────────────────────────────────────────────────────────────────────────────
-- 0709 — «Il limite viene dal credito» non significa «il credito e' zero»
--
-- LA PREMESSA GIUSTA. La mig 0707 ha seminato
--
--   ('openrouter','openrouter_credits',NULL,'credit_exhausted',
--    '/error/metadata/limit_source','measured',127,
--    'stessa classe che lo status 402 gia'' dava: la riga la rende DICHIARATA,
--     non la cambia')
--
-- e la misura era corretta: 127 righe reali con quel valore in
-- `metadata.limit_source`. Quel campo risponde pero' a «DA DOVE viene il
-- limite» — dal credito OpenRouter e non dal fornitore a valle, che e' l'altro
-- valore osservato (`upstream_provider_shared_pool`, 4 righe, rate_limit). NON
-- dice che il credito sia finito.
--
-- LA CONCLUSIONE SBAGLIATA, e cosa la smentisce. MISURATO il 13/08/2026:
--
--   - saldo letto da `GET /api/v1/credits`: 60 stanziati, 49,988 usati,
--     10,01 dollari RESIDUI. Il conto aveva credito mentre lo tenevamo fuori;
--   - 41 messaggi distinti nelle righe registrate, TUTTI nella forma «You
--     requested up to N tokens, but can only afford M», con M fra 432 e 64811.
--     Mai zero: se il saldo fosse esaurito, «can only afford» direbbe 0;
--   - il fornitore ALLEGA il rimedio in `metadata.remedy_hint`: «Add credits
--     **or lower max_tokens**». Riprodotto a costo zero (il rifiuto precede
--     l'esecuzione): senza `max_tokens` -> «up to 65536» e rifiuto; con
--     `max_tokens: 8000` (prenotazione 4,80 dollari, sotto il saldo) -> AMMESSA.
--
-- OpenRouter PRENOTA il costo massimo della richiesta contro il saldo residuo e
-- rifiuta se non ci sta. Il credito c'e' e non copre QUESTA richiesta, che e'
-- una cosa diversa dall'account a secco — e il rimedio implicato da
-- `credit_exhausted`, cioe' sei ore di cooldown, e' inutile quando basterebbe
-- chiedere meno, mentre nel frattempo esclude un fornitore che sta servendo.
--
-- DUE RIGHE, non una:
--   - `openrouter_credits` viene RIASSEGNATO: il valore continua a significare
--     «il limite viene dal credito», ma la causa che ne discende cambia;
--   - `request_exceeds_credit` e' il candidato SINTETICO che l'adapter emette
--     per il 402 di openrouter (`quirk_del_fornitore`, campo 'quirk', rango
--     massimo). Senza la sua riga il quirk emetterebbe un valore che il
--     catalogo non dichiara, `giudica` ricadrebbe sulla tabella per status
--     (402 -> Billing) e il fix resterebbe inerte — lo stesso difetto che la
--     riga ('anthropic','billing_error',400,...) della 0707 previene.
--
-- IL VOCABOLARIO si allarga di una causa: `CausaErrore::RequestExceedsCredit`,
-- che proietta sulla classe di wire omonima. NON su `billing`, che in mcp-core
-- si traduce in `EsclusioneDichiarata::Credito`, cioe' sei ore; e non su
-- `request_too_large`, che manderebbe l'escalation a cercare una finestra piu'
-- grande e chi legge i log a cercare un prompt grande che non c'e'.
--
-- COSA NON CAMBIA: `('openrouter','payment_required',...)` resta
-- `credit_exhausted`. E' il codice che il fornitore DICHIARA per il credito
-- finito, e un saldo davvero a zero deve continuare a produrre un cooldown
-- lungo. Sul 402 il quirk lo scavalca per rango, com'e' inteso: il criterio e'
-- provider + status, entrambi segnali strutturati (regola M, punto 4).
-- ─────────────────────────────────────────────────────────────────────────────

-- 1. Il CHECK replica in SQL il vocabolario chiuso di `CausaErrore`: si allarga
--    insieme all'enum, o la riga qui sotto non entrerebbe.
ALTER TABLE nexus_provider_error_code
    DROP CONSTRAINT IF EXISTS causa_nel_vocabolario;

ALTER TABLE nexus_provider_error_code
    ADD CONSTRAINT causa_nel_vocabolario CHECK (causa IS NULL OR causa IN (
      'credit_exhausted','rate_limit','overloaded','provider_fault',
      'model_not_found','malformed_request','auth_denied','request_too_large',
      'request_exceeds_credit'));

-- 2. La riga misurata dalla 0707 cambia CONCLUSIONE, non misura.
UPDATE nexus_provider_error_code
   SET causa      = 'request_exceeds_credit',
       nota       = 'il valore dice DA DOVE viene il limite (il credito OpenRouter), non che sia zero. Smentita 13/08/2026: saldo 10,01 dollari residui via GET /api/v1/credits, e 41 messaggi distinti tutti "can only afford N" con N fra 432 e 64811. Vedi mig 0709',
       updated_at = now()
 WHERE provider = 'openrouter'
   AND valore   = 'openrouter_credits'
   AND causa    = 'credit_exhausted';

-- 3. Il valore SINTETICO del quirk, dichiarato come per anthropic/billing_error.
--    Lo status e' valorizzato: il quirk nasce dal 402, e una riga jolly
--    renderebbe la stessa stringa valida su status che non l'hanno prodotta.
INSERT INTO nexus_provider_error_code
  (provider, valore, http_status, causa, campo, origine, occorrenze_al_seed, nota) VALUES
('openrouter','request_exceeds_credit',402,'request_exceeds_credit','quirk','measured',127,
 'SINTETICO: error.code porta il NUMERO 402, che non e'' una stringa e non produce candidati, quindi restano provider + status. Emesso da quirk_del_fornitore; senza questa riga il candidato non sarebbe dichiarato e si ricadrebbe su 402 -> Billing')
ON CONFLICT DO NOTHING;
