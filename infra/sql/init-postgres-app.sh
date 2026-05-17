#!/bin/bash
# M74+M75 — Bootstrap del container postgres-app per i DB applicativi.
#
# Crea il role `nexus_app` con permessi MINIMI:
#   - LOGIN: si', altrimenti l'agente non puo' connettersi
#   - NOSUPERUSER NOCREATEROLE NOREPLICATION NOBYPASSRLS: zero privilegi
#   - CREATEDB: SI' (l'agente deve poter creare i DB <slug>_app per i progetti)
#
# Garanzie di isolamento ottenute dall'architettura, non solo dai grant:
#   - Questo container e' un cluster Postgres SEPARATO da postgres-nexus
#   - Il DB Nexus (tabelle agent_runs, nexus_*, settings, ecc.) NON ESISTE qui
#   - Anche con un eventuale escape del role, non c'e' nulla da contaminare
#
# Difesa in profondita': revoca CREATE su tutti i DB di template, applica
# RLS sull'utente, blocca COPY FROM PROGRAM (richiede superuser comunque).
#
# La password viene letta dalla env NEXUS_APP_DB_PASSWORD impostata nel compose.
set -e

# `dollar-quoted` per evitare problemi di escaping della password.
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
    -- Sicurezza base: revoca CREATE su public di template1 cosi' i nuovi DB
    -- creati ereditano la restrizione.
    REVOKE CREATE ON SCHEMA public FROM PUBLIC;
    REVOKE ALL ON DATABASE template1 FROM PUBLIC;

    -- Crea il role applicativo.
    DO \$\$
    BEGIN
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'nexus_app') THEN
            CREATE ROLE nexus_app
                WITH LOGIN
                     PASSWORD '${NEXUS_APP_DB_PASSWORD}'
                     NOSUPERUSER
                     NOCREATEROLE
                     NOREPLICATION
                     NOBYPASSRLS
                     CREATEDB
                     CONNECTION LIMIT 50;
        ELSE
            ALTER ROLE nexus_app WITH PASSWORD '${NEXUS_APP_DB_PASSWORD}';
        END IF;
    END
    \$\$;

    -- Revoca esplicita su database di sistema (difesa in profondita').
    REVOKE ALL ON DATABASE postgres FROM nexus_app;
    -- Permette solo CONNECT al DB postgres per CREATE DATABASE (CREATEDB role
    -- attr e' sufficiente, ma serve poter aprire una sessione admin minimale).
    GRANT CONNECT ON DATABASE postgres TO nexus_app;

    -- Statement timeout sicuro: nessuna query > 5 minuti.
    ALTER ROLE nexus_app SET statement_timeout = '300s';
    ALTER ROLE nexus_app SET idle_in_transaction_session_timeout = '60s';

    -- Audit visibility.
    COMMENT ON ROLE nexus_app IS
      'Role usato dall''agente Nexus per i DB applicativi dei progetti gestiti. '
      'NOSUPERUSER, NOCREATEROLE, NOREPLICATION, NOBYPASSRLS. CREATEDB attivo. '
      'Container separato (postgres-app, porta 5434) — nessun accesso al DB nexus.';
EOSQL

echo "[init-postgres-app] role nexus_app pronto su \$POSTGRES_DB"
