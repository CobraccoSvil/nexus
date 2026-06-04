//! Test connessione e detection automatica del database del progetto.
//!
//! Route:
//!   POST /api/projects/:id/db/detect          -> detect_project_db
//!   POST /api/projects/:id/db/test-connection -> test_project_db_connection

use axum::{
    extract::{Extension, Path as AxumPath, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::shared::{api_err, normalize_pg_connection_string, ApiResult};
use crate::{auth::Claims, AppState};

// ── POST /api/projects/:id/db/detect ─────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
struct DetectionResult {
    engine: Option<String>,
    migration_tool: Option<String>,
    migration_path: Option<String>,
    connection_string: Option<String>,
    hosting_mode: Option<String>,
    hints: Vec<String>,
    evidence: Vec<Value>,
}

fn read_text(path: &std::path::Path, max_bytes: usize) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(max_bytes.min(65536));
    f.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn detect_from_env_content(content: &str) -> Option<(String, String)> {
    // Prima cerca URL completi (DATABASE_URL, POSTGRES_URL, ecc.)
    let mut env_vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches('"').trim_matches('\'');
        if v.is_empty() {
            continue;
        }
        env_vars.insert(k.to_ascii_uppercase(), v.to_string());

        let engine = if v.starts_with("postgres://") || v.starts_with("postgresql://") {
            "postgres"
        } else if v.starts_with("mysql://") || v.starts_with("mariadb://") {
            "mysql"
        } else if v.starts_with("sqlite:") {
            "sqlite"
        } else if v.starts_with("mongodb://") || v.starts_with("mongodb+srv://") {
            "mongodb"
        } else {
            continue;
        };
        let upper = k.to_ascii_uppercase();
        if upper.contains("DATABASE_URL")
            || upper.contains("POSTGRES_URL")
            || upper.contains("MYSQL_URL")
            || upper.contains("DB_URL")
            || upper.contains("MONGO_URL")
            || upper.contains("MONGODB_URI")
        {
            return Some((engine.to_string(), v.to_string()));
        }
    }

    // Fallback: costruisci connection string da variabili separate (POSTGRES_*, DB_*, ecc.)
    if let Some(conn) = build_connection_from_env_vars(&env_vars) {
        return Some(conn);
    }

    None
}

/// Costruisce una connection string da variabili d'ambiente separate
/// come POSTGRES_HOST, POSTGRES_PORT, POSTGRES_DB, POSTGRES_USER, POSTGRES_PASSWORD
/// o varianti come DB_HOST, PGHOST, MYSQL_HOST, ecc.
fn build_connection_from_env_vars(
    vars: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    // Pattern di variabili per engine noti
    struct EnvPattern {
        engine: &'static str,
        host_keys: &'static [&'static str],
        port_keys: &'static [&'static str],
        db_keys: &'static [&'static str],
        user_keys: &'static [&'static str],
        pass_keys: &'static [&'static str],
        default_port: &'static str,
    }

    let patterns = [
        EnvPattern {
            engine: "postgres",
            host_keys: &["POSTGRES_HOST", "PGHOST", "DB_HOST", "DATABASE_HOST"],
            port_keys: &["POSTGRES_PORT", "PGPORT", "DB_PORT", "DATABASE_PORT"],
            db_keys: &[
                "POSTGRES_DB",
                "PGDATABASE",
                "DB_NAME",
                "DATABASE_NAME",
                "POSTGRES_DATABASE",
            ],
            user_keys: &[
                "POSTGRES_USER",
                "PGUSER",
                "DB_USER",
                "DATABASE_USER",
                "POSTGRES_USERNAME",
            ],
            pass_keys: &[
                "POSTGRES_PASSWORD",
                "PGPASSWORD",
                "DB_PASSWORD",
                "DATABASE_PASSWORD",
                "POSTGRES_PASS",
            ],
            default_port: "5432",
        },
        EnvPattern {
            engine: "mysql",
            host_keys: &["MYSQL_HOST", "DB_HOST", "DATABASE_HOST"],
            port_keys: &["MYSQL_PORT", "DB_PORT", "DATABASE_PORT"],
            db_keys: &["MYSQL_DATABASE", "MYSQL_DB", "DB_NAME", "DATABASE_NAME"],
            user_keys: &["MYSQL_USER", "MYSQL_USERNAME", "DB_USER"],
            pass_keys: &["MYSQL_PASSWORD", "MYSQL_PASS", "DB_PASSWORD"],
            default_port: "3306",
        },
    ];

    for pat in &patterns {
        let host = pat.host_keys.iter().find_map(|k| vars.get(*k));
        let db = pat.db_keys.iter().find_map(|k| vars.get(*k));

        // Serve almeno host + database per costruire una connection string utile
        if let (Some(host), Some(db)) = (host, db) {
            let port = pat
                .port_keys
                .iter()
                .find_map(|k| vars.get(*k))
                .map(|s| s.as_str())
                .unwrap_or(pat.default_port);
            let user = pat
                .user_keys
                .iter()
                .find_map(|k| vars.get(*k))
                .map(|s| s.as_str())
                .unwrap_or("");
            let pass = pat
                .pass_keys
                .iter()
                .find_map(|k| vars.get(*k))
                .map(|s| s.as_str())
                .unwrap_or("");

            let conn_str = if !user.is_empty() && !pass.is_empty() {
                format!(
                    "{}://{}:{}@{}:{}/{}",
                    pat.engine, user, pass, host, port, db
                )
            } else if !user.is_empty() {
                format!("{}://{}@{}:{}/{}", pat.engine, user, host, port, db)
            } else {
                format!("{}://{}:{}/{}", pat.engine, host, port, db)
            };

            return Some((pat.engine.to_string(), conn_str));
        }
    }

    None
}

fn scan_project_db(root: &std::path::Path) -> DetectionResult {
    let mut r = DetectionResult::default();

    // 1) .env files
    for name in [".env", ".env.local", ".env.development", ".env.example"] {
        let p = root.join(name);
        if let Some(content) = read_text(&p, 64 * 1024) {
            if let Some((engine, url)) = detect_from_env_content(&content) {
                r.evidence.push(json!({"file": name, "matched": true}));
                r.hints.push(format!("{name}: rilevato {engine}"));
                if r.engine.is_none() {
                    r.engine = Some(engine);
                }
                if r.connection_string.is_none() {
                    r.connection_string = Some(url);
                }
            }
        }
    }

    // 2) docker-compose
    for name in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let p = root.join(name);
        if let Some(content) = read_text(&p, 128 * 1024) {
            let lc = content.to_ascii_lowercase();
            if lc.contains("image: postgres") || lc.contains("image: \"postgres") {
                if r.engine.is_none() {
                    r.engine = Some("postgres".into());
                }
                r.hints.push(format!("{name}: servizio postgres"));
                if r.hosting_mode.is_none() {
                    r.hosting_mode = Some("internal".into());
                }
            }
            if lc.contains("image: mysql") || lc.contains("image: mariadb") {
                if r.engine.is_none() {
                    r.engine = Some("mysql".into());
                }
                r.hints.push(format!("{name}: servizio mysql/mariadb"));
                if r.hosting_mode.is_none() {
                    r.hosting_mode = Some("internal".into());
                }
            }
        }
    }

    // 3) Migration tools
    if root.join("prisma/schema.prisma").exists() {
        r.migration_tool = Some("prisma".into());
        r.migration_path = Some("prisma/migrations".into());
        r.hints.push("Prisma schema rilevato".into());
        if let Some(content) = read_text(&root.join("prisma/schema.prisma"), 64 * 1024) {
            if content.contains("provider = \"postgresql\"") && r.engine.is_none() {
                r.engine = Some("postgres".into());
            } else if content.contains("provider = \"mysql\"") && r.engine.is_none() {
                r.engine = Some("mysql".into());
            } else if content.contains("provider = \"sqlite\"") && r.engine.is_none() {
                r.engine = Some("sqlite".into());
            }
        }
    }
    if root.join("alembic.ini").exists() {
        r.migration_tool = Some("alembic".into());
        r.migration_path = Some("alembic/versions".into());
        r.hints.push("Alembic rilevato".into());
    }
    for knex in ["knexfile.js", "knexfile.ts", "knexfile.cjs", "knexfile.mjs"] {
        if root.join(knex).exists() {
            r.migration_tool = Some("knex".into());
            r.migration_path = Some("migrations".into());
            r.hints.push(format!("Knex: {knex}"));
            break;
        }
    }
    if root.join("flyway.conf").exists() || root.join("conf/flyway.conf").exists() {
        r.migration_tool = Some("flyway".into());
        r.migration_path = Some("db/migration".into());
        r.hints.push("Flyway rilevato".into());
    }
    for dir in [
        "migrations",
        "db/migrations",
        "database/migrations",
        "sql/migrations",
    ] {
        if root.join(dir).is_dir() {
            if r.migration_path.is_none() {
                r.migration_path = Some(dir.into());
            }
            r.hints.push(format!("Cartella migration: {dir}"));
            break;
        }
    }

    // 4) package.json dependencies
    if let Some(content) = read_text(&root.join("package.json"), 128 * 1024) {
        let lc = content.to_ascii_lowercase();
        if lc.contains("\"prisma\"") && r.migration_tool.is_none() {
            r.migration_tool = Some("prisma".into());
        }
        if lc.contains("\"knex\"") && r.migration_tool.is_none() {
            r.migration_tool = Some("knex".into());
        }
        if lc.contains("\"typeorm\"") && r.migration_tool.is_none() {
            r.migration_tool = Some("generic_sql".into());
            r.hints.push("TypeORM rilevato".into());
        }
        if lc.contains("\"pg\"") && r.engine.is_none() {
            r.engine = Some("postgres".into());
            r.hints.push("dep pg".into());
        }
        if (lc.contains("\"mysql2\"") || lc.contains("\"mysql\"")) && r.engine.is_none() {
            r.engine = Some("mysql".into());
            r.hints.push("dep mysql".into());
        }
    }

    // 5) pyproject/requirements
    for f in ["pyproject.toml", "requirements.txt", "Pipfile"] {
        if let Some(content) = read_text(&root.join(f), 64 * 1024) {
            let lc = content.to_ascii_lowercase();
            if lc.contains("alembic") && r.migration_tool.is_none() {
                r.migration_tool = Some("alembic".into());
            }
            if (lc.contains("psycopg") || lc.contains("asyncpg")) && r.engine.is_none() {
                r.engine = Some("postgres".into());
                r.hints.push(format!("{f}: driver postgres"));
            }
            if lc.contains("pymysql") && r.engine.is_none() {
                r.engine = Some("mysql".into());
                r.hints.push(format!("{f}: driver mysql"));
            }
        }
    }

    // 6) Cargo.toml
    if let Some(content) = read_text(&root.join("Cargo.toml"), 64 * 1024) {
        let lc = content.to_ascii_lowercase();
        if lc.contains("sqlx") || lc.contains("diesel") {
            if lc.contains("postgres") && r.engine.is_none() {
                r.engine = Some("postgres".into());
            }
            if lc.contains("mysql") && r.engine.is_none() {
                r.engine = Some("mysql".into());
            }
            if lc.contains("sqlite") && r.engine.is_none() {
                r.engine = Some("sqlite".into());
            }
            r.hints.push("Cargo.toml: driver DB rilevato".into());
        }
    }

    // 7) .NET / ASP.NET Core — appsettings.json e *.csproj
    if r.engine.is_none() {
        // Leggi appsettings.Development.json poi appsettings.json
        for settings in ["appsettings.Development.json", "appsettings.json"] {
            let candidates = [
                root.join(settings),
                root.join("backend").join("FreeLance.Api").join(settings),
                root.join("src").join(settings),
                root.join("Api").join(settings),
            ];
            for candidate in &candidates {
                if let Some(content) = read_text(candidate, 64 * 1024) {
                    for line in content.lines() {
                        let line = line.trim();
                        if !line.contains("Connection") || !line.contains(':') {
                            continue;
                        }
                        let value = line
                            .split(':')
                            .skip(1)
                            .collect::<Vec<_>>()
                            .join(":")
                            .trim()
                            .to_string();
                        let value = value.trim_matches('"').trim_matches(',').trim_matches('"');
                        let lc = value.to_ascii_lowercase();

                        // Helper di classificazione: priorita' ai segnali univoci
                        // di Postgres (Host=, Port=5432, postgres://) PRIMA di SQL Server.
                        // Necessario perche' Npgsql usa "Server=host;Port=5432;Database=...",
                        // stessi token usati da SQL Server (Server=host,1433;Database=...).
                        let detected: Option<&'static str> =
                            if lc.contains("postgresql://") || lc.contains("postgres://") {
                                Some("postgres")
                            } else if lc.contains("host=") {
                                Some("postgres")
                            } else if lc.contains("port=5432") {
                                Some("postgres")
                            } else if lc.contains("port=3306") {
                                Some("mysql")
                            } else if lc.contains("mysql://") {
                                Some("mysql")
                            } else if lc.contains("initial catalog=") {
                                // Univocamente SQL Server
                                Some("sqlserver")
                            } else if lc.contains("server=")
                                && (lc.contains(",1433") || lc.contains(",1434"))
                            {
                                // Sintassi SQL Server con porta inline
                                Some("sqlserver")
                            } else if lc.contains(";port=") || lc.starts_with("port=") {
                                // `Port=` keyword separato (non SQL Server) ma porta non 5432/3306
                                // -> probabile Postgres su porta non standard
                                Some("postgres")
                            } else if lc.contains("server=") && lc.contains("database=") {
                                // Fallback legacy: nessun segnale Postgres/MySQL trovato
                                Some("sqlserver")
                            } else {
                                None
                            };

                        match detected {
                            Some("postgres") => {
                                r.engine = Some("postgres".into());
                                r.hints.push(format!("{}: rilevato PostgreSQL", settings));
                                if r.connection_string.is_none() {
                                    r.connection_string = Some(value.to_string());
                                }
                                break;
                            }
                            Some("mysql") => {
                                r.engine = Some("mysql".into());
                                r.hints.push(format!("{}: rilevato MySQL", settings));
                                if r.connection_string.is_none() {
                                    r.connection_string = Some(value.to_string());
                                }
                                break;
                            }
                            Some("sqlserver") => {
                                r.engine = Some("sqlserver".into());
                                r.hints.push(format!("{}: rilevato SQL Server", settings));
                                if r.connection_string.is_none() {
                                    r.connection_string = Some(value.to_string());
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                    if r.engine.is_some() {
                        break;
                    }
                }
            }
            if r.engine.is_some() {
                break;
            }
        }
    }

    // 8) *.csproj — PackageReference EF Core provider
    if r.engine.is_none() {
        'csproj: for search_dir in [root, &root.join("backend"), &root.join("src")] {
            if let Ok(entries) = std::fs::read_dir(search_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !name.ends_with(".csproj") {
                        continue;
                    }
                    if let Some(content) = read_text(&entry.path(), 32 * 1024) {
                        let lc = content.to_ascii_lowercase();
                        if lc.contains("entityframeworkcore.sqlserver")
                            || lc.contains("microsoft.data.sqlclient")
                        {
                            r.engine = Some("sqlserver".into());
                            r.hints.push(format!("{name}: EF Core SQL Server"));
                            break 'csproj;
                        }
                        if lc.contains("npgsql.entityframeworkcore.postgresql") {
                            r.engine = Some("postgres".into());
                            r.hints.push(format!("{name}: EF Core PostgreSQL (Npgsql)"));
                            break 'csproj;
                        }
                        if lc.contains("pomelo.entityframeworkcore.mysql") {
                            r.engine = Some("mysql".into());
                            r.hints.push(format!("{name}: EF Core MySQL"));
                            break 'csproj;
                        }
                        if lc.contains("microsoft.entityframeworkcore.sqlite") {
                            r.engine = Some("sqlite".into());
                            r.hints.push(format!("{name}: EF Core SQLite"));
                            break 'csproj;
                        }
                    }
                }
            }
            // Cerca anche un livello più in profondità
            if let Ok(subdirs) = std::fs::read_dir(search_dir) {
                for sub in subdirs.flatten() {
                    if !sub.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    if let Ok(entries) = std::fs::read_dir(sub.path()) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if !name.ends_with(".csproj") {
                                continue;
                            }
                            if let Some(content) = read_text(&entry.path(), 32 * 1024) {
                                let lc = content.to_ascii_lowercase();
                                if lc.contains("entityframeworkcore.sqlserver")
                                    || lc.contains("microsoft.data.sqlclient")
                                {
                                    r.engine = Some("sqlserver".into());
                                    r.hints.push(format!("{name}: EF Core SQL Server"));
                                    break 'csproj;
                                }
                                if lc.contains("npgsql.entityframeworkcore.postgresql") {
                                    r.engine = Some("postgres".into());
                                    r.hints.push(format!("{name}: EF Core PostgreSQL (Npgsql)"));
                                    break 'csproj;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if r.migration_tool.is_none() {
        r.migration_tool = Some("generic_sql".into());
    }
    if r.migration_path.is_none() {
        r.migration_path = Some("migrations".into());
    }
    if r.hosting_mode.is_none() {
        r.hosting_mode = Some("external".into());
    }
    r
}

pub async fn detect_project_db(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    let root_path: Option<String> = sqlx::query_scalar(
        r#"SELECT COALESCE(r.root_path, p.analysis_json->>'rootPath', '')
           FROM projects p LEFT JOIN repositories r ON r.project_id = p.id
           WHERE p.id = $1"#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let root_path = root_path.unwrap_or_default();
    if root_path.is_empty() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            "Root path progetto non disponibile. Rianalizza il progetto.",
        ));
    }
    let root = std::path::PathBuf::from(&root_path);
    if !root.is_dir() {
        return Err(api_err(
            StatusCode::BAD_REQUEST,
            format!("Root path non trovato: {}", root.display()),
        ));
    }

    let result = tokio::task::spawn_blocking(move || scan_project_db(&root))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Salva metadata (merge in detection_metadata, senza sovrascrivere config esistente)
    let meta = serde_json::to_value(&result).unwrap_or(json!({}));
    let _ = sqlx::query(
        r#"
        INSERT INTO project_database_config (project_id, detection_metadata)
        VALUES ($1, $2)
        ON CONFLICT (project_id) DO UPDATE SET
            detection_metadata = EXCLUDED.detection_metadata,
            updated_at = NOW()
        "#,
    )
    .bind(project_id)
    .bind(&meta)
    .execute(&state.db)
    .await;

    Ok(Json(json!({
        "ok": true,
        "engine": result.engine,
        "migration_tool": result.migration_tool,
        "migration_path": result.migration_path,
        "connection_string": result.connection_string,
        "hosting_mode": result.hosting_mode,
        "hints": result.hints,
    })))
}

// ── POST /api/projects/:id/db/test-connection ────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TestConnectionBody {
    pub engine: Option<String>,
    pub connection_string: Option<String>,
    /// Identifica la connessione salvata da testare (per name logico o id).
    pub name: Option<String>,
    pub connection_id: Option<Uuid>,
}

pub async fn test_project_db_connection(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<TestConnectionBody>,
) -> ApiResult {
    // URL: dal body (override esplicito) oppure dalla connessione salvata
    // individuata da connection_id / name / primary.
    let url = if let Some(u) = body
        .connection_string
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        u.to_string()
    } else {
        let saved: Option<Vec<u8>> = if let Some(id) = body.connection_id {
            sqlx::query_scalar(
                "SELECT connection_secret FROM project_database_config WHERE project_id=$1 AND id=$2",
            )
            .bind(project_id)
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten()
        } else if let Some(n) = body.name.as_deref() {
            sqlx::query_scalar(
                "SELECT connection_secret FROM project_database_config WHERE project_id=$1 AND LOWER(name)=LOWER($2)",
            )
            .bind(project_id)
            .bind(n)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten()
        } else {
            sqlx::query_scalar(
                "SELECT connection_secret FROM project_database_config WHERE project_id=$1 ORDER BY is_primary DESC, LOWER(name) LIMIT 1",
            )
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten()
        };

        let from_secret = saved
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| !s.trim().is_empty());

        if let Some(s) = from_secret {
            s
        } else {
            let detected: Option<String> = sqlx::query_scalar::<_, Option<String>>(
                r#"SELECT detection_metadata->>'connection_string'
                   FROM project_database_config WHERE project_id=$1
                   ORDER BY is_primary DESC, LOWER(name) LIMIT 1"#,
            )
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .flatten();
            detected.ok_or_else(|| api_err(
                StatusCode::BAD_REQUEST,
                "Nessuna connection string configurata per il progetto. Configura una connessione nel pannello Database o esegui il provisioning interno.",
            ))?
        }
    };

    let engine = body.engine.unwrap_or_else(|| {
        if url.starts_with("mysql") {
            "mysql".into()
        } else if url.starts_with("sqlite") {
            "sqlite".into()
        } else if url.starts_with("jdbc:sqlserver") || {
            let lc = url.to_lowercase();
            lc.contains("server=")
                && (lc.contains("initial catalog=")
                    || lc.contains("database=")
                    || lc.contains("data source="))
        } {
            "sqlserver".into()
        } else {
            "postgres".into()
        }
    });

    let started = std::time::Instant::now();
    match engine.as_str() {
        "postgres" => {
            // Converte stringhe ADO.NET (Host=...;Port=...) in URL postgres://...
            let pg_url = normalize_pg_connection_string(&url);
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&pg_url)
                .await
            {
                Ok(pool) => {
                    let ver: Result<(String,), _> =
                        sqlx::query_as("SELECT version()").fetch_one(&pool).await;
                    let count: Result<(i64,), _> = sqlx::query_as(
                        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='public'"
                    ).fetch_one(&pool).await;
                    pool.close().await;
                    Ok(Json(json!({
                        "ok": true,
                        "engine": "postgres",
                        "server_version": ver.ok().map(|(v,)| v),
                        "table_count": count.ok().map(|(c,)| c),
                        "latency_ms": started.elapsed().as_millis() as u64,
                    })))
                }
                Err(e) => Ok(Json(json!({
                    "ok": false,
                    "engine": "postgres",
                    "error": e.to_string(),
                }))),
            }
        }
        "mysql" => Ok(Json(json!({
            "ok": false,
            "engine": "mysql",
            "error": "Driver MySQL non abilitato in mcp-core; configurare sqlx feature 'mysql' per abilitarlo.",
        }))),
        "sqlite" => Ok(Json(json!({
            "ok": false,
            "engine": "sqlite",
            "error": "Driver SQLite non abilitato in mcp-core; configurare sqlx feature 'sqlite' per abilitarlo.",
        }))),
        "sqlserver" => {
            match test_sqlserver_connection(&url).await {
                Ok((version, table_count)) => Ok(Json(json!({
                    "ok": true,
                    "engine": "sqlserver",
                    "server_version": version,
                    "table_count": table_count,
                    "latency_ms": started.elapsed().as_millis() as u64,
                }))),
                Err(e) => {
                    let msg = e.to_string();
                    // Aggiunge un suggerimento contestuale per gli errori SQL Server più comuni
                    let hint = if msg.contains("4060")
                        || msg.contains("non è possibile aprire il database")
                        || msg.contains("Cannot open database")
                    {
                        Some("Il database esiste ma l'utente non ha accesso: verifica che l'account SQL abbia il permesso 'db_datareader' (o superiore) sul database specificato.")
                    } else if msg.contains("18456")
                        || msg.contains("L'accesso non è riuscito")
                        || msg.contains("Login failed")
                    {
                        Some("Credenziali non valide: verifica utente e password nella connection string.")
                    } else if msg.contains("Impossibile raggiungere")
                        || msg.contains("Connection refused")
                        || msg.contains("timed out")
                    {
                        Some("Server non raggiungibile: verifica host, porta e che il servizio SQL Server sia in ascolto.")
                    } else {
                        None
                    };
                    Ok(Json(json!({
                        "ok": false,
                        "engine": "sqlserver",
                        "error": msg,
                        "hint": hint,
                    })))
                }
            }
        }
        other => Ok(Json(json!({
            "ok": false,
            "engine": other,
            "error": format!("Engine non supportato: {other}"),
        }))),
    }
}

/// Testa la connessione a SQL Server usando tiberius (driver TDS nativo).
/// Accetta connection string in formato ADO.NET oppure JDBC-like:
///   - ADO.NET: `Server=host,port;Database=db;User Id=user;Password=pwd;...`
///   - JDBC:    `jdbc:sqlserver://host:port;databaseName=db;user=user;password=pwd`
async fn test_sqlserver_connection(conn_str: &str) -> anyhow::Result<(String, i64)> {
    use tiberius::{Client, Config};
    use tokio::net::TcpStream;
    use tokio_util::compat::TokioAsyncWriteCompatExt;

    // Usa il parser ADO.NET manuale per massima compatibilita'.
    // tiberius from_ado_string non riconosce "User Id" (chiave ADO.NET ufficiale .NET/C#).
    let config = if conn_str.trim_start().to_lowercase().starts_with("jdbc:") {
        Config::from_jdbc_string(conn_str)
            .map_err(|e| anyhow::anyhow!("Connection string JDBC non valida: {e}"))?
    } else {
        build_sqlserver_config(conn_str)?
    };

    let addr = config.get_addr();
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("Impossibile raggiungere il server ({addr}): {e}"))?;
    tcp.set_nodelay(true)?;

    let mut client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| {
            let raw = e.to_string();
            if let Some(start) = raw.find("Token error: '") {
                let inner = &raw[start + "Token error: '".len()..];
                let clean = if let Some(p) = inner.find("' on server") {
                    &inner[..p]
                } else if let Some(p) = inner.rfind('\'') {
                    &inner[..p]
                } else {
                    inner.trim_end_matches('\'')
                };
                anyhow::anyhow!("{}", clean)
            } else if raw.to_lowercase().contains("tls")
                || raw.to_lowercase().contains("certificate")
            {
                anyhow::anyhow!("Errore TLS/certificato: {raw}")
            } else {
                anyhow::anyhow!("Login SQL Server fallito: {raw}")
            }
        })?;

    // Versione server
    let version: String = client
        .query("SELECT @@VERSION", &[])
        .await
        .map_err(|e| anyhow::anyhow!("Query @@VERSION fallita: {e}"))?
        .into_row()
        .await?
        .and_then(|r| r.get::<&str, usize>(0).map(String::from))
        .unwrap_or_else(|| "sconosciuta".into());

    // Numero tabelle nel database corrente
    let table_count: i64 = client
        .query(
            "SELECT CAST(COUNT(*) AS BIGINT) FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_TYPE='BASE TABLE'",
            &[],
        )
        .await
        .map_err(|e| anyhow::anyhow!("Query tabelle fallita: {e}"))?
        .into_row()
        .await?
        .and_then(|r| r.get::<i64, usize>(0))
        .unwrap_or(0);

    Ok((version, table_count))
}

/// Parser ADO.NET manuale per costruire un `Config` tiberius.
///
/// Gestisce tutte le varianti di chiave usate in .NET:
///   - `Server` / `Data Source`                -> host + porta
///   - `Database` / `Initial Catalog`          -> nome database
///   - `User Id` / `User ID` / `UID` / `User`  -> username SQL
///   - `Password` / `PWD`                      -> password
///   - `Encrypt`                               -> livello crittografia
///   - `TrustServerCertificate`                -> trust certificato self-signed
fn build_sqlserver_config(conn_str: &str) -> anyhow::Result<tiberius::Config> {
    use std::collections::HashMap;
    use tiberius::{AuthMethod, Config, EncryptionLevel};

    // Tokenizza "Key=Value;" ignorando segmenti vuoti (es. ; finale)
    let mut params: HashMap<String, String> = HashMap::new();
    for part in conn_str.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(eq_pos) = part.find('=') {
            // Normalizza chiave: lowercase, spazi multipli -> singolo spazio
            let key = part[..eq_pos]
                .trim()
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let val = part[eq_pos + 1..].trim().to_string();
            params.insert(key, val);
        }
    }

    let mut config = Config::new();

    // ── Host + porta ─────────────────────────────────────────────────────────
    let server = params
        .get("server")
        .or_else(|| params.get("data source"))
        .map(|s| s.as_str())
        .unwrap_or("localhost");

    // Formati: "host,porta" | "tcp:host,porta" | "host\istanza" | "host"
    let server_clean = server.trim_start_matches("tcp:");
    let (host, port) = if let Some(comma) = server_clean.find(',') {
        let h = &server_clean[..comma];
        let p: u16 = server_clean[comma + 1..].trim().parse().unwrap_or(1433);
        (h, p)
    } else if let Some(bs) = server_clean.find('\\') {
        (&server_clean[..bs], 1433u16)
    } else {
        (server_clean, 1433u16)
    };

    config.host(host);
    config.port(port);

    // ── Database ─────────────────────────────────────────────────────────────
    if let Some(db) = params
        .get("database")
        .or_else(|| params.get("initial catalog"))
    {
        config.database(db.as_str());
    }

    // ── Autenticazione ───────────────────────────────────────────────────────
    // Chiavi normalizzate (lowercase, spazio singolo):
    //   "user id" -> "User Id" / "User ID" (ADO.NET ufficiale .NET/C#)
    //   "uid"     -> abbreviazione
    //   "user"    -> variante breve
    let user = params
        .get("user id")
        .or_else(|| params.get("uid"))
        .or_else(|| params.get("user"))
        .cloned();
    let pwd = params
        .get("password")
        .or_else(|| params.get("pwd"))
        .cloned();

    match (user, pwd) {
        (Some(u), Some(p)) => {
            config.authentication(AuthMethod::sql_server(u, p));
        }
        (Some(u), None) => {
            config.authentication(AuthMethod::sql_server(u, String::new()));
        }
        _ => {
            // Nessuna credenziale SQL: Windows Auth (non disponibile su Linux senza Kerberos)
        }
    }

    // ── Crittografia ─────────────────────────────────────────────────────────
    let encrypt = params.get("encrypt").map(|s| s.to_lowercase());
    match encrypt.as_deref() {
        Some("false") | Some("no") | Some("0") | Some("optional") => {
            config.encryption(EncryptionLevel::Off);
        }
        Some("true") | Some("yes") | Some("1") | Some("mandatory") | Some("strict") => {
            config.encryption(EncryptionLevel::Required);
        }
        _ => {}
    }

    // ── TrustServerCertificate ───────────────────────────────────────────────
    let trust = params
        .get("trustservercertificate")
        .or_else(|| params.get("trust server certificate"))
        .map(|s| s.to_lowercase());
    if matches!(trust.as_deref(), Some("true") | Some("yes") | Some("1")) {
        config.trust_cert();
    }

    Ok(config)
}
