// Sotto-moduli per dominio del workspace di progetto
pub mod allocate_port;
pub mod auto_bootstrap;
pub mod browser_check;
pub mod build_diagnostics;
pub mod changes;
pub mod compose_ports;
pub mod execute_cmd;
pub mod fs_events;
pub mod logs;
pub mod monitor_seed;
pub mod playwright_install;
pub mod port_recovery;
pub mod processes;
pub mod run_configs;
pub mod run_mode;
pub mod runtime_issues;
pub mod scan_ports;
pub mod service_discovery;
pub mod service_log_diagnose;
pub mod service_observer;
pub mod service_observer_remediation;
pub mod services;
pub mod sync_ports;
pub mod user_manager;
pub mod wizard;
pub mod workbench;

// Import condivisi usati da tutti i sotto-moduli tramite `use super::*`
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::nexus_gateway::{GwMessage, GwMetadata, GwRequest};
use crate::projects::{
    api_error, list_directory_nodes, load_project_context, load_projects_base_root,
    load_user_project_preferences, parse_user_id, refresh_git_snapshot,
    save_user_project_preferences, sign_terminal_token, terminal_session_secret, terminal_shell,
    upsert_open_session, TerminalSessionClaims, TerminalSessionResponse,
    WorkbenchStateUpdateRequest,
};
use crate::{auth::Claims, AppState};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

// Re-esportazioni pubbliche: mantengono la stessa interfaccia del file monolitico originale
pub use workbench::{
    create_terminal_session, get_workbench_state, open_project, update_workbench_state,
};

pub use changes::get_project_changes;

pub use allocate_port::allocate_port as allocate_project_port;
pub use allocate_port::find_or_allocate as find_or_allocate_port;
pub use allocate_port::kill_orphan_processes as kill_project_orphan_processes;
pub use allocate_port::kill_port_process as kill_project_port_process;

pub use execute_cmd::execute_command;

pub use services::{
    cleanup_project_ports, control_project_service, create_port_allocation, delete_port_allocation,
    get_port_allocations, get_project_ports, get_project_services_status,
    restart_all_project_services,
};

pub use logs::{
    clear_playwright_runs, get_output_channels, get_output_events, get_playwright_run_detail,
    get_playwright_runs, get_project_problems, serve_playwright_artifact, stream_playwright_run,
};

pub use wizard::{uninstall_project_service, wizard_detect_services, wizard_install_service};

pub use run_configs::{
    compute_run_config_suggestions, create_run_config, delete_run_config, detect_run_configs,
    get_run_configs, launch_run_config, save_suggestions_cache, update_run_config,
    CreateRunConfigBody,
};

pub use processes::{
    clear_finished_processes, get_sandbox_config_api, set_sandbox_config_api, stop_agent_process,
    stream_agent_process_logs,
};

// Funzioni pub(crate) usate da project_context.rs e run_configs.rs
pub(crate) use wizard::collect_compose_files;
pub(crate) use wizard::parse_compose_services;

/// Raccoglie ricorsivamente i file .spec.ts / .spec.js in una directory.
/// Condivisa tra wizard.rs (detect_playwright_suggestions) e run_configs.rs (detect_run_configs).
pub(super) fn walkdir_specs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                out.extend(walkdir_specs(&p));
            } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".spec.ts")
                    || name.ends_with(".spec.js")
                    || name.ends_with(".spec.mts")
                {
                    out.push(p);
                }
            }
        }
    }
    out
}

// Test del modulo originale (mantenuti qui per non spezzare la suite)
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn has_label_containing(suggestions: &[Value], fragment: &str) -> bool {
        suggestions.iter().any(|s| {
            s.get("label")
                .and_then(|l| l.as_str())
                .map(|l| l.contains(fragment))
                .unwrap_or(false)
        })
    }

    fn find_label_containing<'a>(suggestions: &'a [Value], fragment: &str) -> Option<&'a Value> {
        suggestions.iter().find(|s| {
            s.get("label")
                .and_then(|l| l.as_str())
                .map(|l| l.contains(fragment))
                .unwrap_or(false)
        })
    }

    #[test]
    fn detects_dev_compose_variant() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("docker-compose.dev.yml"),
            "services:\n  backend:\n    image: foo\n",
        )
        .unwrap();

        let suggestions = compute_run_config_suggestions(root);

        let up = find_label_containing(&suggestions, "docker-compose.dev.yml up backend")
            .expect("atteso suggerimento per il servizio backend del compose dev");
        assert_eq!(up.get("essential").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(up.get("role").and_then(|v| v.as_str()), Some("service"));
        assert_eq!(up.get("group").and_then(|v| v.as_str()), Some("docker"));
        assert_eq!(up.get("command").and_then(|v| v.as_str()), Some("docker"));
    }

    #[test]
    fn dotnet_run_demoted_when_dockerfile_dev_present() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("App.sln"), "").unwrap();
        let proj = root.join("Api");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("Api.csproj"),
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>",
        )
        .unwrap();
        fs::write(root.join("Dockerfile.dev"), "FROM scratch\n").unwrap();

        let suggestions = compute_run_config_suggestions(root);

        let dotnet = find_label_containing(&suggestions, "dotnet run")
            .expect("la voce dotnet run deve comunque esistere");
        assert_eq!(
            dotnet.get("essential").and_then(|v| v.as_bool()),
            Some(false),
            "dotnet run deve essere demosso in progetti containerizzati"
        );
        let group = dotnet.get("group").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            group.contains("richiede SDK locale"),
            "il group deve segnalare il prerequisito, era: {}",
            group
        );
    }

    #[test]
    fn make_target_with_docker_is_essential() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let makefile = "\
.PHONY: dev-backend lint\n\
\n\
dev-backend:\n\
\tdocker compose -f docker-compose.dev.yml up --build backend\n\
\n\
lint:\n\
\techo linting\n";
        fs::write(root.join("Makefile"), makefile).unwrap();

        let suggestions = compute_run_config_suggestions(root);

        let dev = find_label_containing(&suggestions, "make dev-backend")
            .expect("atteso suggerimento per il target make dev-backend");
        assert_eq!(dev.get("essential").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(dev.get("role").and_then(|v| v.as_str()), Some("service"));
        assert_eq!(dev.get("group").and_then(|v| v.as_str()), Some("docker"));

        // `make lint` non wrappa docker: deve restare non-essential e non nel group docker.
        let lint = find_label_containing(&suggestions, "make lint")
            .expect("atteso suggerimento per il target make lint");
        assert_eq!(lint.get("essential").and_then(|v| v.as_bool()), Some(false));
        assert_ne!(lint.get("group").and_then(|v| v.as_str()), Some("docker"));
    }

    #[test]
    fn compose_file_rank_ordering() {
        use std::path::PathBuf;
        assert_eq!(
            wizard::compose_file_rank(&PathBuf::from("docker-compose.dev.yml")),
            0
        );
        assert_eq!(
            wizard::compose_file_rank(&PathBuf::from("compose.dev.yaml")),
            0
        );
        assert_eq!(
            wizard::compose_file_rank(&PathBuf::from("docker-compose.local.yml")),
            1
        );
        assert_eq!(
            wizard::compose_file_rank(&PathBuf::from("docker-compose.yml")),
            2
        );
        assert_eq!(wizard::compose_file_rank(&PathBuf::from("compose.yaml")), 2);
        assert_eq!(
            wizard::compose_file_rank(&PathBuf::from("docker-compose.prod.yml")),
            3
        );
    }

    #[test]
    fn base_compose_demoted_when_dev_variant_exists() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("docker-compose.yml"),
            "services:\n  backend:\n    image: foo\n",
        )
        .unwrap();
        fs::write(
            root.join("docker-compose.dev.yml"),
            "services:\n  backend:\n    image: foo\n",
        )
        .unwrap();

        let suggestions = compute_run_config_suggestions(root);

        let dev_up = find_label_containing(&suggestions, "docker-compose.dev.yml up backend")
            .expect("atteso up backend dal compose dev");
        assert_eq!(
            dev_up.get("essential").and_then(|v| v.as_bool()),
            Some(true)
        );

        // Il bundle `up` del file base non deve essere essential quando c'è un dev.
        let base_up_bundle = suggestions
            .iter()
            .find(|s| {
                let label = s.get("label").and_then(|l| l.as_str()).unwrap_or("");
                label == "docker compose -f docker-compose.yml up"
            })
            .expect("atteso bundle up dal compose base");
        assert_eq!(
            base_up_bundle.get("essential").and_then(|v| v.as_bool()),
            Some(false)
        );

        assert!(has_label_containing(
            &suggestions,
            "docker-compose.yml up backend"
        ));
    }

    #[test]
    fn cleanup_ports_protegge_infrastruttura_nexus() {
        // Anti-suicidio (regola E): il reset porte non deve mai terminare
        // mcp-core (own_pid) ne' un listener su una porta riservata Nexus, anche
        // quando protected_pids (systemctl --user) e' vuoto come in WSL.
        let own = 4242;
        // mcp-core stesso (match per PID)
        assert!(services::is_protected_nexus_listener(own, 39555, own));
        // porte riservate Nexus: mcp-core 4000, admin 4010, gateway 4060, brain 50051
        assert!(services::is_protected_nexus_listener(99999, 4000, own));
        assert!(services::is_protected_nexus_listener(99999, 4010, own));
        assert!(services::is_protected_nexus_listener(99999, 4060, own));
        assert!(services::is_protected_nexus_listener(99999, 50051, own));
        // un dev-server di progetto nel bucket NON e' protetto -> resta killabile
        assert!(!services::is_protected_nexus_listener(99999, 39555, own));
        // PID 0/1 NON terminabili: `kill -TERM 0` colpirebbe il process group di
        // mcp-core (suicidio); i container Docker compaiono con pid 0 da `ss`.
        assert!(services::is_protected_nexus_listener(0, 39555, own));
        assert!(services::is_protected_nexus_listener(1, 39555, own));
    }
}
