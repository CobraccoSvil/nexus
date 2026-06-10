-- 0388_agent_run_generation_phase.sql
-- Marca la fine della FASE GENERATIVA di un run, distinta dall'esito finalizzato.
--
-- Causa radice (incidente "la chat sembra libera ma 409" 2026-06-10): il guard
-- anti-run-concorrente in chat_messages/handlers.rs blocca un nuovo messaggio
-- finche' esiste un run con status IN ('running','awaiting_confirmation')
-- creato da <15 minuti. Ma nel grafo LangGraph l'ordine terminale e'
-- executor -> reflection -> regression_gate -> learner -> END: l'evento SSE
-- `end_turn` (che libera il pulsante di invio nel frontend) viene emesso quando
-- l'executor finisce, MENTRE reflection_node esegue ancora una chiamata LLM di
-- valutazione (campionata via reflection_sample_rate) che dura secondi. In
-- quella finestra il frontend e' libero ma il run e' ancora 'running' -> il
-- nuovo messaggio (o l'auto-continuazione in modalita' Continuo) prende 409.
--
-- Fix infrastrutturale (regola H + L): mcp-core marca `generation_ended_at`
-- all'istante dell'end_turn (in brain_agent_client::run_via_brain). Il guard
-- 409 considera "veramente attivo" solo un run con generation_ended_at IS NULL:
-- reflection/learner (post-processing che non cambia l'esito ne' la risposta
-- all'utente) non bloccano piu' la sessione. L'esito canonico resta derivato e
-- scritto a fine stream (finalize_agent_run): la colonna marca la FASE, non
-- l'esito (separazione di responsabilita').
--
-- Sicurezza concorrenza: nel caso raro in cui regression_gate rilanci a executor
-- dopo l'end_turn, generation_ended_at resta settato e un nuovo messaggio
-- partirebbe superando il run (mig 0370 supersede_active_runs, last-wins +
-- cancellazione cooperativa _check_superseded nel brain). Nessun doppio stream
-- persistente: il vecchio run si auto-cancella.
--
-- awaiting_confirmation NON e' impattato: un run in pausa-conferma non ha
-- emesso end_turn, quindi generation_ended_at IS NULL e resta bloccante (giusto:
-- e' una pausa reale che aspetta l'utente).
--
-- Idempotente.

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS generation_ended_at TIMESTAMPTZ;

COMMENT ON COLUMN agent_runs.generation_ended_at IS
    'Istante in cui la fase generativa del run e'' terminata (evento SSE end_turn). NULL = generazione ancora in corso (o run in awaiting_confirmation). Distinto da completed_at (esito canonico finalizzato dopo reflection/learner). Usato dal guard anti-run-concorrente per non bloccare la sessione durante il post-processing. Vedi mig 0388.';
