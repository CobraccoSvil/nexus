//! Helper filesystem condivisi (punto unico, regola L / ADR 0026).
//!
//! Prima `BrowseDirectoryNode`, `list_root_candidates`, `list_directories` e
//! `validate_directory_name` erano duplicati identici in
//! `crates/admin-service/src/settings.rs` e `crates/mcp-core/src/settings.rs`
//! (cluster ~81 righe del top dei cloni). Ora vivono qui, in `nexus-types`,
//! perche' sono primitive pure (solo `std`) usate da piu' di un crate.
//!
//! Niente axum/sqlx: errori semantici tornano come `Result<&str, &'static str>`,
//! e i call site axum li mappano al loro `ApiError` locale.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Singolo nodo directory restituito da `browse_directories`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseDirectoryNode {
    pub name: String,
    pub path: String,
    pub has_children: bool,
}

/// Lista dei root candidati del filesystem (drive letter su Windows,
/// `/` su Unix). Usata come fallback / lista iniziale dal browser admin.
pub fn list_root_candidates() -> Vec<PathBuf> {
    if cfg!(windows) {
        let mut roots = Vec::new();
        for letter in 'A'..='Z' {
            let candidate = PathBuf::from(format!("{letter}:\\"));
            if candidate.exists() {
                roots.push(candidate);
            }
        }
        if roots.is_empty() {
            roots.push(PathBuf::from("C:\\"));
        }
        roots
    } else {
        vec![PathBuf::from("/")]
    }
}

/// Elenca le sottocartelle di `target`, ordinate alfabeticamente per nome.
/// Ignora errori di lettura su singole entry (best-effort).
pub fn list_directories(target: &Path) -> Vec<BrowseDirectoryNode> {
    let mut directories = std::fs::read_dir(target)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(|entry| entry.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let has_children = std::fs::read_dir(&path)
                .ok()
                .map(|children| {
                    children
                        .filter_map(|c| c.ok())
                        .any(|c| c.metadata().map(|m| m.is_dir()).unwrap_or(false))
                })
                .unwrap_or(false);
            Some(BrowseDirectoryNode {
                name,
                path: path.to_string_lossy().to_string(),
                has_children,
            })
        })
        .collect::<Vec<_>>();
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    directories
}

/// Valida che `name` sia un nome di directory ammissibile (non vuoto, non `.`/`..`,
/// nessun separatore). Restituisce il nome trimmato in caso di successo, oppure
/// un messaggio d'errore che il chiamante puo' mappare a `400 Bad Request`.
pub fn validate_directory_name(name: &str) -> Result<&str, &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Il nome della directory e' obbligatorio");
    }
    if trimmed == "." || trimmed == ".." {
        return Err("Il nome della directory non e' valido");
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err("Il nome della directory non puo' contenere separatori");
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rifiuta_nomi_vuoti_e_punti() {
        assert!(validate_directory_name("").is_err());
        assert!(validate_directory_name("   ").is_err());
        assert!(validate_directory_name(".").is_err());
        assert!(validate_directory_name("..").is_err());
    }

    #[test]
    fn validate_rifiuta_separatori() {
        assert!(validate_directory_name("foo/bar").is_err());
        assert!(validate_directory_name("foo\\bar").is_err());
        assert!(validate_directory_name("foo\0bar").is_err());
    }

    #[test]
    fn validate_accetta_nome_valido_e_trimma() {
        assert_eq!(validate_directory_name("  hello ").unwrap(), "hello");
        assert_eq!(validate_directory_name("project-a").unwrap(), "project-a");
    }

    #[test]
    fn list_root_candidates_non_vuoto() {
        assert!(!list_root_candidates().is_empty());
    }
}
