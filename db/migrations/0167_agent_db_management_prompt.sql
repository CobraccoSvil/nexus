-- 0167: Aggiunge istruzioni database_management al prompt agent.coder.base.
-- L'agente deve chiamare project_db_set_connection dopo aver creato un DB
-- per aggiornare automaticamente il pannello DB dell'IDE via evento SSE.

UPDATE nexus_prompt_templates
SET content = content || '

<!-- 0138:database_management -->
<database_management>
GESTIONE DATABASE PROGETTO -- AGGIORNAMENTO PANNELLO DB.

Quando crei un database per il progetto (docker-compose con postgres/mysql,
container singolo, o database locale), DEVI SEMPRE registrare la connessione
nel pannello DB di Nexus usando il tool project_db_set_connection.

PROCEDURA OBBLIGATORIA:
1. Dopo aver avviato il container/servizio DB con run_service, aspetta che sia pronto.
2. Chiama project_db_set_connection con i parametri:
   - connection_string: la stringa DSN completa (es. postgres://user:pass@localhost:5435/dbname)
   - engine: tipo DB (postgres, mysql, sqlite, mssql)
   - hosting_mode: "internal" per DB locale/docker, "external" per DB remoti
   - name: nome logico (default "primary")
3. Questo aggiorna automaticamente il pannello DB dell IDE Nexus in tempo reale.

ESEMPIO:
Dopo aver creato un docker-compose con PostgreSQL su porta 5435:
  project_db_set_connection({
    "connection_string": "postgres://taskboard:taskboard_secret@localhost:5435/taskboard",
    "engine": "postgres",
    "hosting_mode": "internal",
    "name": "primary"
  })

NON saltare questo passaggio: senza di esso il pannello DB rimane vuoto.
</database_management>'
WHERE key = 'agent.coder.base'
  AND content NOT ILIKE '%database_management%';
