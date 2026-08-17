-- 0727_ledger_duration_ms.sql
--
-- Durata della chiamata al fornitore, PER RIGA di ledger (fase 4, lotto 2).
--
-- La latenza esiste gia' sulla risposta (LlmResponse.latency_ms, misurata dal
-- gateway attorno alla POST verso il fornitore) e moriva li': nessuna riga la
-- persisteva, quindi ogni domanda su "quanto ci mette DAVVERO questa coppia
-- provider/modello in esercizio" doveva ripiegare su ai_model_health_history,
-- che misura i probe sintetici e non il traffico reale.
--
-- NULL = riga scritta da un percorso che non la misura: righe storiche,
-- righe discarded senza risposta osservata (attempt_timeout), scritture
-- mcp-core (reserve/finalize). Mai 0: uno zero sarebbe una misura falsa
-- (regola Q — l'ignoto e' una variante dichiarata, non un valore comodo).
-- La scrive il solo produttore nexus_ledger::record_tokens, dal valore che
-- il gateway passa (billing::record_usage_to_ledger).

ALTER TABLE ai_usage_ledger
    ADD COLUMN IF NOT EXISTS duration_ms BIGINT NULL;

COMMENT ON COLUMN ai_usage_ledger.duration_ms IS
    'Durata della chiamata al fornitore misurata dal gateway (LlmResponse.latency_ms). NULL = riga scritta da un percorso che non la misura (righe storiche, discarded senza risposta, scritture mcp-core).';

-- Vista di lettura: percentili orari per coppia provider/modello sul SOLO
-- traffico finalized. `misurate` dichiara la premessa del numero (regola O):
-- un p50 su 3 righe misurate non e' un p50 su 3000, e senza il conteggio i
-- due sarebbero indistinguibili.
CREATE OR REPLACE VIEW v_ai_call_duration AS
SELECT provider,
       model,
       date_trunc('hour', created_at) AS ora,
       count(*) FILTER (WHERE duration_ms IS NOT NULL) AS misurate,
       percentile_cont(0.5)  WITHIN GROUP (ORDER BY duration_ms) AS p50_ms,
       percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms) AS p95_ms
  FROM ai_usage_ledger
 WHERE status = 'finalized'
 GROUP BY 1, 2, 3;

COMMENT ON VIEW v_ai_call_duration IS
    'Latenza oraria per coppia provider/modello dal traffico reale (ledger finalized). p50/p95 ignorano le righe non misurate; misurate dichiara su quante righe poggia il percentile.';
