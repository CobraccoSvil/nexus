-- 0246_discovery_first_default_off.sql
-- M16 — Disabilita la progressive tool disclosure (discovery-first).
--
-- Motivazione (verifica E2E dalla UI): il pattern previsto (turno 1 solo
-- nexus_mcp_tool_search/call -> turno 2 tool scoperti iniettati -> turno 3 di
-- nuovo search) NON si attiva mai: i modelli, vedendo solo i 2 meta-tool con
-- tool_choice='auto', NON invocano nexus_mcp_tool_search (discovered_in=0 ad
-- ogni turno) e tendono a "narrare senza agire". Il sistema funziona invece
-- correttamente esponendo i tool direttamente, con il tool_choice forzato al
-- primo turno (cablaggio provider Fase A) che evita il narrate-without-act.
--
-- Disabilitiamo la feature (kill-switch). Per riabilitarla in futuro serve:
--   1) forzare tool_choice (required/any) quando il set esposto e' solo i
--      meta-tool di discovery, cosi' il modello e' costretto a cercare;
--   2) allineare il flusso executor affinche' il filtro discovery-first sia
--      applicato e i tool scoperti vengano iniettati nel turno successivo.
-- Idempotente.

INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'agent.tools.discovery_first_enabled', 'false', 'agent',
    'M16 progressive tool disclosure. OFF: il pattern search->inject non si attiva (i modelli non usano il meta-tool con tool_choice auto). Riabilitare solo con tool_choice forzato sul set discovery.',
    'f'
)
ON CONFLICT (key) DO UPDATE SET value = 'false', updated_at = NOW();
