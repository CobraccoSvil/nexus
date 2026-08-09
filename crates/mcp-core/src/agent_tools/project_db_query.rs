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

use serde_json::{json, Value};
use sqlx::Row;

use super::AgentToolContext;
use crate::project_db::exec::{
    self, archive_ddl, execute_query, open_pool, outcome_to_json, resolve_project_conn,
    QueryExecError,
};
use nexus_types::tool_outcome::tool_failure;

/// Costruisce l'esito FALLITO di uno dei tool `nexus_db_*`: marker + payload
/// JSON (contratto `nexus_types::tool_outcome`), qualunque sia la forma del
/// payload. Senza il marker in testa questi fallimenti erano indistinguibili
/// da un risultato riuscito per anti-loop/supervisore/final_gate, che leggono
/// solo `is_tool_failure`.
fn db_tool_failure(payload: Value) -> String {
    tool_failure(payload.to_string())
}

/// Tool `nexus_db_query`. Thin wrapper sopra `crate::project_db::exec::execute_query`.
pub(super) async fn tool_nexus_db_query(ctx: &AgentToolContext, input: &Value) -> String {
    let sql = match input.get("sql").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return db_tool_failure(
                json!({"error": "Parametro 'sql' obbligatorio (stringa non vuota)."}),
            );
        }
    };

    // Placeholder di redazione copiati come valori (incidente Beaty-Book:
    // [REDACTED:email_pii] persistito nel DB applicativo). Punto unico:
    // security::redaction_guard (regola L). Copre sql e params piu' sotto.
    if let Some(msg) = crate::security::redaction_guard::enforce_no_redacted_placeholder(
        ctx,
        "nexus_db_query",
        "sql",
        &sql,
    )
    .await
    {
        return db_tool_failure(json!({ "error": msg }));
    }

    let params: Vec<Option<String>> = match input.get("params") {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| match v {
                Value::Null => None,
                Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            })
            .collect(),
        _ => Vec::new(),
    };

    // Stesso guard sui valori parametrizzati: un placeholder passato come
    // bind param bypasserebbe il check sul testo SQL.
    for p in params.iter().flatten() {
        if let Some(msg) = crate::security::redaction_guard::enforce_no_redacted_placeholder(
            ctx,
            "nexus_db_query",
            "params",
            p,
        )
        .await
        {
            return db_tool_failure(json!({ "error": msg }));
        }
    }

    let max_rows = input
        .get("max_rows")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    // Guard SQL per-statement (governance db/sql_injection): blocca le query
    // distruttive di massa (DELETE/UPDATE senza WHERE), le SCRITTURE sui
    // cataloghi di sistema e gli oggetti di infrastruttura Nexus. Solo blocco +
    // audit (decisione utente: niente auto-fix su DB).
    //
    // NON decide "su quale DB si sta lavorando": quello lo decide la
    // connessione, in `resolve_project_conn` -> `classifica_connessione`, che
    // rifiuta sia il DB META sia il DB metadati per-progetto. E' il motivo per
    // cui la LETTURA di `information_schema`/`pg_catalog` e' consentita: il
    // catalogo che l'agente puo' raggiungere e' quello del DB applicativo del
    // progetto, cioe' del proprio lavoro.
    if crate::security::resource_governance::policy(&ctx.db, "db", "sql_injection")
        .await
        .enabled
    {
        if let Some(motivo) = crate::security::resource_governance::check_dangerous_sql(&sql) {
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
            return db_tool_failure(json!({
                "error": format!("Query rifiutata dalla governance DB: {}", motivo.motivo()),
                "rule": motivo.regola(),
                "blocked": true,
            }));
        }
    }

    // Connessione: se "connection" e' presente nel payload, esegue su quella
    // (es. "analytics", "legacy_replica"); altrimenti usa la primary del
    // progetto. Permette al modello di lavorare su DB multipli senza dover
    // switchare il flag is_primary.
    let connection = input
        .get("connection")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    match execute_query(
        &ctx.db,
        ctx.project_id,
        &sql,
        &params,
        max_rows,
        connection.as_deref(),
    )
    .await
    {
        Ok(outcome) => {
            // Simmetria con l'endpoint REST: archivio le DDL fatte
            // dall'agente come nota KB + file migration, separate per
            // connessione in caso di multi-DB. Best effort: errori solo
            // loggati.
            let archive = archive_ddl(
                &ctx.db,
                ctx.project_id,
                &sql,
                &outcome,
                connection.as_deref(),
            )
            .await;
            let mut payload = outcome_to_json(&outcome);
            if let (Some(archived), Value::Object(ref mut map)) = (archive, &mut payload) {
                map.insert(
                    "archived_ddl".to_string(),
                    json!({
                        "note_id": archived.note_id.to_string(),
                        "migration_filename": archived.migration_filename,
                        "migration_abs_path": archived.migration_abs_path,
                    }),
                );
            }
            payload.to_string()
        }
        Err(e) => match e {
            QueryExecError::ConnectionError(m) => db_tool_failure(json!({"error": m})),
            QueryExecError::Timeout => db_tool_failure(json!({"error": e.message()})),
            QueryExecError::Sql(_) => db_tool_failure(json!({
                "error": e.message(),
                "sql_excerpt": sql.chars().take(200).collect::<String>(),
            })),
        },
    }
}

/// Estrae `schema` (default "public") + `connection` opzionale dall'input, risolve
/// la connessione del progetto e apre il pool. Punto unico (regola L) per i tool
/// `nexus_db_tables` / `nexus_db_describe`: prima il blocco era duplicato pari-pari.
async fn resolve_schema_and_pool(
    ctx: &AgentToolContext,
    input: &Value,
) -> Result<(String, sqlx::PgPool), String> {
    let schema = input
        .get("schema")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "public".to_string());
    let connection = input
        .get("connection")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let conn = resolve_project_conn(&ctx.db, ctx.project_id, connection.as_deref()).await?;
    let pool = open_pool(&conn).await?;
    Ok((schema, pool))
}

/// Tool `nexus_db_tables`: lista tabelle + righe stimate dello schema.
pub(super) async fn tool_nexus_db_tables(ctx: &AgentToolContext, input: &Value) -> String {
    let (schema, pool) = match resolve_schema_and_pool(ctx, input).await {
        Ok(v) => v,
        Err(e) => return db_tool_failure(json!({"error": e})),
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

    let tables_result = match rows {
        Ok(r) => r,
        Err(e) => {
            pool.close().await;
            return db_tool_failure(json!({"error": format!("errore listing tabelle: {e}")}));
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
    let result = json!({"ok": true, "schema": schema, "table_count": tables.len(), "tables": tables});
    pool.close().await;
    result.to_string()
}

/// Tool `nexus_db_describe`: colonne, tipi, vincoli e indici di una tabella.
pub(super) async fn tool_nexus_db_describe(ctx: &AgentToolContext, input: &Value) -> String {
    let table = match input.get("table").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return db_tool_failure(json!({"error": "Parametro 'table' obbligatorio."})),
    };
    let (schema, pool) = match resolve_schema_and_pool(ctx, input).await {
        Ok(v) => v,
        Err(e) => return db_tool_failure(json!({"error": e})),
    };

    let col_rows = sqlx::query(
        r#"SELECT column_name, data_type, is_nullable, column_default,
                  character_maximum_length
           FROM information_schema.columns
           WHERE table_schema = $1 AND table_name = $2
           ORDER BY ordinal_position"#,
    )
    .bind(&schema)
    .bind(&table)
    .fetch_all(&pool)
    .await;

    let columns: Vec<Value> = match col_rows {
        Ok(r) => r
            .iter()
            .map(|row| {
                json!({
                    "name": row.try_get::<String, _>("column_name").unwrap_or_default(),
                    "type": row.try_get::<String, _>("data_type").unwrap_or_default(),
                    "nullable": row.try_get::<String, _>("is_nullable").map(|s| s == "YES").unwrap_or(true),
                    "default": row.try_get::<Option<String>, _>("column_default").unwrap_or(None),
                    "max_length": row.try_get::<Option<i32>, _>("character_maximum_length").unwrap_or(None),
                })
            })
            .collect(),
        Err(e) => {
            pool.close().await;
            return db_tool_failure(json!({"error": format!("errore descrizione colonne: {e}")}));
        }
    };

    if columns.is_empty() {
        pool.close().await;
        return db_tool_failure(json!({
            "error": format!("Tabella '{schema}.{table}' non trovata o senza colonne."),
            "hint": "Usa nexus_db_tables per vedere le tabelle disponibili."
        }));
    }

    let idx_rows = sqlx::query(
        r#"SELECT indexname, indexdef
           FROM pg_indexes
           WHERE schemaname = $1 AND tablename = $2
           ORDER BY indexname"#,
    )
    .bind(&schema)
    .bind(&table)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let indexes: Vec<Value> = idx_rows
        .iter()
        .map(|row| {
            json!({
                "name": row.try_get::<String, _>("indexname").unwrap_or_default(),
                "definition": row.try_get::<String, _>("indexdef").unwrap_or_default(),
            })
        })
        .collect();

    pool.close().await;
    json!({
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
    })
    .to_string()
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

    #[test]
    fn db_tool_failure_dichiara_il_fallimento_e_preserva_il_payload() {
        // Chiama il PRODUTTORE reale usato da tutti i rami di errore dei 3
        // tool `nexus_db_*`: senza il marker in testa questi fallimenti erano
        // indistinguibili da un risultato riuscito per anti-loop/supervisore/
        // final_gate (regola M).
        let out = db_tool_failure(json!({
            "error": "sql fallita",
            "sql_excerpt": "SELECT 1",
        }));
        assert!(nexus_types::tool_outcome::is_tool_failure(&out));
        assert!(out.contains("sql fallita"));
        assert!(out.contains("sql_excerpt"));
        // Il payload resta JSON valido dopo il marker: chi vuole ri-estrarlo
        // strutturalmente puo' farlo togliendo il solo prefisso.
        let after_marker = out
            .trim_start_matches(nexus_types::tool_outcome::TOOL_FAILURE_MARKER)
            .trim_start();
        let parsed: Value =
            serde_json::from_str(after_marker).expect("payload dopo il marker e' JSON valido");
        assert_eq!(parsed["error"], "sql fallita");
    }
}
