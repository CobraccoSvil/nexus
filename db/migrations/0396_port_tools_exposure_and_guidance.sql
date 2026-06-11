-- 0396_port_tools_exposure_and_guidance.sql
--
-- Governance porte (parte preventiva): espone i tool Nexus dedicati ai run
-- agentici, aggiunge un playbook di dominio e aggiorna la direttiva di sistema.
--
-- Causa radice dell'incidente (Beauty-Book, 2026-06-11): su prompt "verifica la
-- gestione delle porte del progetto, usa i tools dedicati di nexus" l'agente non
-- ha mai potuto chiamare request_port (filtrato fuori dal tools_json per gli
-- intent di verifica) e non esisteva un tool READ-ONLY per ispezionare lo stato
-- porte. Risultato: report con porte hardcoded 5000/5173 e un edit con
-- `process.env.PORT || 5000` (fallback che eludeva lo scanner, ora corretto nel
-- codice). Qui si chiude il lato configurazione (regola G: DB unica fonte).
--
-- Nuovo tool read-only: nexus_list_ports (bucket + allocazioni). Esposizione del
-- nuovo tool e di request_port nelle whitelist DB-driven gia' esistenti.
-- Idempotente.

BEGIN;

-- 1. study mode (sola lettura): nexus_list_ports deve poter girare anche nei
--    task di sola verifica/analisi.
UPDATE settings
   SET value = value || ',nexus_list_ports'
 WHERE key = 'automation.study_mode_readonly_tools'
   AND value NOT LIKE '%nexus_list_ports%';

-- 2. inline core whitelist (discovery A.2): quando il messaggio di rifiuto dello
--    scanner dice "chiama request_port", il tool deve essere inline (niente giro
--    di discovery). Stesso per nexus_list_ports (verifica immediata).
UPDATE settings
   SET value = value || ',request_port'
 WHERE key = 'agent.tools.inline_core_whitelist'
   AND value NOT LIKE '%request_port%';
UPDATE settings
   SET value = value || ',nexus_list_ports'
 WHERE key = 'agent.tools.inline_core_whitelist'
   AND value NOT LIKE '%nexus_list_ports%';

-- 3. Playbook di dominio "gestione porte". Il matcher e' substring sul testo
--    utente (task_playbook.py: `k in text`): SOLO keyword multi-parola, altrimenti
--    "porta"/"port" matcherebbero "importante"/"report". Nessun gate intent: deve
--    scattare sia su verifica sia su implementazione.
INSERT INTO nexus_task_playbooks (key, title, description, trigger_json, guidance_text, category, priority, steps_json)
VALUES (
    'ops.port_management',
    'Gestione porte del progetto via tool Nexus',
    'Impone l''uso dei tool Nexus (nexus_list_ports per verifica, request_port per allocazione) e vieta porte hardcoded.',
    '{"keywords": ["gestione delle porte", "gestione porte", "porte del progetto", "allocazione porte", "allocazione delle porte", "porta tcp", "port allocation", "request_port", "nexus_port", "porta hardcoded", "porte hardcoded", "verifica le porte", "verifica della porta", "porte di rete", "quale porta", "quali porte"]}'::jsonb,
    'Gestione porte (regola inderogabile: ogni porta passa dai tool Nexus).
1. VERIFICA/AUDIT delle porte: chiama nexus_list_ports (sola lettura). Ritorna il bucket assegnato al progetto e le allocazioni registrate. Non dedurre MAI le porte leggendo solo i sorgenti.
2. OTTENERE una porta: SOLO request_port(label="<servizio>"). Mai scegliere numeri a mano, nemmeno dentro il bucket 20000-39999. Idempotente: stessa label, stessa porta.
3. Un fallback env con numero (process.env.PORT || 5000, os.environ.get("PORT", 5000), unwrap_or("3000")) E'' una porta hardcoded: viene rifiutato in scrittura. Se vuoi un default, usa la porta ALLOCATA da request_port.
4. Vietato aggirare lo scanner via run_command/sed/heredoc: il port enforcer termina i processi su porte non allocate (con audit e notifica).
5. Nei REPORT le raccomandazioni citano i tool Nexus e il bucket del progetto, mai 3000/5000/5173.',
    'operations',
    100,
    '[
      "Chiama nexus_list_ports per leggere il bucket del progetto e le allocazioni esistenti",
      "Se il task richiede una nuova porta, chiama request_port(label=\"<servizio>\") e usa la porta ritornata",
      "Per ogni porta hardcoded trovata nei sorgenti, sostituiscila con lettura da env e default uguale alla porta allocata; aggiorna .env",
      "Produci il report citando il bucket assegnato e le allocazioni Nexus, mai numeri arbitrari"
    ]'::jsonb
)
ON CONFLICT (key) DO NOTHING;

-- 4. Direttiva <port_allocation> v2 nei system prompt: aggiunge la verifica via
--    nexus_list_ports e il divieto esplicito dei fallback numerici. Marker di
--    idempotenza: presenza di 'nexus_list_ports' nel blocco. In Postgres `.`
--    matcha anche i newline di default in regexp_replace.
UPDATE nexus_prompt_templates
   SET content = regexp_replace(
        content,
        '<port_allocation>.*</port_allocation>',
        '<port_allocation>
Ogni porta TCP del progetto (server HTTP, gRPC, WebSocket, DB, qualsiasi listener) passa SOLO dai tool Nexus. NON hardcodare mai 3000, 8080, 5173 o altre porte fisse.

- VERIFICA/elenco porte: chiama nexus_list_ports (sola lettura: bucket assegnato + allocazioni registrate). Non dedurre le porte leggendo i sorgenti.
- ALLOCAZIONE: chiama request_port(label="<servizio>") e usa la porta ritornata (range 20000-39999, idempotente per label).
- Un fallback env con default numerico (process.env.PORT || 5000, os.environ.get("PORT", 5000), env::var("PORT").unwrap_or("3000")) E'' a tutti gli effetti una porta hardcoded: viene RIFIUTATO in scrittura. Se serve un default, usa la porta ALLOCATA da request_port.
- Vietato aggirare lo scanner con run_command/sed/heredoc: i processi su porte non allocate vengono terminati dal port enforcer.

Se hardcodi una porta il servizio va in conflitto con altri progetti sulla stessa macchina e la scrittura viene rifiutata.
</port_allocation>'
        )
 WHERE key IN ('system.nexus_base', 'agent.coder.base')
   AND content LIKE '%<port_allocation>%'
   AND content NOT LIKE '%nexus_list_ports%';

COMMIT;
