//! `project_db_backup` — esegue pg_dump sul DB del progetto.
//!
//! Salva il backup in `project_root/.nexus/backups/<timestamp>.sql`.
//! Supporta formato plain o custom, schema-only opzionale.

use super::db_helper;
use super::exec;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct ProjectDbBackupTool;

/// Estrae host, port, dbname, user, password da un DSN postgres://user:pass@host:port/dbname
///
/// Gestisce correttamente:
/// - Password con caratteri speciali (`:`, `@`, `/`, `#`, ecc.)
/// - Valori URL-encoded (`%40` -> `@`, `%3A` -> `:`, ecc.)
/// - DSN senza porta (default 5432)
/// - DSN senza credenziali
/// - Query parameters (`?sslmode=require`) ignorati
pub(crate) fn parse_dsn_parts(
    dsn: &str,
) -> Result<(String, String, String, String, String), NexusToolError> {
    // Formato atteso: postgres://user:password@host:port/dbname?params
    let s = dsn
        .strip_prefix("postgres://")
        .or_else(|| dsn.strip_prefix("postgresql://"))
        .ok_or_else(|| NexusToolError::BadInput("DSN deve iniziare con postgres://".into()))?;

    // Separa userinfo@hostinfo con rfind('@') — l'ultimo '@' delimita userinfo
    // da hostinfo. La password puo' contenere '@' URL-encoded (%40) ma anche
    // '@' letterale se il DSN e' stato composto a mano. rfind prende l'ultimo,
    // che e' quello strutturale.
    let (userinfo, rest) = if let Some(at_pos) = s.rfind('@') {
        (&s[..at_pos], &s[at_pos + 1..])
    } else {
        ("", s)
    };

    // Parsing user:password — il PRIMO ':' separa user da password.
    // La password e' tutto cio' che segue il primo ':', inclusi eventuali
    // altri ':' (es. "admin:p@ss:w0rd!#" -> user="admin", pass="p@ss:w0rd!#").
    let (user_raw, password_raw) = if let Some(colon) = userinfo.find(':') {
        (&userinfo[..colon], &userinfo[colon + 1..])
    } else {
        (userinfo, "")
    };

    // Decodifica URL-encoding (%40 -> @, %3A -> :, ecc.)
    let user = url_decode(user_raw);
    let password = url_decode(password_raw);

    // Separa host:port/dbname (rimuovi query params)
    let rest_no_params = rest.split('?').next().unwrap_or(rest);
    let (hostport, dbname_raw) = if let Some(slash) = rest_no_params.find('/') {
        (&rest_no_params[..slash], &rest_no_params[slash + 1..])
    } else {
        (rest_no_params, "")
    };

    // Host e porta — rfind(':') per gestire IPv6 (anche se raro)
    let (host, port) = if let Some(colon) = hostport.rfind(':') {
        let port_candidate = &hostport[colon + 1..];
        // Verifica che sia davvero una porta (solo cifre)
        if port_candidate.chars().all(|c| c.is_ascii_digit()) && !port_candidate.is_empty() {
            (hostport[..colon].to_string(), port_candidate.to_string())
        } else {
            (hostport.to_string(), "5432".to_string())
        }
    } else {
        (hostport.to_string(), "5432".to_string())
    };

    let dbname = url_decode(dbname_raw);

    if dbname.is_empty() {
        return Err(NexusToolError::BadInput(
            "Nome database mancante nel DSN".into(),
        ));
    }

    Ok((host, port, dbname, user, password))
}

/// Decodifica percent-encoding in una stringa DSN.
/// Gestisce %XX dove XX e' un byte esadecimale (es. %40 -> '@', %3A -> ':').
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                } else {
                    // Percentuale non valida, mantieni letterale
                    result.push('%');
                    result.push_str(&hex);
                }
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}

#[async_trait]
impl NexusToolHandler for ProjectDbBackupTool {
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError> {
        let schema_only = args
            .get("schema_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("plain")
            .trim()
            .to_string();

        if format != "plain" && format != "custom" {
            return Err(NexusToolError::BadInput(
                "Formato deve essere 'plain' o 'custom'".into(),
            ));
        }

        // Ottieni DSN del progetto
        let nexus_pool = db_helper::get_pool()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("nexus db: {}", e)))?;

        let dsn = get_project_dsn(&nexus_pool, ctx.project_id).await?;
        nexus_pool.close().await;

        let (host, port, dbname, user, password) = parse_dsn_parts(&dsn)?;

        // Crea directory backup
        let backup_dir = ctx.project_root.join(".nexus").join("backups");
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|e| NexusToolError::BadInput(format!("create backup dir: {}", e)))?;

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let ext = if format == "custom" { "dump" } else { "sql" };
        let suffix = if schema_only { "-schema" } else { "" };
        let filename = format!("{}{}-{}.{}", dbname, suffix, timestamp, ext);
        let backup_path = backup_dir.join(&filename);
        let backup_path_str = backup_path.to_string_lossy().to_string();

        let mut cmd_args: Vec<String> = vec![
            "-h".to_string(),
            host,
            "-p".to_string(),
            port,
            "-U".to_string(),
            user,
            "-d".to_string(),
            dbname.clone(),
            "-f".to_string(),
            backup_path_str.clone(),
        ];

        if format == "custom" {
            cmd_args.push("-Fc".to_string());
        }

        if schema_only {
            cmd_args.push("--schema-only".to_string());
        }

        let args_ref: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();

        // pg_dump con PGPASSWORD
        let start = std::time::Instant::now();
        let child = tokio::process::Command::new("pg_dump")
            .args(&args_ref)
            .current_dir(&ctx.project_root)
            .env("PGPASSWORD", &password)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| NexusToolError::BadInput(format!("pg_dump: {}", e)))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        if child.status.success() {
            let size = tokio::fs::metadata(&backup_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);

            Ok(json!({
                "ok": true,
                "path": backup_path_str,
                "filename": filename,
                "database": dbname,
                "format": format,
                "schema_only": schema_only,
                "size_bytes": size,
                "duration_ms": duration_ms,
            }))
        } else {
            let stderr = String::from_utf8_lossy(&child.stderr);
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
            "properties": {
                "format": {
                    "type": "string",
                    "enum": ["plain", "custom"],
                    "description": "Formato backup: 'plain' (SQL leggibile) o 'custom' (compresso, per pg_restore). Default: plain"
                },
                "schema_only": {
                    "type": "boolean",
                    "description": "Se true, esporta solo lo schema (no dati). Default: false"
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
            "Nessuna connessione DB per il progetto {}. Usa project_db_set_connection.",
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

    let normalized = db_helper::normalize_dsn_pub(dsn.trim())
        .map_err(|e| NexusToolError::BadInput(format!("DSN: {}", e)))?;

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dsn_simple() {
        let (h, p, db, u, pw) =
            parse_dsn_parts("postgres://admin:secret@db.local:5433/mydb").unwrap();
        assert_eq!(h, "db.local");
        assert_eq!(p, "5433");
        assert_eq!(db, "mydb");
        assert_eq!(u, "admin");
        assert_eq!(pw, "secret");
    }

    #[test]
    fn test_parse_dsn_defaults() {
        let (h, p, db, _, _) = parse_dsn_parts("postgres://u:p@localhost/testdb").unwrap();
        assert_eq!(h, "localhost");
        assert_eq!(p, "5432");
        assert_eq!(db, "testdb");
    }

    #[test]
    fn test_parse_dsn_password_with_colon() {
        // Password contiene ':' — deve prendere tutto dopo il primo ':'
        let (_, _, _, u, pw) = parse_dsn_parts("postgres://admin:p@ss:w0rd@host/db").unwrap();
        assert_eq!(u, "admin");
        assert_eq!(pw, "p@ss:w0rd");
    }

    #[test]
    fn test_parse_dsn_password_url_encoded() {
        // Password con caratteri speciali URL-encoded (%40 = @, %3A = :, %23 = #)
        let (_, _, _, u, pw) =
            parse_dsn_parts("postgres://admin:p%40ss%3Aw%23rd@host:5432/db").unwrap();
        assert_eq!(u, "admin");
        assert_eq!(pw, "p@ss:w#rd");
    }

    #[test]
    fn test_parse_dsn_password_with_special_chars() {
        // Password complessa con molti caratteri speciali
        let (_, _, _, _, pw) =
            parse_dsn_parts("postgres://u:My%21P%40ss%3Dw0rd%26x@host/db").unwrap();
        assert_eq!(pw, "My!P@ss=w0rd&x");
    }

    #[test]
    fn test_parse_dsn_user_url_encoded() {
        let (_, _, _, u, _) = parse_dsn_parts("postgres://admin%40domain:pass@host/db").unwrap();
        assert_eq!(u, "admin@domain");
    }

    #[test]
    fn test_parse_dsn_with_query_params() {
        let (h, p, db, _, _) =
            parse_dsn_parts("postgres://u:p@host:5433/mydb?sslmode=require&timeout=30").unwrap();
        assert_eq!(h, "host");
        assert_eq!(p, "5433");
        assert_eq!(db, "mydb");
    }

    #[test]
    fn test_parse_dsn_no_password() {
        let (_, _, _, u, pw) = parse_dsn_parts("postgres://readonly@host/db").unwrap();
        assert_eq!(u, "readonly");
        assert_eq!(pw, "");
    }

    #[test]
    fn test_parse_dsn_no_credentials() {
        let (h, _, db, u, pw) = parse_dsn_parts("postgres://host/db").unwrap();
        assert_eq!(h, "host");
        assert_eq!(db, "db");
        assert_eq!(u, "");
        assert_eq!(pw, "");
    }

    #[test]
    fn test_parse_dsn_missing_db() {
        assert!(parse_dsn_parts("postgres://u:p@host").is_err());
    }

    #[test]
    fn test_parse_dsn_wrong_prefix() {
        assert!(parse_dsn_parts("mysql://u:p@host/db").is_err());
    }

    #[test]
    fn test_url_decode_basic() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("a%40b%3Ac"), "a@b:c");
        assert_eq!(url_decode("no+encoding+here"), "no encoding here");
        assert_eq!(url_decode("plain"), "plain");
    }

    #[test]
    fn test_url_decode_invalid_percent() {
        // Percentuale incompleta o non valida — mantiene letterale
        assert_eq!(url_decode("abc%ZZdef"), "abc%ZZdef");
        assert_eq!(url_decode("abc%2"), "abc%2");
    }

    #[test]
    fn test_safety() {
        let s = ProjectDbBackupTool.safety();
        assert!(!s.read_only);
        assert!(s.can_write_filesystem);
        assert!(s.can_execute_subproc);
    }
}
