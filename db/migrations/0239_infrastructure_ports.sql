-- 0239_infrastructure_ports.sql
--
-- Seed completo delle porte dei servizi infrastruttura Nexus nella tabella
-- `settings` (categoria `infrastructure`), gestibile dal pannello admin
-- /admin/settings/infrastructure.
--
-- Razionale: prima di questa migrazione solo 4 chiavi infrastruttura erano in
-- DB (tool_runner_addr, agent_router_addr, brain_rest_url, mcp_core_url),
-- tutte le altre porte erano hardcoded in .env, deploy-local.sh o nei
-- sorgenti. La sessione del 2026-05-31 ha rivelato un bug grave:
-- `settings.tool_runner_addr` era impostato a 127.0.0.1:50501, identico ad
-- agent_router_addr -> il brain Python si connetteva ad AgentRouter
-- invece del ToolRunner e nessun tool veniva eseguito ("Il sistema non
-- riesce a connettersi ai servizi gRPC di Nexus").
--
-- Realta' del bind di mcp-core (verificata con ss -tlnp):
--   - 127.0.0.1:50500  -> ToolRunner gRPC  (env TOOL_RUNNER_ADDR=...:50500)
--   - 127.0.0.1:50501  -> AgentRouter gRPC (env AGENT_ROUTER_ADDR=...:50501)
--   - 0.0.0.0:4000     -> HTTP REST
-- Le env var .env hanno priorita' sui setting DB, quindi mcp-core bindava
-- correttamente; il brain pero' leggeva il DB sbagliato e finiva sulla porta
-- dell'agent_router.
--
-- Fix definitivo (regola H, mai toppe):
-- 1. UPDATE tool_runner_addr da 50501 a 50500 (allineamento alla realta')
-- 2. INSERT/UPSERT di TUTTE le porte servizi mancanti
-- 3. Tutte le chiavi *_port / *_addr / *_url sono in categoria 'infrastructure'
--    cosi' il pannello admin esistente le mostra in un unico posto.
--
-- Override emergenza (priorita' piu' alta): env var omonime (TOOL_RUNNER_ADDR,
-- AGENT_ROUTER_ADDR, MCP_CORE_HTTP_PORT, ecc.).

-- ============================================================================
-- FIX 1: correggi il valore errato di tool_runner_addr (50501 -> 50500)
-- ============================================================================

UPDATE settings
   SET value = '127.0.0.1:50500',
       description = 'Indirizzo host:porta del server gRPC ToolRunner esposto '
            'da mcp-core e usato dal brain Python per eseguire i tool MCP '
            '(read_file, write_file, run_command, ecc.). Entrambi i servizi '
            'leggono questo valore. Override di emergenza: TOOL_RUNNER_ADDR. '
            'Richiede riavvio di mcp-core e del brain. '
            'NOTA: deve essere DIVERSO da agent_router_addr (porte distinte).',
       category = 'infrastructure',
       updated_at = NOW()
 WHERE key = 'tool_runner_addr';

-- ============================================================================
-- FIX 2: allinea categoria di agent_router_addr (era 'agent', sposto in
-- 'infrastructure' per averli tutti raggruppati nel pannello admin)
-- ============================================================================

UPDATE settings
   SET category = 'infrastructure',
       updated_at = NOW()
 WHERE key IN ('agent_router_addr', 'tool_runner_addr');

-- ============================================================================
-- SEED: porte servizi infrastruttura mancanti
-- ============================================================================

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    -- mcp-core HTTP REST
    ('mcp_core_http_port',
     '4000',
     'infrastructure',
     'Porta HTTP del server REST mcp-core (default 4000). '
     'Override di emergenza: MCP_SERVER_PORT o MCP_CORE_HTTP_PORT. '
     'Richiede riavvio di mcp-core. '
     'Cambiandola servono anche aggiornamenti a mcp_core_url e web-ide proxy.',
     FALSE),

    -- brain Python REST
    ('brain_rest_port',
     '8001',
     'infrastructure',
     'Porta del server REST FastAPI del brain Python (default 8001). '
     'Override di emergenza: BRAIN_REST_PORT. '
     'Richiede riavvio del brain. '
     'Cambiandola serve aggiornare anche brain_rest_url.',
     FALSE),

    -- brain Python gRPC (servizio classifier/embedding)
    ('brain_grpc_port',
     '50051',
     'infrastructure',
     'Porta del server gRPC del brain Python (NeuralCoreService): classifier, '
     'embedding, routing. Default 50051. '
     'Override di emergenza: BRAIN_GRPC_PORT. '
     'Richiede riavvio del brain.',
     FALSE),

    -- gateway LLM
    ('nexus_gateway_port',
     '4060',
     'infrastructure',
     'Porta HTTP del nexus-gateway (Node.js, proxy LLM unificato). '
     'Default 4060. Override di emergenza: NEXUS_GATEWAY_PORT.',
     FALSE),

    -- web-ide (Next.js dev/prod)
    ('web_ide_port',
     '3000',
     'infrastructure',
     'Porta HTTP del frontend web-ide Next.js (default 3000). '
     'Override di emergenza: WEB_APP_PORT o PORT.',
     FALSE),

    -- microservizi builtin
    ('admin_service_port',
     '4010',
     'infrastructure',
     'Porta HTTP del microservizio admin-service (default 4010). '
     'Override: ADMIN_SERVICE_PORT.',
     FALSE),

    ('chat_service_port',
     '4020',
     'infrastructure',
     'Porta HTTP del microservizio chat-service (default 4020). '
     'Override: CHAT_SERVICE_PORT.',
     FALSE),

    ('doc_service_port',
     '4030',
     'infrastructure',
     'Porta HTTP del microservizio doc-service (default 4030). '
     'Override: DOC_SERVICE_PORT.',
     FALSE),

    ('billing_service_port',
     '4040',
     'infrastructure',
     'Porta HTTP del microservizio billing-service (default 4040). '
     'Override: BILLING_SERVICE_PORT.',
     FALSE),

    ('plugin_service_port',
     '4050',
     'infrastructure',
     'Porta HTTP del microservizio plugin-service (default 4050). '
     'Override: PLUGIN_SERVICE_PORT.',
     FALSE),

    ('browser_bridge_port',
     '4055',
     'infrastructure',
     'Porta HTTP del browser-bridge-mcp (default 4055). '
     'Override: BROWSER_BRIDGE_PORT.',
     FALSE)

ON CONFLICT (key) DO UPDATE
    SET description = EXCLUDED.description,
        category    = EXCLUDED.category,
        updated_at  = NOW();
