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

/// Perche' la creazione di una directory non e' andata a buon fine.
///
/// Enum e non una stringa (regola Q): i due chiamanti — `mcp-core::settings` e
/// `admin-service::settings` — mappano ciascuno a un proprio messaggio HTTP, e
/// per farlo devono sapere QUALE caso e' occorso. Era l'unica differenza fra le
/// due copie di `create_directory` che il censimento delle firme ha trovato il
/// 2026-08-05: stessa sequenza, stessi controlli, messaggi diversi.
#[derive(Debug)]
pub enum ErroreCreaDirectory {
    /// Il parent non esiste o non e' canonicalizzabile.
    ParentNonRisolvibile,
    /// Il parent esiste ma non e' una directory.
    ParentNonDirectory,
    /// Il nome proposto non e' ammissibile; porta il motivo di
    /// [`validate_directory_name`], gia' pronto per l'utente.
    NomeNonValido(&'static str),
    /// Esiste gia' qualcosa con quel nome.
    GiaEsistente,
    /// La creazione e' fallita: il `kind` serve al chiamante per scegliere lo
    /// status (permessi -> 403, conflitto -> 409, resto -> 500).
    Io(std::io::Error),
}

/// Crea una directory sotto `parent_path`, validando il nome.
///
/// PURA rispetto all'HTTP: nessuno status, nessun `Json`. Ritorna il path
/// creato, oppure il caso di fallimento perche' sia il chiamante a nominarlo —
/// `nexus-types` non dipende da axum, e non deve.
///
/// L'ordine dei controlli e' quello delle due copie originali e va mantenuto:
/// il parent si risolve PRIMA di validare il nome, cosi' un percorso inesistente
/// non viene segnalato come nome sbagliato.
pub fn crea_directory(parent_path: &str, nome: &str) -> Result<PathBuf, ErroreCreaDirectory> {
    let parent = PathBuf::from(parent_path.trim())
        .canonicalize()
        .map_err(|_| ErroreCreaDirectory::ParentNonRisolvibile)?;
    if !parent.is_dir() {
        return Err(ErroreCreaDirectory::ParentNonDirectory);
    }
    let dir_name = validate_directory_name(nome).map_err(ErroreCreaDirectory::NomeNonValido)?;
    let target = parent.join(dir_name);
    if target.exists() {
        return Err(ErroreCreaDirectory::GiaEsistente);
    }
    std::fs::create_dir(&target).map_err(ErroreCreaDirectory::Io)?;
    Ok(target)
}

#[cfg(test)]
mod tests_crea {
    use super::*;

    #[test]
    fn il_parent_inesistente_non_diventa_un_errore_di_nome() {
        // L'ordine dei controlli conta: un percorso sbagliato deve dirsi tale,
        // non farsi passare per un nome invalido.
        let e = crea_directory("/percorso/che/non/esiste/mai", "x").unwrap_err();
        assert!(matches!(e, ErroreCreaDirectory::ParentNonRisolvibile));
    }

    #[test]
    fn crea_e_poi_segnala_il_conflitto() {
        let td = std::env::temp_dir().join(format!("nexus-fsb-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&td);
        let parent = td.to_string_lossy().to_string();
        let _ = std::fs::remove_dir_all(td.join("nuova"));

        let creata = crea_directory(&parent, "nuova").expect("prima creazione");
        assert!(creata.is_dir());

        let e = crea_directory(&parent, "nuova").unwrap_err();
        assert!(matches!(e, ErroreCreaDirectory::GiaEsistente));

        let _ = std::fs::remove_dir_all(&td);
    }

    #[test]
    fn il_nome_invalido_porta_con_se_il_motivo() {
        let td = std::env::temp_dir();
        let e = crea_directory(&td.to_string_lossy(), "..").unwrap_err();
        match e {
            ErroreCreaDirectory::NomeNonValido(m) => assert!(!m.is_empty()),
            altro => panic!("atteso NomeNonValido, trovato {altro:?}"),
        }
    }
}
