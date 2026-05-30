-- Migrazione 0191: direttiva esplicita port_registry nei system prompt agente.
--
-- Problema: il tool request_port(label) esiste dalla mig 0141 ed espone una
-- porta libera dal bucket 20000-39999 tracciata in nexus_port_allocations.
-- Tuttavia gli agenti continuano a hardcodare 3000/8080/5173 nei sorgenti.
-- Causa radice: i system prompt base non istruiscono ESPLICITAMENTE che la
-- scelta della porta passa SEMPRE per request_port.
--
-- Fix definitivo: aggiunge un blocco <port_allocation> ai system prompt
-- system.nexus_base e agent.coder.base. Idempotente: rileva la presenza
-- del blocco e lo aggiunge solo se mancante.
--
-- Riferimenti:
--  - Tool: crates/mcp-core/src/agent_tools/ports.rs
--  - Endpoint: POST /api/projects/:id/services/allocate-port (main.rs)
--  - Allocazione: nexus_port_allocations bucket 20000-39999

DO $$
DECLARE
    directive TEXT := E'\n\n<port_allocation>\n'
        || E'Quando crei un servizio che apre una porta TCP (server HTTP, gRPC, WebSocket, database, qualsiasi listener), DEVI prima chiamare il tool `request_port(label="<nome_servizio>")` per ottenere una porta libera dal registry Nexus. NON hardcodare mai 3000, 8080, 5173 o altre porte fisse.\n\n'
        || E'Workflow obbligatorio:\n'
        || E'1. Decidi il nome del servizio (es. "web-frontend", "api-backend", "postgres-dev").\n'
        || E'2. Chiama request_port(label=<nome>). Ottieni un numero di porta nel range 20000-39999.\n'
        || E'3. Usa QUEL numero nel codice che scrivi (server.listen(PORT), config DB, ecc.).\n'
        || E'4. Documenta in una variabile env o commento che la porta e'' stata allocata da Nexus.\n\n'
        || E'Se ignori questa regola e hardcodi una porta, il servizio andra'' in conflitto con altri progetti che girano sulla stessa macchina.\n'
        || E'</port_allocation>';
BEGIN
    -- system.nexus_base
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'system.nexus_base'
       AND content NOT LIKE '%<port_allocation>%';

    -- agent.coder.base
    UPDATE nexus_prompt_templates
       SET content = content || directive,
           updated_at = now()
     WHERE key = 'agent.coder.base'
       AND content NOT LIKE '%<port_allocation>%';
END $$;
