-- 0691_governance_sql_lettura_catalogo.sql
--
-- Il catalogo del DB APPLICATIVO si legge; l'infrastruttura Nexus si decide
-- sulla CONNESSIONE, non sul testo della query.
--
-- MISURATO il 09/08/2026 su gestione-corsi: l'agente aveva appena eseguito
-- `dotnet ef database update` e il task gli chiedeva di verificare che lo
-- schema risultante contenesse le tabelle attese. La sua
-- `SELECT ... FROM information_schema.tables` e' stata respinta con
-- "la query tocca oggetti di sistema/infrastruttura ('information_schema')",
-- e non esisteva altro modo di accertare il proprio lavoro.
--
-- Il criterio era LESSICALE (il nome compare nel testo) invece che strutturale
-- (quale database la connessione raggiunge), e confondeva due domande:
--   - "tocca l'infrastruttura di NEXUS?" -> divieto giusto, gia' applicato dove
--     si decide: nexus_project_db::exec::classifica_connessione, che dalla mig
--     0494 puo' riconoscere anche il DB metadati per-progetto
--     (connection_role='nexus_metadata') oltre al DB META;
--   - "legge i metadati dello schema DEL PROGETTO?" -> sola lettura, sul
--     database che l'agente ha appena creato e migrato.
--
-- Che il divieto fosse lessicale lo diceva il codice stesso: sulla STESSA
-- connessione, i tool `nexus_db_tables`/`nexus_db_describe` e
-- `project_db_routes::query::count_public_tables` interrogano gia'
-- `information_schema`, e il pannello SQL (umano) non ha alcun guard. Era
-- vietato solo cio' che l'agente DIGITAVA.
--
-- Questa migrazione allinea la DESCRIZIONE delle due regole in
-- `nexus_resource_policies` a cio' che il codice fa adesso: la riga
-- 'db'/'sql_injection' attribuiva la regola al detector ADR 0021
-- (`sec_sql_injection_check`, che analizza i SORGENTI), che e' un altro punto
-- e un'altra domanda. Nessun vocabolario nuovo in `params`: i nomi dei
-- cataloghi di sistema sono fissati da Postgres e dallo standard SQL, non sono
-- configurazione, e un elenco che ASSOLVE lasciato vuoto spegnerebbe il guard
-- in silenzio. Il kill-switch resta la colonna `enabled`.
--
-- Idempotente.

BEGIN;

UPDATE nexus_resource_policies
   SET description = 'Guard SQL per-statement sulle query dei tool DB del progetto '
                     '(mcp-core/src/security/resource_governance.rs::check_dangerous_sql). '
                     'Blocca: scritture sui cataloghi di sistema (le LETTURE di '
                     'information_schema/pg_catalog sono legittime: sono il catalogo del DB '
                     'applicativo del progetto), oggetti di infrastruttura Nexus (nexus_*, '
                     '_sqlx_migrations), DROP DATABASE/SCHEMA, DELETE/UPDATE senza WHERE. '
                     'Il giudizio e'' per statement, sulle stesse prodotte da split_statements. '
                     'Distinto dal detector SQL-injection sui sorgenti (ADR 0021, tool '
                     'sec_sql_injection_check). Solo blocco + audit, nessun auto-fix.',
       updated_at = NOW()
 WHERE resource_kind = 'db' AND rule_key = 'sql_injection';

UPDATE nexus_resource_policies
   SET description = 'Connessioni verso l''infrastruttura Nexus vietate ai tool di progetto. '
                     'Criterio strutturale in nexus_project_db::exec::classifica_connessione, '
                     'chiamato da resolve_project_conn: rifiuta sia il DB META (nexus su 5433, '
                     'riconosciuto dalla URL) sia il DB metadati per-progetto '
                     '(project_database_config.connection_role = ''nexus_metadata'', mig 0494 - '
                     'la sua URL non lo tradisce: stesso cluster applicativo, database '
                     '<slug>_nexus). E'' QUESTA la sede del divieto, non l''ispezione del testo '
                     'della query.',
       updated_at = NOW()
 WHERE resource_kind = 'db' AND rule_key = 'block_nexus';

COMMIT;
