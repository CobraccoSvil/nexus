//! Punto unico (regola L) dello SCHEMA DI TEST del DB-progetto.
//!
//! Espone il migrator del set `db/migrations/project`: LA STESSA strada per cui
//! quello schema nasce in produzione (`mcp_core::project_db_routes::provision`
//! applica questo identico set al DB `<slug>_nexus` al primo accesso). Un test
//! che lo dichiara
//!
//! ```ignore
//! #[sqlx::test(migrator = "nexus_test_schema::PROJECT_MIGRATOR")]
//! ```
//!
//! gira sullo schema REALE - colonne, CHECK, indici unici e FK compresi - non su
//! una sua imitazione scritta a mano.
//!
//! Perche' esiste (regola O): le fixture `CREATE TABLE` ricopiate nei moduli di
//! test non derivavano dalla migrazione, quindi divergevano in SILENZIO. Il caso
//! che ha aperto il capitolo: `nexus_agent_todos` aveva DUE copie
//! (`agent_graph_adapter::todo_store` e `chat_messages::agent_run`) diverse fra
//! loro e dalla migrazione (`seq INTEGER` vs `BIGINT`, `depends_on UUID[]` vs
//! `TEXT[]`, `content` NOT NULL vs nullable), entrambe prive di `project_id`,
//! dei CHECK su status/priority e della FK verso `nexus_agent_plans`. Nessuna
//! delle due falliva da sola: hanno mentito finche' una query di produzione non
//! ha chiesto una colonna assente (`acceptance_criteria` in `list_todos`: 5 test
//! rossi). Aggiungere la colonna alle fixture sarebbe stata la toppa (regola H);
//! la causa e' che lo schema di test non derivava dalla migrazione.
//!
//! `sqlx::migrate!` incorpora i file a compile-time con `include_str!`, quindi
//! MODIFICARE una migrazione ricompila i test da sola. L'AGGIUNTA di un file
//! nuovo al set non e' invece osservata dalla macro: i crate che usano questo
//! migrator dichiarano `cargo:rerun-if-changed=../../db/migrations/project` nel
//! proprio `build.rs`.
//!
//! Lo stesso vale per lo schema META: [`META_MIGRATOR`] espone il set
//! `db/migrations`. Questa nota diceva che applicarlo per-test "costerebbe piu'
//! di quanto renda": la misura l'ha smentita — l'intero set si applica a un DB
//! vergine in circa 2,5 secondi, lo stesso ordine di grandezza del gemello
//! per-progetto. E il costo di NON averlo era gia' stato pagato: quattro
//! migrazioni della serie 0104-0107 sono `SELECT 1;` e per anni nessun test ha
//! potuto accorgersi che un DB ricostruito da zero non riceveva
//! `nexus_quality_scans` ne' le colonne vettoriali di
//! `project_quality_findings`. I contract test esistenti si connettevano a
//! `DATABASE_URL`, cioe' all'unico DB in cui il difetto non esisteva.

/// Migrator del set `db/migrations/project` (schema del DB-progetto).
pub static PROJECT_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../db/migrations/project");

/// Migrator del set `db/migrations` (schema META: settings, catalog, routing,
/// telemetria, code di servizio).
///
/// Un test che lo dichiara gira su un DB ricostruito da zero, non su quello di
/// sviluppo: e' l'unico modo per accorgersi che una migrazione dichiara un
/// oggetto che non crea.
pub static META_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../db/migrations");
