-- 0671_prompt_db_applicativo_senza_credenziali.sql
--
-- Due template ordinavano al modello di scrivere NEI SORGENTI degli applicativi
-- generati la connection string del cluster META (localhost:5433) con l'utente
-- superuser `nexus` — cioe' il DB di infrastruttura di Nexus, routing matrix e
-- ledger compresi. Il danno non e' ipotetico, e' nel backup del 02/08/2026: sul
-- cluster meta-5433 convivono col DB `nexus` i database APPLICATIVI `e2e_todo` e
-- `vendita_immobile`, creati da app generate seguendo queste istruzioni.
--
-- L'infrastruttura fa gia' la cosa giusta dal cutover al cluster applicativo:
-- `spawn_command_child` / run_service iniettano in OGNI processo avviato da
-- Nexus una DATABASE_URL del cluster app (:5434) con ruolo a privilegi limitati
-- (`nexus_app`), su env pulito (env_clear), proprio perche' il processo non veda
-- i segreti di Nexus. Il prompt scavalcava tutto questo insegnando al modello a
-- HARDCODARE la stringa vecchia: il posto giusto dell'informazione e' l'env
-- iniettato, il prompt deve solo dire di leggerlo (regola G: la configurazione
-- ha UN posto; il prompt che la duplica mente alla prima migrazione di cluster
-- — come ha appena fatto).
--
-- Interventi chirurgici via replace(): il resto dei template non si tocca.

-- 1. automation.mode_automatic_instruction: la regola Postgres riscritta.
UPDATE nexus_prompt_templates SET content = replace(content,
  '- L''unico Postgres disponibile e'' localhost:5433 user=nexus password=nexus (container ideai-postgres-nexus-1).',
  '- Il DB applicativo del progetto e'' gia'' pronto: la connection string completa e'' in process.env.DATABASE_URL, iniettata da Nexus in OGNI processo che avvia (run_command e run_service). Non chiederla, non ricostruirla, non copiarla.')
WHERE key = 'automation.mode_automatic_instruction';

UPDATE nexus_prompt_templates SET content = replace(content,
  '- L''unica connection string ammessa nei sorgenti applicativi: postgres://nexus:nexus@localhost:5433/<slug> (e relativa variante postgresql://). Caricarla da process.env.DATABASE_URL, NON inlinarla.',
  '- VIETATO scrivere host, porta, utente o password del DB in qualunque sorgente, config o .env: nei sorgenti l''unica forma ammessa e'' la LETTURA di process.env.DATABASE_URL. Credenziali copiate in un file smettono di valere al primo cambio di infrastruttura e restano disperse nel repo.')
WHERE key = 'automation.mode_automatic_instruction';

UPDATE nexus_prompt_templates SET content = replace(content,
  'Se DATABASE_URL manca, scrivilo SUBITO in .env e poi avvia.',
  'Se process.env.DATABASE_URL risulta assente, il processo non e'' stato avviato da Nexus (run_command/run_service): avvialo da li'', non inventare una connection string.')
WHERE key = 'automation.mode_automatic_instruction';

UPDATE nexus_prompt_templates SET content = replace(content,
  '- DB: il Postgres Nexus (vedi INFRASTRUTTURA), schema con provider postgresql (mai sqlite); il database applicativo del progetto va creato e migrato REALMENTE prima di dichiararlo pronto.',
  '- DB: il Postgres applicativo del progetto via process.env.DATABASE_URL (provider postgresql, mai sqlite); il database va migrato REALMENTE prima di dichiararlo pronto.')
WHERE key = 'automation.mode_automatic_instruction';

UPDATE nexus_prompt_templates SET content = replace(content,
  '- Postgres del progetto: localhost:5433 (container `ideai-postgres-nexus-1`), user `nexus`, password `nexus`. USA QUESTO per il DB applicativo del progetto target.',
  '- Postgres del progetto: gia'' provisionato sul cluster applicativo dedicato, con ruolo a privilegi limitati; nome DB e credenziali stanno DENTRO process.env.DATABASE_URL. Il cluster di infrastruttura di Nexus non e'' per le app.')
WHERE key = 'automation.mode_automatic_instruction';

-- 2. agent.planner.base: l'esempio di piano insegnava la forma sbagliata.
UPDATE nexus_prompt_templates SET content = replace(content,
  '- DB: Postgres applicativo localhost:5433/rental',
  '- DB: Postgres applicativo del progetto (connection string da process.env.DATABASE_URL)')
WHERE key = 'agent.planner.base';

-- Guard: nessun template deve piu' portare credenziali del DB. Fallisce la
-- migrazione invece di lasciare un prompt che le disperde. Il pattern e' sulle
-- CREDENZIALI, non sulla porta: nominare ":5433" per descrivere l'infrastruttura
-- (subagent.sysadmin.base) e' legittimo, insegnare "user=nexus password=nexus" no.
DO $$
DECLARE
    con_credenziali text;
BEGIN
    SELECT string_agg(key, ', ') INTO con_credenziali
    FROM nexus_prompt_templates
    WHERE content LIKE '%nexus:nexus@%'
       OR content LIKE '%password=nexus%'
       OR content LIKE '%nexus_app_dev_secret%'
       OR content LIKE '%nexus_admin_secret%';
    IF con_credenziali IS NOT NULL THEN
        RAISE EXCEPTION
            'nexus_prompt_templates: credenziali DB ancora presenti in: %',
            con_credenziali;
    END IF;
END $$;
