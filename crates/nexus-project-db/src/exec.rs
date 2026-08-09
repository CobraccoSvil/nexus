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
//! - Guard-rail anti-contaminazione: se la riga risolta NON e' il DB
//!   applicativo del progetto, l'esecuzione viene RIFIUTATA. Il criterio e'
//!   [`classifica_connessione`], punto unico della domanda. Un tool
//!   applicativo non deve mai toccare l'infrastruttura (regola E: isolamento
//!   progetti).
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

/// Valore canonico di `project_database_config.connection_role` (mig 0494) per
/// il DB metadati Nexus per-progetto. Gemello del `DbRole::NexusMetadata` di
/// `mcp_core::project_db_routes::provision`, che scrive la riga: i due crate non
/// si vedono (mcp-core dipende da questo, mai il contrario).
pub const RUOLO_METADATI_NEXUS: &str = "nexus_metadata";

/// «Questa riga di `project_database_config` e' il DB APPLICATIVO del progetto,
/// o e' infrastruttura Nexus?»
///
/// PUNTO UNICO (regola L) del discriminante di connessione dei tool DB. E' la
/// domanda strutturale che rende superflua ogni ispezione del TESTO della query
/// per stabilire "dove si sta scrivendo": chi decide e' la connessione, non cio'
/// che la statement nomina (regola M).
///
/// Le due prove NON si sostituiscono, e sono entrambe necessarie:
///   - il RUOLO dichiarato riconosce il DB metadati per-progetto `<slug>_nexus`
///     (`agent_steps`, `nexus_agent_plans`, `jobs`, ...), che la URL non
///     tradisce: gira sullo STESSO cluster applicativo e il nome del database
///     e' `<slug>_nexus`, non `nexus`. Prima di questo criterio bastava
///     `nexus_db_query(connection: "nexus_metadata")` per aprirci un pool;
///   - la URL riconosce il DB META (`nexus` su 5433) anche quando il ruolo non
///     e' stato scritto da chi ha registrato la riga (connessioni censite a
///     mano, righe anteriori alla mig 0494).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnessioneRisolta {
    /// DB applicativo del progetto: i tool possono usarlo.
    DbApplicativo,
    /// DB metadati Nexus per-progetto (`<slug>_nexus`): infrastruttura.
    MetadatiDiProgetto,
    /// DB infrastruttura Nexus (`nexus` su 5433): infrastruttura.
    InfrastrutturaNexus,
}

impl ConnessioneRisolta {
    /// Testo del rifiuto, composto DAL verdetto (regola Q). `None` per il DB
    /// applicativo, che non si rifiuta.
    pub fn motivo_del_rifiuto(&self) -> Option<String> {
        match self {
            ConnessioneRisolta::DbApplicativo => None,
            ConnessioneRisolta::MetadatiDiProgetto => Some(
                "SICUREZZA: quella connessione e' il DB metadati Nexus del \
                 progetto (chat, run, costi), non il suo DB applicativo. \
                 Operazione rifiutata per isolamento: usa la connessione \
                 applicativa (omettere 'connection' seleziona la primaria)."
                    .to_string(),
            ),
            ConnessioneRisolta::InfrastrutturaNexus => Some(
                "SICUREZZA: la connessione del progetto punta al DB infrastruttura \
                 Nexus. Operazione rifiutata per isolamento. Configura un DB \
                 applicativo dedicato per il progetto."
                    .to_string(),
            ),
        }
    }
}

/// Criterio puro di [`ConnessioneRisolta`]: ruolo dichiarato e URL della riga.
pub fn classifica_connessione(conn_url: &str, connection_role: &str) -> ConnessioneRisolta {
    if connection_role.eq_ignore_ascii_case(RUOLO_METADATI_NEXUS) {
        return ConnessioneRisolta::MetadatiDiProgetto;
    }
    if points_to_nexus_infra_db(conn_url) {
        return ConnessioneRisolta::InfrastrutturaNexus;
    }
    ConnessioneRisolta::DbApplicativo
}

/// Risolve la connection string del DB del progetto.
///
/// Se `connection_name` e' `Some(name)` filtra per `name` (case-insensitive);
/// altrimenti torna la connessione `is_primary=true` (comportamento storico).
///
/// Applica il guard-rail anti-contaminazione ([`classifica_connessione`]). Legge
/// `connection_secret` (bytea con la URL raw) dal pool Nexus passato.
pub async fn resolve_project_conn(
    db: &PgPool,
    project_id: Uuid,
    connection_name: Option<&str>,
) -> Result<String, String> {
    let row = fetch_conn_secret_row(db, project_id, connection_name).await?;
    let conn = decode_connection_secret(&row)?;
    let role: String = row
        .try_get::<Option<String>, _>("connection_role")
        .unwrap_or(None)
        .unwrap_or_default();

    if let Some(motivo) = classifica_connessione(&conn, &role).motivo_del_rifiuto() {
        return Err(motivo);
    }
    Ok(conn)
}

/// Recupera la riga `connection_secret` dal pool Nexus: per `name`
/// (case-insensitive) se dato e non vuoto, altrimenti la connessione
/// `is_primary=true`. Errore descrittivo se la connessione non esiste.
async fn fetch_conn_secret_row(
    db: &PgPool,
    project_id: Uuid,
    connection_name: Option<&str>,
) -> Result<sqlx::postgres::PgRow, String> {
    if let Some(name) = connection_name.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        sqlx::query(
            "SELECT connection_secret, connection_role FROM project_database_config \
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
        })
    } else {
        sqlx::query(
            "SELECT connection_secret, connection_role FROM project_database_config \
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
        })
    }
}

/// Decodifica il campo bytea `connection_secret` (URL raw) in stringa trimmata
/// non vuota. Errore se assente/non-UTF8/vuoto.
fn decode_connection_secret(row: &sqlx::postgres::PgRow) -> Result<String, String> {
    let secret: Option<Vec<u8>> = row.try_get("connection_secret").unwrap_or(None);
    secret
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "connection_secret vuoto o non decodificabile".to_string())
}

/// Guard-rail anti-contaminazione (regola E): vero se la connection string punta
/// al DB infrastruttura Nexus (db `nexus` su porta 5433 o hostname
/// `postgres-nexus`/`ideai-postgres-nexus`). Un tool applicativo non deve mai
/// toccare il DB di sistema.
fn points_to_nexus_infra_db(conn: &str) -> bool {
    let lower = conn.to_lowercase();
    (lower.contains("/nexus") || lower.contains("database=nexus"))
        && (lower.contains(":5433")
            || lower.contains("postgres-nexus")
            || lower.contains("ideai-postgres-nexus"))
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

/// Decodifica una cella secondo il tipo Postgres noto. Ritorna:
///   - `Some(Some(v))` valore decodificato,
///   - `Some(None)`     valore NULL,
///   - `None`           tipo non gestito o decodifica fallita (il chiamante
///                      applica il fallback su String).
fn cell_by_type(
    row: &sqlx::postgres::PgRow,
    idx: usize,
    type_name: &str,
) -> Option<Option<Value>> {
    macro_rules! try_as {
        ($t:ty) => {
            row.try_get::<Option<$t>, _>(idx)
                .ok()
                .map(|o| o.map(Value::from))
        };
    }
    match type_name {
        "BOOL" => try_as!(bool),
        "INT2" => row
            .try_get::<Option<i16>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as i64))),
        "INT4" => row
            .try_get::<Option<i32>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(v as i64))),
        "INT8" => try_as!(i64),
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
        "TIMESTAMPTZ" | "TIMESTAMP" | "DATE" | "TIME" => cell_temporal(row, idx, type_name),
        "BYTEA" => row
            .try_get::<Option<Vec<u8>>, _>(idx)
            .ok()
            .map(|o| o.map(|v| Value::from(format!("\\x{} ({} byte)", hex_prefix(&v), v.len())))),
        _ => None,
    }
}

/// Decodifica i tipi temporali Postgres (`TIMESTAMPTZ`/`TIMESTAMP`/`DATE`/`TIME`)
/// come stringhe: RFC3339 per i timestamptz, formato nativo per gli altri.
fn cell_temporal(
    row: &sqlx::postgres::PgRow,
    idx: usize,
    type_name: &str,
) -> Option<Option<Value>> {
    match type_name {
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
        _ => None,
    }
}

/// Converte una cella di PgRow in serde_json::Value in base al tipo Postgres.
/// I tipi non gestiti esplicitamente ricadono su String; se anche quella
/// fallisce, ritorna un marcatore di tipo.
pub fn cell_to_json(row: &sqlx::postgres::PgRow, idx: usize, type_name: &str) -> Value {
    match cell_by_type(row, idx, type_name) {
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

/// Stato del lexer di [`split_statements`]. Ogni variante indica in quale
/// contesto sintattico si trova lo scanner mentre percorre il testo SQL.
enum SplitState {
    Normal,
    SingleString,         // dentro '...'
    DoubleQuoted,         // dentro "..."
    LineComment,          // dentro -- fino a newline
    BlockComment,         // dentro /* ... */
    DollarQuoted(String), // tag $foo$ ... $foo$
}

/// Lexer incrementale che percorre il testo SQL byte per byte tenendo traccia
/// del contesto (stringhe, commenti, dollar-quoting) per individuare i `;` di
/// separazione a livello top-level. Estratto da [`split_statements`] per
/// distribuire la logica in metodi coesi (un metodo per stato), a comportamento
/// invariato.
struct SqlSplitter<'a> {
    bytes: &'a [u8],
    len: usize,
    i: usize,
    current: String,
    out: Vec<String>,
    state: SplitState,
}

impl<'a> SqlSplitter<'a> {
    fn new(sql: &'a str) -> Self {
        Self {
            bytes: sql.as_bytes(),
            len: sql.len(),
            i: 0,
            current: String::with_capacity(sql.len()),
            out: Vec::new(),
            state: SplitState::Normal,
        }
    }

    /// Carattere all'indice corrente (invariante: `self.i < self.len`).
    fn cur(&self) -> char {
        self.bytes[self.i] as char
    }

    /// Vero se il byte successivo esiste ed e' `c`.
    fn peek_is(&self, c: char) -> bool {
        self.i + 1 < self.len && self.bytes[self.i + 1] as char == c
    }

    /// Consuma tutto l'input applicando la transizione di stato appropriata e
    /// ritorna gli statement non vuoti raccolti.
    fn run(mut self) -> Vec<String> {
        while self.i < self.len {
            match &self.state {
                SplitState::Normal => self.step_normal(),
                SplitState::SingleString => self.step_single_string(),
                SplitState::DoubleQuoted => self.step_double_quoted(),
                SplitState::LineComment => self.step_line_comment(),
                SplitState::BlockComment => self.step_block_comment(),
                SplitState::DollarQuoted(_) => self.step_dollar_quoted(),
            }
        }
        let tail = self.current.trim().to_string();
        if !tail.is_empty() {
            self.out.push(tail);
        }
        self.out
    }

    /// Ramo `Normal`: apre stringhe/commenti/dollar-quote o chiude uno statement
    /// sul `;` top-level.
    fn step_normal(&mut self) {
        let c = self.cur();
        if c == '\'' {
            self.current.push(c);
            self.state = SplitState::SingleString;
            self.i += 1;
        } else if c == '"' {
            self.current.push(c);
            self.state = SplitState::DoubleQuoted;
            self.i += 1;
        } else if c == '-' && self.peek_is('-') {
            self.current.push_str("--");
            self.state = SplitState::LineComment;
            self.i += 2;
        } else if c == '/' && self.peek_is('*') {
            self.current.push_str("/*");
            self.state = SplitState::BlockComment;
            self.i += 2;
        } else if c == '$' {
            self.step_dollar_open();
        } else if c == ';' {
            let stmt = self.current.trim().to_string();
            if !stmt.is_empty() {
                self.out.push(stmt);
            }
            self.current.clear();
            self.i += 1;
        } else {
            self.current.push(c);
            self.i += 1;
        }
    }

    /// Sul `$` in stato `Normal`: se segue un tag `$foo$` valido entra in
    /// dollar-quoting, altrimenti tratta `$` come carattere ordinario.
    fn step_dollar_open(&mut self) {
        // Tenta di leggere $tag$ (tag alfanumerico/underscore, anche vuoto).
        let mut j = self.i + 1;
        while j < self.len {
            let ch = self.bytes[j] as char;
            if ch == '$' {
                break;
            }
            if !(ch.is_alphanumeric() || ch == '_') {
                j = self.i; // non e' un dollar-quote valido
                break;
            }
            j += 1;
        }
        if j > self.i && j < self.len && self.bytes[j] as char == '$' {
            let tag = String::from_utf8_lossy(&self.bytes[self.i..=j]).to_string();
            self.current.push_str(&tag);
            self.state = SplitState::DollarQuoted(tag);
            self.i = j + 1;
        } else {
            self.current.push('$');
            self.i += 1;
        }
    }

    fn step_single_string(&mut self) {
        let c = self.cur();
        self.current.push(c);
        if c == '\'' {
            // Escape standard '': se il prossimo carattere e' anche ', resta
            // nello stato.
            if self.peek_is('\'') {
                self.current.push('\'');
                self.i += 2;
            } else {
                self.state = SplitState::Normal;
                self.i += 1;
            }
        } else {
            self.i += 1;
        }
    }

    fn step_double_quoted(&mut self) {
        let c = self.cur();
        self.current.push(c);
        if c == '"' {
            if self.peek_is('"') {
                self.current.push('"');
                self.i += 2;
            } else {
                self.state = SplitState::Normal;
                self.i += 1;
            }
        } else {
            self.i += 1;
        }
    }

    fn step_line_comment(&mut self) {
        let c = self.cur();
        self.current.push(c);
        if c == '\n' {
            self.state = SplitState::Normal;
        }
        self.i += 1;
    }

    fn step_block_comment(&mut self) {
        let c = self.cur();
        self.current.push(c);
        if c == '*' && self.peek_is('/') {
            self.current.push('/');
            self.state = SplitState::Normal;
            self.i += 2;
        } else {
            self.i += 1;
        }
    }

    fn step_dollar_quoted(&mut self) {
        let SplitState::DollarQuoted(tag) = &self.state else {
            return;
        };
        // Cerca chiusura tag.
        let tag_clone = tag.clone();
        let tlen = tag_clone.len();
        if self.i + tlen <= self.len && self.bytes[self.i..self.i + tlen] == *tag_clone.as_bytes() {
            self.current.push_str(&tag_clone);
            self.state = SplitState::Normal;
            self.i += tlen;
        } else {
            self.current.push(self.cur());
            self.i += 1;
        }
    }
}

/// Splitta uno script SQL nei singoli statement, separati da `;` a livello
/// top-level. Ignora i `;` dentro:
///   - stringhe SQL delimitate da `'...'` (con escape `''` standard)
///   - identifier quotati `"..."`
///   - commenti riga `-- ...`
///   - commenti blocco `/* ... */`
///   - dollar-quoting `$tag$ ... $tag$` (corpi delle procedure PL/pgSQL)
///
/// Ritorna solo statement non vuoti (i `;` finali a vuoto vengono scartati).
/// Trim leading/trailing whitespace per ogni statement.
pub fn split_statements(sql: &str) -> Vec<String> {
    SqlSplitter::new(sql).run()
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

    // Il pool va chiuso su OGNI ritorno (successo o errore): eseguo la query in
    // un blocco interno che produce il Result, poi chiudo il pool una sola volta
    // prima di propagare. Comportamento invariato rispetto ai `pool.close()`
    // sparsi sui singoli early-return.
    let result = run_on_pool(&pool, sql, params, max_rows).await;
    pool.close().await;
    result
}

/// Corpo di [`execute_query`] una volta risolto e aperto il pool: splitta lo
/// script e instrada verso il percorso single- o multi-statement. Non chiude il
/// pool (se ne occupa il chiamante).
async fn run_on_pool(
    pool: &PgPool,
    sql: &str,
    params: &[Option<String>],
    max_rows: usize,
) -> Result<QueryExecOutcome, QueryExecError> {
    let statements = split_statements(sql);
    if statements.is_empty() {
        return Err(QueryExecError::Sql(
            "nessuna statement SQL trovata (solo commenti o whitespace)".to_string(),
        ));
    }

    // Multi-statement con parametri non e' supportato: i `$1`/`$2` valgono
    // per UN solo statement nel protocollo prepared. Se l'utente vuole un
    // batch con parametri, deve splittare lato chiamante.
    if statements.len() > 1 && !params.is_empty() {
        return Err(QueryExecError::Sql(
            "i parametri ($1, $2, ...) sono supportati solo con UNA singola \
             statement. Per eseguire piu' statement insieme, rimuovi i \
             parametri o esegui le statement separatamente."
                .to_string(),
        ));
    }

    let started = std::time::Instant::now();

    if statements.len() == 1 {
        exec_single_statement(pool, &statements[0], params, max_rows, started).await
    } else {
        exec_multi_statement(pool, &statements, max_rows, started).await
    }
}

/// Percorso single-statement: rispetta i parametri bindabili e distingue read
/// (fetch_all) da write (execute) a livello di statement.
async fn exec_single_statement(
    pool: &PgPool,
    single: &str,
    params: &[Option<String>],
    max_rows: usize,
    started: std::time::Instant,
) -> Result<QueryExecOutcome, QueryExecError> {
    let kind = classify_statement(single).to_string();
    if is_read_only(single) {
        exec_single_read(pool, single, params, max_rows, started, kind).await
    } else {
        exec_single_write(pool, single, params, started, kind).await
    }
}

/// Binda i parametri TEXT sullo statement (NULL -> bind null), come nel path
/// classico single-statement.
fn bind_params<'q>(
    single: &'q str,
    params: &'q [Option<String>],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    let mut q = sqlx::query(single);
    for p in params {
        q = q.bind(p.clone());
    }
    q
}

/// Statement read singolo: `fetch_all` con timeout, serializza righe/colonne.
async fn exec_single_read(
    pool: &PgPool,
    single: &str,
    params: &[Option<String>],
    max_rows: usize,
    started: std::time::Instant,
    kind: String,
) -> Result<QueryExecOutcome, QueryExecError> {
    let q = bind_params(single, params);
    let fetch =
        tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.fetch_all(pool)).await;
    let rows = match fetch {
        Err(_) => return Err(QueryExecError::Timeout),
        Ok(Err(e)) => return Err(QueryExecError::Sql(e.to_string())),
        Ok(Ok(r)) => r,
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    let truncated = rows.len() > max_rows;
    let slice = &rows[..rows.len().min(max_rows)];
    let (columns, out_rows) = serialize_rows(slice);

    Ok(QueryExecOutcome {
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
    })
}

/// Statement write singolo: `execute` con timeout, riporta `rows_affected`.
async fn exec_single_write(
    pool: &PgPool,
    single: &str,
    params: &[Option<String>],
    started: std::time::Instant,
    kind: String,
) -> Result<QueryExecOutcome, QueryExecError> {
    let q = bind_params(single, params);
    let exec = tokio::time::timeout(Duration::from_secs(QUERY_TIMEOUT_SECS), q.execute(pool)).await;
    let res = match exec {
        Err(_) => return Err(QueryExecError::Timeout),
        Ok(Err(e)) => return Err(QueryExecError::Sql(e.to_string())),
        Ok(Ok(r)) => r,
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    Ok(QueryExecOutcome {
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
    })
}

/// Percorso multi-statement: esegue il batch in transazione. L'ULTIMO statement
/// determina mode/columns/rows del payload principale; i precedenti finiscono in
/// `per_statement_summary`.
async fn exec_multi_statement(
    pool: &PgPool,
    statements: &[String],
    max_rows: usize,
    started: std::time::Instant,
) -> Result<QueryExecOutcome, QueryExecError> {
    let last_idx = statements.len() - 1;
    let last_kind = classify_statement(&statements[last_idx]).to_string();
    let last_is_read = is_read_only(&statements[last_idx]);

    let tx_result = tokio::time::timeout(
        Duration::from_secs(QUERY_TIMEOUT_SECS),
        run_batch_in_tx(pool, statements, max_rows, last_is_read),
    )
    .await;

    let batch = match tx_result {
        Err(_) => return Err(QueryExecError::Timeout),
        Ok(Err(e)) => return Err(QueryExecError::Sql(e)),
        Ok(Ok(b)) => b,
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    // Aggregato: somma write rows_affected (solo se l'ultimo NON e' read).
    let total_write: u64 = batch
        .per_statement
        .iter()
        .filter_map(|s| s.get("rows_affected").and_then(Value::as_u64))
        .sum();

    Ok(QueryExecOutcome {
        mode: if last_is_read { "read" } else { "write" },
        statement_kind: last_kind,
        row_count: batch.rows.len(),
        columns: batch.columns,
        rows: batch.rows,
        rows_affected: if last_is_read { None } else { Some(total_write) },
        truncated: batch.truncated,
        duration_ms,
        statements_executed: statements.len(),
        per_statement_summary: batch.per_statement,
    })
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
    let mut acc = BatchAccumulator::with_capacity(statements.len());

    for (i, stmt) in statements.iter().enumerate() {
        let is_last = i == last_idx;
        let step = if is_last && last_is_read {
            // L'ultimo read: fetch_all per popolare il risultato principale.
            run_last_read_stmt(&mut tx, &mut acc, stmt, i, max_rows).await
        } else {
            // Statement intermedie + ultima write: execute (rows_affected).
            run_write_stmt(&mut tx, &mut acc, stmt, i).await
        };
        if let Err(e) = step {
            let _ = tx.rollback().await;
            return Err(e);
        }
    }

    tx.commit()
        .await
        .map_err(|e| format!("COMMIT fallito: {e}"))?;

    Ok(acc.into_result())
}

/// Accumulatore per il batch multi-statement: raccoglie il riepilogo
/// per-statement e i dati dell'unico statement read finale.
struct BatchAccumulator {
    per_statement: Vec<Value>,
    columns: Vec<Value>,
    rows: Vec<Value>,
    truncated: bool,
}

impl BatchAccumulator {
    fn with_capacity(n: usize) -> Self {
        Self {
            per_statement: Vec::with_capacity(n),
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        }
    }

    fn into_result(self) -> BatchExecResult {
        BatchExecResult {
            columns: self.columns,
            rows: self.rows,
            truncated: self.truncated,
            per_statement: self.per_statement,
        }
    }
}

/// Esegue lo statement read finale del batch: `fetch_all`, popola
/// columns/rows/truncated sull'accumulatore e aggiunge il riepilogo.
async fn run_last_read_stmt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    acc: &mut BatchAccumulator,
    stmt: &str,
    i: usize,
    max_rows: usize,
) -> Result<(), String> {
    let kind = classify_statement(stmt).to_string();
    let fetched = sqlx::query(stmt)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| format!("errore statement #{}: {}", i + 1, e))?;
    acc.truncated = fetched.len() > max_rows;
    let slice = &fetched[..fetched.len().min(max_rows)];
    let (cols, out_rows) = serialize_rows(slice);
    acc.columns = cols;
    let count = out_rows.len();
    acc.rows = out_rows;
    acc.per_statement.push(json!({
        "index": i,
        "statement_kind": kind,
        "mode": "read",
        "row_count": count,
    }));
    Ok(())
}

/// Esegue uno statement intermedio o l'ultima write: `execute` (rows_affected).
/// Per le read non-finali ignoriamo le righe (lo user vede solo l'output
/// dell'ultima); cosi' supportiamo CTE/script con SELECT diagnostiche
/// intermedie senza saturare il payload.
async fn run_write_stmt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    acc: &mut BatchAccumulator,
    stmt: &str,
    i: usize,
) -> Result<(), String> {
    let kind = classify_statement(stmt).to_string();
    let read_only = is_read_only(stmt);
    let pg_res = sqlx::query(stmt)
        .execute(&mut **tx)
        .await
        .map_err(|e| format!("errore statement #{}: {}", i + 1, e))?;
    acc.per_statement.push(json!({
        "index": i,
        "statement_kind": kind,
        "mode": if read_only { "read_ignored" } else { "write" },
        "rows_affected": pg_res.rows_affected(),
    }));
    Ok(())
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

    // Connessione effettiva risolta lato archive (stessa logica di
    // resolve_project_conn: None/vuoto -> primary): titolo e tag riflettono il
    // DB su cui la DDL e' atterrata.
    let effective_conn = resolve_connection_name(db, project_id, connection_name)
        .await
        .unwrap_or_else(|| "primary".to_string());

    let title = build_ddl_title(trimmed, &effective_conn, timestamp);
    let body_md = build_ddl_body(trimmed, &effective_conn, outcome, timestamp);
    let note_id = Uuid::new_v4();
    let tags = ddl_tags(&effective_conn);

    if !insert_ddl_note(db, note_id, project_id, &title, &body_md, &tags).await {
        return None;
    }
    upsert_knowledge_tags(db, project_id, &tags).await;

    let (mig_filename, mig_abs) =
        write_migration_best_effort(db, project_id, trimmed, &title, &effective_conn).await;

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

/// Tag della nota KB per una DDL: `schema-change`, `ddl` e `db:<conn>` (l'ultimo
/// permette di filtrare le DDL per DB di destinazione nel pannello KB, multi-DB).
fn ddl_tags(effective_conn: &str) -> Vec<String> {
    vec![
        "schema-change".to_string(),
        "ddl".to_string(),
        format!("db:{}", effective_conn),
    ]
}

/// Corpo Markdown della nota KB per una DDL archiviata.
fn build_ddl_body(
    trimmed: &str,
    effective_conn: &str,
    outcome: &QueryExecOutcome,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> String {
    format!(
        "**DDL eseguita sul DB `{}`** ({} righe modificate, {} ms)\n\n\
         ```sql\n{}\n```\n\n\
         _Archiviata automaticamente il {}_",
        effective_conn,
        outcome.rows_affected.unwrap_or(0),
        outcome.duration_ms,
        trimmed,
        timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
    )
}

/// Scrive il file migration versionato (best effort): su errore logga WARN e
/// ritorna `(None, None)`. La connection effettiva instrada verso cartelle
/// separate in multi-DB (`nexus_migrations/<conn_name>/`).
async fn write_migration_best_effort(
    db: &PgPool,
    project_id: Uuid,
    trimmed: &str,
    title: &str,
    effective_conn: &str,
) -> (Option<String>, Option<String>) {
    match write_migration_file(db, project_id, trimmed, title, effective_conn).await {
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
    }
}

/// Costruisce il titolo della nota KB per una DDL archiviata, a partire dai
/// primi token significativi dello statement (azione / oggetto / target).
fn build_ddl_title(
    trimmed: &str,
    effective_conn: &str,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> String {
    let action = first_token_upper(trimmed); // CREATE / ALTER / DROP / TRUNCATE / RENAME
    let object = second_meaningful_token_upper(trimmed); // TABLE / INDEX / VIEW / ...
    let target = third_meaningful_identifier(trimmed); // <nome>
    match (action.as_str(), object.as_str(), target.as_deref()) {
        (a, "", _) => format!(
            "DDL {} on '{}' - {}",
            a,
            effective_conn,
            timestamp.format("%Y-%m-%d %H:%M")
        ),
        (a, o, Some(t)) => format!("DDL {} {} {} on '{}'", a, o, t, effective_conn),
        (a, o, None) => format!("DDL {} {} on '{}'", a, o, effective_conn),
    }
}

/// Inserisce la nota KB della DDL. Ritorna `false` (loggando WARN) se l'INSERT
/// fallisce, cosi' il chiamante interrompe l'archiviazione best-effort.
async fn insert_ddl_note(
    db: &PgPool,
    note_id: Uuid,
    project_id: Uuid,
    title: &str,
    body_md: &str,
    tags: &[String],
) -> bool {
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
    .bind(title)
    .bind(body_md)
    .bind(tags)
    .bind(&file_paths)
    .execute(db)
    .await;

    if let Err(e) = note_insert {
        tracing::warn!(
            error = %e,
            %project_id,
            "archive_ddl: INSERT project_knowledge_notes fallito"
        );
        return false;
    }
    true
}

/// Aggiorna i contatori aggregati dei tag KB (best effort: gli errori sono
/// ignorati, come nel comportamento storico).
async fn upsert_knowledge_tags(db: &PgPool, project_id: Uuid, tags: &[String]) {
    for tag in tags {
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
    let root = fetch_project_root(db, project_id).await?;

    // 2) migration_path: override sulla connessione specifica o fallback
    //    multi-DB-aware.
    let migration_subdir = resolve_migration_subdir(db, project_id, connection_name).await?;

    let mig_dir = std::path::Path::new(&root).join(&migration_subdir);
    tokio::fs::create_dir_all(&mig_dir)
        .await
        .map_err(|e| format!("create_dir_all {}: {e}", mig_dir.display()))?;

    // 3) Calcola prossimo numero progressivo.
    let next = next_migration_number(&mig_dir).await?;
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

/// Legge e valida `projects.repository_root_path` (trimmato, non vuoto).
async fn fetch_project_root(db: &PgPool, project_id: Uuid) -> Result<String, String> {
    let root_row = sqlx::query("SELECT repository_root_path FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("query projects: {e}"))?
        .ok_or_else(|| "progetto non trovato".to_string())?;
    let root: Option<String> = root_row.try_get("repository_root_path").unwrap_or(None);
    root.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "repository_root_path non configurato per il progetto".to_string())
}

/// Risolve la sottocartella migration relativa alla root: override esplicito su
/// `project_database_config.migration_path` per la connessione specifica; in
/// assenza, `nexus_migrations/` per la primary, `nexus_migrations/<slug>/` per
/// le altre.
async fn resolve_migration_subdir(
    db: &PgPool,
    project_id: Uuid,
    connection_name: &str,
) -> Result<String, String> {
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

    Ok(match explicit_path {
        Some(p) => p,
        None => {
            // Default: primary -> nexus_migrations/, altre -> nexus_migrations/<slug>/
            if connection_name.eq_ignore_ascii_case("primary") {
                "nexus_migrations".to_string()
            } else {
                format!("nexus_migrations/{}", safe_dir_segment(connection_name))
            }
        }
    })
}

/// Prossimo numero progressivo per la cartella: max(NNNN dei file esistenti) + 1.
/// Il counter e' per-cartella, quindi ogni connessione ha la sua sequenza.
async fn next_migration_number(mig_dir: &std::path::Path) -> Result<u32, String> {
    let mut max_seen: u32 = 0;
    let mut entries = tokio::fs::read_dir(mig_dir)
        .await
        .map_err(|e| format!("read_dir {}: {e}", mig_dir.display()))?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Some(fname) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if let Some(n) = fname.split('_').next().and_then(|s| s.parse::<u32>().ok()) {
            if n > max_seen {
                max_seen = n;
            }
        }
    }
    Ok(max_seen + 1)
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

#[cfg(test)]
mod tests_connessione {
    use super::*;

    /// Il DB metadati per-progetto NON e' riconoscibile dalla URL: stesso
    /// cluster del DB applicativo, e nome database `<slug>_nexus`. Prima del
    /// ruolo dichiarato la sola prova disponibile era `points_to_nexus_infra_db`,
    /// che su quella URL dice (correttamente) "non e' il DB META" — e da li'
    /// `nexus_db_query(connection: "nexus_metadata")` apriva un pool su
    /// `agent_steps`.
    #[test]
    fn il_db_metadati_di_progetto_non_si_riconosce_dalla_url() {
        let url = "postgres://nexus_admin:pw@127.0.0.1:5434/gestione_corsi_nexus";
        assert!(
            !points_to_nexus_infra_db(url),
            "la URL del DB metadati di progetto non e' quella del DB META: \
             e' il RUOLO a doverlo dire"
        );
        assert_eq!(
            classifica_connessione(url, RUOLO_METADATI_NEXUS),
            ConnessioneRisolta::MetadatiDiProgetto
        );
        assert!(classifica_connessione(url, RUOLO_METADATI_NEXUS)
            .motivo_del_rifiuto()
            .is_some());
    }

    /// La URL resta necessaria: una riga registrata a mano puo' puntare al DB
    /// META senza dichiarare alcun ruolo (le righe anteriori alla mig 0494
    /// hanno il default 'app').
    #[test]
    fn il_db_meta_resta_rifiutato_anche_col_ruolo_applicativo() {
        for url in [
            "postgres://nexus:pw@postgres-nexus:5433/nexus",
            "postgres://nexus:pw@127.0.0.1:5433/nexus",
            "postgresql://u:p@ideai-postgres-nexus:5433/nexus?sslmode=disable",
        ] {
            assert_eq!(
                classifica_connessione(url, "app"),
                ConnessioneRisolta::InfrastrutturaNexus,
                "url {url}"
            );
        }
    }

    /// Il DB applicativo passa: e' il caso ordinario, e senza di esso il guard
    /// sarebbe verde per assenza.
    #[test]
    fn il_db_applicativo_del_progetto_passa() {
        let url = "postgres://app:pw@127.0.0.1:5434/gestione_corsi_app";
        assert_eq!(
            classifica_connessione(url, "app"),
            ConnessioneRisolta::DbApplicativo
        );
        assert!(classifica_connessione(url, "app")
            .motivo_del_rifiuto()
            .is_none());
        // Ruolo assente (colonna NULL letta come stringa vuota): resta applicativo.
        assert_eq!(
            classifica_connessione(url, ""),
            ConnessioneRisolta::DbApplicativo
        );
    }
}
