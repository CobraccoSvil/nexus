//! Risoluzione dei path di LETTURA dei tool agente.
//!
//! Estratto da `mcp-core::projects` (che resta il modulo axum degli handler
//! HTTP di progetto): la funzione non aveva nulla di specifico di quel modulo,
//! e i tool file estratti nel crate basso ne dipendono. `crate::projects`
//! la re-esporta, cosi' i call site storici restano validi e l'implementazione
//! resta UNA sola (regola L).
//!
//! Il tipo di errore e' `nexus_types::ApiError`, gia' il tipo di ritorno
//! storico: cambiare anche quello avrebbe mescolato uno spostamento con una
//! riscrittura dei 20+ call site.

use std::path::{Path, PathBuf};

use nexus_types::workspace_paths::path_within;
use nexus_types::{api_error, ApiError, StatusCode};

pub fn resolve_relative_path(root: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    use nexus_types::workspace_paths::{normalize_into_root, WorkspaceTargetError};

    // Normalizzazione/de-duplicazione nel PUNTO UNICO condiviso con la scrittura
    // (regola L): gestisce il caso in cui l'LLM passa un path che DUPLICA la
    // project_root (es. `home/administrator/projects/Foo/src/x.ts`) o la root
    // assoluta. Prima questo resolver di LETTURA non strippava la root, percio'
    // `read_file` falliva con "Percorso non trovato" sugli stessi file che
    // `edit_file` (resolve_write_target, che gia' de-duplicava) aveva scritto.
    let clean = normalize_into_root(root, relative).map_err(|e| {
        let status = match e {
            WorkspaceTargetError::OutsideRoot => StatusCode::FORBIDDEN,
            WorkspaceTargetError::EmptyPath | WorkspaceTargetError::InvalidChars => {
                StatusCode::BAD_REQUEST
            }
        };
        api_error(status, e.message())
    })?;

    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let target = if clean.is_empty() {
        root_canonical.clone()
    } else {
        if relative.trim() != clean {
            tracing::debug!(
                original = %relative.trim(),
                normalized = %clean,
                "resolve_relative_path: root duplicata/assoluta strippata dal path del tool"
            );
        }
        root_canonical.join(&clean)
    };

    let canonical = target
        .canonicalize()
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "Percorso non trovato"))?;

    if !path_within(&root_canonical, &canonical) {
        return Err(api_error(StatusCode::FORBIDDEN, "Percorso non autorizzato"));
    }

    Ok(canonical)
}
