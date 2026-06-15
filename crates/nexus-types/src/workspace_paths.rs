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

/// Normalizza `raw` in un percorso RELATIVO pulito confinato a `root`, SENZA IO.
/// PUNTO UNICO (regola L / ADR 0026) condiviso da lettura (`resolve_relative_path`)
/// e scrittura (`resolve_write_target`) in mcp-core.
///
/// Root cause storica: la de-duplicazione della root viveva SOLO nel resolver di
/// scrittura, percio' `read_file` falliva con "Percorso non trovato" sugli stessi
/// file che `edit_file` aveva appena scritto, quando l'LLM passava un path
/// contenente la root assoluta. Capita spesso: il prompt mostra all'agente la
/// `project_root` ASSOLUTA e i modelli deboli la ricopiano nel path del tool
/// (es. `home/administrator/projects/Foo/src/x.ts` come "relativo"). La scrittura
/// la strippava, la lettura la concatenava di nuovo -> path doppio inesistente.
///
/// Casi gestiti:
/// 1. relativo normale (`src/x.ts`)
/// 2. assoluto dentro la root (`/<root>/src/x.ts`) -> strip della root
/// 3. relativo che DUPLICA la root completa (`<root-senza-slash>/src/x.ts`) -> strip
/// 4. separatori `\` (stile Windows) normalizzati a `/`
/// 5. componente `..` -> `Err(OutsideRoot)`
/// 6. assoluto Unix fuori dalla root -> `Err(OutsideRoot)`
///
/// Ritorna `Ok("")` se `raw` rappresenta la root stessa (vuoto o == root): il
/// chiamante decide se e' lecito (lettura/list ammettono la root; scrittura no).
pub fn normalize_into_root(root: &Path, raw: &str) -> Result<String, WorkspaceTargetError> {
    let trimmed = raw.trim();
    if trimmed.contains('\0') {
        return Err(WorkspaceTargetError::InvalidChars);
    }
    // Assoluto Unix solo se inizia con '/' PRIMA di convertire i backslash:
    // "\src" e' un relativo in stile Windows, non un assoluto.
    let is_unix_absolute = trimmed.starts_with('/');
    let normalized = trimmed.replace('\\', "/");
    let normalized = normalized.trim_start_matches('/');
    if normalized.is_empty() {
        return Ok(String::new());
    }

    let root_str = root.to_string_lossy().replace('\\', "/");
    let root_str = root_str.trim_end_matches('/');
    // Forma "candidata assoluta" per riconoscere root assoluta o duplicata.
    let candidate_abs = format!("/{normalized}");

    let relative = if candidate_abs == root_str {
        // Il path E' esattamente la root.
        String::new()
    } else if let Some(rest) = candidate_abs.strip_prefix(&format!("{root_str}/")) {
        // (caso 2) assoluto dentro la root oppure (caso 3) relativo che duplica
        // la root: in entrambi usa solo il resto dopo la root.
        rest.to_string()
    } else if is_unix_absolute {
        // (caso 6) assoluto Unix vero ma fuori dalla root.
        return Err(WorkspaceTargetError::OutsideRoot);
    } else {
        // (caso 1) relativo normale che non duplica la root.
        normalized.to_string()
    };

    // Rifiuto esplicito dei componenti "..": `path_within` e' un prefix check
    // lessicale che NON risolve i "..", quindi "a/../../x" lo aggirerebbe.
    // Solo il componente intero ".." e' traversal; nomi che lo contengono
    // (es. "..gitignore", "a..b") sono legittimi.
    if relative.split('/').any(|c| c == "..") {
        return Err(WorkspaceTargetError::OutsideRoot);
    }
    // Confinamento finale (difesa in profondita').
    let candidate = root.join(&relative);
    if !path_within(root, &candidate) {
        return Err(WorkspaceTargetError::OutsideRoot);
    }
    Ok(relative)
}

/// Normalizza un percorso relativo e lo risolve dentro `root`, richiedendo un
/// target NON vuoto (la root nuda non e' un target valido per i suoi chiamanti
/// figma/HTTP). Ritorna `(relativo pulito, path assoluto)`. Delega la
/// normalizzazione (incl. de-duplicazione root) a [`normalize_into_root`].
pub fn resolve_workspace_target(
    root: &Path,
    relative: &str,
) -> Result<(String, PathBuf), WorkspaceTargetError> {
    let clean = normalize_into_root(root, relative)?;
    if clean.is_empty() {
        return Err(WorkspaceTargetError::EmptyPath);
    }
    let candidate = root.join(&clean);
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
    fn relativo_che_duplica_la_root_viene_strippato() {
        // Regressione "read_file fallisce sui file che edit_file ha scritto":
        // l'LLM ricopia la project_root assoluta nel path come se fosse relativo.
        // Senza strip, root.join(dup) produce un path doppio inesistente.
        let root = Path::new("/home/administrator/projects/Beauty-Book");
        let (clean, abs) = resolve_workspace_target(
            root,
            "home/administrator/projects/Beauty-Book/backend/src/app.ts",
        )
        .expect("dup-root deve essere strippato");
        assert_eq!(clean, "backend/src/app.ts");
        assert_eq!(abs, root.join("backend/src/app.ts"));
    }

    #[test]
    fn assoluto_dentro_la_root_viene_strippato() {
        let root = Path::new("/home/administrator/projects/Beauty-Book");
        let (clean, abs) = resolve_workspace_target(
            root,
            "/home/administrator/projects/Beauty-Book/src/app/types/Customer.ts",
        )
        .expect("assoluto dentro root deve essere strippato");
        assert_eq!(clean, "src/app/types/Customer.ts");
        assert_eq!(abs, root.join("src/app/types/Customer.ts"));
    }

    #[test]
    fn assoluto_fuori_dalla_root_rifiutato() {
        let root = Path::new("/home/administrator/projects/Beauty-Book");
        assert_eq!(
            resolve_workspace_target(root, "/etc/passwd"),
            Err(WorkspaceTargetError::OutsideRoot)
        );
    }

    #[test]
    fn normalize_into_root_root_nuda_e_stringa_vuota() {
        // normalize_into_root ammette la root nuda (Ok("")); e' resolve_workspace_target
        // a rifiutarla con EmptyPath per i suoi chiamanti.
        let root = Path::new("/srv/progetto");
        assert_eq!(normalize_into_root(root, ""), Ok(String::new()));
        assert_eq!(
            normalize_into_root(root, "/srv/progetto"),
            Ok(String::new())
        );
        assert_eq!(
            resolve_workspace_target(root, "/srv/progetto"),
            Err(WorkspaceTargetError::EmptyPath)
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
