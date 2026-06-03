-- 0257_discovery_first_core_tools.sql
-- Espone i tool core del filesystem al primo turno con discovery-first attivo.
--
-- Root cause (loop list_files/list_directory): con
-- agent.tools.discovery_first_whitelist = solo i 2 meta-tool
-- (nexus_mcp_tool_search, nexus_mcp_tool_call), al primo turno il modello riceve
-- SOLO la discovery e deve scoprire ogni tool via search prima di usarlo. Modelli
-- deboli (es. gemini-2.5-flash) non stabilizzano il ciclo search->use: chiamano
-- list_files/list_directory, M16 li rifiuta perche' "non scoperti", il modello
-- riprova -> loop -> abort senza risultato utile (la chat "non risolve").
--
-- Fix (regola G: la whitelist e' nel DB, unica fonte): aggiungere i tool core di
-- lettura/scrittura/comando alla whitelist, cosi' sono SEMPRE disponibili al
-- primo turno per i task comuni (file_ops) senza rinunciare al discovery per i
-- ~500 tool esotici. Lato brain la validazione M16 legge la stessa chiave e
-- ammette questi tool (nodes.py).
--
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.tools.discovery_first_whitelist',
    'nexus_mcp_tool_search,nexus_mcp_tool_call,list_files,read_file,read_file_lines,search_in_files,write_file,edit_file,run_command',
    'agent',
    'Tool esposti al primo turno quando discovery-first e'' attivo (CSV). Include i meta di discovery + i tool core del filesystem sempre disponibili (lettura/scrittura/comando). Gli altri tool restano scopribili via nexus_mcp_tool_search.'
)
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value,
    category = EXCLUDED.category,
    description = EXCLUDED.description,
    updated_at = NOW();
