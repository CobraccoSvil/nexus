//! `project_db_restore` — ripristina un backup nel DB del progetto.
//!
//! Esegue `psql -f` (plain) o `pg_restore` (custom) da un file backup.
//! Richiede `confirm: true` esplicito (operazione distruttiva).
//! Il file di backup deve trovarsi in `project_root/.nexus/backups/`.

use super::db_helper;
use super::project_db_backup::parse_dsn_parts;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbRestoreTool;

#[async_trait]
impl NexusToolHandler for ProjectDbRestoreTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let backup_path = args
            .get("backup_path")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'backup_path' obbligatorio".into()))?
            .trim()
            .to_string();

        let confirm = args
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if !confirm {
            return Err(NexusToolError::BadInput(
                "Operazione distruttiva: specifica confirm:true per procedere. Il DB del progetto verra' sovrascritto.".into(),
            ));
        }

        // Valida che il file sia dentro .nexus/backups/
        let backup_dir = ctx.project_root.join(".nexus").join("backups");
        let full_path = if std::path::Path::new(&backup_path).is_absolute() {
            std::path::PathBuf::from(&backup_path)
        } else {
            backup_dir.join(&backup_path)
        };

        let canonical = full_path.canonicalize().map_err(|_| {
            NexusToolError::BadInput(format!("File backup '{}' non trovato", backup_path))
        })?;

        if !canonical.starts_with(&backup_dir) {
            return Err(NexusToolError::BadInput(
                "Il file di backup deve trovarsi in .nexus/backups/ del progetto".into(),
            ));
        }

        if !canonical.is_file() {
            return Err(NexusToolError::BadInput(
                "Il percorso non e' un file".into(),
            ));
        }

        // Ottieni DSN
        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let dsn = get_project_dsn(&nexus_pool, ctx.project_id).await?;
        nexus_pool.close().await;

        let (host, port, dbname, user, password) = parse_dsn_parts(&dsn)?;

        let canonical_str = canonical.to_string_lossy().to_string();
        let is_custom = canonical_str.ends_with(".dump");

        let start = std::time::Instant::now();

        let child = if is_custom {
            // pg_restore per formato custom
            let clean = args.get("clean").and_then(Value::as_bool).unwrap_or(true);
            let mut cmd_args = vec![
                "-h".to_string(),
                host,
                "-p".to_string(),
                port,
                "-U".to_string(),
                user,
                "-d".to_string(),
                dbname.clone(),
            ];
            if clean {
                cmd_args.push("--clean".to_string());
                cmd_args.push("--if-exists".to_string());
            }
            cmd_args.push(canonical_str.clone());

            tokio::process::Command::new("pg_restore")
                .args(&cmd_args)
                .env("PGPASSWORD", &password)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
                .map_err(|e| NexusToolError::BadInput(format!("pg_restore: {}", e)))?
        } else {
            // psql -f per formato plain SQL
            tokio::process::Command::new("psql")
                .args([
                    "-h",
                    &host,
                    "-p",
                    &port,
                    "-U",
                    &user,
                    "-d",
                    &dbname,
                    "-f",
                    &canonical_str,
                ])
                .env("PGPASSWORD", &password)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
                .map_err(|e| NexusToolError::BadInput(format!("psql: {}", e)))?
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let stderr = String::from_utf8_lossy(&child.stderr);

        if child.status.success() || (is_custom && child.status.code() == Some(0)) {
            Ok(json!({
                "ok": true,
                "backup_path": canonical_str,
                "database": dbname,
                "format": if is_custom { "custom" } else { "plain" },
                "duration_ms": duration_ms,
                "warnings": if stderr.is_empty() { None } else { Some(stderr.chars().take(2000).collect::<String>()) },
            }))
        } else {
            Ok(json!({
                "ok": false,
                "error": stderr.chars().take(2000).collect::<String>(),
                "exit_code": child.status.code().unwrap_or(-1),
            }))
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["backup_path", "confirm"],
            "properties": {
                "backup_path": {
                    "type": "string",
                    "description": "Nome file o percorso relativo del backup (dentro .nexus/backups/)"
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Deve essere true per procedere. Operazione distruttiva."
                },
                "clean": {
                    "type": "boolean",
                    "description": "Per formato custom: esegui DROP prima di CREATE (default: true)"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: true,
        }
    }
}

async fn get_project_dsn(
    nexus_pool: &sqlx::PgPool,
    project_id: uuid::Uuid,
) -> Result<String, NexusToolError> {
    let row: Option<(Vec<u8>, String)> = sqlx::query_as(
        r#"SELECT connection_secret, engine
           FROM project_database_config
           WHERE project_id = $1
           ORDER BY is_primary DESC, created_at ASC
           LIMIT 1"#,
    )
    .bind(project_id)
    .fetch_optional(nexus_pool)
    .await
    .map_err(|e| NexusToolError::BadInput(format!("lookup config: {}", e)))?;

    let (secret_bytes, engine) = row.ok_or_else(|| {
        NexusToolError::BadInput(format!(
            "Nessuna connessione DB per il progetto {}",
            project_id
        ))
    })?;

    if engine != "postgres" {
        return Err(NexusToolError::BadInput(format!(
            "Engine '{}' non supportato",
            engine
        )));
    }

    let dsn = String::from_utf8(secret_bytes)
        .map_err(|_| NexusToolError::BadInput("connection_secret non UTF-8".into()))?;

    db_helper::normalize_dsn_pub(dsn.trim())
        .map_err(|e| NexusToolError::BadInput(format!("DSN: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety() {
        let s = ProjectDbRestoreTool.safety();
        assert!(!s.read_only);
        assert!(s.can_execute_subproc);
    }

    #[test]
    fn test_input_requires_confirm() {
        let s = ProjectDbRestoreTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "confirm"));
        assert!(required.iter().any(|v| v == "backup_path"));
    }
}
