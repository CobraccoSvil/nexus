//! Contract test (PR-4 Livello 3): safety.rs blacklist patterns.
//!
//! mcp-core e' bin-only (no lib), quindi i pattern di safety non sono
//! importabili in integration tests. Verifichiamo invece i comportamenti
//! via API live `POST /api/internal/provider-error` + `POST /api/admin/...`
//! oppure direttamente via grep nel binario per accertarsi che i pattern
//! siano compilati. La test suite ricca unit-level vive in
//! `src/agent_tools/safety.rs` modulo `tests` (23 test, gia' verde).

mod support;

use std::process::Command;
use support::{salta, Motivo};

fn release_binary() -> std::path::PathBuf {
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target")
        .join("release")
        .join("mcp-core");
    workspace_root
}

#[test]
fn binary_contiene_tutti_i_pattern_safety_attesi() {
    let bin = release_binary();
    if !bin.exists() {
        eprintln!(
            "skip: binary {} non trovato (esegui 'cargo build -p mcp-core --release' prima)",
            bin.display()
        );
        return;
    }
    let out = Command::new("strings")
        .arg(&bin)
        .output()
        .expect("strings(1) non disponibile");
    let contents = String::from_utf8_lossy(&out.stdout);
    let attesi = [
        "db_access_nexus",
        "db_access_postgres",
        "db_default_target",
        "prisma_migrate_reset",
        "prisma_db_push_force",
        "sql_drop_database",
        "sql_drop_table_nexus",
        "sql_truncate_nexus",
        "sql_delete_nexus",
        "docker_exec_ideai",
        "docker_stop_ideai",
        "docker_compose_ideai",
        "docker_system_prune",
        "docker_stop_all",
        "fs_write_ideai",
        "fs_rm_rf_root",
        "kill_brain_mcp",
        "iptables_route",
        "systemctl_system",
        "database_url_nexus",
        "cat_env_nexus",
    ];
    for cat in attesi.iter() {
        assert!(
            contents.contains(cat),
            "pattern category '{}' assente nel binary release. Possibile regressione M63/M70.",
            cat
        );
    }
}

#[test]
fn binary_contiene_env_injection_db_progetto() {
    let bin = release_binary();
    if !bin.exists() {
        salta(Motivo::ArtefattoAssente(
            "binario release di mcp-core (cargo build --release)",
        ));
        return;
    }
    let out = Command::new("strings").arg(&bin).output().unwrap();
    let contents = String::from_utf8_lossy(&out.stdout);
    for sym in [
        "NEXUS_PROJECT_DB_URL",
        "NEXUS_PROJECT_DB_NAME",
        "ensure_project_db_url",
        "/bin/bash",
    ] {
        assert!(
            contents.contains(sym),
            "stringa '{}' assente: regressione M72/L6 (env injection DB progetto + bash brace expansion)",
            sym
        );
    }
}

#[test]
fn binary_contiene_tool_subagent_poll_resume() {
    let bin = release_binary();
    if !bin.exists() {
        salta(Motivo::ArtefattoAssente(
            "binario release di mcp-core (cargo build --release)",
        ));
        return;
    }
    let out = Command::new("strings").arg(&bin).output().unwrap();
    let contents = String::from_utf8_lossy(&out.stdout);
    for sym in [
        "nexus_subagent_poll",
        "nexus_subagent_resume",
        "dispatch_subagent",
        "nexus_todo_write",
    ] {
        assert!(
            contents.contains(sym),
            "tool '{}' assente nel binary: regressione PR-3",
            sym
        );
    }
}
