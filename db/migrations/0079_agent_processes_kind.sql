-- Aggiunge la colonna kind ad agent_processes per distinguere
-- i processi servizio (long-running, avviati dall'utente) dai task
-- effimeri (comandi brevi dell'agente, chiusi automaticamente al termine).
ALTER TABLE agent_processes
    ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'service';
