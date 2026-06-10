-- 0386_deterministic_run_outcome_settings.sql
-- Settings per la macchina a stati di terminazione deterministica dei run agentici.
--
-- Contesto (regola G: configurazione nel DB, niente hardcode nel codice; regola H:
-- fix alla causa radice del "finale ambiguo"). Due parametri operativi:
--
-- 1) clarify.max_attempts (category 'orchestrator', coerente col loader
--    _load_config in brain/agents/clarify_or_expand_node.py che legge
--    category='orchestrator' AND key LIKE 'clarify.%'): tetto ai tentativi di
--    richiesta-chiarimento per run. Oltre la soglia il clarify_or_expand_node
--    procede col candidato a confidence piu' alta (fail-open verso l'azione)
--    invece di ri-chiedere la stessa domanda, eliminando il loop di
--    disambiguazione ripetuta. Default 1: al massimo una domanda per run.
--
-- 2) agent.repeated_action_force_diagnose_enabled: abilita lo stadio intermedio
--    "force_diagnose" nel progress_controller per l'asse repeated_action. Prima
--    di abortire, l'agente che ripete la stessa azione produttiva senza progresso
--    viene obbligato a leggere il tool_result d'errore, spiegare il fallimento e
--    cambiare strategia. Cosi' un eventuale esito FAILED include sempre una
--    diagnosi, mai una chiusura grezza.
--
-- NB: nessuna mappa semantica statica intent->classe (es. routing.intent_behavior_classes).
-- L'interpretazione del testo resta affidata SOLO al classifier LLM esistente
-- (brain/router/agentic_classifier.py); il SemanticRouter ha gia' rimosso ogni
-- matching keyword/embedding (vedi brain/router/service.py). Niente euristiche
-- reintrodotte qui.
--
-- Idempotente.

-- Pulizia difensiva: una bozza precedente di questa stessa migrazione aveva usato
-- la chiave errata 'agent.clarify.max_attempts' (category 'agent'), che il loader
-- clarify non legge mai. Rimossa se presente (no-op se non c'e').
DELETE FROM settings WHERE key = 'agent.clarify.max_attempts';

INSERT INTO settings (key, value, category, description)
VALUES (
    'clarify.max_attempts',
    '1',
    'orchestrator',
    'Numero massimo di richieste di chiarimento (clarify/disambiguazione) per run. Oltre la soglia l''agente procede col candidato piu'' probabile invece di ri-chiedere (fail-open verso l''azione), evitando domande ripetute.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.repeated_action_force_diagnose_enabled',
    'true',
    'agent',
    'Se true, prima di abortire per azione ripetuta il progress controller obbliga l''agente a diagnosticare il fallimento (leggere l''errore) e cambiare strategia, cosi'' l''esito FAILED include sempre una diagnosi.'
)
ON CONFLICT (key) DO NOTHING;
