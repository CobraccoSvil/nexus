-- Migrazione 0279: elenco esatto dei tool DB e divieto di inventarne i nomi.
--
-- Root cause (regola H): l'agente allucina nomi di tool DB inesistenti
-- (nexus_db_query, nexus_db_tables) e tenta psql via shell, entrando in loop.
-- Gli alias sono stati aggiunti lato Rust (catalog.rs + nexus_builtin/mod.rs),
-- ma il prompt deve elencare i nomi canonici e vietare invenzioni / psql.
-- Estende la direttiva di mig 0278 con un blocco strict, iniettato in
-- system.nexus_base e agent.coder.base. Idempotente (guard su marcatore proprio).

UPDATE nexus_prompt_templates
SET content = content || E'\n\n' || $MARKER$
<database_tools_strict>
Per operare sul database del progetto usa ESCLUSIVAMENTE questi tool (non inventare
nomi, non usare psql ne shell):
- nexus_db_status: stato connessioni e tabelle esistenti.
- nexus_db_provision: crea il database del progetto.
- nexus_db_table_list: elenca le tabelle.
- nexus_db_execute_sql: esegue SELECT, INSERT, UPDATE, DELETE o DDL.
- nexus_db_apply_schema_file: applica uno schema .sql gia presente nei file.
Se un tool restituisce un errore SQL, leggi prima lo schema reale con
nexus_db_table_list / nexus_db_status e correggi la query; NON ripetere la stessa
chiamata in loop e NON tentare nomi di tool diversi a caso.
</database_tools_strict>
$MARKER$
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content NOT LIKE '%<database_tools_strict>%';
