-- 0483_mcp_stdio_call_timeout.sql
--
-- ROOT CAUSE (BUG d2 cause A, regola H + G): le chiamate ai server MCP esterni
-- via stdio (call_tool_stdio / list_tools_stdio in nexus-mcp-client) usavano un
-- deadline FISSO di 30s hardcoded nel codice e chiudevano stdin IMMEDIATAMENTE
-- dopo l'invio dei messaggi. Per server che avviano un browser lento (es.
-- @playwright/mcp) la finestra breve faceva ricevere SOLO la risposta id=1
-- (initialize -> serverInfo) e mai la id=2 (tools/call): l'utente vedeva la
-- "config Playwright" e il tool non veniva mai eseguito.
--
-- FIX: il timeout diventa DB-driven (regola G, niente hardcode), risolto dal
-- punto unico nexus_mcp_client::resolve_stdio_timeout (regola L). Lo stdin
-- viene chiuso DOPO aver ricevuto la risposta attesa (fix lifecycle nel codice).
-- Default >= 60s per coprire l'avvio del browser; alzabile senza redeploy.

BEGIN;

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.mcp.stdio_call_timeout_seconds', '60', 'agent',
     'Timeout massimo (secondi) per ricevere la risposta da un server MCP esterno via stdio (tools/call e tools/list). Deve coprire l''avvio di server lenti che lanciano un browser (es. @playwright/mcp). Risolto dal punto unico nexus_mcp_client::resolve_stdio_timeout; cache effettiva = durata della singola chiamata. Floor di sicurezza nel codice: 60s.',
     NOW())
ON CONFLICT (key) DO NOTHING;

COMMIT;
