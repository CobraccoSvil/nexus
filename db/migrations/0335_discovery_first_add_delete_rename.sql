-- 0335_discovery_first_add_delete_rename.sql
-- Aggiunge delete_file e rename_file alla whitelist discovery-first.
--
-- Root cause: la mig 0257 ha esposto i tool core del filesystem al primo turno
-- (list_files, read_file, read_file_lines, search_in_files, write_file, edit_file,
-- run_command) ma ha OMESSO delete_file e rename_file. Con discovery-first attivo
-- (agent.tools.discovery_first_enabled=true), quando l'agente chiama delete_file
-- la validazione M16 (brain/agents/nodes/__init__.py) lo rifiuta come "non
-- scoperto" e impone un giro via nexus_mcp_tool_search, facendo fallire la
-- cancellazione nel turno (osservato: delete_file su .../index.html -> tool_result
-- "Il tool 'delete_file' non e' disponibile direttamente in questo turno").
--
-- delete_file e rename_file sono operazioni file CORE, gia' protette dal gate
-- can_write esattamente come write_file/edit_file (entrambi gia' in whitelist):
-- l'esclusione era una dimenticanza, non una scelta di sicurezza.
--
-- Fix (regola G: la whitelist e' nel DB, unica fonte; letta sia da mcp-core
-- build_tools_json sia dalla validazione M16 del brain): includere i due tool.
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.tools.discovery_first_whitelist',
    'nexus_mcp_tool_search,nexus_mcp_tool_call,list_files,read_file,read_file_lines,search_in_files,write_file,edit_file,run_command,delete_file,rename_file',
    'agent',
    'Tool esposti al primo turno quando discovery-first e'' attivo (CSV). Include i meta di discovery + i tool core del filesystem sempre disponibili (lettura/scrittura/modifica/cancellazione/rinomina/comando). Gli altri tool restano scopribili via nexus_mcp_tool_search.'
)
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value,
    category = EXCLUDED.category,
    description = EXCLUDED.description,
    updated_at = NOW();
