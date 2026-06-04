//! Modelli dati condivisi del build graph (ADR 0020).
//!
//! `BuildGraphInfo` e' il risultato canonico di un resolver per linguaggio
//! e viene persistito in `nexus_project_build_graph`. `BuildGraphMembership`
//! e' il valore di ritorno della funzione di lookup runtime usata dal
//! preflight di write/edit per decidere se un file fa parte della build.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Mappa autoritativa del build graph di un progetto, derivata dai file di
/// configurazione (tsconfig.json, Cargo.toml, pyproject.toml, go.mod).
///
/// Tutti i pattern glob sono in stile "gitignore-like" (vedi `globset`).
/// Path relativi alla `repository_root_path` del progetto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildGraphInfo {
    pub project_id: Uuid,
    /// Linguaggio dominante riconosciuto: "typescript" | "rust" | "python" | "go" | "unknown".
    pub language: String,
    /// Pattern glob (rispetto alla root) che identificano i file inclusi nella build.
    pub include_globs: Vec<String>,
    /// Pattern glob da escludere (priorita' su `include_globs`).
    pub exclude_globs: Vec<String>,
    /// File entry point riconosciuti (es. `src/main.rs`, `src/index.ts`).
    pub entry_points: Vec<String>,
    /// Membri di un eventuale monorepo (es. `crates/*`, `apps/*`, package.json workspaces).
    pub monorepo_members: Vec<String>,
    /// Directory di output build che non vanno mai modificate manualmente
    /// (es. `target`, `node_modules`, `dist`).
    pub generated_dirs: Vec<String>,
    /// File di configurazione effettivamente letti per produrre questa mappa
    /// (path assoluti). Usati anche dal watcher per invalidare la cache.
    pub sources: Vec<String>,
    pub computed_at: DateTime<Utc>,
}

impl BuildGraphInfo {
    /// Crea un `BuildGraphInfo` "unknown": il linguaggio non e' riconosciuto.
    /// Il consumer (preflight) deve trattarlo come best-effort: nessun warning
    /// stretto, ma loggare ad info.
    pub fn unknown(project_id: Uuid) -> Self {
        Self {
            project_id,
            language: "unknown".to_string(),
            include_globs: vec!["**/*".to_string()],
            exclude_globs: vec![],
            entry_points: vec![],
            monorepo_members: vec![],
            generated_dirs: vec![],
            sources: vec![],
            computed_at: Utc::now(),
        }
    }
}

/// Risultato della query "questo file e' nel build graph?". Rappresentazione
/// strutturata per consentire al preflight di scegliere se bloccare, avvisare
/// o procedere silenziosamente.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildGraphMembership {
    /// File incluso esplicitamente nei pattern include della build.
    InGraph { reason: String },
    /// File non matcha alcun include glob (o matcha un exclude): non viene
    /// compilato/eseguito. Il preflight emette warning ma non blocca.
    OutOfGraph { reason: String },
    /// File riconosciuto come entry point della build (es. `src/main.rs`).
    Entrypoint { reason: String },
    /// File in directory generata dalla build (`target`, `node_modules`, ...).
    /// Il preflight blocca la scrittura.
    Generated { reason: String },
    /// Linguaggio non riconosciuto o config assente: il sistema non puo'
    /// determinare la membership. Best-effort, niente warning bloccanti.
    Unknown { reason: String },
}

impl BuildGraphMembership {
    /// Estrae la ragione testuale dal valore.
    pub fn reason(&self) -> &str {
        match self {
            Self::InGraph { reason }
            | Self::OutOfGraph { reason }
            | Self::Entrypoint { reason }
            | Self::Generated { reason }
            | Self::Unknown { reason } => reason,
        }
    }
}
