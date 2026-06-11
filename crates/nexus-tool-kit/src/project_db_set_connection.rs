//! `project_db_set_connection` — configura la connessione DB per il progetto corrente.
//!
//! Permette all'agente di impostare la stringa di connessione e i metadati
//! del database del progetto, evitando che debba usare psql o modificare
//! tabelle Nexus direttamente.

use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use crate::db_helper::get_pool;
use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::Row;

pub struct ProjectDbSetConnectionTool;

#[async_trait]
impl NexusToolHandler for ProjectDbSetConnectionTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let connection_string = args
            .get("connection_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                NexusToolError::BadInput("Parametro 'connection_string' obbligatorio".into())
            })?;

        if connection_string.trim().is_empty() {
            return Err(NexusToolError::BadInput(
                "La stringa di connessione non puo' essere vuota".into(),
            ));
        }

        let engine = args
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("postgres");

        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("primary");

        let hosting_mode = args
            .get("hosting_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("internal");

        let migration_tool = args.get("migration_tool").and_then(|v| v.as_str());

        let migration_path = args.get("migration_path").and_then(|v| v.as_str());

        let is_primary = args
            .get("is_primary")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let pool = get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("db connect: {}", e)))?;

        let project_id = ctx.project_id;

        // Verifica che il progetto esista
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
                .bind(project_id)
                .fetch_one(&pool)
                .await
                .map_err(|e| NexusToolError::BadInput(format!("verifica progetto: {}", e)))?;

        if !exists {
            pool.close().await;
            return Err(NexusToolError::BadInput(format!(
                "Progetto {} non trovato nel DB",
                project_id
            )));
        }

        // ── PR hardening: DB isolation per hosting_mode='internal' ────────────
        // Se il DSN punta allo stesso cluster PostgreSQL di Nexus, creiamo un
        // ruolo e database dedicato per il progetto con REVOKE sui DB infrastruttura.
        let effective_dsn = if hosting_mode == "internal" && engine == "postgres" {
            match super::db_helper::ensure_project_db_isolation(
                &pool,
                project_id,
                connection_string,
            )
            .await
            {
                Ok(isolated) => isolated,
                Err(e) => {
                    tracing::warn!(
                        project_id = %project_id,
                        error = %e,
                        "DB isolation fallita: uso DSN originale (degradazione graceful)"
                    );
                    connection_string.to_string()
                }
            }
        } else {
            connection_string.to_string()
        };

        let secret_bytes = effective_dsn.as_bytes().to_vec();

        // Se is_primary, azzera il flag sulle altre connessioni
        if is_primary {
            sqlx::query(
                "UPDATE project_database_config SET is_primary = false WHERE project_id = $1",
            )
            .bind(project_id)
            .execute(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("reset primary: {}", e)))?;
        }

        // Upsert: cerca per (project_id, LOWER(name))
        let existing_id: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM project_database_config WHERE project_id = $1 AND LOWER(name) = LOWER($2)",
        )
        .bind(project_id)
        .bind(name)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("check existing: {}", e)))?;

        if let Some(id) = existing_id {
            // Update
            sqlx::query(
                r#"UPDATE project_database_config
                   SET engine = $1, hosting_mode = $2, connection_secret = $3,
                       migration_tool = $4, migration_path = $5, is_primary = $6,
                       updated_at = NOW()
                   WHERE id = $7"#,
            )
            .bind(engine)
            .bind(hosting_mode)
            .bind(&secret_bytes)
            .bind(migration_tool)
            .bind(migration_path)
            .bind(is_primary)
            .bind(id)
            .execute(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("update: {}", e)))?;
        } else {
            // Insert
            sqlx::query(
                r#"INSERT INTO project_database_config
                   (id, project_id, name, engine, hosting_mode, connection_secret,
                    migration_tool, migration_path, is_primary, allow_ddl_override,
                    detection_metadata, created_at, updated_at)
                   VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, false, '{}'::jsonb, NOW(), NOW())"#,
            )
            .bind(project_id)
            .bind(name)
            .bind(engine)
            .bind(hosting_mode)
            .bind(&secret_bytes)
            .bind(migration_tool)
            .bind(migration_path)
            .bind(is_primary)
            .execute(&pool)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("insert: {}", e)))?;
        }

        // Verifica lettura
        let row = sqlx::query(
            r#"SELECT name, engine, hosting_mode, is_primary,
                      ENCODE(connection_secret, 'escape') AS connection_string
               FROM project_database_config
               WHERE project_id = $1 AND LOWER(name) = LOWER($2)"#,
        )
        .bind(project_id)
        .bind(name)
        .fetch_optional(&pool)
        .await
        .map_err(|e| NexusToolError::BadInput(format!("verifica: {}", e)))?;

        pool.close().await;

        match row {
            Some(r) => {
                let saved_name: String = r.try_get("name").unwrap_or_default();
                let saved_engine: Option<String> = r.try_get("engine").unwrap_or(None);
                let saved_dsn: Option<String> = r.try_get("connection_string").unwrap_or(None);
                let action_str = if existing_id.is_some() {
                    "updated"
                } else {
                    "created"
                };

                // Notifica il pannello DB frontend via dispatcher SSE
                nexus_events::dispatcher::emit_global(
                    project_id,
                    nexus_events::event::ProjectEvent::DbConfigUpdated {
                        name: saved_name.clone(),
                        engine: saved_engine.clone(),
                        action: action_str.to_string(),
                    },
                );

                Ok(json!({
                    "ok": true,
                    "action": action_str,
                    "name": saved_name,
                    "engine": saved_engine,
                    "connection_string": saved_dsn,
                    "is_primary": is_primary,
                    "message": format!("Connessione '{}' configurata con successo per il progetto.", saved_name)
                }))
            }
            None => Err(NexusToolError::BadInput(
                "Connessione salvata ma non leggibile — verifica il DB".into(),
            )),
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["connection_string"],
            "properties": {
                "connection_string": {
                    "type": "string",
                    "description": "Stringa di connessione al DB del progetto. Formato PostgreSQL: postgres://user:pass@host:port/dbname. Formato ADO.NET: Server=host;Port=5432;Database=dbname;User Id=user;Password=pass;"
                },
                "engine": {
                    "type": "string",
                    "description": "Tipo di DB: postgres, mysql, sqlite, mssql. Default: postgres",
                    "enum": ["postgres", "mysql", "sqlite", "mssql"]
                },
                "name": {
                    "type": "string",
                    "description": "Nome logico della connessione. Default: 'primary'"
                },
                "hosting_mode": {
                    "type": "string",
                    "description": "Modalita' hosting: internal (DB locale o LAN), external (DB remoto/cloud). Default: internal",
                    "enum": ["internal", "external"]
                },
                "migration_tool": {
                    "type": "string",
                    "description": "Strumento migration: ef-core, sqlx, flyway, liquibase, ecc."
                },
                "migration_path": {
                    "type": "string",
                    "description": "Path relativo alla root del progetto dove risiedono le migration"
                },
                "is_primary": {
                    "type": "boolean",
                    "description": "Se true, questa diventa la connessione primaria del progetto. Default: true"
                }
            }
        })
    }

    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety {
            read_only: false,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: true,
        }
    }
}
