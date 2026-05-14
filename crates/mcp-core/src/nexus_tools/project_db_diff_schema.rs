//! `project_db_diff_schema` — confronta lo schema DB attuale con un file SQL.
//!
//! Esegue pg_dump --schema-only e diff con un file di riferimento.
//! Utile per verificare lo stato post-migration.

use super::db_helper;
use super::project_db_backup::parse_dsn_parts;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbDiffSchemaTool;

#[async_trait]
impl NexusToolHandler for ProjectDbDiffSchemaTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let reference_file = args
            .get("reference_file")
            .and_then(Value::as_str)
            .ok_or_else(|| NexusToolError::BadInput("Parametro 'reference_file' obbligatorio".into()))?
            .trim()
            .to_string();

        // Valida path dentro project_root
        let ref_path = ctx.project_root.join(&reference_file);
        let canonical = ref_path.canonicalize().map_err(|_| {
            NexusToolError::BadInput(format!("File '{}' non trovato", reference_file))
        })?;

        if !canonical.starts_with(&ctx.project_root) {
            return Err(NexusToolError::BadInput(
                "File di riferimento fuori dalla root del progetto".into(),
            ));
        }

        // Ottieni DSN
        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let dsn = get_project_dsn(&nexus_pool, ctx.project_id).await?;
        nexus_pool.close().await;

        let (host, port, dbname, user, password) = parse_dsn_parts(&dsn)?;

        // Dump schema attuale in file temporaneo
        let tmp_dir = ctx.project_root.join(".nexus").join("tmp");
        tokio::fs::create_dir_all(&tmp_dir)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("create tmp dir: {}", e)))?;

        let tmp_file = tmp_dir.join("current_schema.sql");
        let tmp_str = tmp_file.to_string_lossy().to_string();

        let dump = tokio::process::Command::new("pg_dump")
            .args([
                "-h", &host,
                "-p", &port,
                "-U", &user,
                "-d", &dbname,
                "--schema-only",
                "-f", &tmp_str,
            ])
            .env("PGPASSWORD", &password)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("pg_dump: {}", e)))?;

        if !dump.status.success() {
            let stderr = String::from_utf8_lossy(&dump.stderr);
            return Ok(json!({
                "ok": false,
                "error": format!("pg_dump fallito: {}", stderr.chars().take(1000).collect::<String>()),
            }));
        }

        // Esegui diff
        let canonical_str = canonical.to_string_lossy().to_string();
        let diff_out = tokio::process::Command::new("diff")
            .args(["-u", &canonical_str, &tmp_str])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("diff: {}", e)))?;

        // Pulisci file temporaneo
        let _ = tokio::fs::remove_file(&tmp_file).await;

        let diff_text = String::from_utf8_lossy(&diff_out.stdout);
        let has_differences = !diff_out.status.success(); // diff exit 1 = differenze trovate

        let truncated = if diff_text.len() > 6000 {
            format!(
                "{}... [troncato, {} caratteri totali]",
                &diff_text[..6000],
                diff_text.len()
            )
        } else {
            diff_text.to_string()
        };

        Ok(json!({
            "ok": true,
            "has_differences": has_differences,
            "reference_file": reference_file,
            "database": dbname,
            "diff": if has_differences { Some(truncated) } else { None },
            "diff_lines": diff_text.lines().count(),
        }))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["reference_file"],
            "properties": {
                "reference_file": {
                    "type": "string",
                    "description": "Percorso relativo al file SQL di riferimento (dentro project_root)"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: true,
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
        let s = ProjectDbDiffSchemaTool.safety();
        assert!(s.can_execute_subproc);
        assert!(s.can_write_filesystem);
    }

    #[test]
    fn test_input_requires_reference() {
        let s = ProjectDbDiffSchemaTool.input_schema();
        let required = s["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "reference_file"));
    }
}
