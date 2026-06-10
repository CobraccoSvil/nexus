// ═══════════════════════════════════════════════════════════════════════════
// wiki/vault.rs — Serializzazione Markdown/YAML e helper vault unificati.
//
// Sostituisce i tre moduli storici `meta_docs::vault`, `knowledge::vault` e
// `docs_core::vault`. Tutti gli helper agnostici allo scope (slugify,
// sha256_hex, parse_frontmatter, extract_wikilinks, build_vault_path) e gli
// helper per estrarre titolo/tag dal contenuto utente (title_from_content,
// extract_tags) vivono ora qui dentro. Inoltre:
//   - serializzazione del frontmatter wiki unificato (kind + scope-aware)
//   - risoluzione del vault root per scope (meta=docs/.nexus-vault,
//     project=<repository_root_path>/.nexus-vault/)
// ═══════════════════════════════════════════════════════════════════════════

use crate::wiki::model::WikiScope;
use crate::AppState;
use anyhow::{Context, Result};
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
    let body = after_first[end_idx + 4..]
        .trim_start_matches('\n')
        .to_string();
    Some((parse_yaml_simple(yaml_str), body))
}

/// Parsing YAML semplificato (senza dipendenza da serde_yaml).
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
        if raw.trim_start().starts_with("- ") {
            if current_array_key.is_some() {
                let val = trimmed.trim_start_matches("- ").trim();
                let val = val.trim_matches('"').trim_matches('\'').to_string();
                current_array.push(Value::String(val));
            }
            continue;
        }
        if let Some(key) = current_array_key.take() {
            map.insert(key, Value::Array(current_array.clone()));
            current_array.clear();
        }
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

/// Normalizza la prima riga del contenuto come titolo (max `max_len` char).
///
/// Estratto dal vecchio `knowledge::title_from_content` (F8 cleanup). Usato
/// dal pipeline allegati chat per dare un titolo a una nota auto-creata.
pub fn title_from_content(content: &str, max_len: usize) -> String {
    let first_line = content.lines().next().unwrap_or(content);
    let cleaned = first_line
        .trim_start_matches('#')
        .trim_start_matches('*')
        .trim();
    if cleaned.len() > max_len {
        format!("{}...", &cleaned[..max_len.min(cleaned.len())])
    } else if cleaned.is_empty() {
        "Nota senza titolo".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Estrai tag letterali `#xxx` dal contenuto.
///
/// Estratto dal vecchio `knowledge::extract_tags` (F8 cleanup).
pub fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for word in content.split_whitespace() {
        if word.starts_with('#') && word.len() > 1 {
            let tag = word
                .trim_start_matches('#')
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
            if !tag.is_empty() && tag.len() <= 50 {
                let t = tag.to_lowercase();
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
    }
    tags
}

/// Rappresenta un wikilink risolto nel body per la sezione "Note correlate".
pub struct VaultLink {
    pub target_slug: String,
    pub rel_type: String,
}

/// Serializza un documento wiki nel formato Markdown + frontmatter YAML.
///
/// Il frontmatter contiene esattamente i campi gestiti dalla tabella
/// `wiki_docs`. La forma e' Obsidian-compatibile.
#[allow(clippy::too_many_arguments)]
pub fn serialize_doc(
    id: Uuid,
    scope: WikiScope,
    project_id: Option<Uuid>,
    kind: &str,
    title: &str,
    slug: &str,
    tags: &[String],
    intent: Option<&str>,
    auto_generated: bool,
    created_at: &DateTime<Utc>,
    updated_at: &DateTime<Utc>,
    body_md: &str,
    related: &[VaultLink],
) -> String {
    let mut out = String::with_capacity(body_md.len() + 512);
    out.push_str("---\n");
    out.push_str(&format!("id: {id}\n"));
    out.push_str(&format!("scope: {}\n", scope.as_str()));
    if let Some(pid) = project_id {
        out.push_str(&format!("project_id: {pid}\n"));
    }
    out.push_str(&format!("kind: {kind}\n"));
    // Title con quote per YAML safety (se contiene `:` o caratteri speciali).
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
    if let Some(intent) = intent {
        out.push_str(&format!("intent: {intent}\n"));
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
    out.push_str("nexus_wiki_version: 1\n");
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

/// Vault root assoluto per uno specifico scope/progetto.
///
/// - `WikiScope::Meta` -> `<NEXUS_REPO_ROOT>/<settings.meta_docs.vault_path>`
///   (fallback `/home/administrator/ideai/docs/.nexus-vault` se settings vuoto).
/// - `WikiScope::Project` -> `<projects.repository_root_path>/.nexus-vault`
///   (fallback per sviluppo: `/tmp/nexus-vault-<project_id>`).
///
/// La funzione non fa side-effect su filesystem: i chiamanti che devono
/// scrivere il file vault gestiscono `create_dir_all` separatamente.
pub async fn vault_root_for_scope(
    state: &AppState,
    scope: WikiScope,
    project_id: Option<Uuid>,
) -> Result<String> {
    match scope {
        WikiScope::Meta => {
            // Stessa logica di `meta_docs::apply::resolve_vault_root`, replicata
            // qui per non dipendere dal modulo legacy.
            let vault_rel: String =
                sqlx::query_scalar("SELECT value FROM settings WHERE key = 'meta_docs.vault_path'")
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "docs/.nexus-vault".to_string());
            let repo_root = std::env::var("NEXUS_REPO_ROOT")
                .unwrap_or_else(|_| "/home/administrator/ideai".to_string());
            Ok(format!("{}/{}", repo_root.trim_end_matches('/'), vault_rel))
        }
        WikiScope::Project => {
            let pid = project_id
                .context("scope=project ma project_id assente in vault_root_for_scope")?;
            let root: Option<String> =
                sqlx::query_scalar("SELECT repository_root_path FROM projects WHERE id = $1")
                    .bind(pid)
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten();
            let base = root
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("/tmp/nexus-vault-{pid}"));
            Ok(format!("{}/.nexus-vault", base.trim_end_matches('/')))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_kebab() {
        assert_eq!(slugify("Fix Bug Nullpointer"), "fix-bug-nullpointer");
        assert_eq!(
            slugify("Knowledge Base / Obsidian"),
            "knowledge-base-obsidian"
        );
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
        assert_eq!(
            build_vault_path("adr", "test-slug", &dt),
            "adr/test-slug.md"
        );
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

    #[test]
    fn title_from_content_basic() {
        assert_eq!(title_from_content("# Titolo lungo", 80), "Titolo lungo");
        assert_eq!(title_from_content("", 80), "Nota senza titolo");
    }

    #[test]
    fn extract_tags_basic() {
        let tags = extract_tags("Hello #rust and #wiki, also #rust again");
        assert_eq!(tags, vec!["rust".to_string(), "wiki".to_string()]);
    }
}
