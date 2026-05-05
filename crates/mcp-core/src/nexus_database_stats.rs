/// Endpoint per statistiche del database Nexus interno.

use axum::{extract::State, Json};
use serde_json::json;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct TableStats {
    pub name: String,
    pub row_count: Option<i64>,
    pub size_kb: Option<i64>,
    pub last_updated: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct DatabaseStats {
    pub tables: Vec<TableStats>,
    pub stats: serde_json::Value,
}

/// Recupera statistiche del database Nexus — tutte le tabelle public.
pub async fn nexus_database_stats(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
) -> Result<Json<DatabaseStats>, String> {
    let pool = &state.db;

    // Recupera tutte le tabelle dello schema public con dimensione e riga stimata
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT
            t.tablename::text,
            COALESCE(pg_total_relation_size('public.'||quote_ident(t.tablename)), 0)::bigint AS size_bytes,
            COALESCE(s.n_live_tup, 0)::bigint AS live_rows
        FROM pg_tables t
        LEFT JOIN pg_stat_user_tables s ON s.relname = t.tablename
        WHERE t.schemaname = 'public'
        ORDER BY size_bytes DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut tables: Vec<TableStats> = Vec::with_capacity(rows.len());

    for (table_name, size_bytes, live_rows) in &rows {
        let row_count: Option<i64> = if *live_rows > 0 {
            Some(*live_rows)
        } else {
            // Fallback COUNT(*) solo per tabelle piccole (< 1 MB) per evitare scansioni costose
            if *size_bytes < 1_048_576 {
                let cnt: Option<(i64,)> =
                    sqlx::query_as(&format!("SELECT COUNT(*) FROM {table_name}"))
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten();
                cnt.map(|(n,)| n)
            } else {
                // Per tabelle grandi usa la stima di pg_stat
                Some(*live_rows)
            }
        };

        // Ultimo aggiornamento dalla tabella (solo se ha colonna updated_at / created_at)
        let last_updated: Option<String> = {
            let col_exists: Option<(i64,)> = sqlx::query_as(
                "SELECT COUNT(*) FROM information_schema.columns \
                 WHERE table_name=$1 AND column_name IN ('updated_at','created_at') \
                 AND table_schema='public'",
            )
            .bind(table_name)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

            if col_exists.map(|(n,)| n).unwrap_or(0) > 0 && row_count.unwrap_or(0) > 0 {
                let ts: Option<(Option<String>,)> = sqlx::query_as(&format!(
                    "SELECT MAX(COALESCE(updated_at, created_at, now()))::text \
                     FROM {table_name} LIMIT 1"
                ))
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
                ts.and_then(|(t,)| t)
            } else {
                None
            }
        };

        tables.push(TableStats {
            name: table_name.clone(),
            row_count,
            size_kb: Some(size_bytes / 1024),
            last_updated,
        });
    }

    let total_rows: i64 = tables.iter().filter_map(|t| t.row_count).sum();
    let total_size_kb: i64 = tables.iter().filter_map(|t| t.size_kb).sum();

    // Dimensione reale del database
    let db_size: Option<(i64,)> =
        sqlx::query_as("SELECT pg_database_size(current_database())::bigint")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let db_size_mb = db_size.map(|(s,)| s as f64 / 1_048_576.0).unwrap_or(0.0);

    let active_conn: Option<(i64,)> =
        sqlx::query_as("SELECT count(*)::bigint FROM pg_stat_activity WHERE datname = current_database()")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let active_connections = active_conn.map(|(n,)| n).unwrap_or(0);

    let stats = json!({
        "total_rows": total_rows,
        "database_size_mb": (db_size_mb * 10.0).round() / 10.0,
        "database_size_kb": total_size_kb,
        "active_connections": active_connections,
        "table_count": tables.len(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    Ok(Json(DatabaseStats { tables, stats }))
}
