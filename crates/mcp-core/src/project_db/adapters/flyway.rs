//! Adapter Flyway (JVM) — crea migration nel formato `V<ver>__<nome>.sql`.

use super::MigrationAdapter;
use crate::project_db::{
    AppliedMigration, Migration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct FlywayAdapter;

#[async_trait]
impl MigrationAdapter for FlywayAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        super::list_pending_files(
            ctx,
            |n| (n.starts_with('V') || n.starts_with('R')) && n.ends_with(".sql"),
            true,
        )
    }

    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        let dir = ctx.project_root.join(&ctx.migration_path);
        std::fs::create_dir_all(&dir)?;
        // Calcola prossima versione
        let next_ver = next_flyway_version(&dir);
        let safe_name: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let filename = format!("V{}_{}__{}.sql", next_ver.0, next_ver.1, safe_name);
        let path = dir.join(&filename);
        std::fs::write(&path, format!("-- Flyway migration: {}\n\n{}\n", name, sql))?;
        Ok(path)
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let output = std::process::Command::new("flyway")
            .args([&format!("-url={}", connection_url), "migrate"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("flyway migrate: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "flyway migrate fallita: {}",
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
        let output = std::process::Command::new("flyway")
            .args([&format!("-url={}", connection_url), "undo"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("flyway undo: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "flyway undo fallita: {}",
                stderr
            )));
        }
        Ok(Some(RolledBackMigration {
            filename: "flyway:last".into(),
        }))
    }
}

fn next_flyway_version(dir: &std::path::Path) -> (u32, u32) {
    let mut max: u32 = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('V') {
                let rest = &name[1..];
                let ver_str: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '_')
                    .collect();
                let parts: Vec<u32> = ver_str.split('_').filter_map(|s| s.parse().ok()).collect();
                if let Some(&major) = parts.first() {
                    max = max.max(major);
                }
            }
        }
    }
    (max + 1, 0)
}
