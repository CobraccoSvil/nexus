-- 0247_discovery_first_reenable.sql
-- M16 — Riabilita la progressive tool disclosure (discovery-first).
--
-- Corregge 0246 (che l'aveva disabilitata su diagnosi errata). Verifica E2E
-- dalla UI: con discovery-first ON il modello riceve solo i 2 meta-tool
-- (nexus_mcp_tool_search / nexus_mcp_tool_call) e, FORZATO da tool_choice al
-- primo turno (tool_choice_first_turn_force=true nelle capabilities), usa
-- nexus_mcp_tool_call per eseguire i tool builtin (es. write_file) -> il file
-- viene creato (verificato: zeta.txt creato con tools=2). Negli agent_steps il
-- tool compare col nome target (write_file) perche' call esegue il tool reale.
-- Il prompt resta minimo (2 tool invece di ~22/479), che e' lo scopo di M16.
--
-- Il "discovered_in=0" osservato non e' un fallimento: il modello chiama
-- direttamente i tool builtin via call (nome noto) anziche' passare per search;
-- search resta disponibile per scoprire i tool meno comuni.
-- Idempotente.

INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'agent.tools.discovery_first_enabled', 'true', 'agent',
    'M16 progressive tool disclosure. ON: il modello riceve solo i 2 meta-tool (search/call) ed esegue i tool builtin via nexus_mcp_tool_call (tool_choice forzato al primo turno). Prompt minimo.',
    'f'
)
ON CONFLICT (key) DO UPDATE SET value = 'true', updated_at = NOW();
