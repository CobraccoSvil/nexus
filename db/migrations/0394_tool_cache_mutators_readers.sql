-- 0394_tool_cache_mutators_readers.sql
--
-- Coerenza della cache tool_result dopo mutazioni del filesystem.
--
-- Incidente Beauty-Book 2026-06-11 (run 704a5c19): la skiplist della cache
-- (default legacy "run_command,write_file,edit_file,delete_file") non escludeva
-- ne' rename_file ne' il meta-tool nexus_mcp_tool_call (dietro cui passano
-- estrattori che scrivono su disco, es. nexus_extract_figma_code). Il modello ha
-- ricevuto manifest e list_files cacheati di ~700 secondi che descrivevano una
-- directory PIENA mentre i suoi stessi rename l'avevano appena svuotata -> ha
-- chiuso con un resoconto falso ("scritto in figma_export/") senza poterlo
-- sapere: il sistema gli mentiva.
--
-- Fix (codice mcp-core, punto unico agent_tool_result_cache):
-- 1. nuova lista MUTATORS (default nel codice, override qui): tool che mutano
--    filesystem/progetto. Mai cacheabili STRUTTURALMENTE (indipendentemente
--    dalla skiplist) e, dopo l'esecuzione, invalidano le entry cache dei READERS.
-- 2. nuova lista READERS: tool di lettura le cui entry vengono invalidate a
--    ogni mutazione, cosi' il modello non vede mai uno stato antecedente.
--
-- Le chiavi sono ASSENTI di default (il codice usa i propri default aggiornati);
-- questo INSERT le materializza per renderle visibili/modificabili da admin.
-- Idempotente.

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.tools.result_cache_mutators',
    'write_file,edit_file,delete_file,rename_file,file_write,fs_copy,fs_mkdir,fs_move,format_file,run_lint_fix,run_command,command,run_in_terminal,git_command,git_pull,git_commit,git_stage,git_push,nexus_extract_figma_code,nexus_install_shadcn_components,nexus_mcp_tool_call,cargo_install,run_service,service_restart,stop_service',
    'agent',
    'Tool che MUTANO filesystem/progetto: mai cacheati nella tool_result_cache e, dopo l''esecuzione, invalidano le entry dei reader (vedi result_cache_readers). CSV.'
)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.tools.result_cache_readers',
    'list_files,read_file,read_file_lines,file_read,find,search_in_files,git_status,nexus_verify_scaffold,tail_service_logs,read_service_output,list_active_services',
    'agent',
    'Tool di LETTURA dello stato filesystem/progetto: le loro entry nella tool_result_cache vengono invalidate quando un mutatore viene eseguito. CSV.'
)
ON CONFLICT (key) DO NOTHING;

-- Allinea la skiplist legacy SOLO se e' rimasta al default storico (un valore
-- personalizzato dall'admin non viene toccato). Con il nuovo codice i mutatori
-- sono comunque esclusi strutturalmente: questo UPDATE e' igiene visiva admin.
UPDATE settings
SET value = 'run_command,write_file,edit_file,delete_file,rename_file,nexus_mcp_tool_call'
WHERE key = 'agent.tools.result_cache_skip_for'
  AND value = 'run_command,write_file,edit_file,delete_file';
