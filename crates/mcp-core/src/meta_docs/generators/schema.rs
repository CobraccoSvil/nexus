// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators/schema.rs — Genera schema/postgres-tables.md,
// schema/migrations-log.md, schema/qdrant-collections.md
//
// Pattern: query DB introspection + filesystem read migrations.
// ═══════════════════════════════════════════════════════════════════════════

use super::{GeneratedDoc, MetaDocContext, MetaDocGenerator};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use sqlx::Row;

pub struct SchemaGenerator;

#[async_trait]
impl MetaDocGenerator for SchemaGenerator {
    fn name(&self) -> &'static str {
        "schema"
    }

    fn relevant_for(&self, files: &[String]) -> bool {
        // Solo se sono cambiate migrazioni o se viene richiesto refresh completo (files vuoto)
        files.is_empty() || files.iter().any(|f| f.starts_with("db/migrations/"))
    }

    async fn generate(&self, ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>> {
        let now = Utc::now();
        let mut docs = Vec::new();

        // 1. postgres-tables.md
        if let Ok(body) = generate_postgres_tables(ctx).await {
            docs.push(GeneratedDoc {
                kind: "schema".to_string(),
                title: "Schema Postgres".to_string(),
                slug: "postgres-tables".to_string(),
                body_md: body,
                tags: vec!["schema".to_string(), "postgres".to_string()],
                source_files: vec!["db/migrations/".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "schema/postgres-tables.md".to_string(),
                now,
            });
        }

        // 2. migrations-log.md
        if let Ok(body) = generate_migrations_log(ctx).await {
            docs.push(GeneratedDoc {
                kind: "schema".to_string(),
                title: "Log migrazioni Postgres".to_string(),
                slug: "migrations-log".to_string(),
                body_md: body,
                tags: vec!["schema".to_string(), "migrations".to_string()],
                source_files: vec!["db/migrations/".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "schema/migrations-log.md".to_string(),
                now,
            });
        }

        // 3. qdrant-collections.md
        if let Ok(body) = generate_qdrant_collections(ctx).await {
            docs.push(GeneratedDoc {
                kind: "schema".to_string(),
                title: "Collection Qdrant".to_string(),
                slug: "qdrant-collections".to_string(),
                body_md: body,
                tags: vec!["schema".to_string(), "qdrant".to_string()],
                source_files: vec!["crates/mcp-core/src/vector_memory.rs".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "schema/qdrant-collections.md".to_string(),
                now,
            });
        }

        Ok(docs)
    }
}

async fn generate_postgres_tables(ctx: &MetaDocContext<'_>) -> Result<String> {
    let rows = sqlx::query(
        r#"
        SELECT
            t.table_name,
            c.column_name,
            c.data_type,
            c.is_nullable,
            c.column_default
        FROM information_schema.tables t
        JOIN information_schema.columns c
            ON c.table_schema = t.table_schema AND c.table_name = t.table_name
        WHERE t.table_schema = 'public'
          AND t.table_type = 'BASE TABLE'
          AND t.table_name NOT LIKE '\_%' ESCAPE '\'
        ORDER BY t.table_name, c.ordinal_position
        "#,
    )
    .fetch_all(ctx.db)
    .await?;

    let mut out = String::with_capacity(8192);
    out.push_str("Tabelle attuali nello schema `public` di PostgreSQL. Generato automaticamente da `information_schema`.\n\n");

    let mut current_table = String::new();
    for row in &rows {
        let table: String = row.try_get("table_name")?;
        if table != current_table {
            if !current_table.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("## `{table}`\n\n"));
            out.push_str("| Colonna | Tipo | Nullable | Default |\n");
            out.push_str("|---|---|---|---|\n");
            current_table = table;
        }
        let column: String = row.try_get("column_name")?;
        let dtype: String = row.try_get("data_type")?;
        let nullable: String = row.try_get("is_nullable")?;
        let default: Option<String> = row.try_get("column_default").ok();
        let default_disp = default.unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "| `{column}` | {dtype} | {nullable} | `{default_disp}` |\n"
        ));
    }

    out.push_str("\n---\n\nFonte: query `SELECT FROM information_schema.tables JOIN columns`.\n");
    Ok(out)
}

async fn generate_migrations_log(ctx: &MetaDocContext<'_>) -> Result<String> {
    let migrations_dir = format!("{}/db/migrations", ctx.repo_root);
    let mut entries: Vec<(String, String)> = Vec::new();

    if let Ok(mut rd) = tokio::fs::read_dir(&migrations_dir).await {
        while let Ok(Some(ent)) = rd.next_entry().await {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.ends_with(".sql") {
                let content = tokio::fs::read_to_string(ent.path()).await.unwrap_or_default();
                // Estrae il primo commento `-- ...` come descrizione
                let first_comment = content
                    .lines()
                    .find(|l| l.trim_start().starts_with("--") && l.len() > 4)
                    .map(|l| {
                        l.trim_start_matches("--")
                            .trim_start_matches('-')
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_else(|| "(senza descrizione)".to_string());
                entries.push((name, first_comment));
            }
        }
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::with_capacity(4096);
    out.push_str("Cronologia migrazioni SQL in `db/migrations/`. Generato automaticamente.\n\n");
    out.push_str("| File | Descrizione |\n");
    out.push_str("|---|---|\n");
    for (fname, desc) in &entries {
        out.push_str(&format!("| `{fname}` | {desc} |\n"));
    }
    out.push_str(&format!(
        "\n**Totale**: {} migrazioni.\n\nUltima migrazione: `{}`.\n",
        entries.len(),
        entries.last().map(|e| e.0.as_str()).unwrap_or("(nessuna)")
    ));
    Ok(out)
}

async fn generate_qdrant_collections(ctx: &MetaDocContext<'_>) -> Result<String> {
    // Legge URL Qdrant dalle settings; default localhost:6333
    let qdrant_url: String = sqlx::query_scalar(
        "SELECT value FROM settings WHERE key = 'qdrant_url'"
    )
    .fetch_optional(ctx.db)
    .await?
    .unwrap_or_else(|| "http://localhost:6333".to_string());

    let client = reqwest::Client::new();
    let mut out = String::with_capacity(2048);
    out.push_str("Collection Qdrant attualmente create. Generato chiamando `GET /collections`.\n\n");

    match client
        .get(format!("{qdrant_url}/collections"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let payload: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
            let collections = payload
                .pointer("/result/collections")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            out.push_str("| Nome | Status |\n|---|---|\n");
            for c in collections {
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                out.push_str(&format!("| `{name}` | listed |\n"));
            }
        }
        Ok(resp) => {
            out.push_str(&format!("\n_Qdrant ha risposto {}: collection non disponibili._\n", resp.status()));
        }
        Err(e) => {
            out.push_str(&format!("\n_Errore di connessione a Qdrant: {e}._\n"));
        }
    }

    out.push_str("\n---\n\nVedi anche [[crates-rust#vector_memory]] per l'uso programmatico delle collection.\n");
    Ok(out)
}
