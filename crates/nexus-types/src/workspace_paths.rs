//! Punto unico path-safety workspace (regola L / ADR 0026).
//!
//! `resolve_workspace_target` normalizza e valida un percorso relativo dentro
//! la root del progetto SENZA richiedere che il target esista (a differenza
//! di `resolve_relative_path` in mcp-core::projects, che canonicalizza).
//! L'errore e' neutro (niente axum): gli strati HTTP lo mappano su StatusCode
//! tramite l'adapter in mcp-core::projects::resolve_workspace_target.
//!
//! Estratto da mcp-core::projects (split 7.4) per permettere ai tool agente
//! in nexus-agent-tools (figma_tools) di delegare allo stesso punto unico.

use std::path::{Path, PathBuf};

/// Esito negativo della validazione di un target workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceTargetError {
    /// Percorso relativo vuoto dopo il trim.
    EmptyPath,
    /// Caratteri non validi nel percorso (NUL).
    InvalidChars,
    /// Il candidato non e' dentro la root del progetto.
    OutsideRoot,
}

impl WorkspaceTargetError {
    /// Messaggio utente (stesso testo storico dell'implementazione mcp-core).
    pub fn message(self) -> &'static str {
        match self {
            Self::EmptyPath => "Il percorso relativo e' obbligatorio",
            Self::InvalidChars => "Il percorso contiene caratteri non validi",
            Self::OutsideRoot => "Percorso non autorizzato",
        }
    }
}

/// True se `candidate` e' dentro `base` (prefix check sui componenti, nessun IO).
pub fn path_within(base: &Path, candidate: &Path) -> bool {
    candidate.starts_with(base)
}

/// Normalizza un percorso relativo (separatori `/`, niente prefissi `/`)
/// e lo risolve dentro `root`. Ritorna `(relativo pulito, path assoluto)`.
pub fn resolve_workspace_target(
    root: &Path,
    relative: &str,
) -> Result<(String, PathBuf), WorkspaceTargetError> {
    let clean = relative
        .trim()
        .trim_start_matches(['\\', '/'])
        .replace('\\', "/");
    if clean.is_empty() {
        return Err(WorkspaceTargetError::EmptyPath);
    }
    if clean.contains('\0') {
        return Err(WorkspaceTargetError::InvalidChars);
    }
    // Rifiuto esplicito dei componenti "..": `path_within` e' un prefix check
    // lessicale che NON risolve i "..", quindi "a/../../x" lo aggirerebbe.
    // Solo il componente intero ".." e' traversal; nomi che lo contengono
    // (es. "..gitignore", "a..b") sono legittimi.
    if clean.split('/').any(|c| c == "..") {
        return Err(WorkspaceTargetError::OutsideRoot);
    }

    let candidate = root.join(&clean);
    if !path_within(root, &candidate) {
        return Err(WorkspaceTargetError::OutsideRoot);
    }

    Ok((clean, candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percorso_valido_normalizza_separatori() {
        let root = Path::new("/srv/progetto");
        let (clean, abs) =
            resolve_workspace_target(root, "\\src\\app/main.ts").expect("path valido");
        assert_eq!(clean, "src/app/main.ts");
        assert_eq!(abs, root.join("src/app/main.ts"));
    }

    #[test]
    fn percorso_vuoto_rifiutato() {
        let root = Path::new("/srv/progetto");
        assert_eq!(
            resolve_workspace_target(root, "   "),
            Err(WorkspaceTargetError::EmptyPath)
        );
    }

    #[test]
    fn carattere_nul_rifiutato() {
        let root = Path::new("/srv/progetto");
        assert_eq!(
            resolve_workspace_target(root, "a\0b"),
            Err(WorkspaceTargetError::InvalidChars)
        );
    }

    #[test]
    fn path_within_prefix_check() {
        let base = Path::new("/srv/progetto");
        assert!(path_within(base, &base.join("src/x.ts")));
        assert!(!path_within(base, Path::new("/srv/altro/x.ts")));
    }

    #[test]
    fn traversal_doppio_dotdot_rifiutato() {
        // Regressione path traversal: "a/../../fuori.txt" produce un candidato
        // (root/a/../../fuori.txt) i cui primi componenti combaciano lessicalmente
        // con la root -> starts_with passa, ma il path RISOLVE fuori dalla root.
        // Senza il rifiuto esplicito dei componenti ".." la guardia era aggirabile.
        let root = Path::new("/srv/progetto");
        assert_eq!(
            resolve_workspace_target(root, "a/../../fuori.txt"),
            Err(WorkspaceTargetError::OutsideRoot)
        );
    }

    #[test]
    fn traversal_dotdot_iniziale_rifiutato() {
        let root = Path::new("/srv/progetto");
        assert_eq!(
            resolve_workspace_target(root, "../fuori.txt"),
            Err(WorkspaceTargetError::OutsideRoot)
        );
    }

    #[test]
    fn traversal_dotdot_backslash_rifiutato() {
        // I backslash vengono convertiti in "/" prima del controllo: anche la
        // variante Windows del traversal deve essere bloccata.
        let root = Path::new("/srv/progetto");
        assert_eq!(
            resolve_workspace_target(root, "..\\..\\fuori.txt"),
            Err(WorkspaceTargetError::OutsideRoot)
        );
    }

    #[test]
    fn dotdot_solo_in_mezzo_al_nome_e_consentito() {
        // ".." DEVE essere rifiutato solo come componente intero: un file il cui
        // nome contiene ".." (es. "..gitignore", "a..b") e' legittimo e non
        // produce traversal.
        let root = Path::new("/srv/progetto");
        let (clean, abs) =
            resolve_workspace_target(root, "..gitignore").expect("nome legittimo");
        assert_eq!(clean, "..gitignore");
        assert_eq!(abs, root.join("..gitignore"));

        let (clean, _) =
            resolve_workspace_target(root, "src/a..b/file.ts").expect("nome legittimo");
        assert_eq!(clean, "src/a..b/file.ts");
    }
}
