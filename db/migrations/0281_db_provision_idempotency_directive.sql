-- Migrazione 0281: rafforza la direttiva di provisioning DB con il check
-- anti-duplicazione (idempotenza lato agente).
--
-- Causa radice (regola H): un agente ha chiamato nexus_db_provision su un
-- progetto che AVEVA GIA' una connessione configurata, creando una seconda
-- connessione logica verso lo STESSO database fisico (e lasciando il progetto
-- senza una primary). Il fix nel codice (provision_internal_core) riusa la
-- connessione esistente e garantisce una primary; questa direttiva chiude il
-- problema anche a monte, istruendo l'agente a non chiamare il provisioning
-- quando una connessione esiste gia'.
--
-- Iniettata in coda alla direttiva <database_provisioning> (mig 0278) nei
-- system prompt system.nexus_base e agent.coder.base. Idempotente: guard
-- content NOT LIKE sul nuovo marcatore.

UPDATE nexus_prompt_templates
SET content = content || E'\n\n' || $MARKER$
<database_provisioning_idempotency>
Prima di creare un database chiama SEMPRE nexus_db_status. Se il progetto ha
gia' una connessione configurata (lo status la elenca con la sua primary), NON
chiamare nexus_db_provision: useresti per errore una seconda connessione verso
lo stesso database. Passa direttamente ai tool di query/schema
(nexus_db_apply_schema_file, nexus_db_execute_sql, nexus_db_query) sulla
connessione esistente. Chiama nexus_db_provision solo quando nexus_db_status non
riporta alcuna connessione per il progetto.
</database_provisioning_idempotency>
$MARKER$
WHERE key IN ('system.nexus_base', 'agent.coder.base')
  AND content NOT LIKE '%<database_provisioning_idempotency>%';
