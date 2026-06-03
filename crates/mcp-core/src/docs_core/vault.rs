// ═══════════════════════════════════════════════════════════════════════════
// docs_core/vault.rs — Helper Obsidian-compatible condivisi tra meta-vault e KB.
//
// Fusione delle due copie storiche (`meta_docs/vault.rs` e `knowledge/vault.rs`).
// Contiene solo funzioni pure agnostiche allo scope. Le funzioni di
// serializzazione specifiche (serialize_meta_doc / serialize_note) e i tipi link
// restano nei rispettivi moduli e riusano questi helper.
// ═══════════════════════════════════════════════════════════════════════════

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

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

/// SHA-256 hex del contenuto (usato anche per la loop-detection dei watcher).
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

/// Parsing YAML semplificato (senza dipendenza da serde_yaml).
///
/// Superset delle due implementazioni storiche: gestisce scalari stringa,
/// `[]`, booleani `true`/`false`, array indentati `- item` e strip delle quote
/// sia sugli scalari sia sugli item di array.
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
                let val = trimmed.trim_start_matches("- ").trim();
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
///
/// Supporta la sintassi Obsidian `[[slug|display]]` (mantiene solo `slug`) e
/// preserva eventuali ancore `[[slug#section]]`.
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("]]") {
            let raw = after[..end].trim();
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
        "decision" => format!("decisions/{}-{}.md", date.format("%Y-%m-%d"), slug),
        "architecture" | "adr" | "api" | "schema" | "runbook" | "other" => {
            format!("{kind}/{slug}.md")
        }
        "concept" => format!("concepts/{slug}.md"),
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
    fn extract_wikilinks_simple() {
        let body = "Vedi [[overview]] e anche [[crates-rust#section|crates Rust]].";
        let links = extract_wikilinks(body);
        assert_eq!(
            links,
            vec!["overview".to_string(), "crates-rust#section".to_string()]
        );
    }

    #[test]
    fn build_vault_path_kinds() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap();
        assert_eq!(build_vault_path("adr", "test-slug", &dt), "adr/test-slug.md");
        assert_eq!(
            build_vault_path("changelog", "fix-bug", &dt),
            "changelog/2026/2026-05-23-fix-bug.md"
        );
        assert_eq!(
            build_vault_path("decision", "k-meta", &dt),
            "decisions/2026-05-23-k-meta.md"
        );
    }

    #[test]
    fn parse_frontmatter_roundtrip() {
        let content = "---\nkind: adr\nslug: adr-test\nauto_generated: true\ntags:\n  - x\n  - y\n---\n\nBody content here.";
        let (fm, body) = parse_frontmatter(content).expect("parse frontmatter");
        assert_eq!(fm.get("kind").and_then(|v| v.as_str()), Some("adr"));
        assert_eq!(fm.get("slug").and_then(|v| v.as_str()), Some("adr-test"));
        assert_eq!(
            fm.get("auto_generated").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            fm.get("tags").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(2)
        );
        assert!(body.starts_with("Body content here."));
    }
}
