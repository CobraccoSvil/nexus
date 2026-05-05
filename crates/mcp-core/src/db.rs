use std::path::Path;

use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn init_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(database_url)
        .await?;

    tracing::info!("PostgreSQL connection pool established");

    // Run migrations from the project-level migrations directory
    let migrations_path = Path::new("db/migrations");
    if migrations_path.exists() {
        let migrator = sqlx::migrate::Migrator::new(migrations_path).await?;
        migrator.run(&pool).await?;
        tracing::info!("Database migrations applied");
    } else {
        tracing::warn!(
            "Migrations directory not found at {:?}, skipping",
            migrations_path
        );
    }

    Ok(pool)
}
