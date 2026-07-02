-- 0504: esposizione dei tool di orchestrazione sub-agenti al modello.
--
-- Root cause (stesso pattern della mig 0502/task_complete): dispatch_subagent
-- e dispatch_subagents erano definiti in AGENT_TOOLS_JSON con l'handler nativo
-- completo (subagent_native.rs: batch parallelo a ondate, tetto
-- orchestrator.max_parallel_subagents, 11 kind con prompt dedicati in
-- nexus_prompt_templates) e il system prompt ne parlava al modello — ma i tool
-- NON erano in nessuna whitelist attiva del catalogo. Con discovery-first
-- attivo (default) il modello non li ha MAI visti: zero righe in
-- nexus_subagent_runs da sempre. Nessun run ha mai potuto usare piu' agenti
-- in parallelo.
--
-- Scelta di design (esplicita):
--   - discovery_first / core / inline_core: SI (i set operativi standard);
--   - automation.study_mode_readonly_tools: NO — dispatchare un sub-agente
--     implementativo NON e' read-only;
--   - automation.o_series_essential_tools: NO — i modelli o-series hanno il
--     set ridotto proprio perche' gestiscono male cataloghi/deleghe complesse.
-- Il parallelismo resta governato dal setting admin
-- orchestrator.max_parallel_subagents (default 3, cap 8 dallo schema).
--
-- Append idempotente: solo se la chiave non contiene gia' dispatch_subagent.

UPDATE settings
   SET value = value || ',dispatch_subagent,dispatch_subagents'
 WHERE key IN (
         'agent.tools.discovery_first_whitelist',
         'agent.tools.core_whitelist',
         'agent.tools.inline_core_whitelist'
       )
   AND value NOT LIKE '%dispatch_subagent%';
