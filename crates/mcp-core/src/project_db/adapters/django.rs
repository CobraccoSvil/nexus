//! Adapter Django — crea migration tramite `python manage.py makemigrations`.

use super::{sha256_hex, MigrationAdapter};
use crate::project_db::{
    AppliedMigration, Migration, ProjectDbContext, ProjectDbError, RolledBackMigration,
};
use async_trait::async_trait;
use std::path::PathBuf;

pub struct DjangoAdapter;

#[async_trait]
impl MigrationAdapter for DjangoAdapter {
    async fn list_pending(&self, ctx: &ProjectDbContext) -> Result<Vec<Migration>, ProjectDbError> {
        let dir = ctx.project_root.join(&ctx.migration_path);
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<_> = std::fs::read_dir(&dir)?
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.ends_with(".py") && !n.starts_with("__")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());
        let mut result = Vec::new();
        for entry in files {
            let path = entry.path();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            result.push(Migration {
                filename: filename.clone(),
                checksum: sha256_hex(&content),
                description: Some(filename.clone()),
                sql: None,
            });
        }
        Ok(result)
    }

    async fn create_migration(
        &self,
        ctx: &ProjectDbContext,
        name: &str,
        _sql: &str,
    ) -> Result<PathBuf, ProjectDbError> {
        let python = find_python(&ctx.project_root);
        let output = std::process::Command::new(&python)
            .args(["manage.py", "makemigrations", "--name", name])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("makemigrations: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "makemigrations fallita: {}",
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
        let python = find_python(&ctx.project_root);
        let output = std::process::Command::new(&python)
            .args(["manage.py", "migrate"])
            .current_dir(&ctx.project_root)
            .output()
            .map_err(|e| ProjectDbError::Adapter(format!("migrate: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProjectDbError::Adapter(format!(
                "django migrate fallita: {}",
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
        // Django rollback richiede nome app e migrazione target — non supportato in V1
        Err(ProjectDbError::Adapter(
            "Rollback Django richiede nome app e target. Usa: python manage.py migrate <app> <migration_precedente>".into()
        ))
    }
}

fn find_python(root: &std::path::Path) -> String {
    for candidate in &["venv/bin/python", ".venv/bin/python", "python3", "python"] {
        if root.join(candidate).exists() || candidate.starts_with("python") {
            return candidate.to_string();
        }
    }
    "python3".into()
}
