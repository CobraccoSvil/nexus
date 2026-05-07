# Policy: modifiche schema DB nei progetti gestiti da Nexus

## Obiettivo

Ogni database **dell’applicazione** collegato a un progetto Nexus deve poter essere **ricostruito** da:

1. **File di migration** versionati nel repository (cartella configurata in `migration_path`, es. `migrations/`).
2. **Registro Nexus** `project_migration_history` (checksum, stato, chi ha applicato, SQL associato quando creato via agente).

Nexus **non** sostituisce il commit Git: la fonte di verità rimane il repo; il registro integra tracciamento operativo e UI.

## Comportamento predefinito

- `project_database_config.allow_ddl_override` è **`false` per default** (vedi migration `0081`).
- Con `allow_ddl_override = false`:
  - L’agente **non** deve applicare DDL con client SQL da shell (`psql -c "ALTER…"`, ecc.): `run_command` viene bloccato se il comando contiene DDL evidente (euristica).
  - Le modifiche di schema passano da **file migration** creati con `project_db_create_migration` (o toolchain del progetto: Flyway, Alembic, Prisma, EF, …) e applicati con `project_db_apply_migration` o comando equivalente esplicitamente consentito.
- Con `allow_ddl_override = true` (solo dopo scelta consapevole in UI / admin):
  - È possibile usare `POST /api/projects/:id/db/override-request` per DDL straordinario; il SQL viene comunque **registrato** in `project_migration_history` (stato `overridden` / `pending_override`).

## Processo raccomandato per l’agente

1. `project_db_status` — verifica engine, `migration_path`, pending.
2. `project_db_create_migration` — aggiunge file SQL versionato + riga in `project_migration_history` (`pending`).
3. `project_db_apply_migration` — esegue sul DB del progetto e marca `applied`.
4. Commit Git dei file in `migration_path` come parte normale del workflow di sviluppo.

## Bypass CLI riconosciuti

Comandi che applicano **soli** artefatti di migration già versionati (es. `flyway migrate`, `prisma migrate deploy`, `dotnet ef database update`) non vengono trattati come DDL ad-hoc.

## Note

- Il DB **interno di Nexus** (`db/migrations`) è separato: questa policy riguarda i DB delle applicazioni collegate ai progetti utente.
- Per eccezioni operative prolungate, documentare perché `allow_ddl_override` è stato abilitato e ripristinare il default quando possibile.
