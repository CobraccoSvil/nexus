-- 0630_review_correction_cycles.sql
--
-- CAUSA (diagnosi 21/07): i run di creazione app chiudono quasi sempre `failed`
-- con "Review adversariale: NON superata (difetti bloccanti)". La ReviewGate
-- scatta nel funnel di CHIUSURA (a fine run, sull'app intera) e
-- `orchestrator.review_max_correction_cycles` = 1 da' all'agente UN SOLO rimando
-- per correggere TUTTI i difetti bloccanti insieme: se al ri-esame resta anche un
-- solo finding `high` (review_fail_on_high_severity=true) la bocciatura diventa
-- RejectedFinal e il run chiude. Un ciclo non basta a far convergere N difetti.
--
-- FIX A (rapido, alto rendimento): alza il cap a 3 -> l'agente corregge a ONDATE
-- (fixa alcuni difetti per ciclo, ri-review, ripete). I run hanno tipicamente
-- ancora ~35 iterazioni residue quando arriva la review (usate 63-65 su cap 100),
-- quindi il collo di bottiglia e' il cap dei cicli, NON il budget iterazioni. 3
-- panel di ri-review in piu' hanno un costo (sub-run revisori) ma sbloccano la
-- convergenza. Il fix DEFINITIVO (B, shift-left: verifica build incrementale per
-- non accumulare i difetti fino alla fine) e' separato.
--
-- Reversibile a caldo (regola G): UPDATE del value, refresh cache <=60s, nessun
-- redeploy. Idempotente/replay-safe.
UPDATE settings
   SET value = '3', updated_at = NOW()
 WHERE key = 'orchestrator.review_max_correction_cycles';

INSERT INTO settings (key, value, category, description)
SELECT 'orchestrator.review_max_correction_cycles', '3', 'orchestrator',
       'Rimandi max in correzione dopo bocciatura ReviewGate (alzato 1->3, mig 0630: 1 ciclo non faceva convergere i difetti bloccanti trovati tutti a fine run).'
WHERE NOT EXISTS (SELECT 1 FROM settings WHERE key = 'orchestrator.review_max_correction_cycles');
