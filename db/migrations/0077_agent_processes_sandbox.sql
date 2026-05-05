-- Aggiunge la colonna sandboxed ad agent_processes
-- per tracciare se il processo e stato eseguito in container Docker.
ALTER TABLE agent_processes
    ADD COLUMN IF NOT EXISTS sandboxed BOOLEAN NOT NULL DEFAULT FALSE;

-- Impostazioni sandbox (abilitazione, limiti risorse).
INSERT INTO settings (key, value, description) VALUES
    ('sandbox_enabled', 'true', 'Abilita isolamento Docker per i processi agente'),
    ('sandbox_memory_mb', '1024', 'Limite memoria sandbox in MB'),
    ('sandbox_cpus', '2', 'Limite CPU sandbox (core)')
ON CONFLICT (key) DO NOTHING;
