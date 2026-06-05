//! Helper condivisi per i tool Docker (regola L / ADR 0026, step S31).
//! Prima `validate_not_protected` + `verify_container_label` duplicati in
//! `docker_rm.rs` e `docker_stop.rs`.

use super::{exec, NexusToolContext, NexusToolError};

/// Estrae il parametro `container` dagli args (richiesto + trim + non-vuoto)
/// e calcola lo slug del progetto da `ctx.project_root.file_name`. Punto unico
/// (regola L, S79) per il pattern duplicato in `docker_rm`, `docker_logs`,
/// `docker_stop`. Il chiamante poi applica il proprio `validate_not_protected`
/// (con verbo specifico) e `verify_container_label*`.
pub fn extract_container_and_slug(
    ctx: &NexusToolContext,
    args: &serde_json::Value,
) -> Result<(String, String), NexusToolError> {
    let container = args
        .get("container")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| NexusToolError::BadInput("Parametro 'container' obbligatorio".into()))?
        .trim()
        .to_string();
    if container.is_empty() {
        return Err(NexusToolError::BadInput("Nome container vuoto".into()));
    }
    let slug = ctx
        .project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ctx.project_id.to_string());
    Ok((container, slug))
}

const PROTECTED_PREFIX: &str = "ideai-";

/// Vieta operazioni distruttive sui container infrastrutturali (prefix `ideai-`).
pub fn validate_not_protected(name: &str) -> Result<(), NexusToolError> {
    validate_not_protected_with_verb(name, "rimuoverlo")
}

/// Variante di `validate_not_protected` con verbo personalizzabile per il
/// messaggio errore (es. "fermarlo", "rimuoverlo").
pub fn validate_not_protected_with_verb(name: &str, verb: &str) -> Result<(), NexusToolError> {
    if name.starts_with(PROTECTED_PREFIX) {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' e' infrastruttura Nexus. VIETATO {}.",
            name, verb
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

/// Esegue `docker inspect --format '{{index .Config.Labels "com.docker.compose.project"}}'`
/// e ritorna il valore (trimmato) della label compose-project del container.
/// Punto unico (regola L, S79): prima la chiamata era duplicata in 3+ helper
/// `verify_container_label*`. I caller decidono il messaggio errore.
pub async fn fetch_container_compose_project(
    name: &str,
    project_root: &std::path::Path,
) -> Result<String, NexusToolError> {
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
    Ok(out.stdout.trim().to_string())
}

/// Verifica che `name` sia un container appartenente al `slug` del progetto
/// (label `com.docker.compose.project`). Rifiuta operazioni su container di
/// altri progetti, con `action` (es. "Stop negato.") concatenata al messaggio.
pub async fn verify_container_label_with_action(
    name: &str,
    slug: &str,
    project_root: &std::path::Path,
    action_msg: &str,
) -> Result<(), NexusToolError> {
    let container_slug = fetch_container_compose_project(name, project_root).await?;
    if container_slug != slug {
        return Err(NexusToolError::BadInput(format!(
            "Container '{}' non appartiene al progetto corrente. {}",
            name, action_msg
        )));
    }
    Ok(())
}

/// Variante "rimozione" usata dalle operazioni distruttive (docker_rm).
pub async fn verify_container_label(
    name: &str,
    slug: &str,
    project_root: &std::path::Path,
) -> Result<(), NexusToolError> {
    verify_container_label_with_action(name, slug, project_root, "Rimozione negata.").await
}
