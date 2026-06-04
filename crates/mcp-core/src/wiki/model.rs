// ═══════════════════════════════════════════════════════════════════════════
// wiki/model.rs — Tipi Rust riflettenti lo schema unificato (mig 0295).
//
// Tutti i tipi qui usano `sqlx::FromRow` per mapping esplicito da query
// dinamiche `sqlx::query_as::<_, T>` (la regola del progetto e' niente macro
// compile-checked, vedi richiesta utente).
// ═══════════════════════════════════════════════════════════════════════════

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Scope di un documento wiki. La differenza meta vs project vive solo qui e
/// nel middleware ACL: niente codice scope-specifico altrove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiScope {
    Meta,
    Project,
}

impl WikiScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            WikiScope::Meta => "meta",
            WikiScope::Project => "project",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "meta" => Some(WikiScope::Meta),
            "project" => Some(WikiScope::Project),
            _ => None,
        }
    }
}

/// Stato del lock di modifica di un documento (auto vs protected vs frozen).
/// Mantiene compatibilita' semantica con `nexus_meta_docs.edit_lock` storico.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditLock {
    None,
    Protected,
    Frozen,
}

impl EditLock {
    pub fn as_str(&self) -> &'static str {
        match self {
            EditLock::None => "none",
            EditLock::Protected => "protected",
            EditLock::Frozen => "frozen",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "protected" => EditLock::Protected,
            "frozen" => EditLock::Frozen,
            _ => EditLock::None,
        }
    }
}

/// Riga della tabella `wiki_docs`. Riflette esattamente lo schema della mig 0295.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WikiDoc {
    pub id: Uuid,
    pub scope: String,
    pub project_id: Option<Uuid>,
    pub slug: String,
    pub title: String,
    pub body_md: String,
    pub body_hash: Option<String>,
    pub kind: String,
    pub intent: Option<String>,
    pub tags: Vec<String>,
    pub vault_file_path: Option<String>,
    pub qdrant_point_id: Option<String>,
    pub edit_lock: String,
    pub protected_sections: Vec<String>,
    pub manually_edited: bool,
    pub generated_hash: Option<String>,
    pub edited_hash: Option<String>,
    pub last_generated_at: Option<DateTime<Utc>>,
    pub last_edited_at: Option<DateTime<Utc>>,
    pub edited_by: Option<String>,
    pub current_version: i32,
    pub auto_generated: bool,
    pub public_read: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Riga della tabella `wiki_links` (grafo a livello di documenti).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WikiLink {
    pub from_doc_id: Uuid,
    pub to_doc_id: Uuid,
    pub rel_type: String,
    pub confidence: f32,
    pub created_by: String,
    pub evidence: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Riga della tabella `wiki_concept_triples` (knowledge graph reale).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WikiTriple {
    pub id: Uuid,
    pub subj_doc_id: Uuid,
    pub predicate: String,
    pub obj_doc_id: Option<Uuid>,
    pub obj_text: Option<String>,
    pub obj_external: Option<String>,
    pub source: String,
    pub confidence: f32,
    pub evidence: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Riga della tabella `wiki_doc_revisions` (versioning).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct WikiRevision {
    pub id: Uuid,
    pub doc_id: Uuid,
    pub version_no: i32,
    pub title: String,
    pub body_md: String,
    pub body_hash: String,
    pub tags: Vec<String>,
    pub source: String,
    pub author: Option<String>,
    pub edit_summary: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Patch parziale di un documento. `None` significa "lascia invariato",
/// `Some(v)` aggiorna. La protezione anti-overwrite (edit_lock=frozen) vive
/// in `storage::update_doc`.
#[derive(Debug, Default, Deserialize)]
pub struct WikiDocPatch {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub tags: Option<Vec<String>>,
    pub intent: Option<String>,
    /// Sorgente della revisione (default "manual"); usata dal restore per
    /// marcare la revisione come "revert".
    #[serde(default)]
    pub revision_source: Option<String>,
    /// Edit summary opzionale, viene scritto in `wiki_doc_revisions.edit_summary`.
    #[serde(default)]
    pub edit_summary: Option<String>,
}
