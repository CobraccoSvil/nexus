//! Tool agente per gestione runtime del DB applicativo del progetto.
//!
//! Bug osservato 31/05/2026: l'utente chiede "inserisci un utente con email X"
//! ma il modello (Vertex gemini-2.5-pro) tenta `psql` (non installato nel WSL
//! host) e si blocca. Non esisteva un tool per eseguire SQL ad-hoc sul DB
//! applicativo del progetto: i tool `project_db_*` coprono solo le migration
//! versionate, non query interattive.
//!
//! Questo modulo espone 3 tool builtin (thin wrapper sopra
//! `crate::project_db::exec` per non duplicare la logica con l'endpoint
//! REST `POST /api/projects/:id/db/query`):
//!   - `nexus_db_query`    : esegue SQL arbitrario (SELECT/INSERT/UPDATE/DELETE/DDL)
//!   - `nexus_db_tables`   : lista le tabelle dello schema public
//!   - `nexus_db_describe` : colonne/tipi/vincoli/indici di una tabella
//!
//! Sicurezza (regola E CLAUDE.md - isolamento progetti):
//!   - La connessione viene SEMPRE risolta da `project_database_config` del
//!     progetto attivo (via `crate::project_db::exec::resolve_project_conn`).
//!     E' li' che si decide QUALE database si tocca: `classifica_connessione`
//!     rifiuta il DB META e il DB metadati per-progetto. Il guard sul testo
//!     della statement (`check_dangerous_sql`) NON risponde a quella domanda e
//!     non deve provarci: leggere il catalogo del DB applicativo e' legittimo.
//!   - Limiti: timeout query 30s, max 1000 righe ritornate.
//!
//! Esito (regola Q): i tre tool ritornano [`RispostaTool`] — il payload JSON sta
//! nel testo, l'esito e la NATURA del fallimento stanno nei campi. Prima il
//! fallimento viaggiava come marker anteposto al JSON: spezzava il payload, e
//! chi doveva sapere com'era andata era costretto a rileggere il testo.

use serde_json::{json, Value};
use sqlx::Row;

use super::AgentToolContext;
use crate::project_db::exec::{
    self, archive_ddl, execute_query, open_pool, outcome_to_json, resolve_project_conn,
    QueryExecError,
};
use nexus_agent_tools::input_contract::InputTool;
use nexus_agent_tools::tool_inputs::{NexusDbDescribeInput, NexusDbQueryInput, NexusDbTablesInput};
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

/// Costruisce l'esito FALLITO di uno dei tool `nexus_db_*`: il payload JSON nel
/// testo, l'esito e la natura nei campi.
fn db_tool_failure(payload: Value, natura: NaturaFallimento) -> RispostaTool {
    RispostaTool::fallito(payload.to_string()).con_natura(natura)
}

/// Il caso comune: il payload porta il solo campo `error`.
fn db_tool_error(messaggio: impl std::fmt::Display, natura: NaturaFallimento) -> RispostaTool {
    db_tool_failure(json!({ "error": messaggio.to_string() }), natura)
}

/// Toglie `connection` dall'input e lo restituisce a parte, insieme al resto.
///
/// DIVERGENZA fra i due cataloghi che portano a questo handler, e la ragione
/// per cui il campo non passa dal contratto d'ingresso. Il catalogo del dispatch
/// agente (`nexus-agent-tools::tool_schema` + `tool_inputs`) NON promette
/// `connection`; il catalogo BUILTIN (`crate::nexus_builtin::catalog`) lo
/// promette per `nexus_db_query`, e quella strada arriva qui passando da
/// `nexus_mcp_tool_call` con `server_id="builtin"`. Poiche' il contratto e'
/// `deny_unknown_fields`, leggerlo prima della deserializzazione e' cio' che
/// evita di respingere come "parametri non validi" una chiamata che un catalogo
/// ha promesso. La divergenza si chiude allineando i due cataloghi, non qui.
fn separa_connection(input: &Value) -> (Option<String>, Value) {
    let mut resto = input.clone();
    let connection = resto
        .as_object_mut()
        .and_then(|map| map.remove("connection"))
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());
    (connection, resto)
}

/// I parametri di `nexus_db_query` gia' letti dal contratto e validati.
struct QueryInput {
    sql: String,
    params: Vec<Option<String>>,
    max_rows: Option<usize>,
    connection: Option<String>,
}

/// Legge l'input di `nexus_db_query` dal contratto d'ingresso e ne valida i
/// campi che lo schema non puo' vincolare da solo (stringa non vuota, intero
/// positivo). Ogni rifiuto e' RIMEDIABILE e nomina il campo da correggere.
fn leggi_query_input(input: &Value) -> Result<QueryInput, RispostaTool> {
    let (connection, resto) = separa_connection(input);
    let letto = NexusDbQueryInput::leggi(&resto)?;
    let sql = letto.sql.trim().to_string();
    if sql.is_empty() {
        return Err(db_tool_error(
            "Il campo 'sql' e' vuoto: passa UNA statement SQL da eseguire \
             (es. \"SELECT * FROM users LIMIT 10\").",
            NaturaFallimento::Rimediabile,
        ));
    }
    // Prima `max_rows` era letto con `as_u64`: un valore negativo diventava
    // silenziosamente "campo assente", cioe' il default. Il contratto lo
    // dichiara `integer`, quindi il negativo arriva fin qui ed e' un errore
    // della chiamata: dirlo e' meglio che ignorarlo. Il `min` non e' il cap
    // (quello lo applica `execute_query`): rende sicura la conversione a
    // `usize` di un i64 arbitrariamente grande.
    let max_rows = match letto.max_rows {
        None => None,
        Some(n) if n > 0 => Some(n.min(exec::MAX_ROWS as i64) as usize),
        Some(n) => {
            let msg = format!(
                "'max_rows' deve essere un intero positivo (ricevuto {n}). \
                 Omettilo per il default di {} righe.",
                exec::MAX_ROWS
            );
            return Err(db_tool_error(msg, NaturaFallimento::Rimediabile));
        }
    };
    let params = letto
        .params
        .unwrap_or_default()
        .iter()
        .map(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .collect();
    Ok(QueryInput {
        sql,
        params,
        max_rows,
        connection,
    })
}

/// Le guardie che precedono l'esecuzione. Ritorna `Some(fallimento)` se la
/// query non deve partire.
async fn guardie_query(ctx: &AgentToolContext, q: &QueryInput) -> Option<RispostaTool> {
    // Placeholder di redazione copiati come valori (incidente Beaty-Book:
    // [REDACTED:email_pii] persistito nel DB applicativo). Punto unico:
    // security::redaction_guard (regola L). Il controllo copre il testo SQL E
    // ogni bind param: un placeholder passato come parametro bypasserebbe il
    // check sul solo SQL. RIMEDIABILE perche' il messaggio del guard dice da
    // dove prendere il valore vero (env gia' iniettate, `request_port`).
    let campi = std::iter::once(("sql", q.sql.as_str()))
        .chain(q.params.iter().flatten().map(|p| ("params", p.as_str())));
    for (campo, testo) in campi {
        let rifiuto = crate::security::redaction_guard::enforce_no_redacted_placeholder(
            ctx,
            "nexus_db_query",
            campo,
            testo,
        )
        .await;
        if let Some(msg) = rifiuto {
            return Some(db_tool_error(msg, NaturaFallimento::Rimediabile));
        }
    }
    governance_query(ctx, &q.sql).await
}

/// Guard SQL per-statement (governance db/sql_injection): blocca le query
/// distruttive di massa (DELETE/UPDATE senza WHERE), le SCRITTURE sui cataloghi
/// di sistema e gli oggetti di infrastruttura Nexus. Solo blocco + audit
/// (decisione utente: niente auto-fix su DB).
///
/// NON decide "su quale DB si sta lavorando": quello lo decide la connessione,
/// in `resolve_project_conn` -> `classifica_connessione`, che rifiuta sia il DB
/// META sia il DB metadati per-progetto. E' il motivo per cui la LETTURA di
/// `information_schema`/`pg_catalog` e' consentita: il catalogo che l'agente
/// puo' raggiungere e' quello del DB applicativo del progetto, cioe' del
/// proprio lavoro.
///
/// RIMEDIABILE: ogni [`MotivoBlocco`](crate::security::resource_governance::MotivoBlocco)
/// nomina cio' che va cambiato nella query (aggiungi un WHERE, non scrivere sul
/// catalogo), e riscriverla e' nella portata dell'agente.
async fn governance_query(ctx: &AgentToolContext, sql: &str) -> Option<RispostaTool> {
    let policy = crate::security::resource_governance::policy(&ctx.db, "db", "sql_injection").await;
    if !policy.enabled {
        return None;
    }
    let motivo = crate::security::resource_governance::check_dangerous_sql(sql)?;
    let mut entry = crate::security::AuditEntry::blocked(
        ctx.project_id,
        "db_dangerous_statement_blocked",
        "db",
    )
    .with_resource(sql.chars().take(120).collect::<String>())
    .with_details(json!({ "rule": motivo.regola(), "reason": motivo.motivo() }))
    .with_actor_user(ctx.user_id);
    if let Some(s) = ctx.session_id {
        entry = entry.with_actor_session(s);
    }
    crate::security::record_audit(entry);
    let payload = json!({
        "error": format!("Query rifiutata dalla governance DB: {}", motivo.motivo()),
        "rule": motivo.regola(),
        "blocked": true,
    });
    Some(db_tool_failure(payload, NaturaFallimento::Rimediabile))
}

/// La natura di un fallimento di `execute_query`, letta dalla VARIANTE
/// dell'errore (segnale strutturato, regola M) e non dal suo messaggio.
fn fallimento_query(e: &QueryExecError, sql: &str) -> RispostaTool {
    let estratto = sql.chars().take(200).collect::<String>();
    match e {
        // La connessione del progetto non e' configurata, non e' raggiungibile
        // o il guard-rail anti-Nexus e' scattato: nessun campo della chiamata la
        // cambia. L'errore arriva gia' appiattito in `String` da
        // `resolve_project_conn` / `open_pool`, quindi non c'e' un kind da
        // leggere — e DEL SISTEMA e' anche la scelta prudente, perche' manda a
        // cercare un'altra strada invece di far ripetere una chiamata che
        // rifallira' identica.
        QueryExecError::ConnectionError(m) => db_tool_error(m, NaturaFallimento::DelSistema),
        // NON transitorio: ritentare la stessa query pesante la fa scadere di
        // nuovo dopo altri 30s. Cio' che rimedia lo dice il messaggio, ed e'
        // nella portata dell'agente.
        QueryExecError::Timeout => {
            let payload = json!({
                "error": e.message(),
                "hint": "Restringi la query (WHERE piu' selettivo, LIMIT, meno JOIN) \
                         oppure spezzala in piu' statement.",
                "sql_excerpt": estratto,
            });
            db_tool_failure(payload, NaturaFallimento::Rimediabile)
        }
        // Sintassi, colonna inesistente, vincolo violato: l'errore Postgres e'
        // nel messaggio e dice cosa correggere; i due tool che danno lo schema
        // esatto sono nominati, cosi' il "rimediabile" porta con se' il come.
        QueryExecError::Sql(_) => {
            let payload = json!({
                "error": e.message(),
                "hint": "Verifica nomi e tipi con nexus_db_describe (colonne di una tabella) \
                         o nexus_db_tables (tabelle esistenti).",
                "sql_excerpt": estratto,
            });
            db_tool_failure(payload, NaturaFallimento::Rimediabile)
        }
    }
}

/// Esegue la query e compone il payload di successo, con l'eventuale archivio
/// della DDL.
async fn esegui_query(ctx: &AgentToolContext, q: &QueryInput) -> RispostaTool {
    let conn = q.connection.as_deref();
    let esito = execute_query(&ctx.db, ctx.project_id, &q.sql, &q.params, q.max_rows, conn).await;
    let outcome = match esito {
        Ok(o) => o,
        Err(e) => return fallimento_query(&e, &q.sql),
    };
    // Simmetria con l'endpoint REST: archivio le DDL fatte dall'agente come
    // nota KB + file migration, separate per connessione in caso di multi-DB.
    // Best effort: errori solo loggati.
    let archive = archive_ddl(&ctx.db, ctx.project_id, &q.sql, &outcome, conn).await;
    let mut payload = outcome_to_json(&outcome);
    if let (Some(archived), Value::Object(ref mut map)) = (archive, &mut payload) {
        let dettaglio = json!({
            "note_id": archived.note_id.to_string(),
            "migration_filename": archived.migration_filename,
            "migration_abs_path": archived.migration_abs_path,
        });
        map.insert("archived_ddl".to_string(), dettaglio);
    }
    RispostaTool::riuscito(payload.to_string())
}

/// Tool `nexus_db_query`. Thin wrapper sopra `crate::project_db::exec::execute_query`.
pub(super) async fn tool_nexus_db_query(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let q = match leggi_query_input(input) {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };
    if let Some(rifiuto) = guardie_query(ctx, &q).await {
        return rifiuto;
    }
    esegui_query(ctx, &q).await
}

/// Normalizza `schema` (default "public"), risolve la connessione del progetto e
/// apre il pool. Punto unico (regola L) per i tool `nexus_db_tables` /
/// `nexus_db_describe`: prima il blocco era duplicato pari-pari.
///
/// DEL SISTEMA: l'errore che ne esce viene da `resolve_project_conn` /
/// `open_pool`, cioe' dalla configurazione DB del progetto o dalla sua
/// raggiungibilita'. Nessun campo della chiamata lo corregge.
async fn apri_pool_progetto(
    ctx: &AgentToolContext,
    schema: Option<String>,
    connection: Option<&str>,
) -> Result<(String, sqlx::PgPool), RispostaTool> {
    // Uno `schema` presente ma vuoto valeva come schema vuoto, e produceva un
    // elenco di zero tabelle indistinguibile da uno schema davvero vuoto: qui
    // vale come "non specificato", che e' cio' che il catalogo dichiara.
    let schema = schema
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "public".to_string());
    let conn = resolve_project_conn(&ctx.db, ctx.project_id, connection)
        .await
        .map_err(|e| db_tool_error(e, NaturaFallimento::DelSistema))?;
    let pool = open_pool(&conn)
        .await
        .map_err(|e| db_tool_error(e, NaturaFallimento::DelSistema))?;
    Ok((schema, pool))
}

/// Tool `nexus_db_tables`: lista tabelle + righe stimate dello schema.
pub(super) async fn tool_nexus_db_tables(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let (connection, resto) = separa_connection(input);
    let letto = match NexusDbTablesInput::leggi(&resto) {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };
    let (schema, pool) = match apri_pool_progetto(ctx, letto.schema, connection.as_deref()).await {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };

    let rows = sqlx::query(
        r#"SELECT t.table_name,
                  COALESCE(c.reltuples::bigint, 0) AS est_rows
           FROM information_schema.tables t
           LEFT JOIN pg_class c ON c.relname = t.table_name
           WHERE t.table_schema = $1 AND t.table_type = 'BASE TABLE'
           ORDER BY t.table_name"#,
    )
    .bind(&schema)
    .fetch_all(&pool)
    .await;
    pool.close().await;

    let tables_result = match rows {
        Ok(r) => r,
        // La query e' NOSTRA e interroga information_schema: se fallisce non c'e'
        // nulla che l'agente possa correggere nella propria chiamata.
        Err(e) => {
            let msg = format!("errore listing tabelle: {e}");
            return db_tool_error(msg, NaturaFallimento::DelSistema);
        }
    };
    let tables: Vec<Value> = tables_result
        .iter()
        .map(|row| {
            json!({
                "name": row.try_get::<String, _>("table_name").unwrap_or_default(),
                "estimated_rows": row.try_get::<i64, _>("est_rows").unwrap_or(0),
            })
        })
        .collect();
    // Zero tabelle e' un SUCCESSO: la lettura e' riuscita e lo schema e' vuoto
    // (stesso criterio dei file: una directory vuota e' un successo, una
    // directory assente e' un errore).
    let result = json!({"ok": true, "schema": schema, "table_count": tables.len(), "tables": tables});
    RispostaTool::riuscito(result.to_string())
}

/// Le colonne della tabella, nell'ordine dichiarato. Un `Ok(vec![])` significa
/// "la tabella non esiste o non ha colonne": la distinzione fra assenza ed
/// errore la fa il chiamante, che qui riceve l'errore come tale.
async fn colonne_tabella(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<Value>, String> {
    let rows = sqlx::query(
        r#"SELECT column_name, data_type, is_nullable, column_default,
                  character_maximum_length
           FROM information_schema.columns
           WHERE table_schema = $1 AND table_name = $2
           ORDER BY ordinal_position"#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("errore descrizione colonne: {e}"))?;
    Ok(rows
        .iter()
        .map(|row| {
            let nullable = row
                .try_get::<String, _>("is_nullable")
                .map(|s| s == "YES")
                .unwrap_or(true);
            json!({
                "name": row.try_get::<String, _>("column_name").unwrap_or_default(),
                "type": row.try_get::<String, _>("data_type").unwrap_or_default(),
                "nullable": nullable,
                "default": row.try_get::<Option<String>, _>("column_default").unwrap_or(None),
                "max_length": row.try_get::<Option<i32>, _>("character_maximum_length").unwrap_or(None),
            })
        })
        .collect())
}

/// Gli indici della tabella.
///
/// L'errore RISALE invece di essere inghiottito: prima un `unwrap_or_default()`
/// trasformava una `pg_indexes` fallita in una lista vuota, cioe' il tool
/// AFFERMAVA "questa tabella non ha indici" dove non aveva potuto guardare — e
/// su quella affermazione un agente decide di crearne uno che esiste gia'.
async fn indici_tabella(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<Value>, String> {
    let rows = sqlx::query(
        r#"SELECT indexname, indexdef
           FROM pg_indexes
           WHERE schemaname = $1 AND tablename = $2
           ORDER BY indexname"#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("errore lettura indici: {e}"))?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "name": row.try_get::<String, _>("indexname").unwrap_or_default(),
                "definition": row.try_get::<String, _>("indexdef").unwrap_or_default(),
            })
        })
        .collect())
}

/// Compone la descrizione della tabella. Separata dal tool perche' il pool va
/// chiuso una volta sola, dal chiamante, qualunque sia l'esito.
async fn descrivi_tabella(pool: &sqlx::PgPool, schema: &str, table: &str) -> RispostaTool {
    let columns = match colonne_tabella(pool, schema, table).await {
        Ok(c) => c,
        // Query nostra su information_schema: fuori dalla portata dell'agente.
        Err(e) => return db_tool_error(e, NaturaFallimento::DelSistema),
    };
    if columns.is_empty() {
        // Una tabella ASSENTE e' un errore, non un risultato vuoto: e' il nome
        // passato a essere sbagliato, e il messaggio dice con quale tool
        // trovarlo — quindi rimediabile davvero.
        let payload = json!({
            "error": format!("Tabella '{schema}.{table}' non trovata o senza colonne."),
            "hint": "Usa nexus_db_tables per vedere le tabelle disponibili."
        });
        return db_tool_failure(payload, NaturaFallimento::Rimediabile);
    }
    let indexes = match indici_tabella(pool, schema, table).await {
        Ok(i) => i,
        Err(e) => return db_tool_error(e, NaturaFallimento::DelSistema),
    };
    let payload = json!({
        "ok": true,
        "schema": schema,
        "table": table,
        "columns": columns,
        "indexes": indexes,
        // Costanti esposte per documentazione (riusate via exec module).
        "_limits": {
            "max_rows": exec::MAX_ROWS,
            "query_timeout_secs": exec::QUERY_TIMEOUT_SECS,
            "max_cell_chars": exec::MAX_CELL_CHARS,
        }
    });
    RispostaTool::riuscito(payload.to_string())
}

/// Tool `nexus_db_describe`: colonne, tipi, vincoli e indici di una tabella.
pub(super) async fn tool_nexus_db_describe(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let (connection, resto) = separa_connection(input);
    let letto = match NexusDbDescribeInput::leggi(&resto) {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };
    let table = letto.table.trim().to_string();
    if table.is_empty() {
        return db_tool_error(
            "Il campo 'table' e' vuoto: passa il nome della tabella da descrivere \
             (nexus_db_tables elenca quelle esistenti).",
            NaturaFallimento::Rimediabile,
        );
    }
    let (schema, pool) = match apri_pool_progetto(ctx, letto.schema, connection.as_deref()).await {
        Ok(v) => v,
        Err(risposta) => return risposta,
    };
    let esito = descrivi_tabella(&pool, &schema, &table).await;
    pool.close().await;
    esito
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;
    use uuid::Uuid;

    /// La query REALE dell'incidente, letta da `agent_steps.tool_input` del
    /// DB-progetto di gestione-corsi (passo `nexus_db_query`, `status='failed'`,
    /// 2026-08-09 13:34:40 UTC). Byte per byte, punto e virgola compreso: e'
    /// l'input che la produzione ha respinto, non una sua parafrasi.
    const SQL_INCIDENTE: &str =
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name;";

    /// Registra una connessione per il progetto. Scrive nella tabella REALE
    /// della migrazione (regola O): se `connection_role` cambiasse vincolo o
    /// nome, questo test lo vedrebbe, una fixture `CREATE TABLE` no.
    async fn registra_connessione(pool: &PgPool, project_id: Uuid, nome: &str, url: &str, ruolo: &str) {
        sqlx::query(
            "INSERT INTO project_database_config \
                (project_id, name, engine, hosting_mode, connection_secret, is_primary, connection_role) \
             VALUES ($1, $2, 'postgres', 'internal', $3::bytea, $4, $5)",
        )
        .bind(project_id)
        .bind(nome)
        .bind(url.as_bytes())
        .bind(ruolo == "app")
        .bind(ruolo)
        .execute(pool)
        .await
        .expect("registra connessione");
    }

    /// I due casi speculari dell'incidente, presi per la strada della
    /// produzione: la STESSA query, e a decidere e' la CONNESSIONE.
    ///
    /// Il DB metadati per-progetto era raggiungibile: `nexus_metadata` e' una
    /// riga vera di `project_database_config` (mig 0494), la sua URL non e'
    /// quella del DB META (stesso cluster applicativo, database
    /// `<slug>_nexus`), e `resolve_project_conn` filtra per nome o per
    /// `is_primary` senza guardare il ruolo. Bastava
    /// `nexus_db_query(connection: "nexus_metadata")`.
    ///
    /// Mutazione che rende rosso: togliere il ramo `MetadatiDiProgetto` da
    /// `classifica_connessione` -> la seconda asserzione cade e il tool apre un
    /// pool su `agent_steps`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_catalogo_si_legge_sul_db_del_progetto_mai_su_quello_di_nexus(pool: PgPool) {
        let (_user, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;

        registra_connessione(
            &pool,
            project_id,
            "primary",
            "postgres://app:pw@127.0.0.1:5434/gestione_corsi_app",
            "app",
        )
        .await;
        registra_connessione(
            &pool,
            project_id,
            "nexus_metadata",
            "postgres://nexus_admin:pw@127.0.0.1:5434/gestione_corsi_nexus",
            "nexus_metadata",
        )
        .await;

        // Il testo della query non e' cio' che decide: e' lo stesso nei due casi.
        assert_eq!(
            crate::security::resource_governance::check_dangerous_sql(SQL_INCIDENTE),
            None,
            "la lettura del catalogo non e' piu' respinta dal guard sul testo"
        );

        // Connessione applicativa: risolta, la query puo' partire.
        let applicativa = resolve_project_conn(&pool, project_id, None).await;
        assert!(
            applicativa.is_ok(),
            "il DB applicativo del progetto deve restare raggiungibile: {applicativa:?}"
        );

        // Connessione di infrastruttura: rifiutata PRIMA di aprire qualunque pool.
        let metadati = resolve_project_conn(&pool, project_id, Some("nexus_metadata")).await;
        assert!(
            metadati.is_err(),
            "il DB metadati Nexus del progetto non e' un DB applicativo"
        );

        // E la stessa query, instradata li', non arriva al database.
        let esito = execute_query(&pool, project_id, SQL_INCIDENTE, &[], None, Some("nexus_metadata")).await;
        assert!(
            matches!(esito, Err(QueryExecError::ConnectionError(_))),
            "atteso rifiuto di connessione, ottenuto {esito:?}"
        );
    }

    /// Il rifiuto di [`leggi_query_input`].
    ///
    /// Esiste al posto di `expect_err` perche' [`QueryInput`] non implementa
    /// `Debug` di proposito: porta il testo SQL e i bind param, che possono
    /// contenere dati personali, e `expect_err` li stamperebbe nell'output dei
    /// test in chiaro.
    fn rifiuto(input: Value) -> RispostaTool {
        match leggi_query_input(&input) {
            Ok(_) => panic!("un input che doveva essere rifiutato e' passato"),
            Err(e) => e,
        }
    }

    #[test]
    fn db_tool_failure_dichiara_il_fallimento_nei_campi_e_preserva_il_payload() {
        // Chiama il PRODUTTORE reale usato da tutti i rami di errore dei 3 tool
        // `nexus_db_*`. L'esito e la natura stanno nei campi: nessun consumatore
        // deve rileggere il testo per sapere com'e' andata (regola Q).
        let out = db_tool_failure(
            json!({"error": "sql fallita", "sql_excerpt": "SELECT 1"}),
            NaturaFallimento::Rimediabile,
        );
        assert!(out.esito.e_fallito(), "{out:?}");
        assert_eq!(out.natura, Some(NaturaFallimento::Rimediabile));
        // Il payload resta un JSON INTEGRO: il marker in testa lo spezzava.
        let parsed: Value = serde_json::from_str(&out.testo).expect("payload JSON valido");
        assert_eq!(parsed["error"], "sql fallita");
        assert_eq!(parsed["sql_excerpt"], "SELECT 1");
    }

    #[test]
    fn connection_non_passa_dal_contratto_ma_non_fa_fallire_la_lettura() {
        // Il contratto d'ingresso e' `deny_unknown_fields` e non dichiara
        // `connection`; il catalogo builtin invece lo promette. Se il campo
        // arrivasse alla deserializzazione, questa chiamata fallirebbe come
        // "parametri non validi" — che e' cio' che `separa_connection` evita.
        let input = json!({"sql": "SELECT 1", "connection": " analytics "});
        let q = leggi_query_input(&input).expect("input valido");
        assert_eq!(q.connection.as_deref(), Some("analytics"));
        assert_eq!(q.sql, "SELECT 1");
    }

    #[test]
    fn sql_vuoto_e_max_rows_non_positivo_sono_rimediabili_e_nominano_il_campo() {
        let vuoto = rifiuto(json!({"sql": "   "}));
        assert_eq!(vuoto.natura, Some(NaturaFallimento::Rimediabile));
        assert!(vuoto.testo.contains("'sql'"), "{vuoto:?}");
        // Prima `as_u64` faceva sparire il negativo nel default, in silenzio.
        let negativo = rifiuto(json!({"sql": "SELECT 1", "max_rows": -5}));
        assert_eq!(negativo.natura, Some(NaturaFallimento::Rimediabile));
        assert!(negativo.testo.contains("max_rows"), "{negativo:?}");
    }

    #[test]
    fn max_rows_resta_entro_il_cap_e_i_params_diventano_bind_testuali() {
        let input = json!({
            "sql": "SELECT $1, $2, $3",
            "params": ["ciao", null, 42],
            "max_rows": 99_999_999_999_i64,
        });
        let q = leggi_query_input(&input).expect("input valido");
        assert_eq!(q.max_rows, Some(exec::MAX_ROWS));
        assert_eq!(
            q.params,
            vec![Some("ciao".to_string()), None, Some("42".to_string())]
        );
    }

    #[test]
    fn params_non_array_e_campo_ignoto_sono_rifiutati_invece_che_ignorati() {
        // Prima un `params` non-array cadeva nel ramo `_ => Vec::new()`: la
        // query partiva senza i bind che il modello credeva di aver passato.
        let err = rifiuto(json!({"sql": "SELECT $1", "params": "ciao"}));
        assert_eq!(err.natura, Some(NaturaFallimento::Rimediabile));
        let ignoto = rifiuto(json!({"sql": "SELECT 1", "limit": 3}));
        assert!(ignoto.testo.contains("limit"), "{ignoto:?}");
    }

    #[test]
    fn fallimento_query_legge_la_natura_dalla_variante_non_dal_testo() {
        // Regola M: la variante e' il segnale strutturato. La connessione e'
        // fuori dalla portata dell'agente, la query sbagliata no.
        let conn = fallimento_query(
            &QueryExecError::ConnectionError("DB progetto non configurato".into()),
            "SELECT 1",
        );
        assert_eq!(conn.natura, Some(NaturaFallimento::DelSistema));
        let sql = fallimento_query(&QueryExecError::Sql("colonna inesistente".into()), "SELECT x");
        assert_eq!(sql.natura, Some(NaturaFallimento::Rimediabile));
        assert!(sql.testo.contains("nexus_db_describe"), "{sql:?}");
        // Il timeout NON e' transitorio: ripetere la stessa query pesante la fa
        // scadere di nuovo, e cio' che rimedia sta nel messaggio.
        let scaduta = fallimento_query(&QueryExecError::Timeout, "SELECT pg_sleep(60)");
        assert_eq!(scaduta.natura, Some(NaturaFallimento::Rimediabile));
        assert!(scaduta.testo.contains("sql_excerpt"), "{scaduta:?}");
    }
}
