-- 0553: Consiglio delle Competenze — settings di attivazione e figure.
--
-- Root cause: il codice del pre-step programmatico del consiglio leggeva
-- `orchestrator.council_enabled`, `orchestrator.council_figures`,
-- `orchestrator.council_infra_figures` e `orchestrator.council_max_figures`,
-- ma nessuna migrazione li seedava. Inoltre la direttiva 0549 citava
-- `db_architect` tra le figure advisory anche se quel kind e' operativo
-- (tool di scrittura), non una figura read-only con `advisory_verdict`.
--
-- Il nome prodotto della feature e' "Consiglio delle Competenze": un panel di
-- professionalita' read-only che analizza il problema prima dell'esecuzione.

INSERT INTO settings (key, value, category, description) VALUES
  ('orchestrator.council_enabled', 'true', 'orchestrator',
   'Consiglio delle Competenze: abilita il pre-step programmatico multi-professionalita sui task in ambiti sensibili. Richiede comunque orchestrator.subagents_enabled=true: il kill-switch globale dei sub-agent resta autoritativo.'),
  ('orchestrator.council_feature_name', 'Consiglio delle Competenze', 'orchestrator',
   'Nome prodotto della funzionalita di analisi multi-professionalita a monte del flusso agentico.'),
  ('orchestrator.council_figures',
   'functional_analyst,software_architect,security_engineer,project_manager,program_manager',
   'orchestrator',
   'Consiglio delle Competenze: figure read-only convocate di base. Devono essere kind con advisory_verdict nella whitelist.'),
  ('orchestrator.council_infra_figures', 'sysadmin', 'orchestrator',
   'Consiglio delle Competenze: figure read-only aggiunte quando il task tocca infrastruttura, deploy, servizi o osservabilita.'),
  ('orchestrator.council_infra_keywords',
   'deploy,docker,compose,container,systemd,winsw,servizio,servizi,porta,porte,traefik,nginx,certificat,https,tls,monitor,grafana,log,osservabilita,worker,cron,scheduler',
   'orchestrator',
   'Consiglio delle Competenze: keyword di ambito infrastruttura/deploy che aggiungono le figure infra. Match substring case-insensitive.'),
  ('orchestrator.council_max_figures', '6', 'orchestrator',
   'Consiglio delle Competenze: cap massimo di figure convocate in un pre-step.')
ON CONFLICT (key) DO NOTHING;

-- Correzione della direttiva prompt 0549: `db_architect` e' un kind operativo,
-- non una figura advisory read-only. La lente DB resta coperta da functional
-- analyst + software architect + security engineer + project manager.
UPDATE nexus_prompt_templates
   SET content = replace(
           content,
           '- Schema o dati DB: db_architect, functional_analyst, software_architect,',
           '- Schema o dati DB: functional_analyst, software_architect,'
       ),
       updated_at = NOW()
 WHERE key IN ('system.nexus_base', 'agent.coder.base')
   AND content LIKE '%Schema o dati DB: db_architect, functional_analyst%';
