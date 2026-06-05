//! Helper condivisi per i tool Docker (regola L / ADR 0026, step S31).
//! Prima `validate_not_protected` + `verify_container_label` duplicati in
//! `docker_rm.rs` e `docker_stop.rs`.

use super::{exec, NexusToolError};

const PROTECTED_PREFIX: &str = "ideai-";

/// Vieta operazioni distruttive sui container infrastrutturali (prefix `ideai-`).
pub fn validate_not_protected(name: &str) -> Result<(), NexusToolError> {
    if name.starts_with(PROTECTED_PREFIX) {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' e' infrastruttura Nexus. VIETATO rimuoverlo.",
            name
        )));
    }
    Ok(())
}

/// Valida che il path compose sia dentro la project_root (no globali, no path
/// traversal). Punto unico (regola L, step S32): prima duplicato in
/// `docker_compose_down.rs` e `docker_compose_up.rs`.
pub fn validate_compose_path(
    root: &std::path::Path,
    compose_file: &str,
) -> Result<String, NexusToolError> {
    if compose_file.is_empty() {
        return Err(NexusToolError::BadInput(
            "Parametro 'compose_file' obbligatorio. Non e' permesso usare compose globali.".into(),
        ));
    }

    let full = root.join(compose_file);
    let canonical = full.canonicalize().map_err(|_| {
        NexusToolError::BadInput(format!("File compose '{}' non trovato", compose_file))
    })?;

    if !canonical.starts_with(root) {
        return Err(NexusToolError::BadInput(
            "File compose fuori dalla root del progetto. Path traversal non permesso.".into(),
        ));
    }

    Ok(canonical.to_string_lossy().to_string())
}

/// Verifica che `name` sia un container appartenente al `slug` del progetto
/// (label `com.docker.compose.project`). Rifiuta operazioni su container di
/// altri progetti.
pub async fn verify_container_label(
    name: &str,
    slug: &str,
    project_root: &std::path::Path,
) -> Result<(), NexusToolError> {
    let out = exec::run_cmd(
        "docker",
        &[
            "inspect",
            "--format",
            "{{index .Config.Labels \"com.docker.compose.project\"}}",
            name,
        ],
        project_root,
        10,
    )
    .await?;

    let container_slug = out.stdout.trim();
    if container_slug != slug {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' non appartiene al progetto corrente. Rimozione negata.",
            name
        )));
    }
    Ok(())
}
