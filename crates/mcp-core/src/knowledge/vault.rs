// ═══════════════════════════════════════════════════════════════════════════
// knowledge/vault.rs — Serializzazione Markdown/YAML Obsidian-compatible
// ═══════════════════════════════════════════════════════════════════════════

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Rappresenta un link tra note nel formato vault.
pub struct VaultNoteLink {
    pub slug: String,
    pub rel_type: String,
    pub confidence: f32,
}

/// Serializza una nota nel formato Markdown con frontmatter YAML Obsidian-compatible.
#[allow(clippy::too_many_arguments)]
pub fn serialize_note(
    id: Uuid,
    project_id: Uuid,
    source_message_id: Option<Uuid>,
    source_run_id: Option<Uuid>,
    intent: Option<&str>,
    status: &str,
    tags: &[String],
    file_paths: &[String],
    created_at: &DateTime<Utc>,
    updated_at: &DateTime<Utc>,
    title: &str,
    body_md: &str,
    related_notes: &[VaultNoteLink],
) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("---\n");
    out.push_str(&format!("id: {id}\n"));
    out.push_str(&format!("project_id: {project_id}\n"));
    if let Some(mid) = source_message_id {
        out.push_str(&format!("source_message_id: {mid}\n"));
    }
    if let Some(rid) = source_run_id {
        out.push_str(&format!("source_run_id: {rid}\n"));
    }
    if let Some(intent) = intent {
        out.push_str(&format!("intent: {intent}\n"));
    }
    out.push_str(&format!("status: {status}\n"));
    // Tags in formato YAML array
    if !tags.is_empty() {
        out.push_str("tags:\n");
        for tag in tags {
            out.push_str(&format!("  - {tag}\n"));
        }
    } else {
        out.push_str("tags: []\n");
    }
    // File paths
    if !file_paths.is_empty() {
        out.push_str("file_paths:\n");
        for fp in file_paths {
            out.push_str(&format!("  - {fp}\n"));
        }
    }
    out.push_str(&format!(
        "created_at: {}\n",
        created_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str(&format!(
        "updated_at: {}\n",
        updated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str("nexus_version: 1\n");
    out.push_str("---\n\n");

    // Heading
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!(
        "> Intent: **{}** | Status: **{status}** | {}\n\n",
        intent.unwrap_or("unknown"),
        created_at.format("%Y-%m-%d"),
    ));

    // Body
    out.push_str("## Richiesta originale\n\n");
    out.push_str(body_md);
    out.push('\n');

    // Note correlate (wikilink Obsidian)
    if !related_notes.is_empty() {
        out.push_str("\n## Note correlate\n\n");
        for link in related_notes {
            out.push_str(&format!(
                "- [[{}]] _({}, conf {:.2})_\n",
                link.slug, link.rel_type, link.confidence
            ));
        }
    }

    out
}

/// Parsing base del frontmatter YAML da un file .md vault.
/// Ritorna (frontmatter_map, body_senza_frontmatter).
pub fn parse_frontmatter(content: &str) -> Option<(serde_json::Value, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---")?;
    let yaml_str = &after_first[..end_idx];
    let body = after_first[end_idx + 4..].trim_start_matches('\n').to_string();

    // Parsing YAML semplificato: usiamo serde_yaml se disponibile,
    // altrimenti un parsing manuale delle coppie key: value
    let fm = parse_yaml_simple(yaml_str);
    Some((fm, body))
}

/// Parsing YAML semplificato (senza dipendenza da serde_yaml).
fn parse_yaml_simple(yaml: &str) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let mut map = Map::new();
    let mut current_array_key: Option<String> = None;
    let mut current_array: Vec<Value> = Vec::new();

    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Array item
        if trimmed.starts_with("- ") {
            if let Some(ref key) = current_array_key {
                let val = trimmed.trim_start_matches("- ").trim().to_string();
                current_array.push(Value::String(val));
                let _ = key; // keep borrow checker happy
            }
            continue;
        }
        // Flush previous array
        if let Some(key) = current_array_key.take() {
            map.insert(key, Value::Array(current_array.clone()));
            current_array.clear();
        }
        // Key: value
        if let Some(colon_idx) = trimmed.find(':') {
            let key = trimmed[..colon_idx].trim().to_string();
            let val_str = trimmed[colon_idx + 1..].trim();
            if val_str.is_empty() {
                // Prossime righe saranno array items
                current_array_key = Some(key);
                current_array.clear();
            } else if val_str == "[]" {
                map.insert(key, json!([]));
            } else {
                map.insert(key, Value::String(val_str.to_string()));
            }
        }
    }
    // Flush finale
    if let Some(key) = current_array_key {
        map.insert(key, Value::Array(current_array));
    }

    Value::Object(map)
}

/// Estrai wikilinks [[...]] dal body markdown.
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("]]") {
            let link = after[..end].trim().to_string();
            if !link.is_empty() && !links.contains(&link) {
                links.push(link);
            }
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    links
}

/// Assicura che `.nexus/` sia presente nel .gitignore del progetto.
pub async fn ensure_gitignore_entry(repo_root: &str) {
    let gitignore_path = format!("{repo_root}/.gitignore");
    let content = tokio::fs::read_to_string(&gitignore_path)
        .await
        .unwrap_or_default();
    if !content.lines().any(|l| l.trim() == ".nexus/" || l.trim() == ".nexus") {
        let mut new_content = content;
        if !new_content.ends_with('\n') && !new_content.is_empty() {
            new_content.push('\n');
        }
        new_content.push_str("\n# Nexus Knowledge Base vault\n.nexus/\n");
        let _ = tokio::fs::write(&gitignore_path, new_content).await;
    }
}
