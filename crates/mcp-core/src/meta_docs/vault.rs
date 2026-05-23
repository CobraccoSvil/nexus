// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/vault.rs — Serializza/deserializza note del meta-vault
//
// Differenze rispetto a knowledge/vault.rs (per-progetto):
//   - Singleton: nessun project_id nel frontmatter
//   - Campi: kind, source_commit, source_files, auto_generated
//   - Body: piu' libero (non assume "Richiesta originale" sezione)
// ═══════════════════════════════════════════════════════════════════════════

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Slug Obsidian-compatibile (basename file `.md` senza estensione).
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        let mapped = match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => Some(ch.to_ascii_lowercase()),
            ' ' | '_' | '-' | '\t' => Some('-'),
            _ => None,
        };
        if let Some(c) = mapped {
            if c == '-' {
                if !prev_dash {
                    out.push(c);
                    prev_dash = true;
                }
            } else {
                out.push(c);
                prev_dash = false;
            }
        }
    }
    out.trim_matches('-').chars().take(80).collect()
}

/// SHA-256 hex del contenuto file.
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Rappresenta un link wikilink risolto nel body di una nota.
pub struct VaultMetaLink {
    pub target_slug: String,
    pub rel_type: String,
}

/// Serializza una nota meta-docs nel formato Markdown + frontmatter YAML.
#[allow(clippy::too_many_arguments)]
pub fn serialize_meta_doc(
    id: Uuid,
    kind: &str,
    title: &str,
    slug: &str,
    tags: &[String],
    source_commit: Option<&str>,
    source_files: &[String],
    auto_generated: bool,
    created_at: &DateTime<Utc>,
    updated_at: &DateTime<Utc>,
    body_md: &str,
    related: &[VaultMetaLink],
) -> String {
    let mut out = String::with_capacity(body_md.len() + 512);
    out.push_str("---\n");
    out.push_str(&format!("id: {id}\n"));
    out.push_str(&format!("kind: {kind}\n"));
    // Title con quote per YAML safety (se contiene `:` o caratteri speciali)
    let needs_quote = title.contains(':') || title.contains('#') || title.starts_with('-');
    if needs_quote {
        let escaped = title.replace('"', "\\\"");
        out.push_str(&format!("title: \"{escaped}\"\n"));
    } else {
        out.push_str(&format!("title: {title}\n"));
    }
    out.push_str(&format!("slug: {slug}\n"));
    if !tags.is_empty() {
        out.push_str("tags:\n");
        for tag in tags {
            out.push_str(&format!("  - {tag}\n"));
        }
    } else {
        out.push_str("tags: []\n");
    }
    if let Some(sha) = source_commit {
        out.push_str(&format!("source_commit: {sha}\n"));
    }
    if !source_files.is_empty() {
        out.push_str("source_files:\n");
        for fp in source_files {
            out.push_str(&format!("  - {fp}\n"));
        }
    }
    out.push_str(&format!("auto_generated: {auto_generated}\n"));
    out.push_str(&format!(
        "created_at: {}\n",
        created_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str(&format!(
        "updated_at: {}\n",
        updated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str("nexus_meta_version: 1\n");
    out.push_str("---\n\n");

    out.push_str(body_md);
    if !body_md.ends_with('\n') {
        out.push('\n');
    }

    if !related.is_empty() {
        out.push_str("\n## Note correlate\n\n");
        for link in related {
            out.push_str(&format!(
                "- [[{}]] _({})_\n",
                link.target_slug, link.rel_type
            ));
        }
    }

    out
}

/// Parsing del frontmatter YAML da un file vault. Ritorna `(frontmatter_json, body)`.
pub fn parse_frontmatter(content: &str) -> Option<(serde_json::Value, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---")?;
    let yaml_str = &after_first[..end_idx];
    let body = after_first[end_idx + 4..].trim_start_matches('\n').to_string();
    Some((parse_yaml_simple(yaml_str), body))
}

fn parse_yaml_simple(yaml: &str) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let mut map = Map::new();
    let mut current_array_key: Option<String> = None;
    let mut current_array: Vec<Value> = Vec::new();

    for line in yaml.lines() {
        let raw = line;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Array item (indented `- ...`)
        if raw.trim_start().starts_with("- ") {
            if current_array_key.is_some() {
                let val = trimmed.trim_start_matches("- ").trim().to_string();
                // Strip eventuali quote
                let val = val.trim_matches('"').trim_matches('\'').to_string();
                current_array.push(Value::String(val));
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
                current_array_key = Some(key);
                current_array.clear();
            } else if val_str == "[]" {
                map.insert(key, json!([]));
            } else if val_str == "true" {
                map.insert(key, json!(true));
            } else if val_str == "false" {
                map.insert(key, json!(false));
            } else {
                // Strip quote
                let v = val_str.trim_matches('"').trim_matches('\'').to_string();
                map.insert(key, Value::String(v));
            }
        }
    }
    if let Some(key) = current_array_key {
        map.insert(key, Value::Array(current_array));
    }
    Value::Object(map)
}

/// Estrae wikilink `[[slug]]` dal body markdown.
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("]]") {
            let raw = after[..end].trim();
            // Supporta sintassi [[slug|display]] di Obsidian
            let slug = raw.split('|').next().unwrap_or(raw).trim().to_string();
            if !slug.is_empty() && !links.contains(&slug) {
                links.push(slug);
            }
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    links
}

/// Costruisce il path file `.md` relativo al vault root, dato kind e slug.
/// Per `changelog` usa sotto-cartella anno: `changelog/YYYY/YYYY-MM-DD-<slug>.md`.
pub fn build_vault_path(kind: &str, slug: &str, date: &DateTime<Utc>) -> String {
    match kind {
        "changelog" => format!(
            "changelog/{}/{}-{}.md",
            date.format("%Y"),
            date.format("%Y-%m-%d"),
            slug
        ),
        "decision" => format!(
            "decisions/{}-{}.md",
            date.format("%Y-%m-%d"),
            slug
        ),
        "architecture" | "adr" | "api" | "schema" | "runbook" | "other" => {
            format!("{kind}/{slug}.md")
        }
        _ => format!("other/{slug}.md"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_kebab() {
        assert_eq!(slugify("Fix Bug Nullpointer"), "fix-bug-nullpointer");
        assert_eq!(slugify("Knowledge Base / Obsidian"), "knowledge-base-obsidian");
        assert_eq!(slugify("--leading dash---"), "leading-dash");
    }

    #[test]
    fn sha256_deterministic() {
        let a = sha256_hex("hello");
        let b = sha256_hex("hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn roundtrip_serialize_parse() {
        use chrono::TimeZone;
        let id = Uuid::nil();
        let dt = Utc.with_ymd_and_hms(2026, 5, 23, 10, 0, 0).unwrap();
        let serialized = serialize_meta_doc(
            id,
            "adr",
            "ADR test",
            "adr-test",
            &["x".to_string(), "y".to_string()],
            Some("abc123"),
            &["src/main.rs".to_string()],
            true,
            &dt,
            &dt,
            "Body content here.\n\nMore lines.",
            &[],
        );
        let (fm, body) = parse_frontmatter(&serialized).expect("parse frontmatter");
        assert_eq!(fm.get("kind").and_then(|v| v.as_str()), Some("adr"));
        assert_eq!(fm.get("slug").and_then(|v| v.as_str()), Some("adr-test"));
        assert_eq!(fm.get("auto_generated").and_then(|v| v.as_bool()), Some(true));
        assert!(body.starts_with("Body content here."));
    }

    #[test]
    fn extract_wikilinks_simple() {
        let body = "Vedi [[overview]] e anche [[crates-rust#section|crates Rust]].";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["overview".to_string(), "crates-rust#section".to_string()]);
    }

    #[test]
    fn build_vault_path_kinds() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap();
        assert_eq!(build_vault_path("adr", "test-slug", &dt), "adr/test-slug.md");
        assert_eq!(build_vault_path("changelog", "fix-bug", &dt), "changelog/2026/2026-05-23-fix-bug.md");
        assert_eq!(build_vault_path("decision", "k-meta", &dt), "decisions/2026-05-23-k-meta.md");
    }
}
