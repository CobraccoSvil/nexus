//! `detector` — rileva motore DB e migration tool dal filesystem del progetto utente.
//!
//! Strategia: cerca marker file noti nella root del progetto (e in path standard).
//! Produce un [`DbProfile`] con confidenza, marker trovati, engine e tool.

use super::{DbEngine, DbProfile, MigrationTool};
use std::path::Path;

/// Entry point principale: riceve la root del progetto e restituisce un `DbProfile`.
/// Non fallisce mai: se nulla viene rilevato restituisce un profilo con engine Unknown.
pub fn detect_db_profile(project_root: &Path) -> DbProfile {
    let mut markers: Vec<String> = Vec::new();
    let mut engine = DbEngine::Unknown("unknown".into());
    let mut migration_tool: Option<MigrationTool> = None;
    let mut migration_path: Option<String> = None;
    let mut confidence: f32 = 0.0;

    // ── 1. Alembic (Python) ───────────────────────────────────────────────
    if project_root.join("alembic.ini").exists() {
        markers.push("alembic.ini".into());
        migration_tool = Some(MigrationTool::Alembic);
        migration_path = Some("migrations".into());
        confidence = 0.95;
    } else if project_root.join("migrations").join("env.py").exists() {
        markers.push("migrations/env.py".into());
        migration_tool = Some(MigrationTool::Alembic);
        migration_path = Some("migrations".into());
        confidence = 0.90;
    }

    // ── 2. Prisma (Node/TypeScript) ───────────────────────────────────────
    if project_root.join("prisma").join("schema.prisma").exists() {
        markers.push("prisma/schema.prisma".into());
        migration_tool = Some(MigrationTool::Prisma);
        migration_path = Some("prisma/migrations".into());
        confidence = confidence.max(0.98);
        // Leggi provider dal schema.prisma per rilevare engine
        if let Ok(schema) = std::fs::read_to_string(project_root.join("prisma/schema.prisma")) {
            engine = engine_from_prisma_schema(&schema);
        }
    }

    // ── 3. sqlx (Rust) ────────────────────────────────────────────────────
    let cargo_toml_path = project_root.join("Cargo.toml");
    if cargo_toml_path.exists() {
        if let Ok(cargo) = std::fs::read_to_string(&cargo_toml_path) {
            if cargo.contains("sqlx") {
                markers.push("Cargo.toml[sqlx]".into());
                // Cerca anche migrations/*.sql
                if project_root.join("migrations").exists() {
                    markers.push("migrations/".into());
                    migration_tool = migration_tool.or(Some(MigrationTool::Sqlx));
                    migration_path = migration_path.or(Some("migrations".into()));
                    confidence = confidence.max(0.88);
                }
            }
            // Rilevamento engine da DATABASE_URL in .env o da feature sqlx
            if cargo.contains("postgres") || cargo.contains("postgresql") {
                engine = DbEngine::Postgres;
            } else if cargo.contains("mysql") {
                engine = DbEngine::Mysql;
            } else if cargo.contains("sqlite") {
                engine = DbEngine::Sqlite;
            }
        }
    }

    // ── 4. Rails ActiveRecord (Ruby) ──────────────────────────────────────
    if project_root.join("db").join("migrate").exists() && project_root.join("Gemfile").exists() {
        markers.push("db/migrate/".into());
        markers.push("Gemfile".into());
        migration_tool = migration_tool.or(Some(MigrationTool::Rails));
        migration_path = migration_path.or(Some("db/migrate".into()));
        confidence = confidence.max(0.92);
        if let Ok(gemfile) = std::fs::read_to_string(project_root.join("Gemfile")) {
            if gemfile.contains("pg") || gemfile.contains("activerecord-postgresql") {
                engine = DbEngine::Postgres;
            } else if gemfile.contains("mysql") {
                engine = DbEngine::Mysql;
            } else if gemfile.contains("sqlite") {
                engine = DbEngine::Sqlite;
            }
        }
    }

    // ── 5. Flyway (JVM) ───────────────────────────────────────────────────
    if project_root.join("flyway.conf").exists() || project_root.join("flyway.toml").exists() {
        let marker = if project_root.join("flyway.conf").exists() {
            "flyway.conf"
        } else {
            "flyway.toml"
        };
        markers.push(marker.into());
        migration_tool = migration_tool.or(Some(MigrationTool::Flyway));
        migration_path = migration_path.or(Some("db/migration".into()));
        confidence = confidence.max(0.95);
    } else {
        // Cerca pattern V*__*.sql in db/migration
        let flyway_dir = project_root.join("db").join("migration");
        if flyway_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&flyway_dir) {
                let has_flyway = entries.flatten().any(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    name.starts_with('V') && name.contains("__") && name.ends_with(".sql")
                });
                if has_flyway {
                    markers.push("db/migration/V*__*.sql".into());
                    migration_tool = migration_tool.or(Some(MigrationTool::Flyway));
                    migration_path = migration_path.or(Some("db/migration".into()));
                    confidence = confidence.max(0.85);
                }
            }
        }
    }

    // ── 6. Django (Python) ────────────────────────────────────────────────
    if !markers.iter().any(|m| m.contains("alembic")) {
        // Django usa directory migrations/ con __init__.py e file 0001_*.py
        if let Ok(entries) = std::fs::read_dir(project_root) {
            for app_entry in entries.flatten() {
                let migrations_dir = app_entry.path().join("migrations");
                let init_py = migrations_dir.join("__init__.py");
                if migrations_dir.exists() && init_py.exists() {
                    if let Ok(sub) = std::fs::read_dir(&migrations_dir) {
                        let has_django = sub.flatten().any(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            name.starts_with("000") && name.ends_with(".py")
                        });
                        if has_django {
                            let rel = migrations_dir
                                .strip_prefix(project_root)
                                .unwrap_or(&migrations_dir)
                                .to_string_lossy()
                                .to_string();
                            markers.push(format!("{}/0001_*.py", rel));
                            migration_tool = migration_tool.or(Some(MigrationTool::Django));
                            migration_path = migration_path.or(Some(rel));
                            confidence = confidence.max(0.88);
                            break;
                        }
                    }
                }
            }
        }
    }

    // ── 7. Knex (Node.js) ─────────────────────────────────────────────────
    for knex_file in &["knexfile.js", "knexfile.ts", "knexfile.cjs"] {
        if project_root.join(knex_file).exists() {
            markers.push((*knex_file).into());
            migration_tool = migration_tool.or(Some(MigrationTool::Knex));
            migration_path = migration_path.or(Some("migrations".into()));
            confidence = confidence.max(0.90);
            break;
        }
    }

    // ── 8. Liquibase ──────────────────────────────────────────────────────
    for lb_file in &[
        "liquibase.properties",
        "changelog.xml",
        "db/changelog/db.changelog-master.xml",
    ] {
        if project_root.join(lb_file).exists() {
            markers.push((*lb_file).into());
            migration_tool = migration_tool.or(Some(MigrationTool::Liquibase));
            migration_path = migration_path.or(Some("db/changelog".into()));
            confidence = confidence.max(0.88);
            break;
        }
    }

    // ── 9. Fallback: cartella migrations/ con SQL files (generic-sql) ─────
    if migration_tool.is_none() {
        let generic_dir = project_root.join("migrations");
        if generic_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&generic_dir) {
                let has_sql = entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().ends_with(".sql"));
                if has_sql {
                    markers.push("migrations/*.sql".into());
                    migration_tool = Some(MigrationTool::GenericSql);
                    migration_path = Some("migrations".into());
                    confidence = 0.60;
                }
            }
        }
    }

    // ── 10. .NET / ASP.NET Core ───────────────────────────────────────────
    if engine == DbEngine::Unknown("unknown".into()) {
        if let Some((net_engine, net_tool, net_path)) = detect_dotnet(project_root) {
            engine = net_engine;
            migration_tool = migration_tool.or(net_tool);
            migration_path = migration_path.or(net_path);
            confidence = confidence.max(0.92);
        }
    }

    // ── 11. Engine da .env / package.json ────────────────────────────────
    if engine == DbEngine::Unknown("unknown".into()) {
        engine = detect_engine_from_config(project_root);
    }

    // Postgres è default V1 se non rilevato e c'è un migration tool
    if engine == DbEngine::Unknown("unknown".into()) && migration_tool.is_some() {
        engine = DbEngine::Postgres;
        confidence = confidence.max(0.50);
    }

    DbProfile {
        engine,
        migration_tool,
        migration_path,
        marker_files: markers,
        confidence,
    }
}

// ── Helper privati ────────────────────────────────────────────────────────

fn engine_from_prisma_schema(schema: &str) -> DbEngine {
    for line in schema.lines() {
        let line = line.trim();
        if line.starts_with("provider") && line.contains('=') {
            let val = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            return match val {
                "postgresql" | "postgres" => DbEngine::Postgres,
                "mysql" => DbEngine::Mysql,
                "sqlite" => DbEngine::Sqlite,
                "mongodb" => DbEngine::Mongodb,
                other => DbEngine::Unknown(other.to_string()),
            };
        }
    }
    DbEngine::Postgres
}

/// Rileva engine e migration tool da progetti .NET (ASP.NET Core, EF Core).
///
/// Legge nell'ordine:
/// 1. `appsettings.Development.json` → connection string
/// 2. `appsettings.json` → connection string
/// 3. `*.csproj` → PackageReference EF provider
/// 4. `*.sln` come marker presenza progetto .NET
fn detect_dotnet(project_root: &Path) -> Option<(DbEngine, Option<MigrationTool>, Option<String>)> {
    let mut engine: Option<DbEngine> = None;
    let mut migration_path: Option<String> = None;

    // Cerca file appsettings (Development ha priorità)
    for settings_file in &["appsettings.Development.json", "appsettings.json"] {
        let p = project_root.join(settings_file);
        // Cerca anche nelle sottodirectory (es. backend/FreeLance.Api/)
        let candidates = [
            p.clone(),
            project_root
                .join("backend")
                .join("FreeLance.Api")
                .join(settings_file),
            project_root.join("src").join(settings_file),
            project_root.join("Api").join(settings_file),
        ];
        for candidate in &candidates {
            if let Ok(content) = std::fs::read_to_string(candidate) {
                if let Some(eng) = engine_from_appsettings(&content) {
                    engine = Some(eng);
                    break;
                }
            }
        }
        if engine.is_some() {
            break;
        }
    }

    // Cerca *.csproj per rilevare il provider EF Core
    if engine.is_none() {
        if let Some(csproj_engine) = detect_from_csproj(project_root) {
            engine = Some(csproj_engine);
        }
    }

    // Verifica presenza *.sln come conferma progetto .NET
    let has_sln = project_root
        .join("backend")
        .read_dir()
        .into_iter()
        .flatten()
        .any(|e| {
            e.map(|d| d.file_name().to_string_lossy().ends_with(".sln"))
                .unwrap_or(false)
        })
        || project_root.read_dir().into_iter().flatten().any(|e| {
            e.map(|d| d.file_name().to_string_lossy().ends_with(".sln"))
                .unwrap_or(false)
        });

    // Cerca cartella sql/ come migration path (pattern .NET senza EF migrations)
    if project_root.join("backend").join("sql").is_dir() {
        migration_path = Some("backend/sql".into());
    } else if project_root.join("sql").is_dir() {
        migration_path = Some("sql".into());
    }

    let found_engine = engine?;

    // Se non abbiamo trovato *.sln ma abbiamo una connection string, è comunque valido
    if !has_sln && migration_path.is_none() {
        // Accettiamo se abbiamo trovato l'engine da appsettings
    }

    Some((
        found_engine,
        Some(MigrationTool::GenericSql),
        migration_path,
    ))
}

/// Estrae l'engine DB dalla connection string in appsettings JSON.
///
/// La detection deve gestire correttamente entrambe le sintassi che usano
/// la keyword `Server=`:
///   - SQL Server:   `Server=host[,port];Database=name;...`           (port via virgola)
///   - Npgsql:       `Server=host;Port=5432;Database=name;...`        (port keyword separata)
///
/// Prima dei controlli generici facciamo passare i segnali univoci di Postgres
/// (Host=, Port=5432, schema postgres://) per evitare il falso positivo SQL Server.
fn engine_from_appsettings(content: &str) -> Option<DbEngine> {
    for line in content.lines() {
        let line = line.trim();
        if !(line.contains("Connection") && line.contains(':')) {
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

        // ── 1. PostgreSQL: segnali univoci ────────────────────────────────
        // Schema URL non ambiguo
        if lc.contains("postgresql://") || lc.contains("postgres://") {
            return Some(DbEngine::Postgres);
        }
        // Keyword `Host=` e' usata SOLO da Npgsql, mai da SQL Server
        if lc.contains("host=") {
            return Some(DbEngine::Postgres);
        }
        // Porta esplicita 5432: certa Postgres
        if lc.contains("port=5432") {
            return Some(DbEngine::Postgres);
        }
        // Npgsql usa "Server=host;Port=NNNN;..." (keyword `Port=` separata).
        // SQL Server usa invece "Server=host,1433;..." (porta dopo virgola).
        // Se troviamo `Port=` come keyword separata (preceduta da `;` o inizio
        // stringa) e nessun `,1433`/`,1434`, e' Npgsql/Postgres.
        let has_explicit_port_kw = lc.contains(";port=") || lc.starts_with("port=");
        let has_sqlserver_inline_port = lc.contains(",1433") || lc.contains(",1434");
        if has_explicit_port_kw && !has_sqlserver_inline_port {
            // Se la porta e' fra le tipiche Postgres
            // (5432, 5433, 5434...) o non specificata diversamente, Postgres.
            // Ma controlliamo anche MySQL prima (3306).
            if lc.contains("port=3306") {
                return Some(DbEngine::Mysql);
            }
            return Some(DbEngine::Postgres);
        }

        // ── 2. MySQL ──────────────────────────────────────────────────────
        if lc.contains("mysql://") {
            return Some(DbEngine::Mysql);
        }
        if lc.contains("server=") && lc.contains("port=3306") {
            return Some(DbEngine::Mysql);
        }

        // ── 3. SQL Server (fallback) ──────────────────────────────────────
        // Pattern certi: "Initial Catalog=" o "Data Source=" + "Initial Catalog="
        if lc.contains("initial catalog=") {
            return Some(DbEngine::Sqlserver);
        }
        if lc.contains("data source=") && lc.contains("initial catalog=") {
            return Some(DbEngine::Sqlserver);
        }
        // Pattern legacy: Server= + Database= senza nessun segnale Postgres/MySQL
        // (gia' filtrati sopra)
        if lc.contains("server=") && lc.contains("database=") {
            return Some(DbEngine::Sqlserver);
        }
    }
    None
}

/// Rileva engine da PackageReference nei file *.csproj.
fn detect_from_csproj(project_root: &Path) -> Option<DbEngine> {
    // Cerca *.csproj nella root e nelle sottodirectory comuni
    let search_dirs = [
        project_root.to_path_buf(),
        project_root.join("backend"),
        project_root.join("src"),
    ];
    for dir in &search_dirs {
        if let Some(engine) = scan_csproj_in(dir) {
            return Some(engine);
        }
        // Cerca anche in sottodirectory del livello successivo
        if let Ok(subdirs) = std::fs::read_dir(dir) {
            for sub in subdirs.flatten() {
                if !sub.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                if let Some(engine) = scan_csproj_in(&sub.path()) {
                    return Some(engine);
                }
            }
        }
    }
    None
}

/// Scansiona `dir` cercando .csproj e rilevando il package EF Core/SqlClient.
/// Punto unico (regola L, S35) per il pattern di detection duplicato fra
/// search_dirs root e sub-dirs.
fn scan_csproj_in(dir: &Path) -> Option<DbEngine> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".csproj") {
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let lc = content.to_ascii_lowercase();
        if lc.contains("entityframeworkcore.sqlserver") || lc.contains("microsoft.data.sqlclient") {
            return Some(DbEngine::Sqlserver);
        }
        if lc.contains("npgsql.entityframeworkcore.postgresql") || lc.contains("npgsql") {
            return Some(DbEngine::Postgres);
        }
        if lc.contains("pomelo.entityframeworkcore.mysql") || lc.contains("mysqlconnector") {
            return Some(DbEngine::Mysql);
        }
        if lc.contains("microsoft.entityframeworkcore.sqlite") {
            return Some(DbEngine::Sqlite);
        }
    }
    None
}

fn detect_engine_from_config(project_root: &Path) -> DbEngine {
    // Cerca DATABASE_URL in .env
    for env_file in &[".env", ".env.local", ".env.development"] {
        let path = project_root.join(env_file);
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("DATABASE_URL") {
                    let url = line
                        .split('=')
                        .skip(1)
                        .collect::<Vec<_>>()
                        .join("=")
                        .to_lowercase();
                    if url.contains("postgres") {
                        return DbEngine::Postgres;
                    }
                    if url.contains("mysql") {
                        return DbEngine::Mysql;
                    }
                    if url.contains("sqlite") {
                        return DbEngine::Sqlite;
                    }
                    if url.contains("mongodb") {
                        return DbEngine::Mongodb;
                    }
                }
            }
        }
    }

    // Cerca in package.json (dependencies)
    let pkg = project_root.join("package.json");
    if let Ok(content) = std::fs::read_to_string(&pkg) {
        let lower = content.to_lowercase();
        if lower.contains("\"pg\"")
            || lower.contains("\"postgres\"")
            || lower.contains("\"postgresql\"")
        {
            return DbEngine::Postgres;
        }
        if lower.contains("\"mysql\"") || lower.contains("\"mysql2\"") {
            return DbEngine::Mysql;
        }
        if lower.contains("\"sqlite\"") || lower.contains("\"better-sqlite3\"") {
            return DbEngine::Sqlite;
        }
        if lower.contains("\"mongodb\"") || lower.contains("\"mongoose\"") {
            return DbEngine::Mongodb;
        }
    }

    // Cerca in requirements.txt
    let req = project_root.join("requirements.txt");
    if let Ok(content) = std::fs::read_to_string(&req) {
        let lower = content.to_lowercase();
        if lower.contains("psycopg") || lower.contains("asyncpg") {
            return DbEngine::Postgres;
        }
        if lower.contains("pymysql") || lower.contains("mysqlclient") {
            return DbEngine::Mysql;
        }
        if lower.contains("pymongo") {
            return DbEngine::Mongodb;
        }
    }

    DbEngine::Unknown("unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_detect_alembic() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("alembic.ini"), "[alembic]\n").unwrap();
        let profile = detect_db_profile(dir.path());
        assert_eq!(profile.migration_tool, Some(MigrationTool::Alembic));
        assert!(profile.confidence >= 0.90);
    }

    #[test]
    fn test_detect_prisma_postgres() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("prisma")).unwrap();
        fs::write(
            dir.path().join("prisma/schema.prisma"),
            "datasource db { provider = \"postgresql\" url = env(\"DATABASE_URL\") }",
        )
        .unwrap();
        let profile = detect_db_profile(dir.path());
        assert_eq!(profile.migration_tool, Some(MigrationTool::Prisma));
        assert_eq!(profile.engine, DbEngine::Postgres);
    }

    #[test]
    fn test_engine_from_appsettings_npgsql_with_server_keyword() {
        // Regression: Npgsql usa "Server=host;Port=5432;Database=...;" — non deve
        // essere classificato come SQL Server solo perche' contiene "Server=" e "Database=".
        let conn = r#""DefaultConnection": "Server=192.168.0.6;Port=5432;Database=redemptor;User Id=app;Password=secret;""#;
        assert_eq!(engine_from_appsettings(conn), Some(DbEngine::Postgres));
    }

    #[test]
    fn test_engine_from_appsettings_sqlserver_with_comma_port() {
        // SQL Server usa "Server=host,1433;Database=name;..." (porta dopo virgola)
        let conn = r#""DefaultConnection": "Server=localhost,1433;Database=mydb;User Id=sa;Password=pwd;""#;
        assert_eq!(engine_from_appsettings(conn), Some(DbEngine::Sqlserver));
    }

    #[test]
    fn test_engine_from_appsettings_postgres_host_keyword() {
        let conn = r#""DefaultConnection": "Host=localhost;Database=mydb;Username=u;Password=p;""#;
        assert_eq!(engine_from_appsettings(conn), Some(DbEngine::Postgres));
    }

    #[test]
    #[ignore] // richiede il progetto Redemptor in projects/
    fn test_detect_real_redemptor_project() {
        let project_root = std::path::PathBuf::from("/home/administrator/ideai/projects/redemptor");
        if !project_root.exists() {
            eprintln!("test ignored: project root not present");
            return;
        }
        let profile = detect_db_profile(&project_root);
        eprintln!(
            "REAL profile: engine={:?} marker_files={:?} confidence={}",
            profile.engine, profile.marker_files, profile.confidence
        );
        assert_eq!(
            profile.engine,
            DbEngine::Postgres,
            "expected Postgres but got {:?}",
            profile.engine
        );
    }

    #[test]
    fn test_engine_from_appsettings_full_file_with_jwt_section() {
        // Riproduzione del bug reale: il file appsettings completo contiene
        // anche sezioni come "Jwt" con "Issuer", "Audience" che potrebbero matchare
        // il filtro `Connection` se non corretto.
        let full = r#"{
  "Logging": { "LogLevel": { "Default": "Information" } },
  "Jwt": {
    "Key": "key",
    "Issuer": "http://localhost:3000",
    "Audience": "http://localhost:3000"
  },
  "ConnectionStrings": {
    "DefaultConnection": "Server=192.168.0.6;Port=5432;Database=redemptor;User Id=redemptor_app;Password=N3tm3d42dc2;Pooling=true;"
  },
  "GITHUB_WEBHOOK_BASE_URL": "https://api.redemptor.it"
}"#;
        let result = engine_from_appsettings(full);
        assert_eq!(
            result,
            Some(DbEngine::Postgres),
            "expected Postgres but got {:?} from full file",
            result
        );
    }

    #[test]
    fn test_engine_from_appsettings_sqlserver_initial_catalog() {
        let conn =
            r#""DefaultConnection": "Server=tcp:srv;Initial Catalog=db;User Id=sa;Password=pwd;""#;
        assert_eq!(engine_from_appsettings(conn), Some(DbEngine::Sqlserver));
    }

    #[test]
    fn test_no_markers_returns_unknown() {
        let dir = TempDir::new().unwrap();
        let profile = detect_db_profile(dir.path());
        assert!(profile.migration_tool.is_none());
        assert!(profile.confidence == 0.0 || matches!(profile.engine, DbEngine::Unknown(_)));
    }
}
