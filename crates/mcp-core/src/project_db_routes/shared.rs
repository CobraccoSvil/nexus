//! Tipi ed helper condivisi tra i sottomoduli `project_db_routes`.
//!
//! Visibilita': `pub(crate)`/`pub(super)`, mai esposti verso l'esterno del
//! package (regola H: una sola sorgente per la risoluzione DSN/target fisico).

use axum::{http::StatusCode, Json};
use serde_json::{json, Value};

pub(crate) type ApiError = (StatusCode, Json<Value>);
pub(crate) type ApiResult = Result<Json<Value>, ApiError>;

pub(crate) fn api_err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(json!({ "error": msg.into() })))
}

/// Converte una stringa di connessione in formato ADO.NET/Npgsql
/// (`Host=...;Port=...;Database=...;Username=...;Password=...`)
/// nel formato URL PostgreSQL richiesto da sqlx (`postgres://user:pass@host:port/db`).
/// Se la stringa e` gia` in formato URL, la restituisce invariata.
pub(crate) fn normalize_pg_connection_string(raw: &str) -> String {
    let trimmed = raw.trim();
    // Se e` gia` un URL postgres, ritorna invariata
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        return trimmed.to_string();
    }
    // Parsing dei parametri ADO.NET (key=value separati da ;)
    let mut host = "localhost";
    let mut port = "5432";
    let mut database = "postgres";
    let mut username = "postgres";
    let mut password = "";
    let mut ssl_mode = "";
    for part in trimmed.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let key_lower = k.trim().to_lowercase();
        let val = v.trim();
        match key_lower.as_str() {
            "host" | "server" | "data source" => host = val,
            "port" => port = val,
            "database" | "initial catalog" | "db" => database = val,
            "username" | "user id" | "user" | "uid" => username = val,
            "password" | "pwd" => password = val,
            "sslmode" | "ssl mode" => ssl_mode = val,
            _ => {}
        }
    }
    let encoded_pass = urlencoding::encode(password);
    let mut url = format!(
        "postgres://{}:{}@{}:{}/{}",
        username, encoded_pass, host, port, database
    );
    if !ssl_mode.is_empty() {
        url.push_str(&format!("?sslmode={}", ssl_mode));
    }
    url
}

/// Estrae il bersaglio fisico `(host, port, dbname)` da una URL/DSN postgres,
/// normalizzando prima eventuali stringhe ADO.NET. Usato per l'idempotenza del
/// provisioning: due connessioni che risolvono allo stesso host+port+dbname
/// puntano allo STESSO database fisico, anche se hanno nomi logici diversi.
/// Ritorna `None` se la URL non e' parsabile come postgres.
pub(crate) fn pg_physical_target(raw: &str) -> Option<(String, u16, String)> {
    let normalized = normalize_pg_connection_string(raw);
    // Formato atteso: postgres[ql]://[user[:pass]@]host[:port]/dbname[?params]
    let after_scheme = normalized
        .strip_prefix("postgresql://")
        .or_else(|| normalized.strip_prefix("postgres://"))?;

    // Rimuove la parte userinfo (tutto fino all'ultima '@' prima del path).
    let authority_and_path = after_scheme;
    let (authority, path) = match authority_and_path.split_once('/') {
        Some((a, p)) => (a, p),
        None => return None,
    };
    // host:port e' la parte dopo l'eventuale userinfo.
    let host_port = match authority.rsplit_once('@') {
        Some((_userinfo, hp)) => hp,
        None => authority,
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(5432)),
        None => (host_port, 5432),
    };
    let host = host.trim().to_lowercase();
    if host.is_empty() {
        return None;
    }
    // dbname e' il primo segmento del path, senza query string.
    let dbname = path.split(['?', '/']).next().unwrap_or("").to_string();
    if dbname.is_empty() {
        return None;
    }
    Some((host, port, dbname))
}

#[cfg(test)]
mod tests {
    use super::pg_physical_target;

    #[test]
    fn pg_physical_target_estrae_host_port_db() {
        let t = pg_physical_target("postgresql://nexus_app:secret@localhost:5434/beauty_book_app")
            .unwrap();
        assert_eq!(
            t,
            ("localhost".to_string(), 5434, "beauty_book_app".to_string())
        );
    }

    #[test]
    fn pg_physical_target_ignora_userinfo_e_query() {
        // Stesso DB fisico nonostante credenziali e parametri diversi.
        let a = pg_physical_target("postgresql://u1:p1@localhost:5434/beauty_book_app").unwrap();
        let b =
            pg_physical_target("postgres://u2:p2@localhost:5434/beauty_book_app?sslmode=disable")
                .unwrap();
        assert_eq!(
            a, b,
            "userinfo e query string non devono influenzare il target"
        );
    }

    #[test]
    fn pg_physical_target_db_diverso_non_collide() {
        let a = pg_physical_target("postgresql://u:p@localhost:5434/beauty_book_app").unwrap();
        let b = pg_physical_target("postgresql://u:p@localhost:5434/altro_db").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn pg_physical_target_porta_default_5432() {
        let t = pg_physical_target("postgresql://u:p@db.example.com/app").unwrap();
        assert_eq!(t, ("db.example.com".to_string(), 5432, "app".to_string()));
    }

    #[test]
    fn pg_physical_target_ado_net() {
        // Stringa ADO.NET normalizzata internamente.
        let t = pg_physical_target(
            "Host=localhost;Port=5434;Database=beauty_book_app;Username=u;Password=p",
        )
        .unwrap();
        assert_eq!(
            t,
            ("localhost".to_string(), 5434, "beauty_book_app".to_string())
        );
    }
}
