//! Adapter Rails ActiveRecord — crea migration tramite `bin/rails generate migration`.

use super::MigrationAdapter;
use crate::{
    AppliedMigration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct RailsAdapter;

#[async_trait]
impl MigrationAdapter for RailsAdapter {
    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        _sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        let camel_name: String = name
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            })
            .collect();
        let rails_bin = if ctx.project_root.join("bin/rails").exists() {
            "bin/rails"
        } else {
            "rails"
        };
        let output = std::process::Command::new(rails_bin)
            .args(["generate", "migration", &camel_name])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("rails generate migration: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "rails generate migration fallita: {}",
                stderr
            )));
        }
        Ok(ctx.project_root.join(&ctx.migration_path))
    }

    async fn apply_pending(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Vec<AppliedMigration>, ProjectDbError> {
        let rails_bin = if ctx.project_root.join("bin/rails").exists() {
            "bin/rails"
        } else {
            "rails"
        };
        let output = std::process::Command::new(rails_bin)
            .args(["db:migrate"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("rails db:migrate: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "rails db:migrate fallita: {}",
                stderr
            )));
        }
        Ok(vec![])
    }

    async fn rollback_last(
        &self,
        ctx: &ProjectDbContext,
        _connection_url: &str,
    ) -> Result<Option<RolledBackMigration>, ProjectDbError> {
        let rails_bin = if ctx.project_root.join("bin/rails").exists() {
            "bin/rails"
        } else {
            "rails"
        };
        let output = std::process::Command::new(rails_bin)
            .args(["db:rollback"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("rails db:rollback: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "rails db:rollback fallita: {}",
                stderr
            )));
        }
        Ok(Some(RolledBackMigration {
            filename: "rails:last".into(),
        }))
    }
}
