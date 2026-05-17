-- 0160_prompt_provider_neutral_tools.sql
-- Rende la sezione <tool_usage> di agent.coder.base provider-neutral.
--
-- Problema: la lista esplicita dei tool name nel system prompt induce
-- modelli non-Anthropic (DeepSeek, Mistral) a emettere tag XML inline
-- invece di usare il meccanismo nativo tool_calls JSON.
-- Soluzione: rimuovere la lista tool dal prompt (sono gia' nel parametro
-- `tools` dell'API) e aggiungere istruzione esplicita per tool calling nativo.

UPDATE nexus_prompt_templates
SET content = REPLACE(
    content,
    E'<tool_usage>\nTool consentiti: read_file, read_file_lines, list_files, search_in_files,\nsearch_codebase_semantic, search_file_semantic, scan_code_quality,\nbatch_analyze_code, write_file, edit_file, git_status, git_stage, git_commit,\nrun_command, run_tests.\n\nBATCHING: nello stesso turno raggruppa piu'' read/edit indipendenti\n(esempio: 3 letture parallele + 2 edit nello stesso messaggio).\n\nCAP ITERAZIONI: massimo 12 iterazioni per task. Se al 10mo non hai concluso,\nprepara un report di stato e chiedi guida.\n</tool_usage>',
    E'<tool_usage>\nI tool disponibili sono quelli passati nel parametro tools della richiesta API.\nPer invocare un tool usa ESCLUSIVAMENTE il meccanismo nativo della tua API\n(function_call / tool_calls JSON). NON scrivere invocazioni come testo libero\n(no tag XML, no JSON inline nel messaggio, no pseudo-codice).\n\nBATCHING: nello stesso turno raggruppa piu'' chiamate indipendenti\n(esempio: 3 letture parallele + 2 edit nello stesso messaggio).\n\nCAP ITERAZIONI: massimo 12 iterazioni per task. Se al 10mo non hai concluso,\nprepara un report di stato e chiedi guida.\n</tool_usage>'
),
    version = version + 1,
    updated_at = NOW(),
    updated_by = 'migration_0160'
WHERE key = 'agent.coder.base'
  AND content LIKE '%Tool consentiti: read_file, read_file_lines%';
