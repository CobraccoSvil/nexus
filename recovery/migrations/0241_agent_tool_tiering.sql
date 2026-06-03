-- 0241_agent_tool_tiering.sql
--
-- Tool tiering per ridurre il payload dei tool inviato al modello.
--
-- Problema: AGENT_TOOLS_JSON contiene 80 tool definition (~202KB / ~50k token)
-- inviate INTERE in ogni prompt, anche per richieste banali. Questo gonfia il
-- prompt a ~59k token, fa scattare ctx_needed elevato che esclude i modelli
-- leggeri e forza fallback su modelli grandi/lenti.
--
-- Soluzione: inviare solo un CORE di tool essenziali + lasciar scoprire gli
-- altri on-demand via nexus_mcp_tool_search (che ritorna i builtin con schema
-- completo) e invocarli via nexus_mcp_tool_call (server_id="builtin").
--
-- Letti da crates/mcp-core/src/brain_agent_client.rs:
--   - is_tiering_enabled()  -> agent.tools.tiering_enabled
--   - load_core_tools()     -> agent.tools.core_whitelist (CSV)
-- Fallback hardcoded (CORE_TOOLS_FALLBACK) solo per DB down (regola G CLAUDE.md).
--
-- I nomi tool nel CORE sono verificati 1:1 contro AGENT_TOOLS_JSON in
-- crates/mcp-core/src/agent_tools/mod.rs.

INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('agent.tools.tiering_enabled', 'true', 'agent', 'Abilita il tool tiering: invia al modello solo il CORE di tool + discovery (nexus_mcp_tool_search/call). Disattivare per esporre tutti gli 80 tool.', FALSE),
    ('agent.tools.core_whitelist', 'read_file,read_file_lines,write_file,edit_file,list_files,search_in_files,run_command,run_service,request_port,git_status,run_tests,search_codebase_semantic,nexus_mcp_tool_search,nexus_mcp_tool_call', 'agent', 'CSV dei tool CORE sempre esposti quando il tool tiering e attivo. Gli altri tool restano scopribili via nexus_mcp_tool_search.', FALSE)
ON CONFLICT (key) DO NOTHING;
