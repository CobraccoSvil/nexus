// ═══════════════════════════════════════════════════════════════════════════
// knowledge/vault.rs — Serializzazione Markdown/YAML Obsidian-compatible (KB).
//
// Gli helper agnostici allo scope (parse_frontmatter, extract_wikilinks) vivono
// in `crate::docs_core::vault` e sono re-esportati qui per compatibilita' con i
// call site esistenti (`knowledge::vault::parse_frontmatter`, ecc.). Qui resta
// la serializzazione specifica della Knowledge Base per-progetto.
// ═══════════════════════════════════════════════════════════════════════════

use chrono::{DateTime, Utc};
use uuid::Uuid;

pub use crate::docs_core::vault::{extract_wikilinks, parse_frontmatter};

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

/// Assicura che `.nexus/` sia presente nel .gitignore del progetto.
pub async fn ensure_gitignore_entry(repo_root: &str) {
    let gitignore_path = format!("{repo_root}/.gitignore");
    let content = tokio::fs::read_to_string(&gitignore_path)
        .await
        .unwrap_or_default();
    if !content
        .lines()
        .any(|l| l.trim() == ".nexus/" || l.trim() == ".nexus")
    {
        let mut new_content = content;
        if !new_content.ends_with('\n') && !new_content.is_empty() {
            new_content.push('\n');
        }
        new_content.push_str("\n# Nexus Knowledge Base vault\n.nexus/\n");
        let _ = tokio::fs::write(&gitignore_path, new_content).await;
    }
}
