//! Adapter Liquibase — crea changeset XML nella directory changelog.

use super::MigrationAdapter;
use crate::project_db::{
    AppliedMigration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct LiquibaseAdapter;

#[async_trait]
impl MigrationAdapter for LiquibaseAdapter {
    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        // Punto unico in super::write_timestamped_sql_migration (regola L, S68).
        super::write_timestamped_sql_migration(ctx, name, sql, |n, ts, body| {
            format!("-- Liquibase changeset: {}\n-- id: {}\n\n{}\n", n, ts, body)
        })
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("liquibase")
            .args(["--url", connection_url, "update"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("liquibase update: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "liquibase update fallita: {}",
                stderr
            )));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        let output = std::process::Command::new("liquibase")
            .args(["--url", connection_url, "rollbackCount", "1"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("liquibase rollback: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "liquibase rollback fallita: {}",
                stderr
            )));
        }
        Ok(Some(RolledBackMigration {
            filename: "liquibase:last".into(),
        }))
    }
}
