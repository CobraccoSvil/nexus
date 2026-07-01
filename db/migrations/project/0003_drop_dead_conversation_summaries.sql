-- Schema morto anche nei DB per-progetto: nexus_conversation_summaries era stata
-- replicata in 0001_chat.sql ma nessun codice la scrive (superata dal messaggio
-- role='summary' su chat_messages). La rimuoviamo dai DB-progetto (regola H).
-- Idempotente: DROP IF EXISTS su tabella vuota.

DROP TABLE IF EXISTS nexus_conversation_summaries;
