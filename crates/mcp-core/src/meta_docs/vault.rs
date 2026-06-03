// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/vault.rs — Serializza note del meta-vault (formato Markdown + YAML).
//
// Gli helper agnostici allo scope (slugify, sha256_hex, parse_frontmatter,
// extract_wikilinks, build_vault_path) vivono in `crate::docs_core::vault` e
// sono re-esportati qui per compatibilita' con i call site esistenti
// (`meta_docs::vault::slugify`, ecc.). Qui resta solo la serializzazione
// specifica del meta-vault.
//
// Differenze rispetto a knowledge/vault.rs (per-progetto):
//   - Singleton: nessun project_id nel frontmatter
//   - Campi: kind, source_commit, source_files, auto_generated
//   - Body: piu' libero (non assume "Richiesta originale" sezione)
// ═══════════════════════════════════════════════════════════════════════════

use chrono::{DateTime, Utc};
use uuid::Uuid;

pub use crate::docs_core::vault::{
    build_vault_path, extract_wikilinks, parse_frontmatter, sha256_hex, slugify,
};

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
            out.push_str(&format!("- [[{}]] _({})_\n", link.target_slug, link.rel_type));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
