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

use super::shared::{api_err, normalize_pg_connection_string, ApiError, ApiResult};
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
    let f = std::fs::File::open(path).ok()?;
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

/// Pattern di variabili d'ambiente per un engine noto: per ogni campo della
/// connection string, l'elenco ordinato delle chiavi alternative accettate.
struct EnvPattern {
    engine: &'static str,
    host_keys: &'static [&'static str],
    port_keys: &'static [&'static str],
    db_keys: &'static [&'static str],
    user_keys: &'static [&'static str],
    pass_keys: &'static [&'static str],
    default_port: &'static str,
}

/// Pattern noti, nell'ordine di precedenza applicato dalla detection.
const ENV_PATTERNS: &[EnvPattern] = &[
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

/// Primo valore presente in `vars` tra le chiavi `keys`, nell'ordine dato.
fn first_var<'a>(
    vars: &'a std::collections::HashMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter().find_map(|k| vars.get(*k)).map(|s| s.as_str())
}

/// Connection string per un singolo pattern. Serve almeno host + database:
/// senza uno dei due ritorna `None` e il chiamante passa al pattern seguente.
/// Porta, utente e password hanno un default (porta dell'engine / stringa vuota).
fn env_pattern_conn(
    pat: &EnvPattern,
    vars: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    let host = first_var(vars, pat.host_keys)?;
    let db = first_var(vars, pat.db_keys)?;
    let port = first_var(vars, pat.port_keys).unwrap_or(pat.default_port);
    let user = first_var(vars, pat.user_keys).unwrap_or("");
    let pass = first_var(vars, pat.pass_keys).unwrap_or("");

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

    Some((pat.engine.to_string(), conn_str))
}

/// Costruisce una connection string da variabili d'ambiente separate
/// come POSTGRES_HOST, POSTGRES_PORT, POSTGRES_DB, POSTGRES_USER, POSTGRES_PASSWORD
/// o varianti come DB_HOST, PGHOST, MYSQL_HOST, ecc.
fn build_connection_from_env_vars(
    vars: &std::collections::HashMap<String, String>,
) -> Option<(String, String)> {
    ENV_PATTERNS
        .iter()
        .find_map(|pat| env_pattern_conn(pat, vars))
}

/// Sostituisce hostname che sono nomi di servizio compose con 'localhost' per
/// l'accesso dall'host esterno (il mapping di porta espone il servizio sul
/// loopback host). Lista non esaustiva ma copre i nomi convenzionali piu'
/// comuni: `db`, `postgres`, `postgresql`, `pg`, `mysql`, `mariadb`. Altri
/// hostname (es. `db.local`, IP, FQDN) vengono lasciati invariati.
fn normalize_compose_host(host: &str) -> &str {
    match host.to_ascii_lowercase().as_str() {
        "db" | "postgres" | "postgresql" | "pg" | "mysql" | "mariadb" => "localhost",
        _ => host,
    }
}

/// Cerca una connection string DB nel content di un docker-compose. Strategia:
///
/// (a) Una variabile env tipo `DATABASE_URL=postgres://user:pass@host:port/db`
///     dentro un blocco `environment:` (sotto qualsiasi servizio): e' la fonte
///     piu' affidabile perche' e' gia' parametrizzata correttamente per
///     l'applicazione. L'host viene normalizzato via `normalize_compose_host`.
///
/// (b) Se non c'e' una URL gia' pronta, si combinano i valori
///     POSTGRES_USER/POSTGRES_PASSWORD/POSTGRES_DB visti nel docker-compose con
///     la porta HOST del mapping `ports: HOST:CONTAINER`. La porta default e'
///     5432 per postgres. Host fisso a `localhost` perche' siamo sull'host
///     esterno che accede al servizio via port mapping.
///
/// Ritorna None se non riesce a costruire una stringa utile. Niente full YAML
/// parser: lettura riga per riga con regex/string match, sufficiente per i
/// pattern reali osservati nei docker-compose generati dall'agente Nexus.
fn extract_compose_connection_string(content: &str) -> Option<String> {
    compose_url_from_env(content).or_else(|| compose_conn_from_pg_vars(content))
}

/// Strategia (a): DATABASE_URL / POSTGRES_URL / DB_URL gia' pronta dentro un
/// blocco `environment:`. L'hostname viene normalizzato con
/// [`normalize_compose_host`], il resto della URL resta invariato.
fn compose_url_from_env(content: &str) -> Option<String> {
    use regex::Regex;
    let url_re = Regex::new(
        r#"(?im)^\s*-?\s*(?:DATABASE_URL|POSTGRES_URL|DB_URL)\s*[:=]\s*['"]?(\w+://[^\s'"]+)['"]?"#,
    )
    .ok()?;
    let cap = url_re.captures(content)?;
    let raw = cap.get(1)?.as_str();
    // Sostituzione hostname: cerchiamo lo schema://[user[:pass]@]host[:port]/db
    let split_re = Regex::new(r#"^(\w+://)(?:([^@/]+)@)?([^:/]+)(:\d+)?(.*)$"#).ok()?;
    if let Some(parts) = split_re.captures(raw) {
        let scheme = parts.get(1)?.as_str();
        let userinfo = parts.get(2).map(|m| m.as_str()).unwrap_or("");
        let host = parts.get(3)?.as_str();
        let port = parts.get(4).map(|m| m.as_str()).unwrap_or("");
        let tail = parts.get(5).map(|m| m.as_str()).unwrap_or("");
        let host_norm = normalize_compose_host(host);
        let auth = if userinfo.is_empty() {
            String::new()
        } else {
            format!("{userinfo}@")
        };
        return Some(format!("{scheme}{auth}{host_norm}{port}{tail}"));
    }
    // URL non scomponibile: si ritorna cosi' com'e'.
    Some(raw.to_string())
}

/// Strategia (b): POSTGRES_USER + POSTGRES_PASSWORD + POSTGRES_DB combinati con
/// la porta HOST del mapping `ports:`. Host fisso a `localhost` perche' siamo
/// sull'host esterno che accede al servizio via port mapping.
fn compose_conn_from_pg_vars(content: &str) -> Option<String> {
    use regex::Regex;
    let postgres_var = |name: &str| -> Option<String> {
        let re = Regex::new(&format!(
            r#"(?im)^\s*-?\s*{name}\s*[:=]\s*['"]?([^\s'"]+)['"]?"#,
        ))
        .ok()?;
        re.captures(content)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
    };
    let user = postgres_var("POSTGRES_USER")?;
    let db = postgres_var("POSTGRES_DB")?;
    let pass = postgres_var("POSTGRES_PASSWORD").unwrap_or_default();
    // Porta host dal mapping `- "5432:5432"` o `- 5432:5432`. Default 5432.
    let port_re = Regex::new(r#"(?m)^\s*-\s*['"]?(\d{2,5})\s*:\s*5432['"]?"#).ok()?;
    let port = port_re
        .captures(content)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .unwrap_or_else(|| "5432".to_string());
    let auth = if pass.is_empty() {
        format!("{user}@")
    } else {
        format!("{user}:{pass}@")
    };
    Some(format!("postgres://{auth}localhost:{port}/{db}"))
}

/// Scansiona la root del progetto e deduce engine, tool/percorso di migration e
/// connection string. Ogni passo raffina lo stesso [`DetectionResult`] e non
/// sovrascrive i campi gia' valorizzati: vince la prima evidenza trovata,
/// nell'ordine dei passi.
fn scan_project_db(root: &std::path::Path) -> DetectionResult {
    let mut r = DetectionResult::default();
    scan_env_files(root, &mut r);
    scan_compose_files(root, &mut r);
    scan_prisma(root, &mut r);
    scan_migration_tools(root, &mut r);
    scan_package_json(root, &mut r);
    scan_python_manifests(root, &mut r);
    scan_cargo_manifest(root, &mut r);
    scan_dotnet_appsettings(root, &mut r);
    scan_csproj(root, &mut r);
    apply_detection_defaults(&mut r);
    r
}

/// (1) File `.env*`: engine e connection string dalle variabili d'ambiente.
fn scan_env_files(root: &std::path::Path, r: &mut DetectionResult) {
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
}

/// Engine e hosting_mode dall'immagine del servizio DB dichiarata nel compose.
/// `lc` e' il contenuto del file gia' in minuscolo, `name` il nome del file.
fn compose_service_image(name: &str, lc: &str, r: &mut DetectionResult) {
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

/// (2) docker-compose: servizio DB + connection string EFFETTIVA.
///
/// Bug storico: il pannello DB rilevava il servizio postgres ma metteva
/// credenziali placeholder (username/password/db vuoti) perche' non si parsava
/// la sezione environment del servizio. Risultato: l'utente vedeva "Database
/// progetto non configurato" e non capiva se il DB applicativo esistesse o no.
/// La connection string viene ora estratta da
/// [`extract_compose_connection_string`], che cerca in ordine:
///   (a) DATABASE_URL / POSTGRES_URL / DB_URL gia' pronto (la fonte piu'
///       affidabile, e' una connection string completa);
///   (b) altrimenti combina POSTGRES_USER/PASSWORD/DB visti nella sezione
///       environment del servizio db + porta host del mapping ports
///       (es. 5432:5432 -> host=localhost:5432).
/// Gli hostname che corrispondono a nomi di servizio docker (db, postgres,
/// postgresql, pg, mysql, mariadb) vengono sostituiti con 'localhost' perche'
/// dall'host esterno il servizio risponde sul mapping di porta, non sul nome di
/// servizio del network compose.
fn scan_compose_files(root: &std::path::Path, r: &mut DetectionResult) {
    for name in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let p = root.join(name);
        let Some(content) = read_text(&p, 128 * 1024) else {
            continue;
        };
        compose_service_image(name, &content.to_ascii_lowercase(), r);
        if r.connection_string.is_none() {
            if let Some(cs) = extract_compose_connection_string(&content) {
                r.connection_string = Some(cs);
                r.hints.push(format!("{name}: connection string estratta"));
            }
        }
    }
}

/// (3a) Prisma: schema, cartella migration ed engine dal `provider` dichiarato.
fn scan_prisma(root: &std::path::Path, r: &mut DetectionResult) {
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
}

/// (3b) Altri tool di migration (alembic, knex, flyway) e cartelle migration
/// convenzionali. Applicato dopo [`scan_prisma`], che ha la precedenza.
fn scan_migration_tools(root: &std::path::Path, r: &mut DetectionResult) {
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
}

/// Tool di migration dalle dipendenze di `package.json` (contenuto minuscolo).
fn package_json_migration_tool(lc: &str, r: &mut DetectionResult) {
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
}

/// Engine dai driver DB in `package.json` (contenuto minuscolo).
fn package_json_engine(lc: &str, r: &mut DetectionResult) {
    if lc.contains("\"pg\"") && r.engine.is_none() {
        r.engine = Some("postgres".into());
        r.hints.push("dep pg".into());
    }
    if (lc.contains("\"mysql2\"") || lc.contains("\"mysql\"")) && r.engine.is_none() {
        r.engine = Some("mysql".into());
        r.hints.push("dep mysql".into());
    }
}

/// (4) Dipendenze dichiarate in `package.json`.
fn scan_package_json(root: &std::path::Path, r: &mut DetectionResult) {
    let Some(content) = read_text(&root.join("package.json"), 128 * 1024) else {
        return;
    };
    let lc = content.to_ascii_lowercase();
    package_json_migration_tool(&lc, r);
    package_json_engine(&lc, r);
}

/// (5) Manifest Python: `pyproject.toml`, `requirements.txt`, `Pipfile`.
fn scan_python_manifests(root: &std::path::Path, r: &mut DetectionResult) {
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
}

/// (6) `Cargo.toml`: engine dedotto dalle feature di sqlx/diesel.
fn scan_cargo_manifest(root: &std::path::Path, r: &mut DetectionResult) {
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
}

/// Classifica una connection string .NET (`lc` gia' in minuscolo) e ritorna
/// `(engine, etichetta per l'hint)`.
///
/// Priorita' ai segnali univoci di Postgres (Host=, Port=5432, postgres://)
/// PRIMA di SQL Server: Npgsql usa "Server=host;Port=5432;Database=...", cioe'
/// gli stessi token di SQL Server ("Server=host,1433;Database=...").
fn classify_dotnet_connection(lc: &str) -> Option<(&'static str, &'static str)> {
    if lc.contains("postgresql://")
        || lc.contains("postgres://")
        || lc.contains("host=")
        || lc.contains("port=5432")
    {
        Some(("postgres", "PostgreSQL"))
    } else if lc.contains("port=3306") || lc.contains("mysql://") {
        Some(("mysql", "MySQL"))
    } else if lc.contains("initial catalog=") {
        // Univocamente SQL Server
        Some(("sqlserver", "SQL Server"))
    } else if lc.contains("server=") && (lc.contains(",1433") || lc.contains(",1434")) {
        // Sintassi SQL Server con porta inline
        Some(("sqlserver", "SQL Server"))
    } else if lc.contains(";port=") || lc.starts_with("port=") {
        // `Port=` keyword separato (non SQL Server) ma porta non 5432/3306
        // -> probabile Postgres su porta non standard
        Some(("postgres", "PostgreSQL"))
    } else if lc.contains("server=") && lc.contains("database=") {
        // Fallback legacy: nessun segnale Postgres/MySQL trovato
        Some(("sqlserver", "SQL Server"))
    } else {
        None
    }
}

/// Applica a `r` il primo engine riconosciuto tra le righe "Connection" di un
/// file appsettings. Le righe non classificabili vengono ignorate.
fn apply_appsettings_content(content: &str, settings: &str, r: &mut DetectionResult) {
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

        let Some((engine, label)) = classify_dotnet_connection(&lc) else {
            continue;
        };
        r.engine = Some(engine.into());
        r.hints.push(format!("{}: rilevato {}", settings, label));
        if r.connection_string.is_none() {
            r.connection_string = Some(value.to_string());
        }
        break;
    }
}

/// (7) .NET / ASP.NET Core — `appsettings.json`. Legge
/// `appsettings.Development.json` e poi `appsettings.json`, fermandosi al primo
/// engine riconosciuto.
fn scan_dotnet_appsettings(root: &std::path::Path, r: &mut DetectionResult) {
    if r.engine.is_some() {
        return;
    }
    for settings in nexus_project_db::detector::APPSETTINGS_FILES {
        let candidates = [
            root.join(settings),
            root.join("backend").join("FreeLance.Api").join(settings),
            root.join("src").join(settings),
            root.join("Api").join(settings),
        ];
        for candidate in &candidates {
            let Some(content) = read_text(candidate, 64 * 1024) else {
                continue;
            };
            apply_appsettings_content(&content, settings, r);
            if r.engine.is_some() {
                break;
            }
        }
        if r.engine.is_some() {
            break;
        }
    }
}

/// Provider EF Core riconoscibile da un `PackageReference` in un `.csproj`.
struct CsprojPattern {
    /// Marcatori alternativi: basta che uno compaia nel file.
    needles: &'static [&'static str],
    engine: &'static str,
    hint: &'static str,
}

/// Provider noti, nell'ordine di precedenza applicato dentro un singolo file.
const CSPROJ_PATTERNS: &[CsprojPattern] = &[
    CsprojPattern {
        needles: &["entityframeworkcore.sqlserver", "microsoft.data.sqlclient"],
        engine: "sqlserver",
        hint: "EF Core SQL Server",
    },
    CsprojPattern {
        needles: &["npgsql.entityframeworkcore.postgresql"],
        engine: "postgres",
        hint: "EF Core PostgreSQL (Npgsql)",
    },
    CsprojPattern {
        needles: &["pomelo.entityframeworkcore.mysql"],
        engine: "mysql",
        hint: "EF Core MySQL",
    },
    CsprojPattern {
        needles: &["microsoft.entityframeworkcore.sqlite"],
        engine: "sqlite",
        hint: "EF Core SQLite",
    },
];

/// Primo provider tra `patterns` trovato nei `.csproj` direttamente dentro
/// `dir`. Ritorna `(engine, hint completo di nome file)`.
fn csproj_engine_in_dir(
    dir: &std::path::Path,
    patterns: &[CsprojPattern],
) -> Option<(String, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".csproj") {
            continue;
        }
        let Some(content) = read_text(&entry.path(), 32 * 1024) else {
            continue;
        };
        let lc = content.to_ascii_lowercase();
        let found = patterns
            .iter()
            .find(|p| p.needles.iter().any(|n| lc.contains(n)));
        if let Some(pat) = found {
            return Some((pat.engine.to_string(), format!("{}: {}", name, pat.hint)));
        }
    }
    None
}

/// Come [`csproj_engine_in_dir`] ma un livello piu' in profondita': cerca nelle
/// sottodirectory dirette di `dir`.
fn csproj_engine_in_subdirs(
    dir: &std::path::Path,
    patterns: &[CsprojPattern],
) -> Option<(String, String)> {
    let subdirs = std::fs::read_dir(dir).ok()?;
    for sub in subdirs.flatten() {
        if !sub.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some(found) = csproj_engine_in_dir(&sub.path(), patterns) {
            return Some(found);
        }
    }
    None
}

/// (8) `*.csproj` — provider EF Core dichiarato come PackageReference.
///
/// Nota comportamentale storica: nel livello diretto si riconoscono tutti i
/// provider noti, nelle sottodirectory solo i primi due (sqlserver, postgres).
/// L'asimmetria e' preservata passando un prefisso di [`CSPROJ_PATTERNS`].
fn scan_csproj(root: &std::path::Path, r: &mut DetectionResult) {
    if r.engine.is_some() {
        return;
    }
    for search_dir in [root, &root.join("backend"), &root.join("src")] {
        let found = csproj_engine_in_dir(search_dir, CSPROJ_PATTERNS)
            .or_else(|| csproj_engine_in_subdirs(search_dir, &CSPROJ_PATTERNS[..2]));
        if let Some((engine, hint)) = found {
            r.engine = Some(engine);
            r.hints.push(hint);
            return;
        }
    }
}

/// Default finali per i campi che nessun passo ha valorizzato.
fn apply_detection_defaults(r: &mut DetectionResult) {
    if r.migration_tool.is_none() {
        r.migration_tool = Some("generic_sql".into());
    }
    if r.migration_path.is_none() {
        r.migration_path = Some("migrations".into());
    }
    if r.hosting_mode.is_none() {
        r.hosting_mode = Some("external".into());
    }
}

/// Root path del progetto (repository oppure `analysis_json->>'rootPath'`),
/// validata come directory esistente.
async fn project_root_dir(
    state: &AppState,
    project_id: Uuid,
) -> Result<std::path::PathBuf, ApiError> {
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
    Ok(root)
}

/// Salva la detection sulla riga PRIMARIA: e' quella che il pannello mostra
/// (`get_project_db_config`) e da cui si legge il fallback
/// `detection_metadata->>'connection_string'` in `test_project_db_connection`.
///
/// `ON CONFLICT (project_id) WHERE is_primary` aggancia l'indice PARZIALE
/// `uq_project_database_config_project_primary`. Il conflict target va
/// qualificato perche' l'UNIQUE sul solo `project_id` NON esiste piu' (mig
/// 0083: la tabella e' multi-connessione per progetto): la query precedente
/// diceva `ON CONFLICT (project_id)` e falliva SEMPRE con "no unique or
/// exclusion constraint matching the ON CONFLICT specification", per giunta
/// ingoiata da un `let _`, quindi la detection_metadata non veniva mai salvata e
/// nessuno se ne accorgeva.
///
/// Best-effort dichiarato: la detection e' comunque ritornata al chiamante. Ma
/// l'errore si LOGGA (regola H): era proprio il `let _` a rendere invisibile una
/// query rotta in modo incondizionato.
async fn save_detection_metadata(state: &AppState, project_id: Uuid, result: &DetectionResult) {
    let meta = serde_json::to_value(result).unwrap_or(json!({}));
    let saved = match (result.engine.as_deref(), result.hosting_mode.as_deref()) {
        // Detection completa: se la config manca la creiamo, se c'e' gia'
        // aggiorniamo SOLO la metadata (engine/hosting_mode scelti dall'utente
        // non si toccano - "senza sovrascrivere config esistente").
        (Some(engine), Some(hosting_mode)) => {
            sqlx::query(
                r#"
                INSERT INTO project_database_config
                    (project_id, engine, hosting_mode, detection_metadata)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (project_id) WHERE is_primary DO UPDATE SET
                    detection_metadata = EXCLUDED.detection_metadata,
                    updated_at = NOW()
                "#,
            )
            .bind(project_id)
            .bind(engine)
            .bind(hosting_mode)
            .bind(&meta)
            .execute(&state.db)
            .await
        }
        // Engine o hosting_mode non rilevati: NON inventiamo una config (le due
        // colonne sono NOT NULL senza default, regola G: niente magic fallback).
        // Aggiorniamo la metadata solo se la riga esiste gia'.
        _ => {
            sqlx::query(
                r#"
                UPDATE project_database_config
                SET detection_metadata = $2, updated_at = NOW()
                WHERE project_id = $1 AND is_primary
                "#,
            )
            .bind(project_id)
            .bind(&meta)
            .execute(&state.db)
            .await
        }
    };
    if let Err(e) = saved {
        tracing::warn!(
            project_id = %project_id,
            error = %e,
            "detect_project_db: salvataggio di detection_metadata fallito"
        );
    }
}

pub async fn detect_project_db(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
) -> ApiResult {
    let root = project_root_dir(&state, project_id).await?;

    let result = tokio::task::spawn_blocking(move || scan_project_db(&root))
        .await
        .map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    save_detection_metadata(&state, project_id, &result).await;

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

/// `connection_secret` della connessione indicata dal body: per `connection_id`,
/// altrimenti per `name`, altrimenti quella primaria.
async fn saved_connection_secret(
    state: &AppState,
    project_id: Uuid,
    body: &TestConnectionBody,
) -> Result<Option<Vec<u8>>, ApiError> {
    let row = if let Some(id) = body.connection_id {
        sqlx::query_scalar(
            "SELECT connection_secret FROM project_database_config WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&state.db)
        .await
    } else if let Some(n) = body.name.as_deref() {
        sqlx::query_scalar(
            "SELECT connection_secret FROM project_database_config WHERE project_id=$1 AND LOWER(name)=LOWER($2)",
        )
        .bind(project_id)
        .bind(n)
        .fetch_optional(&state.db)
        .await
    } else {
        sqlx::query_scalar(
            "SELECT connection_secret FROM project_database_config WHERE project_id=$1 ORDER BY is_primary DESC, LOWER(name) LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
    };
    let row: Option<Option<Vec<u8>>> =
        row.map_err(|e| api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(row.flatten())
}

/// URL da testare: dal body (override esplicito), altrimenti dal secret della
/// connessione salvata, altrimenti dalla `detection_metadata`.
async fn resolve_test_connection_url(
    state: &AppState,
    project_id: Uuid,
    body: &TestConnectionBody,
) -> Result<String, ApiError> {
    if let Some(u) = body
        .connection_string
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(u.to_string());
    }

    let from_secret = saved_connection_secret(state, project_id, body)
        .await?
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|s| !s.trim().is_empty());
    if let Some(s) = from_secret {
        return Ok(s);
    }

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
    ))
}

/// Engine dedotto dalla forma della URL, quando il body non lo dichiara.
fn infer_engine_from_url(url: &str) -> String {
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
}

/// Test postgres: pool monoconnessione, versione server e numero tabelle dello
/// schema `public`. Un errore di connessione non e' un errore HTTP: viene
/// riportato nel payload con `ok: false`.
/// Classifica un fallimento di connessione Postgres da segnali STRUTTURATI —
/// SQLSTATE (`DatabaseError::code`) e `std::io::ErrorKind` — mai dal testo del
/// messaggio (regola M).
///
/// Il `Display` di `sqlx::Error::Database` e' fisso: `"error returned from
/// database: {message del server}"` (sqlx-core 0.8.6, error.rs). Quel prefisso
/// contiene SEMPRE la parola "database", quindi qualunque classificazione
/// testuale a valle che cerchi "does not exist" insieme a "database" scambia
/// un ruolo/tabella/estensione inesistente (qualunque `does not exist` del
/// server) per un database inesistente — verificato sul sorgente sqlx, non
/// assunto (regola O). Lo SQLSTATE non compare mai nel Display: e' raggiungibile
/// solo via `.code()`.
fn classify_pg_connection_error(e: &sqlx::Error) -> &'static str {
    match e {
        sqlx::Error::Database(db_err) => match db_err.code().as_deref() {
            // 3D000 invalid_catalog_name: il database indicato non esiste.
            Some("3D000") => "no_database",
            // Classe 28 (invalid_authorization_specification / invalid_password):
            // Postgres restituisce indifferentemente 28000 o 28P01 per ruolo
            // inesistente o password errata (non distingue i due casi per non
            // rivelare se il ruolo esiste).
            Some(code) if code.starts_with("28") => "auth_failed",
            // Classe 08 (connection_exception): il server ha accettato il TCP
            // ma ha rifiutato/chiuso la sessione applicativa.
            Some(code) if code.starts_with("08") => "unreachable",
            _ => "unknown",
        },
        // La richiesta non e' mai arrivata a un server Postgres: nessuno SQLSTATE
        // possibile, il segnale e' il kind del socket.
        sqlx::Error::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::TimedOut => "unreachable",
            _ => "unknown",
        },
        // Pool che non riesce ad aprire la prima connessione entro il timeout:
        // stesso esito pratico di un socket che non risponde.
        sqlx::Error::PoolTimedOut => "unreachable",
        sqlx::Error::Tls(_) => "unreachable",
        _ => "unknown",
    }
}

async fn test_postgres_engine(url: &str, started: std::time::Instant) -> ApiResult {
    // Converte stringhe ADO.NET (Host=...;Port=...) in URL postgres://...
    let pg_url = normalize_pg_connection_string(url);
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
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='public'",
            )
            .fetch_one(&pool)
            .await;
            pool.close().await;
            Ok(Json(json!({
                "ok": true,
                "engine": "postgres",
                "server_version": ver.ok().map(|(v,)| v),
                "table_count": count.ok().map(|(c,)| c),
                "latency_ms": started.elapsed().as_millis() as u64,
            })))
        }
        Err(e) => {
            let category = classify_pg_connection_error(&e);
            Ok(Json(json!({
                "ok": false,
                "engine": "postgres",
                "error": e.to_string(),
                "category": category,
            })))
        }
    }
}

/// Suggerimento contestuale per gli errori SQL Server piu' comuni. Solo per
/// display all'utente: nessuna decisione tecnica dipende da questo testo.
fn sqlserver_error_hint(msg: &str) -> Option<&'static str> {
    if msg.contains("4060")
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
    }
}

/// Test SQL Server: esito e suggerimento diagnostico nel payload JSON.
async fn test_sqlserver_engine(url: &str, started: std::time::Instant) -> ApiResult {
    match test_sqlserver_connection(url).await {
        Ok((version, table_count)) => Ok(Json(json!({
            "ok": true,
            "engine": "sqlserver",
            "server_version": version,
            "table_count": table_count,
            "latency_ms": started.elapsed().as_millis() as u64,
        }))),
        Err(e) => {
            let msg = e.to_string();
            let hint = sqlserver_error_hint(&msg);
            Ok(Json(json!({
                "ok": false,
                "engine": "sqlserver",
                "error": msg,
                "hint": hint,
            })))
        }
    }
}

pub async fn test_project_db_connection(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<Uuid>,
    Json(body): Json<TestConnectionBody>,
) -> ApiResult {
    let url = resolve_test_connection_url(&state, project_id, &body).await?;
    let engine = body.engine.unwrap_or_else(|| infer_engine_from_url(&url));

    let started = std::time::Instant::now();
    match engine.as_str() {
        "postgres" => test_postgres_engine(&url, started).await,
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
        "sqlserver" => test_sqlserver_engine(&url, started).await,
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
        .map_err(map_sqlserver_login_error)?;

    let version = query_sqlserver_version(&mut client).await?;
    let table_count = query_sqlserver_table_count(&mut client).await?;

    Ok((version, table_count))
}

/// Client tiberius sul TCP compat usato da [`test_sqlserver_connection`].
type SqlServerClient = tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>;

/// Traduce l'errore di login tiberius in un messaggio leggibile, estraendo il
/// testo del "Token error" del server quando presente.
fn map_sqlserver_login_error(e: impl std::fmt::Display) -> anyhow::Error {
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
    } else if raw.to_lowercase().contains("tls") || raw.to_lowercase().contains("certificate") {
        anyhow::anyhow!("Errore TLS/certificato: {raw}")
    } else {
        anyhow::anyhow!("Login SQL Server fallito: {raw}")
    }
}

/// Versione del server (`@@VERSION`), "sconosciuta" se la riga manca.
async fn query_sqlserver_version(client: &mut SqlServerClient) -> anyhow::Result<String> {
    let version = client
        .query("SELECT @@VERSION", &[])
        .await
        .map_err(|e| anyhow::anyhow!("Query @@VERSION fallita: {e}"))?
        .into_row()
        .await?
        .and_then(|r| r.get::<&str, usize>(0).map(String::from))
        .unwrap_or_else(|| "sconosciuta".into());
    Ok(version)
}

/// Numero di tabelle BASE TABLE nel database corrente, 0 se la riga manca.
async fn query_sqlserver_table_count(client: &mut SqlServerClient) -> anyhow::Result<i64> {
    let table_count = client
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
    Ok(table_count)
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
    use tiberius::Config;

    let params = parse_ado_params(conn_str);
    let mut config = Config::new();

    // ── Host + porta ─────────────────────────────────────────────────────────
    let server = params
        .get("server")
        .or_else(|| params.get("data source"))
        .map(|s| s.as_str())
        .unwrap_or("localhost");
    let (host, port) = split_server_host_port(server);
    config.host(host);
    config.port(port);

    // ── Database ─────────────────────────────────────────────────────────────
    if let Some(db) = params
        .get("database")
        .or_else(|| params.get("initial catalog"))
    {
        config.database(db.as_str());
    }

    apply_sqlserver_auth(&mut config, &params);
    apply_sqlserver_encryption(&mut config, &params);

    Ok(config)
}

/// Tokenizza una connection string ADO.NET "Key=Value;" ignorando i segmenti
/// vuoti (es. `;` finale). Le chiavi sono normalizzate a lowercase con gli spazi
/// multipli compattati, cosi' "User  ID" e "user id" collassano sulla stessa.
fn parse_ado_params(conn_str: &str) -> std::collections::HashMap<String, String> {
    let mut params: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for part in conn_str.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(eq_pos) = part.find('=') {
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
    params
}

/// Host e porta dal valore di `Server` / `Data Source`. Formati accettati:
/// "host,porta" | "tcp:host,porta" | "host" con istanza dopo il backslash |
/// "host". Porta di default 1433.
fn split_server_host_port(server: &str) -> (&str, u16) {
    let server_clean = server.trim_start_matches("tcp:");
    if let Some(comma) = server_clean.find(',') {
        let h = &server_clean[..comma];
        let p: u16 = server_clean[comma + 1..].trim().parse().unwrap_or(1433);
        (h, p)
    } else if let Some(bs) = server_clean.find('\\') {
        (&server_clean[..bs], 1433u16)
    } else {
        (server_clean, 1433u16)
    }
}

/// Autenticazione SQL dalle chiavi normalizzate (lowercase, spazio singolo):
///   "user id" -> "User Id" / "User ID" (ADO.NET ufficiale .NET/C#)
///   "uid"     -> abbreviazione
///   "user"    -> variante breve
/// Senza credenziali SQL resta la Windows Auth (non disponibile su Linux senza
/// Kerberos).
fn apply_sqlserver_auth(
    config: &mut tiberius::Config,
    params: &std::collections::HashMap<String, String>,
) {
    use tiberius::AuthMethod;

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
        _ => {}
    }
}

/// Livello di crittografia (`Encrypt`) e trust del certificato self-signed
/// (`TrustServerCertificate`).
fn apply_sqlserver_encryption(
    config: &mut tiberius::Config,
    params: &std::collections::HashMap<String, String>,
) {
    use tiberius::EncryptionLevel;

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

    let trust = params
        .get("trustservercertificate")
        .or_else(|| params.get("trust server certificate"))
        .map(|s| s.to_lowercase());
    if matches!(trust.as_deref(), Some("true") | Some("yes") | Some("1")) {
        config.trust_cert();
    }
}

#[cfg(test)]
mod compose_detection_tests {
    use super::extract_compose_connection_string;

    #[test]
    fn estrae_database_url_e_normalizza_host_servizio() {
        // Caso Beauty-Book reale: DATABASE_URL referenzia il nome servizio 'db'
        // (network compose). Dall'host esterno si accede via localhost + port
        // mapping, quindi il detector deve sostituire 'db' con 'localhost'.
        let compose = r#"
services:
  db:
    image: postgres:14-alpine
    environment:
      - POSTGRES_USER=user
      - POSTGRES_PASSWORD=password
      - POSTGRES_DB=beauty_book
    ports:
      - '5432:5432'
  backend:
    environment:
      - DATABASE_URL=postgres://user:password@db:5432/beauty_book
"#;
        let cs = extract_compose_connection_string(compose).expect("estratta");
        assert_eq!(cs, "postgres://user:password@localhost:5432/beauty_book");
    }

    #[test]
    fn fallback_a_pg_vars_se_no_database_url() {
        let compose = r#"
services:
  db:
    image: postgres:16
    environment:
      POSTGRES_USER: myapp
      POSTGRES_PASSWORD: s3cr3t
      POSTGRES_DB: app_dev
    ports:
      - "5433:5432"
"#;
        let cs = extract_compose_connection_string(compose).expect("estratta");
        // porta HOST 5433, container 5432: il detector deve usare 5433.
        assert_eq!(cs, "postgres://myapp:s3cr3t@localhost:5433/app_dev");
    }

    #[test]
    fn nessuna_url_e_nessuna_pg_var_ritorna_none() {
        let compose = r#"
services:
  redis:
    image: redis:7
"#;
        assert!(extract_compose_connection_string(compose).is_none());
    }
}

#[cfg(test)]
mod classify_pg_connection_error_tests {
    use super::classify_pg_connection_error;

    /// Mock del trait `sqlx::error::DatabaseError`: `PgDatabaseError` non e'
    /// costruibile fuori da `sqlx-postgres` (campo interno privato), quindi il
    /// test attraversa lo stesso trait object del produttore reale
    /// (`Box<dyn DatabaseError>`, regola O) senza dipendere da un server.
    #[derive(Debug)]
    struct MockDbError {
        message: String,
        code: Option<String>,
    }

    impl std::fmt::Display for MockDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.message)
        }
    }

    impl std::error::Error for MockDbError {}

    impl sqlx::error::DatabaseError for MockDbError {
        fn message(&self) -> &str {
            &self.message
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            self.code.as_deref().map(std::borrow::Cow::Borrowed)
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn db_error(code: &str, message: &str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(MockDbError {
            message: message.to_string(),
            code: Some(code.to_string()),
        }))
    }

    #[test]
    fn sqlstate_3d000_database_inesistente() {
        // Messaggio reale: contiene "database" E "does not exist" nel
        // prefisso sqlx ("error returned from database: ..."). Prima del fix
        // il TS ci sarebbe arrivato comunque per caso; qui verifichiamo che il
        // segnale primario (SQLSTATE) lo classifichi correttamente.
        let e = db_error("3D000", "database \"nonexistent\" does not exist");
        assert_eq!(classify_pg_connection_error(&e), "no_database");
    }

    #[test]
    fn sqlstate_28000_ruolo_inesistente_e_auth_failed_non_no_database() {
        // Questo e' il caso che il matching testuale sbagliava: il messaggio
        // "role \"foo\" does not exist" contiene "does not exist", e il
        // prefisso fisso di sqlx::Error::Database ("error returned from
        // database: ...") contiene "database" — un matcher testuale che
        // cerca does_not_exist+database lo classificherebbe "no_database".
        // Il SQLSTATE dice la verita': e' un problema di autorizzazione.
        let e = db_error("28000", "role \"foo\" does not exist");
        assert_eq!(classify_pg_connection_error(&e), "auth_failed");
    }

    #[test]
    fn sqlstate_28p01_password_errata_e_auth_failed() {
        let e = db_error("28P01", "password authentication failed for user \"foo\"");
        assert_eq!(classify_pg_connection_error(&e), "auth_failed");
    }

    #[test]
    fn sqlstate_classe_08_e_unreachable() {
        let e = db_error("08006", "connection failure");
        assert_eq!(classify_pg_connection_error(&e), "unreachable");
    }

    #[test]
    fn sqlstate_ignoto_e_unknown() {
        let e = db_error("42P01", "relation \"foo\" does not exist");
        assert_eq!(classify_pg_connection_error(&e), "unknown");
    }

    #[test]
    fn io_connection_refused_e_unreachable() {
        // Nessun server ha risposto: non esiste alcuno SQLSTATE, il segnale
        // e' il socket. Il Display di reqwest/sqlx per un io::Error e' generico
        // ("error communicating with database: ...") — stesso principio del
        // fix in task_watchdog.rs per reqwest.
        let io_err = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let e = sqlx::Error::Io(io_err);
        assert_eq!(classify_pg_connection_error(&e), "unreachable");
    }

    #[test]
    fn io_timed_out_e_unreachable() {
        let io_err = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let e = sqlx::Error::Io(io_err);
        assert_eq!(classify_pg_connection_error(&e), "unreachable");
    }

    #[test]
    fn pool_timed_out_e_unreachable() {
        assert_eq!(
            classify_pg_connection_error(&sqlx::Error::PoolTimedOut),
            "unreachable"
        );
    }
}
