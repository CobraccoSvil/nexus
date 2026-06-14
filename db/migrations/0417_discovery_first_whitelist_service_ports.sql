-- 0417_discovery_first_whitelist_service_ports.sql
-- Fix: discovery-first nascondeva i tool di gestione PORTE e SERVIZI.
--
-- Root cause (test E2E beauty-book 2026-06-14): con
-- agent.tools.discovery_first_enabled=true, il modello riceve solo i tool nella
-- whitelist agent.tools.discovery_first_whitelist; gli altri vanno "scoperti" via
-- nexus_mcp_tool_search. La whitelist conteneva il dominio FILE completo
-- (list_files/read_file/write_file/edit_file/run_command/...), ma NON i tool di
-- porte/servizi. Risultato: per avviare un'app l'agente chiama request_port (come
-- istruito dai system prompt, regola I/CLAUDE.md), il brain lo RIFIUTA
-- ("non disponibile direttamente, usa nexus_mcp_tool_search") e il modello entra
-- in loop chiamandolo a memoria (brain/agents/nodes/__init__.py M16). Le porte non
-- vengono mai allocate e l'agente CONFABULA "porte allocate" (run hollow). Stesso
-- guasto descritto dal commento del codice: "i tool core ... verrebbero rifiutati
-- e il modello entra in loop search->reject".
--
-- Fix definitivo (regola L: la whitelist e' il punto unico dei tool core esposti;
-- regola G: config nel DB). Aggiunge il dominio porte/servizi, simmetrico al
-- dominio file gia' presente. La stessa whitelist e' letta sia da mcp-core
-- (build_tools_json: ESPONE i tool) sia dal brain (validazione M16: li AMMETTE),
-- cache 60s -> nessun restart necessario, attivo entro la TTL. Idempotente:
-- ogni tool e' aggiunto solo se non gia' presente (preserva personalizzazioni).

BEGIN;

DO $$
DECLARE
    needed TEXT[] := ARRAY[
        'request_port',
        'nexus_list_ports',
        'run_service',
        'read_service_output',
        'stop_service',
        'service_restart',
        'tail_service_logs',
        'list_active_services'
    ];
    tool TEXT;
BEGIN
    FOREACH tool IN ARRAY needed LOOP
        UPDATE settings
        SET value = value || ',' || tool,
            updated_at = NOW()
        WHERE key = 'agent.tools.discovery_first_whitelist'
          AND ',' || value || ',' NOT LIKE '%,' || tool || ',%';
    END LOOP;
END $$;

COMMIT;
