//! Registrazione handler dominio: build_deploy
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        build_artifact_age::BuildArtifactAgeTool, build_debug_size::BuildDebugSizeTool,
        build_incremental_dir::BuildIncrementalDirTool, build_lockfile_age::BuildLockfileAgeTool,
        build_log_tail::BuildLogTailTool, build_profile_list::BuildProfileListTool,
        build_project::BuildProjectTool, build_release_size::BuildReleaseSizeTool,
        build_rerun_checks::BuildRerunChecksTool, build_script_count::BuildScriptCountTool,
        build_target_list::BuildTargetListTool, build_workspace_check::BuildWorkspaceCheckTool,
        cargo_build::CargoBuildTool, cargo_build_artifact_check::CargoBuildArtifactCheckTool,
        cargo_clean::CargoCleanTool, cargo_clean_dry::CargoCleanDryTool, cargo_doc::CargoDocTool,
        cargo_env_overrides::CargoEnvOverridesTool, cargo_locate_project::CargoLocateProjectTool,
        cargo_pkgid::CargoPkgidTool, cargo_publish_dry::CargoPublishDryTool,
        cargo_run::CargoRunTool, cargo_targets_list::CargoTargetsListTool,
        cargo_workspace_members::CargoWorkspaceMembersTool,
        deploy_ansible_check::DeployAnsibleCheckTool, deploy_check::DeployCheckTool,
        deploy_compose_check::DeployComposeCheckTool,
        deploy_dockerfile_count::DeployDockerfileCountTool,
        deploy_env_files_count::DeployEnvFilesCountTool, deploy_helm_check::DeployHelmCheckTool,
        deploy_k8s_check::DeployK8sCheckTool, deploy_nginx_check::DeployNginxCheckTool,
        deploy_release_artifacts::DeployReleaseArtifactsTool,
        deploy_systemd_check::DeploySystemdCheckTool,
        deploy_terraform_check::DeployTerraformCheckTool, docker_build::DockerBuildTool,
        docker_compose_down::DockerComposeDownTool, docker_compose_up::DockerComposeUpTool,
        docker_logs::DockerLogsTool, docker_ps::DockerPsTool, docker_rm::DockerRmTool,
        docker_run::DockerRunTool, docker_stop::DockerStopTool, shell_exec::ShellExecTool,
    };

    // Build
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_build",
            NexusToolCategory::Build,
            "Run `cargo build --message-format=json` and parse diagnostics",
        ),
        Arc::new(CargoBuildTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_clean",
            NexusToolCategory::Build,
            "Run `cargo clean` to remove target directory",
        ),
        Arc::new(CargoCleanTool),
    );

    // Deployment (Fase 9C)
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_check",
            NexusToolCategory::Deployment,
            "Pre-deploy readiness audit (uncommitted, upstream, deploy files, env, lockfiles)",
        ),
        Arc::new(DeployCheckTool),
    );

    // Build (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "build_project",
            NexusToolCategory::Build,
            "Multi-stack build dispatcher (cargo / npm run build / make / python -m build)",
        ),
        Arc::new(BuildProjectTool),
    );

    // Build / Deploy (Fase 9Q, 21 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "build_target_list",
            NexusToolCategory::Build,
            "List subdirectories under target/",
        ),
        Arc::new(BuildTargetListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_artifact_age",
            NexusToolCategory::Build,
            "Newest mtime under target/release",
        ),
        Arc::new(BuildArtifactAgeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_release_size",
            NexusToolCategory::Build,
            "Sum binary sizes in target/release",
        ),
        Arc::new(BuildReleaseSizeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_debug_size",
            NexusToolCategory::Build,
            "Sum binary sizes in target/debug",
        ),
        Arc::new(BuildDebugSizeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_incremental_dir",
            NexusToolCategory::Build,
            "Check incremental compilation directory",
        ),
        Arc::new(BuildIncrementalDirTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_lockfile_age",
            NexusToolCategory::Build,
            "Mtime/size of Cargo.lock",
        ),
        Arc::new(BuildLockfileAgeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_log_tail",
            NexusToolCategory::Build,
            "Tail .rustc_info.json / fingerprint logs",
        ),
        Arc::new(BuildLogTailTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_rerun_checks",
            NexusToolCategory::Build,
            "Count cargo:rerun-if- directives",
        ),
        Arc::new(BuildRerunChecksTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_script_count",
            NexusToolCategory::Build,
            "Count build.rs files in workspace",
        ),
        Arc::new(BuildScriptCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_workspace_check",
            NexusToolCategory::Build,
            "`cargo check --workspace --quiet`",
        ),
        Arc::new(BuildWorkspaceCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "build_profile_list",
            NexusToolCategory::Build,
            "List [profile.*] sections in root Cargo.toml",
        ),
        Arc::new(BuildProfileListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_dockerfile_count",
            NexusToolCategory::Deployment,
            "Count Dockerfile* files",
        ),
        Arc::new(DeployDockerfileCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_compose_check",
            NexusToolCategory::Deployment,
            "Find docker-compose*.yml files",
        ),
        Arc::new(DeployComposeCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_k8s_check",
            NexusToolCategory::Deployment,
            "Find kubernetes manifests",
        ),
        Arc::new(DeployK8sCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_helm_check",
            NexusToolCategory::Deployment,
            "Find Chart.yaml/values.yaml files",
        ),
        Arc::new(DeployHelmCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_terraform_check",
            NexusToolCategory::Deployment,
            "Find *.tf and tfstate files",
        ),
        Arc::new(DeployTerraformCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_ansible_check",
            NexusToolCategory::Deployment,
            "Find ansible playbooks/configs",
        ),
        Arc::new(DeployAnsibleCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_systemd_check",
            NexusToolCategory::Deployment,
            "Find systemd unit files",
        ),
        Arc::new(DeploySystemdCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_nginx_check",
            NexusToolCategory::Deployment,
            "Find nginx*.conf files",
        ),
        Arc::new(DeployNginxCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_env_files_count",
            NexusToolCategory::Deployment,
            "Count .env / .envrc files",
        ),
        Arc::new(DeployEnvFilesCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deploy_release_artifacts",
            NexusToolCategory::Deployment,
            "List common release artifact paths",
        ),
        Arc::new(DeployReleaseArtifactsTool),
    );

    // Fase 5: Docker / Container
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_build",
            NexusToolCategory::Deployment,
            "Costruisce un'immagine Docker dal progetto con auto-label. Il Dockerfile deve trovarsi dentro la project_root.",
        ),
        Arc::new(DockerBuildTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_run",
            NexusToolCategory::Deployment,
            "Esegue un container Docker con label progetto. Vieta nomi 'ideai-*' (infrastruttura Nexus). Supporta porte, env, volumi.",
        ),
        Arc::new(DockerRunTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_ps",
            NexusToolCategory::Deployment,
            "Lista container del progetto corrente (filtro per label). Non espone container ideai-* ne' di altri progetti.",
        ),
        Arc::new(DockerPsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_logs",
            NexusToolCategory::Deployment,
            "Legge i log di un container del progetto. Verifica label prima dell'accesso. Supporta tail e timestamps.",
        ),
        Arc::new(DockerLogsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_stop",
            NexusToolCategory::Deployment,
            "Ferma un singolo container del progetto. Verifica label progetto PRIMA dello stop. Container ideai-* sempre rifiutati.",
        ),
        Arc::new(DockerStopTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_rm",
            NexusToolCategory::Deployment,
            "Rimuove un container fermo del progetto. Verifica label progetto. Container ideai-* sempre rifiutati.",
        ),
        Arc::new(DockerRmTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_compose_up",
            NexusToolCategory::Deployment,
            "Avvia servizi con docker compose. OBBLIGATORIO specificare il file compose (mai compose globali). Supporta build e servizi specifici.",
        ),
        Arc::new(DockerComposeUpTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "docker_compose_down",
            NexusToolCategory::Deployment,
            "Ferma e rimuove servizi compose del progetto. OBBLIGATORIO il file compose. Opzione per rimuovere volumi e immagini.",
        ),
        Arc::new(DockerComposeDownTool),
    );

    // Fase 9G: Cargo / Build batch (3)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_doc",
            NexusToolCategory::Documentation,
            "Run `cargo doc --no-deps` and count generated HTML pages",
        ),
        Arc::new(CargoDocTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_locate_project",
            NexusToolCategory::Build,
            "`cargo locate-project` (root + workspace manifest paths)",
        ),
        Arc::new(CargoLocateProjectTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_pkgid",
            NexusToolCategory::Build,
            "`cargo pkgid` resolved package URL (parsed name+version)",
        ),
        Arc::new(CargoPkgidTool),
    );

    // Build (Fase 9H)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_run",
            NexusToolCategory::Build,
            "`cargo run [--release] [--bin name]` execution wrapper",
        ),
        Arc::new(CargoRunTool),
    );
    c.register_with_handler(
    NexusToolSpec::new(
        "shell_exec",
        NexusToolCategory::Utility,
        "Esegui comandi shell arbitrari. Timeout default 300s (5 min). \
         Per Docker: usa 'docker compose -f <file> up -d' per avvio in background (ritorna subito); \
         'docker compose -f <file> up -d --build' se il codice e' cambiato; \
         'docker compose -f <file> logs --tail=80 <servizio>' per leggere i log; \
         'docker compose -f <file> ps' per verificare che i container siano Running. \
         Per build lunghe (>2 min) passa timeout_secs=600. \
         Non usare per operazioni gia coperte da tool specifici (cargo_build, git, ecc.).",
    ),
    Arc::new(ShellExecTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_publish_dry",
            NexusToolCategory::Build,
            "`cargo publish --dry-run --allow-dirty` rehearsal",
        ),
        Arc::new(CargoPublishDryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_targets_list",
            NexusToolCategory::Build,
            "List targets (bin/lib/example/test/bench) via `cargo metadata`",
        ),
        Arc::new(CargoTargetsListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_workspace_members",
            NexusToolCategory::Build,
            "List workspace members via `cargo metadata`",
        ),
        Arc::new(CargoWorkspaceMembersTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_env_overrides",
            NexusToolCategory::Build,
            "Read CARGO_*/RUSTFLAGS/RUSTDOCFLAGS env vars affecting builds",
        ),
        Arc::new(CargoEnvOverridesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_build_artifact_check",
            NexusToolCategory::Build,
            "List binaries in target/<profile>/ with sizes",
        ),
        Arc::new(CargoBuildArtifactCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_clean_dry",
            NexusToolCategory::Build,
            "Compute target/ directory size without removing anything",
        ),
        Arc::new(CargoCleanDryTool),
    );
}
