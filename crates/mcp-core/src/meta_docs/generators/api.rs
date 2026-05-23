// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators/api.rs — Genera api/*.md
//
// Output:
//   - api/rest-endpoints.md (parsa axum router pattern `.route("/path", method(handler))`)
//   - api/settings-keys.md  (SELECT key, value, description FROM settings)
// ═══════════════════════════════════════════════════════════════════════════

use super::{GeneratedDoc, MetaDocContext, MetaDocGenerator};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;
use sqlx::Row;

pub struct ApiGenerator;

#[async_trait]
impl MetaDocGenerator for ApiGenerator {
    fn name(&self) -> &'static str {
        "api"
    }

    fn relevant_for(&self, files: &[String]) -> bool {
        files.is_empty()
            || files.iter().any(|f| {
                f.contains("router") || f.contains("routes")
                    || f.starts_with("crates/mcp-core/src/")
                    || f.ends_with("main.rs")
                    || f.starts_with("db/migrations/")  // settings cambiano via migrazioni
            })
    }

    async fn generate(&self, ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>> {
        let now = Utc::now();
        let mut docs = Vec::new();

        if let Ok(body) = generate_rest_endpoints(ctx).await {
            docs.push(GeneratedDoc {
                kind: "api".to_string(),
                title: "Endpoint REST".to_string(),
                slug: "rest-endpoints".to_string(),
                body_md: body,
                tags: vec!["api".to_string(), "rest".to_string()],
                source_files: vec!["crates/mcp-core/src/main.rs".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "api/rest-endpoints.md".to_string(),
                now,
            });
        }

        if let Ok(body) = generate_settings_keys(ctx).await {
            docs.push(GeneratedDoc {
                kind: "api".to_string(),
                title: "Settings keys".to_string(),
                slug: "settings-keys".to_string(),
                body_md: body,
                tags: vec!["api".to_string(), "settings".to_string()],
                source_files: vec!["db/migrations/".to_string()],
                source_commit: ctx.commit_sha.clone(),
                vault_file_path: "api/settings-keys.md".to_string(),
                now,
            });
        }

        Ok(docs)
    }
}

async fn generate_rest_endpoints(ctx: &MetaDocContext<'_>) -> Result<String> {
    // Scansiona crates/mcp-core/src/main.rs e file *router*.rs
    let main_path = format!("{}/crates/mcp-core/src/main.rs", ctx.repo_root);
    let main_content = tokio::fs::read_to_string(&main_path).await.unwrap_or_default();

    // Pattern: .route("PATH", METHOD(handler))
    // Supporta anche .route(PATH, METHOD(handler).patch(other))
    let re = Regex::new(r#"\.route\(\s*"([^"]+)"\s*,\s*([a-z_]+)\(([a-z_:]+)"#)
        .map_err(|e| anyhow::anyhow!("regex error: {e}"))?;

    let mut endpoints: Vec<(String, String, String)> = Vec::new();
    for caps in re.captures_iter(&main_content) {
        let path = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let method = caps.get(2).map(|m| m.as_str().to_uppercase()).unwrap_or_default();
        let handler = caps.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
        endpoints.push((method, path, handler));
    }
    endpoints.sort();

    let mut out = String::new();
    out.push_str("Endpoint REST esposti da mcp-core (axum). Generato parsando `crates/mcp-core/src/main.rs`.\n\n");
    out.push_str(&format!("**Totale endpoint**: {}\n\n", endpoints.len()));

    // Raggruppa per prefisso (/api/projects, /api/chat, ...)
    let mut groups: std::collections::BTreeMap<String, Vec<(String, String, String)>> =
        std::collections::BTreeMap::new();
    for (method, path, handler) in &endpoints {
        let group = if let Some(rest) = path.strip_prefix("/api/") {
            format!("/api/{}", rest.split('/').next().unwrap_or(""))
        } else if let Some(rest) = path.strip_prefix('/') {
            format!("/{}", rest.split('/').next().unwrap_or(""))
        } else {
            path.clone()
        };
        groups
            .entry(group)
            .or_default()
            .push((method.clone(), path.clone(), handler.clone()));
    }

    for (group, items) in &groups {
        out.push_str(&format!("\n## `{group}`\n\n"));
        out.push_str("| Metodo | Path | Handler |\n|---|---|---|\n");
        for (m, p, h) in items {
            out.push_str(&format!("| `{m}` | `{p}` | `{h}` |\n"));
        }
    }

    Ok(out)
}

async fn generate_settings_keys(ctx: &MetaDocContext<'_>) -> Result<String> {
    let rows = sqlx::query(
        "SELECT key, value, category, description FROM settings ORDER BY category, key",
    )
    .fetch_all(ctx.db)
    .await?;

    let mut out = String::new();
    out.push_str("Tutte le chiavi di configurazione di Nexus (tabella `settings`). Generato dal DB.\n\n");

    let mut current_cat = String::new();
    for r in &rows {
        let cat: String = r.try_get("category").unwrap_or_default();
        if cat != current_cat {
            out.push_str(&format!("\n## `{cat}`\n\n"));
            out.push_str("| Chiave | Valore default | Descrizione |\n|---|---|---|\n");
            current_cat = cat;
        }
        let key: String = r.try_get("key").unwrap_or_default();
        let value: String = r.try_get("value").unwrap_or_default();
        let desc: String = r.try_get("description").unwrap_or_default();
        // Tronca valori lunghi (chiavi tipo jwt_secret) per leggibilita'
        let value_disp = if value.len() > 60 {
            format!("{}...", &value[..57])
        } else {
            value
        };
        out.push_str(&format!("| `{key}` | `{value_disp}` | {desc} |\n"));
    }

    out.push_str(&format!("\n---\n\n**Totale chiavi**: {}\n", rows.len()));
    Ok(out)
}
