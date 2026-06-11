//! Esecuzione SQL ad-hoc sul DB applicativo del progetto.
//!
//! Modulo condiviso tra:
//!   - tool MCP `nexus_db_query`/`nexus_db_tables`/`nexus_db_describe`
//!     (`crates/mcp-core/src/agent_tools/project_db_query.rs`), invocato
//!     dall'agente via chat.
//!   - endpoint REST `POST /api/projects/:id/db/query` esposto da
//!     `project_db_routes.rs`, invocato dal pannello SQL del frontend.
//!
//! La logica deve stare in UN SOLO posto (regola H: niente duplicazione).
//!
//! ## Sicurezza
//!
//! - La connessione e' SEMPRE risolta da `project_database_config` (filtro
//!   `is_primary = true`). Mai da input utente/modello.
//! - Guard-rail anti-contaminazione: se la URL punta al DB infrastruttura
//!   Nexus (db `nexus` su porta 5433, oppure hostname `postgres-nexus`/
//!   `ideai-postgres-nexus`), l'esecuzione viene RIFIUTATA. Un tool
//!   applicativo non deve mai toccare il DB di sistema (regola E:
//!   isolamento progetti).
//! - Limiti: timeout query 30s, max 1000 righe ritornate, 20_000 caratteri
//!   per cella stringa.

use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Column, PgPool, Row, TypeInfo};
use std::time::Duration;
use uuid::Uuid;

pub const QUERY_TIMEOUT_SECS: u64 = 30;
pub const MAX_ROWS: usize = 1000;
pub const MAX_CELL_CHARS: usize = 20_000;

/// Risolve la connection string del DB del progetto.
///
/// Se `connection_name` e' `Some(name)` filtra per `name` (case-insensitive);
/// altrimenti torna la connessione `is_primary=true` (comportamento storico).
///
/// Applica il guard-rail anti-contaminazione verso il DB Nexus. Legge
/// `connection_secret` (bytea con la URL raw) dal pool Nexus passato.
pub async fn resolve_project_conn(
    db: &PgPool,
    project_id: Uuid,
    connection_name: Option<&str>,
) -> Result<String, String> {
    let row = if let Some(name) = connection_name.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        sqlx::query(
            "SELECT connection_secret FROM project_database_config \
             WHERE project_id = $1 AND LOWER(name) = LOWER($2) \
             LIMIT 1",
        )
        .bind(project_id)
        .bind(name)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("query project_database_config fallita: {e}"))?
        .ok_or_else(|| {
            format!(
                "Connessione '{}' non trovata per questo progetto. Usa il \
                 pannello Database per crearla.",
                name
            )
        })?
    } else {
        sqlx::query(
            "SELECT connection_secret FROM project_database_config \
             WHERE project_id = $1 AND is_primary = true \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("query project_database_config fallita: {e}"))?
        .ok_or_else(|| {
            "Nessun database configurato per questo progetto. Usa il pannello \
             Database del progetto per aggiungere una connessione, oppure esegui \
             un comando che avvii il DB applicativo (auto-provisioning)."
                .to_string()
        })?
    };

    let secret: Option<Vec<u8>> = row.try_get("connection_secret").unwrap_or(None);
    let conn = secret
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "connection_secret vuoto o non decodificabile".to_string())?;

    // Guard-rail: blocca connessioni verso il DB infrastruttura Nexus.
    let lower = conn.to_lowercase();
    let touches_nexus_db = (lower.contains("/nexus") || lower.contains("database=nexus"))
        && (lower.contains(":5433")
            || lower.contains("postgres-nexus")
            || lower.contains("ideai-postgres-nexus"));
    if touches_nexus_db {
        return Err(
            "SICUREZZA: la connessione del progetto punta al DB infrastruttura \
             Nexus. Operazione rifiutata per isolamento. Configura un DB \
             applicativo dedicato per il progetto."
                .to_string(),
        );
    }
    Ok(conn)
}

/// Apre un pool sqlx (max 2 conn) verso il DB del progetto.
pub async fn open_pool(conn: &str) -> Result<PgPool, String> {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect(conn)
        .await
        .map_err(|e| format!("connessione DB progetto fallita: {e}"))
}

/// Converte una cella di PgRow in serde_json::Value in base al tipo Postgres.
/// I tipi non gestiti esplicitamente ricadono su String; se anche quella
/// fallisce, ritorna un marcatore di tipo.
pub fn cell_to_json(row: &sqlx::postgres::PgRow, idx: usize, type_name: &str) -> Value {
    macro_rules! try_as {
        ($t:ty) => {
            row.try_get::<Option<$t>, _>(idx)
                .ok()
                .map(|o| o.map(Value::from))
        };
    }
    let val: Option<Option<Value>> = match type_name {
        "BOOL" => row
            .try_get::<Option<bool>, _>(idx)
            .ok()
            .map(|o| o.map(Value::from)),
        "INT2" => row
            .try_get::<Option<i16>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as i64))),
        "INT4" => row
            .try_get::<Option<i32>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as i64))),
        "INT8" => row
            .try_get::<Option<i64>, _>(idx)
            .ok()
            .map(|o| o.map(Value::from)),
        "FLOAT4" => row
            .try_get::<Option<f32>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as f64))),
        "FLOAT8" => try_as!(f64),
        "NUMERIC" => try_as!(f64),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" | "CHAR" | "CITEXT" => try_as!(String),
        "UUID" => row
            .try_get::<Option<uuid::Uuid>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "JSON" | "JSONB" => row.try_get::<Option<Value>, _>(idx).ok(),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_rfc3339()))),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v.to_string()))),
        "BYTEA" => row
            .try_get::<Option<Vec<u8>>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(format!("\\x{} ({} byte)", hex_prefix(&v), v.len())))),
        _ => None,
    };
    match val {
        Some(Some(v)) => clamp_cell(v),
        Some(None) => Value::Null,
        None => match row.try_get::<Option<String>, _>(idx) {
            Ok(Some(s)) => clamp_cell(Value::from(s)),
            Ok(None) => Value::Null,
            Err(_) => Value::from(format!("<tipo non serializzabile: {type_name}>")),
        },
    }
}

/// Tronca celle stringa enormi per non saturare il context window.
pub fn clamp_cell(v: Value) -> Value {
    if let Value::String(s) = &v {
        if s.chars().count() > MAX_CELL_CHARS {
            let truncated: String = s.chars().take(MAX_CELL_CHARS).collect();
            return Value::from(format!("{truncated}...[TRONCATO]"));
        }
    }
    v
}

fn hex_prefix(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Rimuove commenti riga (`-- ...\n`) e commenti blocco (`/* ... */`)
/// iniziali, insieme allo whitespace, ritornando lo slice "significativo"
/// che inizia con il primo token reale dello statement.
///
/// Necessario perche' il backend riceve dal pannello SQL statement come
/// `-- Verifica\n SELECT * FROM users;` e `is_read_only`/`classify_statement`
/// guardavano solo `sql.trim_start()` → leggevano `--` come primo token e
/// classificavano la statement come `other` invece di `select`. Il bug
/// faceva ritornare il payload come `mode=write` e nascondeva colonne/righe
/// della SELECT finale nel pannello.
fn skip_leading_noise(sql: &str) -> &str {
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    loop {
        // Salta whitespace.
        while i < len && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= len {
            return "";
        }
        let c = bytes[i] as char;
        // Commento riga `-- ...` fino a newline.
        if c == '-' && i + 1 < len && bytes[i + 1] as char == '-' {
            i += 2;
            while i < len && bytes[i] as char != '\n' {
                i += 1;
            }
            continue;
        }
        // Commento blocco `/* ... */` (NON nested — sufficiente per SQL utente).
        if c == '/' && i + 1 < len && bytes[i + 1] as char == '*' {
            i += 2;
            while i + 1 < len && !(bytes[i] as char == '*' && bytes[i + 1] as char == '/') {
                i += 1;
            }
            i = (i + 2).min(len);
            continue;
        }
        // Primo carattere significativo: ritorna lo slice.
        return &sql[i..];
    }
}

/// Determina se la query e' di sola lettura (SELECT / WITH ... SELECT / SHOW /
/// EXPLAIN). Usato per scegliere fetch_all vs execute.
pub fn is_read_only(sql: &str) -> bool {
    let t = skip_leading_noise(sql).to_lowercase();
    t.starts_with("select")
        || t.starts_with("show")
        || t.starts_with("explain")
        || t.starts_with("with")
}

/// Determina il "kind" semantico della statement per il dispatcher event
/// `ProjectEvent::DbQueryRun.statement_kind` e per il tagging KB.
///
/// Valori: "select", "insert", "update", "delete", "ddl", "tx", "other".
pub fn classify_statement(sql: &str) -> &'static str {
    let t = skip_leading_noise(sql).to_lowercase();
    if t.starts_with("select")
        || t.starts_with("with")
        || t.starts_with("show")
        || t.starts_with("explain")
    {
        "select"
    } else if t.starts_with("insert") {
        "insert"
    } else if t.starts_with("update") {
        "update"
    } else if t.starts_with("delete") {
        "delete"
    } else if t.starts_with("begin")
        || t.starts_with("commit")
        || t.starts_with("rollback")
        || t.starts_with("savepoint")
    {
        "tx"
    } else if t.starts_with("create")
        || t.starts_with("alter")
        || t.starts_with("drop")
        || t.starts_with("truncate")
        || t.starts_with("rename")
    {
        "ddl"
    } else {
        "other"
    }
}

/// Splitta uno script SQL nei singoli statement, separati da `;` a livello
/// top-level. Ignora i `;` dentro:
///   - stringhe SQL delimitate da `'...'` (con escape `''` standard)
///   - identifier quotati `"..."`
///   - commenti riga `-- ...`
///   - commenti blocco `/* ... */`
///   - dollar-quoting `$tag$ ... $tag$` (Postgres function bodies)
///
/// Ritorna solo statement non vuoti (i `;` finali a vuoto vengono scartati).
/// Trim leading/trailing whitespace per ogni statement.
pub fn split_statements(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::with_capacity(sql.len());
    let mut i = 0;
    let len = bytes.len();

    enum State {
        Normal,
        SingleString,         // dentro '...'
        DoubleQuoted,         // dentro "..."
        LineComment,          // dentro -- fino a newline
        BlockComment,         // dentro /* ... */
        DollarQuoted(String), // tag $foo$ ... $foo$
    }

    let mut state = State::Normal;

    while i < len {
        let c = bytes[i] as char;
        match &state {
            State::Normal => {
                if c == '\'' {
                    current.push(c);
                    state = State::SingleString;
                    i += 1;
                } else if c == '"' {
                    current.push(c);
                    state = State::DoubleQuoted;
                    i += 1;
                } else if c == '-' && i + 1 < len && bytes[i + 1] as char == '-' {
                    current.push_str("--");
                    state = State::LineComment;
                    i += 2;
                } else if c == '/' && i + 1 < len && bytes[i + 1] as char == '*' {
                    current.push_str("/*");
                    state = State::BlockComment;
                    i += 2;
                } else if c == '$' {
                    // Tenta di leggere $tag$ (tag alfanumerico/underscore, anche vuoto).
                    let mut j = i + 1;
                    while j < len {
                        let ch = bytes[j] as char;
                        if ch == '$' {
                            break;
                        }
                        if !(ch.is_alphanumeric() || ch == '_') {
                            j = i; // non e' un dollar-quote valido
                            break;
                        }
                        j += 1;
                    }
                    if j > i && j < len && bytes[j] as char == '$' {
                        let tag = String::from_utf8_lossy(&bytes[i..=j]).to_string();
                        current.push_str(&tag);
                        state = State::DollarQuoted(tag);
                        i = j + 1;
                    } else {
                        current.push(c);
                        i += 1;
                    }
                } else if c == ';' {
                    let stmt = current.trim().to_string();
                    if !stmt.is_empty() {
                        out.push(stmt);
                    }
                    current.clear();
                    i += 1;
                } else {
                    current.push(c);
                    i += 1;
                }
            }
            State::SingleString => {
                current.push(c);
                if c == '\'' {
                    // Escape standard '': se il prossimo carattere e' anche ',
                    // resta nello stato.
                    if i + 1 < len && bytes[i + 1] as char == '\'' {
                        current.push('\'');
                        i += 2;
                    } else {
                        state = State::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            State::DoubleQuoted => {
                current.push(c);
                if c == '"' {
                    if i + 1 < len && bytes[i + 1] as char == '"' {
                        current.push('"');
                        i += 2;
                    } else {
                        state = State::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            State::LineComment => {
                current.push(c);
                if c == '\n' {
                    state = State::Normal;
                }
                i += 1;
            }
            State::BlockComment => {
                current.push(c);
                if c == '*' && i + 1 < len && bytes[i + 1] as char == '/' {
                    current.push('/');
                    state = State::Normal;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::DollarQuoted(tag) => {
                // Cerca chiusura tag.
                let tag_clone = tag.clone();
                let tlen = tag_clone.len();
                if i + tlen <= len && &bytes[i..i + tlen] == tag_clone.as_bytes() {
                    current.push_str(&tag_clone);
                    state = State::Normal;
                    i += tlen;
                } else {
                    current.push(c);
                    i += 1;
                }
            }
        }
    }

    let tail = current.trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }

    out
}

/// Esito strutturato di un'esecuzione SQL.
#[derive(Debug)]
pub struct QueryExecOutcome {
    pub mode: &'static str,         // "read" | "write"
    pub statement_kind: String, // kind del LAST statement (select|insert|update|delete|ddl|tx|other)
    pub columns: Vec<Value>,    // [{name,type}, ...]
    pub rows: Vec<Value>,       // [{col:val, ...}, ...]
    pub row_count: usize,       // numero righe ritornate (read) o 0 (write)
    pub rows_affected: Option<u64>, // somma rows_affected delle statement write (None se l'ultima e' read)
    pub truncated: bool,            // true se ho clippato per max_rows
    pub duration_ms: u64,
    /// Numero di statement eseguiti (>=1). Se >1 il batch e' stato eseguito in transazione.
    pub statements_executed: usize,
    /// Riepilogo per-statement quando il batch contiene piu' di 1 statement.
    pub per_statement_summary: Vec<Value>,
}

/// Errore di esecuzione query.
#[derive(Debug)]
pub enum QueryExecError {
    /// Connessione progetto non risolvibile o guard-rail anti-Nexus scattato.
    ConnectionError(String),
    /// Timeout server-side (oltre QUERY_TIMEOUT_SECS).
    Timeout,
    /// Errore SQL dal driver (sintassi, vincoli, permessi, ecc.).
    Sql(String),
}

impl QueryExecError {
    pub fn message(&self) -> String {
        match self {
            QueryExecError::ConnectionError(m) => m.clone(),
            QueryExecError::Timeout => format!("timeout query dopo {QUERY_TIMEOUT_SECS}s"),
            QueryExecError::Sql(m) => format!("errore SQL: {m}"),
        }
    }
}

/// Esegue una query SQL completa. Ritorna un esito strutturato.
///
/// `params` sono bindati come TEXT (NULL -> bind null). Il chiamante puo' usare
/// cast espliciti nel SQL quando serve un tipo non-testo: es.
/// `INSERT INTO t(qty) VALUES ($1::int)`.
///
/// `max_rows` viene clippato a [`MAX_ROWS`].
pub async fn execute_query(
    db: &PgPool,
    project_id: Uuid,
    sql: &str,
    params: &[Option<String>],
    max_rows: Option<usize>,
    connection_name: Option<&str>,
) -> Result<QueryExecOutcome, QueryExecError> {
    let max_rows = max_rows.map(|n| n.min(MAX_ROWS)).unwrap_or(MAX_ROWS);

    let conn = resolve_project_conn(db, project_id, connection_name)
        .await
        .map_err(QueryExecError::ConnectionError)?;
    let pool = open_pool(&conn)
        .await
        .map_err(QueryExecError::ConnectionError)?;

    let statements = split_statements(sql);
    if statements.is_empty() {
        pool.close().await;
        return Err(QueryExecError::Sql(
            "nessuna statement SQL trovata (solo commenti o whitespace)".to_string(),
        ));
    }

    // Multi-statement con parametri non e' supportato: i `$1`/`$2` valgono
    // per UN solo statement nel protocollo prepared. Se l'utente vuole un
    // batch con parametri, deve splittare lato chiamante.
    if statements.len() > 1 && !params.is_empty() {
        pool.close().await;
        return Err(QueryExecError::Sql(
            "i parametri ($1, $2, ...) sono supportati solo con UNA singola \
             statement. Per eseguire piu' statement insieme, rimuovi i \
             parametri o esegui le statement separatamente."
                .to_string(),
        ));
    }

    let started = std::time::Instant::now();

    let outcome = if statements.len() == 1 {
        // Path classico single-statement: rispetta i param bindabili e
        // distingue read vs write a livello di statement.
        let single = &statements[0];
        let kind = classify_statement(single).to_string();
        let read_only = is_read_only(single);

        let mut q = sqlx::query(single);
        for p in params {
            q = q.bind(p.clone());
        }

        if read_only {
            let fetch =
                tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.fetch_all(&pool))
                    .await;
            let rows = match fetch {
                Err(_) => {
                    pool.close().await;
                    return Err(QueryExecError::Timeout);
                }
                Ok(Err(e)) => {
                    pool.close().await;
                    return Err(QueryExecError::Sql(e.to_string()));
                }
                Ok(Ok(r)) => r,
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            let truncated = rows.len() > max_rows;
            let slice = &rows[..rows.len().min(max_rows)];
            let (columns, out_rows) = serialize_rows(slice);

            QueryExecOutcome {
                mode: "read",
                statement_kind: kind,
                row_count: out_rows.len(),
                columns,
                rows: out_rows,
                rows_affected: None,
                truncated,
                duration_ms,
                statements_executed: 1,
                per_statement_summary: Vec::new(),
            }
        } else {
            let exec =
                tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.execute(&pool))
                    .await;
            let res = match exec {
                Err(_) => {
                    pool.close().await;
                    return Err(QueryExecError::Timeout);
                }
                Ok(Err(e)) => {
                    pool.close().await;
                    return Err(QueryExecError::Sql(e.to_string()));
                }
                Ok(Ok(r)) => r,
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            QueryExecOutcome {
                mode: "write",
                statement_kind: kind,
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                rows_affected: Some(res.rows_affected()),
                truncated: false,
                duration_ms,
                statements_executed: 1,
                per_statement_summary: Vec::new(),
            }
        }
    } else {
        // Multi-statement: eseguo in transazione una per una. L'ULTIMO
        // statement determina mode/columns/rows del payload principale; i
        // precedenti vengono riassunti in `per_statement_summary`.
        let last_idx = statements.len() - 1;
        let last_kind = classify_statement(&statements[last_idx]).to_string();
        let last_is_read = is_read_only(&statements[last_idx]);

        let tx_result = tokio::time::timeout(
            Duration::from_secs(QUERY_TIMEOUT_SECS),
            run_batch_in_tx(&pool, &statements, max_rows, last_is_read),
        )
        .await;

        let batch = match tx_result {
            Err(_) => {
                pool.close().await;
                return Err(QueryExecError::Timeout);
            }
            Ok(Err(e)) => {
                pool.close().await;
                return Err(QueryExecError::Sql(e));
            }
            Ok(Ok(b)) => b,
        };
        let duration_ms = started.elapsed().as_millis() as u64;

        // Aggregato: somma write rows_affected (solo se l'ultimo NON e' read).
        let total_write: u64 = batch
            .per_statement
            .iter()
            .filter_map(|s| s.get("rows_affected").and_then(Value::as_u64))
            .sum();

        QueryExecOutcome {
            mode: if last_is_read { "read" } else { "write" },
            statement_kind: last_kind,
            row_count: batch.rows.len(),
            columns: batch.columns,
            rows: batch.rows,
            rows_affected: if last_is_read {
                None
            } else {
                Some(total_write)
            },
            truncated: batch.truncated,
            duration_ms,
            statements_executed: statements.len(),
            per_statement_summary: batch.per_statement,
        }
    };

    pool.close().await;
    Ok(outcome)
}

/// Esito del batch multi-statement (interno).
struct BatchExecResult {
    columns: Vec<Value>,
    rows: Vec<Value>,
    truncated: bool,
    per_statement: Vec<Value>,
}

/// Esegue una lista di statement dentro una transazione. Se uno fallisce, fa
/// rollback e ritorna l'errore. Il LAST statement, se read-only, viene
/// fetch_all-ato e i rows ritornati popolano `columns`/`rows`.
async fn run_batch_in_tx(
    pool: &PgPool,
    statements: &[String],
    max_rows: usize,
    last_is_read: bool,
) -> Result<BatchExecResult, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("BEGIN fallito: {e}"))?;

    let last_idx = statements.len() - 1;
    let mut per_statement: Vec<Value> = Vec::with_capacity(statements.len());
    let mut columns: Vec<Value> = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    let mut truncated = false;

    for (i, stmt) in statements.iter().enumerate() {
        let kind = classify_statement(stmt).to_string();
        let read_only = is_read_only(stmt);
        let is_last = i == last_idx;

        if is_last && last_is_read {
            // L'ultimo read: fetch_all per popolare il risultato principale.
            let r = sqlx::query(stmt).fetch_all(&mut *tx).await;
            match r {
                Ok(fetched) => {
                    truncated = fetched.len() > max_rows;
                    let slice = &fetched[..fetched.len().min(max_rows)];
                    let (cols, out_rows) = serialize_rows(slice);
                    columns = cols;
                    let count = out_rows.len();
                    rows = out_rows;
                    per_statement.push(json!({
                        "index": i,
                        "statement_kind": kind,
                        "mode": "read",
                        "row_count": count,
                    }));
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(format!("errore statement #{}: {}", i + 1, e));
                }
            }
        } else {
            // Statement intermedie + ultima write: execute (rows_affected).
            // Per le read non-finali ignoriamo le righe (lo user vede solo
            // l'output dell'ultima); cosi' supportiamo CTE/script con
            // SELECT diagnostiche intermedie senza saturare il payload.
            let r = if read_only {
                // Per evitare prepared con multiple-row result inutile,
                // converto a EXPLAIN-less execute: usiamo execute() che
                // non porta indietro le righe (sqlx accetta).
                sqlx::query(stmt).execute(&mut *tx).await
            } else {
                sqlx::query(stmt).execute(&mut *tx).await
            };
            match r {
                Ok(pg_res) => {
                    per_statement.push(json!({
                        "index": i,
                        "statement_kind": kind,
                        "mode": if read_only { "read_ignored" } else { "write" },
                        "rows_affected": pg_res.rows_affected(),
                    }));
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(format!("errore statement #{}: {}", i + 1, e));
                }
            }
        }
    }

    tx.commit()
        .await
        .map_err(|e| format!("COMMIT fallito: {e}"))?;

    Ok(BatchExecResult {
        columns,
        rows,
        truncated,
        per_statement,
    })
}

/// Helper condiviso: converte un set di PgRow in `(columns, rows)` JSON.
fn serialize_rows(slice: &[sqlx::postgres::PgRow]) -> (Vec<Value>, Vec<Value>) {
    let columns: Vec<Value> = slice
        .first()
        .map(|r| {
            r.columns()
                .iter()
                .map(|c| json!({"name": c.name(), "type": c.type_info().name()}))
                .collect()
        })
        .unwrap_or_default();

    let out_rows: Vec<Value> = slice
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in row.columns().iter().enumerate() {
                let type_name = col.type_info().name();
                obj.insert(col.name().to_string(), cell_to_json(row, i, type_name));
            }
            Value::Object(obj)
        })
        .collect();

    (columns, out_rows)
}

/// Esito dell'archiviazione automatica di una DDL.
#[derive(Debug, Clone)]
pub struct DdlArchiveOutcome {
    pub note_id: Uuid,
    pub migration_filename: Option<String>,
    pub migration_abs_path: Option<String>,
}

/// Se la query e' DDL (CREATE/ALTER/DROP/TRUNCATE/RENAME), archivia
/// l'evento in due posti:
/// 1. Una nota nella Knowledge Base del progetto (`project_knowledge_notes`,
///    intent="database_migration", tag ["schema-change","ddl"]). Nessun
///    embedding qui: la nota e' comunque visibile nel pannello KB e
///    indicizzabile a posteriori.
/// 2. Un file migration versionato nella cartella migration del progetto
///    (path da `project_database_config.migration_path`, default
///    `nexus_migrations/` sotto la root). Il numero progressivo e' max+1
///    sui file `NNNN_*.sql` gia' presenti.
///
/// Idempotente sui retry: il caller deve invocarla SOLO dopo esecuzione
/// SQL riuscita (per non archiviare DDL fallite).
///
/// Errori vengono loggati come WARN ma non propagati: l'archiviazione e' un
/// "best effort". L'esecuzione SQL e' gia' avvenuta e il dato e' nel DB
/// applicativo.
pub async fn archive_ddl(
    db: &PgPool,
    project_id: Uuid,
    sql: &str,
    outcome: &QueryExecOutcome,
    connection_name: Option<&str>,
) -> Option<DdlArchiveOutcome> {
    // Scatta se il LAST statement e' DDL oppure se il batch (multi-statement)
    // contiene almeno una DDL. Cosi' "CREATE TABLE ...; INSERT ...; SELECT ..."
    // viene archiviato correttamente anche se l'ultima e' SELECT.
    if outcome.statement_kind != "ddl" && !batch_contains_ddl(sql) {
        return None;
    }

    let trimmed = sql.trim();
    let timestamp = chrono::Utc::now();
    let action = first_token_upper(trimmed); // CREATE / ALTER / DROP / TRUNCATE / RENAME
    let object = second_meaningful_token_upper(trimmed); // TABLE / INDEX / VIEW / ...
    let target = third_meaningful_identifier(trimmed); // <nome>

    // Identifica la connessione effettiva risolta lato archive: stessa logica
    // di resolve_project_conn (None / vuoto -> primary). Cosi' il titolo
    // della nota KB e il tag riflettono il DB su cui la DDL e' atterrata.
    let effective_conn = resolve_connection_name(db, project_id, connection_name)
        .await
        .unwrap_or_else(|| "primary".to_string());

    let title = match (action.as_str(), object.as_str(), target.as_deref()) {
        (a, "", _) => format!(
            "DDL {} on '{}' - {}",
            a,
            effective_conn,
            timestamp.format("%Y-%m-%d %H:%M")
        ),
        (a, o, Some(t)) => format!("DDL {} {} {} on '{}'", a, o, t, effective_conn),
        (a, o, None) => format!("DDL {} {} on '{}'", a, o, effective_conn),
    };

    let body_md = format!(
        "**DDL eseguita sul DB `{}`** ({} righe modificate, {} ms)\n\n\
         ```sql\n{}\n```\n\n\
         _Archiviata automaticamente il {}_",
        effective_conn,
        outcome.rows_affected.unwrap_or(0),
        outcome.duration_ms,
        trimmed,
        timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
    );

    let note_id = Uuid::new_v4();
    let mut tags: Vec<String> = vec!["schema-change".to_string(), "ddl".to_string()];
    // Tag aggiuntivo con il nome della connessione, cosi' nel pannello KB e'
    // facile filtrare le DDL per DB di destinazione (utile in multi-DB).
    tags.push(format!("db:{}", effective_conn));
    let file_paths: Vec<String> = Vec::new();

    let note_insert = sqlx::query(
        r#"
        INSERT INTO project_knowledge_notes
            (id, project_id, intent, title, body_md, status, qdrant_point_id, tags, file_paths)
        VALUES ($1, $2, 'database_migration', $3, $4, 'active', NULL, $5, $6)
        "#,
    )
    .bind(note_id)
    .bind(project_id)
    .bind(&title)
    .bind(&body_md)
    .bind(&tags)
    .bind(&file_paths)
    .execute(db)
    .await;

    if let Err(e) = note_insert {
        tracing::warn!(
            error = %e,
            %project_id,
            "archive_ddl: INSERT project_knowledge_notes fallito"
        );
        return None;
    }

    // Tag aggregati (best effort).
    for tag in &tags {
        let _ = sqlx::query(
            r#"
            INSERT INTO project_knowledge_tags (project_id, tag, note_count, last_used_at)
            VALUES ($1, $2, 1, NOW())
            ON CONFLICT (project_id, tag) DO UPDATE SET
                note_count = project_knowledge_tags.note_count + 1,
                last_used_at = NOW()
            "#,
        )
        .bind(project_id)
        .bind(tag)
        .execute(db)
        .await;
    }

    // File migration (best effort - se lo scrivi senza root, salta).
    // Passa la connection effettiva cosi' multi-DB scrive in cartelle
    // separate (nexus_migrations/<conn_name>/).
    let (mig_filename, mig_abs) =
        match write_migration_file(db, project_id, trimmed, &title, &effective_conn).await {
            Ok((name, abs)) => (Some(name), Some(abs)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    %project_id,
                    connection = %effective_conn,
                    "archive_ddl: scrittura file migration fallita"
                );
                (None, None)
            }
        };

    tracing::info!(
        %project_id,
        %note_id,
        migration_filename = mig_filename.as_deref().unwrap_or("(skip)"),
        "archive_ddl: DDL archiviata in KB"
    );

    Some(DdlArchiveOutcome {
        note_id,
        migration_filename: mig_filename,
        migration_abs_path: mig_abs,
    })
}

/// Scrive il file migration progressivo `<root>/<migration_path>/<NNNN>_<slug>.sql`.
///
/// Path scelto con priorita':
///   1. Override esplicito: `project_database_config.migration_path` per
///      la **connessione specifica** (lookup per name, non per is_primary).
///      Se non-null/non-vuoto, viene usato as-is (relativo alla root).
///   2. Multi-DB default: se la connessione e' diversa dalla `primary`,
///      la cartella default e' `nexus_migrations/<connection_name_safe>/`
///      cosi' DB diversi non si sovrappongono nello stesso storico.
///   3. Single-DB default: `nexus_migrations/` (retrocompatibilita').
///
/// - `<NNNN>` = max(numero progressivo gia' presente nella cartella) + 1
///   (4 cifre, zero-padded). Il counter e' **per cartella** quindi ogni
///   connessione ha la sua sequenza indipendente.
/// - `<slug>` = primi 6 token sanificati del SQL.
///
/// `connection_name` e' il nome effettivo gia' risolto da
/// `resolve_connection_name` (es. "primary", "analytics"). Mai vuoto.
async fn write_migration_file(
    db: &PgPool,
    project_id: Uuid,
    sql: &str,
    title: &str,
    connection_name: &str,
) -> Result<(String, String), String> {
    // 1) Root del progetto.
    let root_row = sqlx::query("SELECT repository_root_path FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("query projects: {e}"))?
        .ok_or_else(|| "progetto non trovato".to_string())?;
    let root: Option<String> = root_row.try_get("repository_root_path").unwrap_or(None);
    let root = root
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "repository_root_path non configurato per il progetto".to_string())?;

    // 2) migration_path: prima cerca override sulla connessione SPECIFICA
    //    (LOWER name match). Se vuoto -> fallback multi-DB-aware.
    let cfg_row = sqlx::query(
        "SELECT migration_path FROM project_database_config \
         WHERE project_id = $1 AND LOWER(name) = LOWER($2) \
         LIMIT 1",
    )
    .bind(project_id)
    .bind(connection_name)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("query project_database_config: {e}"))?;
    let explicit_path: Option<String> = cfg_row
        .and_then(|r| {
            r.try_get::<Option<String>, _>("migration_path")
                .unwrap_or(None)
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let migration_subdir: String = match explicit_path {
        Some(p) => p,
        None => {
            // Default: primary -> nexus_migrations/, altre -> nexus_migrations/<slug>/
            if connection_name.eq_ignore_ascii_case("primary") {
                "nexus_migrations".to_string()
            } else {
                format!("nexus_migrations/{}", safe_dir_segment(connection_name))
            }
        }
    };

    let mig_dir = std::path::Path::new(&root).join(&migration_subdir);
    tokio::fs::create_dir_all(&mig_dir)
        .await
        .map_err(|e| format!("create_dir_all {}: {e}", mig_dir.display()))?;

    // 3) Calcola prossimo numero progressivo.
    let mut max_seen: u32 = 0;
    let mut entries = tokio::fs::read_dir(&mig_dir)
        .await
        .map_err(|e| format!("read_dir {}: {e}", mig_dir.display()))?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(fname) = entry.file_name().to_str() {
            if let Some(num_str) = fname.split('_').next() {
                if let Ok(n) = num_str.parse::<u32>() {
                    if n > max_seen {
                        max_seen = n;
                    }
                }
            }
        }
    }
    let next = max_seen + 1;
    let slug = slugify_sql(sql);
    let filename = format!("{:04}_{}.sql", next, slug);
    let abs_path = mig_dir.join(&filename);

    // 4) Scrivi il file con header commento.
    let header = format!(
        "-- {} (auto-archived by Nexus SQL panel)\n\
         -- Generated: {}\n\
         -- Project: {}\n\
         -- Connection: {}\n\n",
        title,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        project_id,
        connection_name,
    );
    let content = format!("{}{}\n", header, sql.trim_end());
    tokio::fs::write(&abs_path, content)
        .await
        .map_err(|e| format!("write {}: {e}", abs_path.display()))?;

    Ok((filename, abs_path.to_string_lossy().into_owned()))
}

fn first_token_upper(sql: &str) -> String {
    sql.split_whitespace().next().unwrap_or("").to_uppercase()
}

fn second_meaningful_token_upper(sql: &str) -> String {
    // Salta IF EXISTS / IF NOT EXISTS dopo CREATE/DROP.
    let iter = sql.split_whitespace().skip(1);
    for tok in iter {
        let up = tok.to_uppercase();
        if up == "IF" || up == "NOT" || up == "EXISTS" || up == "OR" || up == "REPLACE" {
            continue;
        }
        return up;
    }
    String::new()
}

fn third_meaningful_identifier(sql: &str) -> Option<String> {
    let iter = sql.split_whitespace().skip(2);
    for tok in iter {
        let up = tok.to_uppercase();
        if up == "IF" || up == "NOT" || up == "EXISTS" || up == "OR" || up == "REPLACE" {
            continue;
        }
        // Strip trailing punctuation (es. "users(" o "schema.users,").
        let cleaned: String = tok
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

/// Risolve il nome effettivo della connessione da usare per logging/path:
///   - Se `connection_name` e' `Some(name)` non vuoto -> ritorna `Some(name)`
///     se quella connessione esiste, altrimenti `None`.
///   - Se `connection_name` e' `None`/vuoto -> ritorna il `name` della
///     connessione `is_primary=true`, altrimenti `None`.
///
/// Best effort: i caller fanno fallback a "primary" se ritorna None.
async fn resolve_connection_name(
    db: &PgPool,
    project_id: Uuid,
    connection_name: Option<&str>,
) -> Option<String> {
    if let Some(name) = connection_name.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        sqlx::query(
            "SELECT name FROM project_database_config \
             WHERE project_id = $1 AND LOWER(name) = LOWER($2) \
             LIMIT 1",
        )
        .bind(project_id)
        .bind(name)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<String, _>("name").ok())
    } else {
        sqlx::query(
            "SELECT name FROM project_database_config \
             WHERE project_id = $1 AND is_primary = true \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<String, _>("name").ok())
    }
}

/// Sanifica un nome connessione per usarlo come segmento di path: tiene solo
/// ASCII alfanumerico + underscore + trattino, tronca a 40 char, lowercase.
/// Garantisce che `nexus_migrations/<segment>/` sia un path filesystem-safe.
fn safe_dir_segment(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let truncated: String = cleaned.chars().take(40).collect();
    if truncated.trim_matches('_').is_empty() {
        "unnamed".to_string()
    } else {
        truncated
    }
}

fn slugify_sql(sql: &str) -> String {
    let words: Vec<String> = sql
        .split_whitespace()
        .take(6)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let slug = words.join("_");
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() {
        "ddl".to_string()
    } else {
        slug
    }
}

/// Serializza un [`QueryExecOutcome`] nel formato JSON canonico usato sia dal
/// tool agente sia dall'endpoint REST.
pub fn outcome_to_json(outcome: &QueryExecOutcome) -> Value {
    let mut base = match outcome.mode {
        "read" => json!({
            "ok": true,
            "mode": "read",
            "statement_kind": outcome.statement_kind,
            "columns": outcome.columns,
            "row_count": outcome.row_count,
            "rows": outcome.rows,
            "truncated": outcome.truncated,
            "duration_ms": outcome.duration_ms,
        }),
        _ => json!({
            "ok": true,
            "mode": "write",
            "statement_kind": outcome.statement_kind,
            "rows_affected": outcome.rows_affected.unwrap_or(0),
            "duration_ms": outcome.duration_ms,
            "hint": "Per leggere i dati inseriti usa una SELECT, oppure aggiungi RETURNING alla INSERT.",
        }),
    };
    if outcome.statements_executed > 1 {
        if let Value::Object(ref mut map) = base {
            map.insert(
                "statements_executed".to_string(),
                Value::from(outcome.statements_executed as u64),
            );
            map.insert(
                "per_statement_summary".to_string(),
                Value::Array(outcome.per_statement_summary.clone()),
            );
        }
    }
    base
}

/// True se il batch SQL contiene almeno UNA statement DDL (rilevazione robusta
/// post-split: ignora commenti e stringhe). Usato da `archive_ddl` per
/// scattare anche se la DDL non e' la statement finale del batch (es.
/// `CREATE TABLE ...; INSERT ...; SELECT ...`).
pub fn batch_contains_ddl(sql: &str) -> bool {
    split_statements(sql)
        .iter()
        .any(|s| classify_statement(s) == "ddl")
}
