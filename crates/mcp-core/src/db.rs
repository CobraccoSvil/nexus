use nexus_migrations::{OrigineSet, Set};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Apre il pool META e porta lo schema alla versione del set.
///
/// IL SILENZIO E' MORTO. Qui c'era `if migrations_path.exists() { .. } else {
/// warn!("skipping") }`: un guard scritto a mano che convertiva in un avviso
/// l'errore che `Migrator::new` produce gia' da solo. Il risultato era un
/// servizio che si avviava con lo schema vecchio senza che nessuno lo sapesse —
/// e siccome il percorso era relativo alla directory di lavoro, bastava
/// avviarlo da un'altra cartella. Funzionava solo perche' il manifest di
/// servizio fissa `workingdirectory` alla radice del repo: una garanzia che
/// nessun test copriva e che non era scritta da nessuna parte.
///
/// Ora l'applicazione passa dal punto unico `nexus-migrations`, lo stesso che
/// usano il provisioning dei DB-progetto e `xtask migrate`: sulla stessa
/// condizione il verdetto e' uno solo perche' e' lo stesso codice, non perche'
/// due autori hanno scelto lo stesso comportamento.
///
/// L'origine del set si installa QUI, una volta per processo: e' verita' di
/// processo, non di richiesta, e i chiamanti a valle la leggono senza doversela
/// far passare lungo la catena di `project_data_pool`.
pub async fn init_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(database_url)
        .await?;

    tracing::info!("PostgreSQL connection pool established");

    let origine = OrigineSet::directory_di_lavoro()?;
    nexus_migrations::installa_origine(origine.clone())?;
    tracing::info!(
        radice = %origine.radice().display(),
        provenienza = ?origine.provenienza(),
        "set di migrazioni: origine installata per il processo"
    );

    nexus_migrations::applica(&pool, Set::Meta, &origine).await?;
    tracing::info!("Database migrations applied");

    Ok(pool)
}
