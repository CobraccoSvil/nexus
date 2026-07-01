-- Rimozione di 4 tabelle SCHEMA MORTO (regola H: si chiude la causa, non il
-- sintomo). Verificato con audit su tutto il repo (Rust + Python + TS): create da
-- vecchie migrazioni ma con ZERO writer/INSERT applicativi, quindi eternamente
-- vuote e senza scopo.
--
--   nexus_agent_clarifications (creata mig 0158): il grafo agentico NON persiste
--     qui le clarification (restano nello stato del run / nexus_agent_meta_steps).
--   nexus_conversation_summaries (creata mig 0119): superata dal messaggio
--     role='summary' su chat_messages; nessun codice la scrive.
--   nexus_e2e_runs (creata mig 0177): un poller la LEGGEVA ogni 300s ma nessun
--     codice ha mai scritto (vedi commento in main.rs); il poller e' gia' rimosso.
--   nexus_events_audit (creata mig 0164): nessun writer.
--
-- DROP IF EXISTS: idempotente e sicuro (tabelle vuote, nessuna FK entrante).

DROP TABLE IF EXISTS nexus_agent_clarifications;
DROP TABLE IF EXISTS nexus_conversation_summaries;
DROP TABLE IF EXISTS nexus_e2e_runs;
DROP TABLE IF EXISTS nexus_events_audit;
