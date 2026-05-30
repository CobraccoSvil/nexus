-- Migrazione 0190: centralizza URL dei servizi interni nella tabella settings
--
-- Queste configurazioni erano precedentemente SOLO in env var, senza colonna
-- nel DB. Cio' creava lo stesso tipo di mismatch risolto in 0189 per
-- tool_runner_addr: se un servizio non eredita la stessa env var, usa il
-- fallback hardcoded e si connette alla porta sbagliata.
--
-- Con questa migrazione, brain_rest_url, mcp_core_url e agent_router_addr
-- diventano configurabili dal DB (admin panel). Le env var omonime restano
-- come override di emergenza con priorita' piu' alta.
--
-- Nota: qdrant_url e neural_core_url sono gia' nella tabella settings
-- (dalla migrazione 0002), quindi non vengono ripetute qui.

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    ('brain_rest_url',
     'http://127.0.0.1:8001',
     'infrastructure',
     'URL del server REST del brain Python (FastAPI su porta 8001). '
     'Usato da mcp-core per chiamare /agent/run/stream, /classify-intent-agentic, '
     '/catalog/sync e altri endpoint REST del brain. '
     'Override di emergenza: BRAIN_REST_URL o NEURAL_CORE_REST_URL. '
     'Richiede riavvio di mcp-core.',
     FALSE),

    ('mcp_core_url',
     'http://127.0.0.1:4000',
     'infrastructure',
     'URL del server HTTP mcp-core (porta 4000). '
     'Usato dal brain Python per leggere settings via _get_core_setting(), '
     'dal router semantico, dal cooldown bridge e dall''agent router client. '
     'Override di emergenza: MCP_CORE_URL. '
     'Richiede riavvio del brain.',
     FALSE),

    ('agent_router_addr',
     '127.0.0.1:50501',
     'agent',
     'Indirizzo host:porta del server gRPC AgentRouter esposto da mcp-core '
     'e usato dal brain Python per consultare il Q-Learning router. '
     'Override di emergenza: AGENT_ROUTER_ADDR. '
     'Richiede riavvio di mcp-core e del brain.',
     FALSE)

ON CONFLICT (key) DO UPDATE
    SET description = EXCLUDED.description,
        category    = EXCLUDED.category;
