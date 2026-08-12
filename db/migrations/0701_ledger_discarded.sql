-- 0701: le completion SCARTATE entrano nel ledger — status 'discarded' + causa.
--
-- Il gateway scrive la riga di ledger SOLO sulla risposta di successo
-- (record_and_declare in server/routes.rs, dopo run_fallback). Ma dentro
-- run_fallback/complete_with_retry esistono tentativi CONSUMATI la cui risposta
-- non diventa mai la risposta della richiesta:
--
--   - la risposta DEGENERE (is_degenerate_completion: 200 con content vuoto,
--     zero tool-call, finish non terminale): l'inference e' avvenuta, il
--     provider l'ha fatturata con un usage reale sul wire, e la risposta viene
--     convertita in Err(empty_completion) e buttata mentre la chain passa oltre;
--   - il cap PER-TENTATIVO scaduto DOPO l'avvio della chiamata: la connessione
--     viene chiusa, nessun usage e' osservato, ma il provider puo' aver generato
--     (e fatturato) comunque.
--
-- In entrambi i casi il costo reale eccede il costo misurato di una quota che
-- oggi non e' osservabile da nessuna query. Questa migrazione da' a quelle
-- righe uno status proprio e una causa TIPIZZATA (regola Q: l'esito in un
-- campo, mai nel testo dei details).
--
-- Vocabolario di discard_reason (canonico, regola N — solo cio' che il codice
-- PRODUCE oggi, niente valori "per il futuro"):
--   - 'degenerate_hollow': risposta degenere, usage reale dal wire;
--   - 'attempt_timeout':   cap per-tentativo scaduto dopo l'avvio, zero token
--                          (spesa possibile ma non osservata, e lo zero e'
--                          dichiarato dalla causa stessa).
--
-- Decisione contabile (12/08/2026): le 'degenerate_hollow' CONSUMANO quota
-- (spesa reale fatturata dal provider); le 'attempt_timeout' no (nessun usage
-- osservato: contare zero non sposta la quota, e inventare un numero sarebbe
-- peggio). Il filtro sta in nexus-ledger (quote.rs, usage_for_quotas) e il
-- criterio in DiscardReason::consuma_quota — punto unico, regola L.
--
-- La riga 'discarded' e' TERMINALE: nasce chiusa (finalized_at valorizzato),
-- non e' mai prenotabile ne' finalizzabile, e non viene MAI dichiarata sul wire
-- (LedgerOutcome resta solo sulla risposta di successo): il contratto
-- anti-doppio-addebito con mcp-core non cambia.

-- Il CHECK originale e' inline nella 0006 (nome auto-generato da Postgres).
ALTER TABLE ai_usage_ledger
    DROP CONSTRAINT ai_usage_ledger_status_check;

ALTER TABLE ai_usage_ledger
    ADD CONSTRAINT ai_usage_ledger_status_check
    CHECK (status IN ('reserved', 'finalized', 'rejected', 'failed', 'released', 'discarded'));

ALTER TABLE ai_usage_ledger
    ADD COLUMN discard_reason TEXT;

ALTER TABLE ai_usage_ledger
    ADD CONSTRAINT ai_usage_ledger_discard_reason_check
    CHECK (discard_reason IN ('degenerate_hollow', 'attempt_timeout'));

-- Una riga scartata senza causa (o una causa su una riga non scartata) non e'
-- rappresentabile: "non ho classificato" non deve poter degradare a silenzio.
ALTER TABLE ai_usage_ledger
    ADD CONSTRAINT ai_usage_ledger_discard_coerenza_check
    CHECK ((status = 'discarded') = (discard_reason IS NOT NULL));

COMMENT ON COLUMN ai_usage_ledger.discard_reason IS
    'Causa tipizzata di una riga discarded (degenerate_hollow | attempt_timeout). NOT NULL se e solo se status=discarded. Scrittore unico: nexus_ledger::record_discarded.';
