//! Contract test (PR-4 Livello 3): isolation Livello 6 + Livello 2.
//!
//! Verifica che:
//!   - Esista un container/cluster Postgres separato per i DB applicativi.
//!   - Il role `nexus_app` sia presente nel cluster app e ABBIA i privilegi minimali
//!     (NOSUPERUSER, NOCREATEROLE, NOREPLICATION, NOBYPASSRLS, CREATEDB).
//!   - Il role `nexus_app` NON ESISTA nel cluster Nexus (postgres-nexus su 5433).
//!   - Le tabelle infrastruttura Nexus (`agent_runs`, `nexus_*`) NON esistano nel
//!     cluster app.
//!
//! Salta se i due cluster non sono raggiungibili (CI senza docker compose up).
//! Lo skip passa dal punto unico `support::salta`: prima quattro dei sei skip
//! stampavano il solo `"skip"`, senza dire QUALE cluster mancasse, e nel gate
//! erano indistinguibili da un'isolation verificata.

mod support;

use sqlx::{PgPool, Row};
use std::env;
use support::db_url_o_salta;

async fn nexus_pool() -> Option<PgPool> {
    let url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nexus:nexus@localhost:5433/nexus".into());
    db_url_o_salta(&url, "cluster nexus (DATABASE_URL / 5433)").await
}

async fn app_pool() -> Option<PgPool> {
    // Admin del cluster postgres-app (porta 5434) per ispezione roles.
    let url = env::var("NEXUS_APP_ADMIN_URL").unwrap_or_else(|_| {
        "postgres://nexus_admin:nexus_admin_secret@localhost:5434/postgres".into()
    });
    db_url_o_salta(&url, "cluster app (NEXUS_APP_ADMIN_URL / 5434)").await
}

#[tokio::test]
async fn cluster_app_e_separato_dal_cluster_nexus() {
    let Some(nexus) = nexus_pool().await else { return };
    let Some(app) = app_pool().await else { return };
    // Identifico i cluster via system_identifier (univoco per data directory).
    let nexus_id: i64 =
        sqlx::query_scalar("SELECT system_identifier::bigint FROM pg_control_system()")
            .fetch_one(&nexus)
            .await
            .unwrap_or(0);
    let app_id: i64 =
        sqlx::query_scalar("SELECT system_identifier::bigint FROM pg_control_system()")
            .fetch_one(&app)
            .await
            .unwrap_or(0);
    assert!(
        nexus_id != 0 && app_id != 0 && nexus_id != app_id,
        "i due cluster condividono il system_identifier {nexus_id} → NON sono fisicamente separati"
    );
}

#[tokio::test]
async fn role_nexus_app_esiste_solo_nel_cluster_app() {
    let Some(nexus) = nexus_pool().await else { return };
    let Some(app) = app_pool().await else { return };
    // Nel cluster app: deve esistere
    let in_app: i64 =
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM pg_roles WHERE rolname = 'nexus_app'")
            .fetch_one(&app)
            .await
            .unwrap_or(0);
    assert_eq!(
        in_app, 1,
        "role nexus_app DEVE esistere nel cluster postgres-app"
    );
    // Nel cluster nexus: NON deve esistere (isolation fisica)
    let in_nexus: i64 =
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM pg_roles WHERE rolname = 'nexus_app'")
            .fetch_one(&nexus)
            .await
            .unwrap_or(0);
    assert_eq!(
        in_nexus, 0,
        "role nexus_app NON deve esistere nel cluster postgres-nexus (rottura isolation L2+L6)"
    );
}

#[tokio::test]
async fn nexus_app_ha_privilegi_minimali() {
    let Some(app) = app_pool().await else { return };
    let row = sqlx::query(
        "SELECT rolsuper, rolcreaterole, rolreplication, rolbypassrls, rolcreatedb
         FROM pg_roles WHERE rolname = 'nexus_app'",
    )
    .fetch_optional(&app)
    .await
    .expect("query");
    let r = row.expect("nexus_app non esiste");
    let super_: bool = r.try_get("rolsuper").unwrap_or(true);
    let createrole: bool = r.try_get("rolcreaterole").unwrap_or(true);
    let replication: bool = r.try_get("rolreplication").unwrap_or(true);
    let bypassrls: bool = r.try_get("rolbypassrls").unwrap_or(true);
    let createdb: bool = r.try_get("rolcreatedb").unwrap_or(false);
    assert!(!super_, "nexus_app NON deve essere superuser");
    assert!(!createrole, "nexus_app NON deve poter creare role");
    assert!(!replication, "nexus_app NON deve poter fare replication");
    assert!(!bypassrls, "nexus_app NON deve bypassare RLS");
    assert!(
        createdb,
        "nexus_app DEVE poter CREATE DATABASE (provisioning app DBs)"
    );
}

#[tokio::test]
async fn cluster_app_non_ha_tabelle_infrastruttura_nexus() {
    let Some(app) = app_pool().await else { return };
    let proibite = [
        "agent_runs",
        "nexus_agent_plans",
        "chat_sessions",
        "settings",
        "projects",
    ];
    for t in proibite {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM information_schema.tables WHERE table_name = $1",
        )
        .bind(t)
        .fetch_one(&app)
        .await
        .unwrap_or(0);
        assert_eq!(
            exists, 0,
            "tabella infrastruttura '{t}' presente nel cluster app → contaminazione"
        );
    }
}
