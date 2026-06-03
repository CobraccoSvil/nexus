-- Migrazione 0278: direttiva <database_provisioning> nei system prompt agente.
--
-- Regola D (CLAUDE.md): un tool non documentato nel prompt non viene usato.
-- Quando l'utente chiede di creare/configurare un database, l'agente deve usare
-- i tool collegati al pannello Database (nexus_db_provision ->
-- nexus_db_apply_schema_file / nexus_db_execute_sql) invece di fermarsi a
-- chiedere host/porta/credenziali: il provisioning interno non le richiede.
-- Iniettata in system.nexus_base e agent.coder.base (idempotente).

UPDATE nexus_prompt_templates
SET content = content || E'\n\n' || $MARKER$
<database_provisioning>
Quando l'utente chiede di creare o configurare un database per il progetto, NON
chiedere host, porta o credenziali: il provisioning interno gestito da Nexus non
le richiede. Procedi con i tool collegati al pannello Database:
- nexus_db_status: verifica le connessioni gia configurate e le tabelle esistenti.
- nexus_db_provision (mode="internal", default): crea un Postgres dedicato gestito
  da Nexus senza alcuna credenziale. Usa mode="external" solo se l'utente fornisce
  esplicitamente una connection string verso un DB esistente.
- nexus_db_apply_schema_file: se nel repo esiste un file schema (es.
  backend/db_schema.sql, schema.sql, migrations/*.sql), importalo per creare le
  tabelle. Preferiscilo quando il file e gia presente.
- nexus_db_execute_sql: per creare/alterare tabelle o inserire dati con SQL
  esplicito quando non esiste un file schema. Le DDL vengono archiviate in
  automatico (nota KB + file migration versionato).
Flusso tipico: nexus_db_provision -> nexus_db_apply_schema_file (oppure
nexus_db_execute_sql). Non bloccarti chiedendo dati che il provisioning interno
non necessita.
</database_provisioning>
$MARKER$
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content NOT LIKE '%<database_provisioning>%';
