-- Migrazione 0189: aggiunge tool_runner_addr alla tabella settings
--
-- La porta del ToolRunner gRPC (usata sia da mcp-core che dal brain Python)
-- era precedentemente hardcoded nel codice (50071) e nell'env var
-- TOOL_RUNNER_ADDR (.env), creando un mismatch quando il brain non ereditava
-- la stessa env var di mcp-core.
--
-- Con questa migrazione, entrambi i servizi leggono l'indirizzo canonico
-- dalla tabella settings. L'env var TOOL_RUNNER_ADDR resta come override
-- di emergenza con priorita' piu' alta.
--
-- Richiede riavvio di mcp-core E del brain per applicare la modifica.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('tool_runner_addr',
     '127.0.0.1:50071',
     'agent',
     'Indirizzo host:porta del server gRPC ToolRunner esposto da mcp-core e '
     'usato dal brain Python per eseguire i tool MCP (read_file, write_file, '
     'run_command, ecc.). Entrambi i servizi leggono questo valore. '
     'Override di emergenza: TOOL_RUNNER_ADDR. '
     'Richiede riavvio di mcp-core e del brain.',
     FALSE)
ON CONFLICT (key) DO UPDATE
    SET description = EXCLUDED.description,
        category    = EXCLUDED.category;
