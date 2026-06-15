-- 0422_closure_judge_active.sql
--
-- Promozione del closure_judge da SHADOW a DECISORE (de-lessicalizzazione esito).
--
-- La mig 0391 ha introdotto il judge LLM binario di chiusura in modalita' SHADOW
-- (agent.closure_judge.shadow_enabled): gira sui casi ambigui e registra il
-- DISACCORDO con la blacklist lessicale _detect_unfulfilled_intent senza cambiare
-- la decisione. La finestra di confronto ha mostrato il judge piu' accurato della
-- blacklist (es. run di remediation con output "non risolto": judge=unfulfilled
-- corretto, blacklist=fulfilled errato -> il run veniva chiuso come a posto).
--
-- Questa migrazione attiva il judge come DECISORE: quando l'esito NON e'
-- dichiarato via task_complete (segnale PRIMARIO, invariato), l'executor_node
-- interpella judge() a fine turno e scrive closure_verdict nello state;
-- route_after_executor usa quel verdetto come segnale "compiuto/non compiuto" al
-- posto della blacklist lessicale (che resta come FALLBACK solo quando il judge
-- si astiene: provider down / timeout / verdetto non parsabile). Gerarchia:
--   task_complete -> structural_unfulfilled_signal -> closure_judge -> blacklist.
--
-- regola H: niente rimozione cieca delle blacklist (restano come fallback);
-- regola G: nessun modello hardcoded (purpose closure_judge, tier light, mig 0391);
-- DB-driven: disattivabile (value='false') senza redeploy, cache 60s lato brain.
-- shadow_enabled resta attivo: la telemetria di confronto continua a girare anche
-- dopo la promozione (misura le divergenze sui casi in cui il judge non decide).
--
-- Idempotente.

BEGIN;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.closure_judge.active',
    'true',
    'agent',
    'Promozione mig 0422: se true il verdetto del closure_judge DECIDE l''esito "compiuto/non compiuto" (l''executor_node scrive closure_verdict, route_after_executor lo usa al posto della blacklist lessicale _detect_unfulfilled_intent, che resta solo come fallback in caso di astensione del judge). false = comportamento storico SHADOW (solo telemetria). DB-driven, cache 60s. Il task_complete (declared_outcome) resta sempre il segnale PRIMARIO.'
)
ON CONFLICT (key) DO NOTHING;

COMMIT;
