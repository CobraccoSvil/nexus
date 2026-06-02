//! Nexus Builtin MCP Server
//!
//! Implementa un set di ~22 tool MCP eseguiti in-process (nessuna rete, nessun sottoprocesso).
//! Espone le funzionalità della piattaforma Nexus che non sono già presenti nei tool nativi
//! dell'agente (agent_tools.rs), come: gestione run config, git avanzato, profili, prompt
//! template e admin settings.
//!
//! Il server è registrato con UUID fisso `00000000-0000-0000-0000-000000000001` in `mcp_servers`
//! e i tool sono caricati da `mcp_server_tools` (upsertati in `seed_tools_and_server()`).

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Sotto-moduli per dominio
// ---------------------------------------------------------------------------

mod catalog;
mod run_config;
mod git;
mod project;
mod prompt_admin;
mod mcp_runtime;
mod docs;
mod services;

// Re-export pubblico delle costanti e della funzione server-id
pub use catalog::{nexus_builtin_server_id, NEXUS_BUILTIN_SERVER_ID_STR};

// Import privati usati dai sotto-moduli tramite `use super::*`
use catalog::NEXUS_TOOLS;
use run_config::{
    get_project_root, run_git,
    handle_run_config_list, handle_run_config_detect, handle_run_config_create,
    handle_run_config_update, handle_run_config_delete, handle_run_config_launch,
};
use git::{
    handle_git_log, handle_git_diff, handle_git_branches,
    handle_git_checkout, handle_git_create_branch,
};
use project::{
    handle_project_list, handle_project_analyze,
    handle_project_quality_scan, handle_project_quality_findings,
    handle_profile_list, handle_profile_delete, handle_profile_set_default,
};
use prompt_admin::{
    parse_uuid, format_json,
    handle_prompt_template_list, handle_prompt_template_update,
    handle_admin_setting_get, handle_admin_setting_update,
};
use mcp_runtime::{
    handle_mcp_tool_search, handle_mcp_tool_search_with_neural,
    handle_mcp_tool_call, handle_mcp_tool_reindex,
};
pub use mcp_runtime::index_tool;
use docs::{
    bump_version, get_project_slug,
    handle_doc_generate, handle_doc_update, handle_doc_list,
    handle_doc_search, handle_doc_status,
};
use services::{handle_service_status, handle_service_control};

// ---------------------------------------------------------------------------
// Seeding: upsert server row e tool definitions al startup
// ---------------------------------------------------------------------------

pub async fn seed_tools_and_server(db: &PgPool) {
    let server_id = nexus_builtin_server_id();

    // Upsert server row (idempotente — già in migration 0044, ma ridondante è sicuro)
    let _ = sqlx::query(
        "INSERT INTO mcp_servers (id, name, description, transport, enabled, scope)
         VALUES ($1, 'Nexus Builtin',
                 'Tool integrati della piattaforma Nexus: run config, profili, git avanzato, qualità, admin',
                 'builtin', true, 'global')
         ON CONFLICT (id) DO UPDATE SET enabled=true"
    )
    .bind(server_id)
    .execute(db)
    .await;

    // Upsert tool definitions nel cache DB
    for tool in NEXUS_TOOLS {
        let schema: Value = serde_json::from_str(tool.schema).unwrap_or(json!({"type":"object","properties":{}}));
        let _ = sqlx::query(
            "INSERT INTO mcp_server_tools (server_id, tool_name, description, input_schema, discovered_at)
             VALUES ($1, $2, $3, $4, NOW())
             ON CONFLICT (server_id, tool_name) DO UPDATE
             SET description=$3, input_schema=$4, discovered_at=NOW()"
        )
        .bind(server_id)
        .bind(tool.name)
        .bind(tool.description)
        .bind(schema)
        .execute(db)
        .await;
    }

    tracing::info!("Nexus Builtin MCP server: {} tool registrati", NEXUS_TOOLS.len());
}

/// Scatena il reindex semantico dei tool builtin (e di tutti i tool MCP abilitati)
/// in background. Da chiamare subito dopo `seed_tools_and_server()` al startup.
/// Il delay di 30s garantisce che il server sia completamente inizializzato prima
/// di aprire connessioni a Qdrant e all'embedder.
pub fn spawn_tool_reindex(db: PgPool, neural: crate::orchestrator::NeuralCoreClient) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let args = serde_json::json!({ "force": false });
        let result = handle_mcp_tool_reindex(&db, Some(&neural), &args).await;
        tracing::info!("tool_reindex background completato: {}", result);
    });
}

// ---------------------------------------------------------------------------
// Dispatch principale
// ---------------------------------------------------------------------------

pub async fn execute(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    user_role: &str,
    tool_name: &str,
    arguments: Value,
) -> String {
    match tool_name {
        // ── impact analysis (M13.2) ───────────────────────────────────
        "nexus_impact_brief" => handle_impact_brief(db, project_id, &arguments).await,
        // ── run_config ────────────────────────────────────────────────
        "nexus_run_config_list" => handle_run_config_list(db, &arguments).await,
        "nexus_run_config_detect" => handle_run_config_detect(db, &arguments).await,
        "nexus_run_config_create" => handle_run_config_create(db, &arguments).await,
        "nexus_run_config_update" => handle_run_config_update(db, &arguments).await,
        "nexus_run_config_delete" => handle_run_config_delete(db, &arguments).await,
        "nexus_run_config_launch" => handle_run_config_launch(db, &arguments).await,
        // ── git_advanced ──────────────────────────────────────────────
        "nexus_git_log" => handle_git_log(db, &arguments).await,
        "nexus_git_diff" => handle_git_diff(db, &arguments).await,
        "nexus_git_branches" => handle_git_branches(db, &arguments).await,
        "nexus_git_checkout" => handle_git_checkout(db, &arguments).await,
        "nexus_git_create_branch" => handle_git_create_branch(db, &arguments).await,
        // ── project ───────────────────────────────────────────────────
        "nexus_project_list" => handle_project_list(db, user_id).await,
        "nexus_project_analyze" => handle_project_analyze(db, &arguments).await,
        "nexus_project_quality_scan" => handle_project_quality_scan(db, &arguments).await,
        "nexus_project_quality_findings" => handle_project_quality_findings(db, &arguments).await,
        // ── profile ───────────────────────────────────────────────────
        "nexus_profile_list" => handle_profile_list(db, user_id).await,
        "nexus_profile_delete" => handle_profile_delete(db, user_id, &arguments).await,
        "nexus_profile_set_default" => handle_profile_set_default(db, user_id, &arguments).await,
        // ── prompt_template ───────────────────────────────────────────
        "nexus_prompt_template_list" => handle_prompt_template_list(db, &arguments).await,
        "nexus_prompt_template_update" => handle_prompt_template_update(db, &arguments).await,
        // ── mcp_runtime (discovery + call + reindex) ──────────────────
        "nexus_mcp_tool_search" => handle_mcp_tool_search(db, user_id, project_id, &arguments).await,
        "nexus_mcp_tool_call" => handle_mcp_tool_call(db, user_id, project_id, &arguments).await,
        "nexus_mcp_tool_reindex" => {
            if user_role != "admin" {
                return "[Accesso negato] nexus_mcp_tool_reindex richiede ruolo admin.".to_string();
            }
            handle_mcp_tool_reindex(db, None, &arguments).await
        }
        // ── admin_settings ────────────────────────────────────────────
        "nexus_admin_setting_get" => {
            if user_role != "admin" {
                return "[Accesso negato] nexus_admin_setting_get richiede ruolo admin.".to_string();
            }
            handle_admin_setting_get(db, &arguments).await
        }
        "nexus_admin_setting_update" => {
            if user_role != "admin" {
                return "[Accesso negato] nexus_admin_setting_update richiede ruolo admin.".to_string();
            }
            handle_admin_setting_update(db, &arguments).await
        }
        // ── documents ─────────────────────────────────────────────────
        "nexus_doc_generate" => handle_doc_generate(db, project_id, user_id, &arguments).await,
        "nexus_doc_update" => handle_doc_update(db, project_id, &arguments).await,
        "nexus_doc_list" => handle_doc_list(db, &arguments).await,
        "nexus_doc_search" => handle_doc_search(db, project_id, &arguments).await,
        "nexus_doc_status" => handle_doc_status(db, &arguments).await,
        // ── editor UI ────────────────────────────────────────────────
        "nexus_open_file_in_editor" => handle_open_file_in_editor(db, project_id, &arguments).await,
        // ── nexus_tool_catalog (Fase 9A) ──────────────────────────────
        // I tool eseguiti da NexusToolCatalog sono invocati tramite il
        // dispatcher qui sotto: si estrae lo short-name (senza prefisso
        // `nexus_`), si costruisce il NexusToolContext dal project_id e si
        // delega al catalog. Il vantaggio è che aggiungere un 21°, 22°,
        // ... handler richiede solo `register_with_handler` nel catalog e
        // un `ToolDef` nel NEXUS_TOOLS array — nessuna nuova branch di
        // match in questo file.
        "nexus_cargo_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_check", &arguments).await
        }
        "nexus_cargo_build" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_build", &arguments).await
        }
        "nexus_cargo_test" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_test", &arguments).await
        }
        "nexus_cargo_bench" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_bench", &arguments).await
        }
        "nexus_cargo_clean" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_clean", &arguments).await
        }
        "nexus_cargo_update" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_update", &arguments).await
        }
        "nexus_cargo_tree" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_tree", &arguments).await
        }
        "nexus_cargo_metadata" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_metadata", &arguments).await
        }
        "nexus_cargo_audit" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_audit", &arguments).await
        }
        "nexus_cargo_outdated" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_outdated", &arguments).await
        }
        "nexus_clippy_lint" => {
            dispatch_catalog_tool(db, user_id, project_id, "clippy_lint", &arguments).await
        }
        "nexus_rustc_version" => {
            dispatch_catalog_tool(db, user_id, project_id, "rustc_version", &arguments).await
        }
        "nexus_rustc_explain" => {
            dispatch_catalog_tool(db, user_id, project_id, "rustc_explain", &arguments).await
        }
        "nexus_test_coverage" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_coverage", &arguments).await
        }
        "nexus_secret_scan" => {
            dispatch_catalog_tool(db, user_id, project_id, "secret_scan", &arguments).await
        }
        "nexus_license_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "license_check", &arguments).await
        }
        "nexus_git_status" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_status", &arguments).await
        }
        // I nomi `_structured` evitano collisione con i vecchi handler
        // `nexus_git_log` / `nexus_git_diff` in-house (handle_git_log/diff)
        // che restano attivi per retrocompat.
        "nexus_git_log_structured" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_log", &arguments).await
        }
        "nexus_git_diff_structured" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_diff", &arguments).await
        }
        "nexus_git_blame" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_blame", &arguments).await
        }
        // ── Fase 9C ──────────────────────────────────────────────────
        "nexus_format_code" => {
            dispatch_catalog_tool(db, user_id, project_id, "format_code", &arguments).await
        }
        "nexus_deploy_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_check", &arguments).await
        }
        "nexus_gh_issue_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_issue_list", &arguments).await
        }
        "nexus_memory_ns_read" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_ns_read", &arguments).await
        }
        "nexus_memory_ns_write" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_ns_write", &arguments).await
        }
        "nexus_regex_match" => {
            dispatch_catalog_tool(db, user_id, project_id, "regex_match", &arguments).await
        }
        // ── Fase 9D ──────────────────────────────────────────────────
        "nexus_ast_parse" => {
            dispatch_catalog_tool(db, user_id, project_id, "ast_parse", &arguments).await
        }
        "nexus_ast_query" => {
            dispatch_catalog_tool(db, user_id, project_id, "ast_query", &arguments).await
        }
        "nexus_lint_run" => {
            dispatch_catalog_tool(db, user_id, project_id, "lint_run", &arguments).await
        }
        "nexus_test_generate" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_generate", &arguments).await
        }
        "nexus_coverage_report" => {
            dispatch_catalog_tool(db, user_id, project_id, "coverage_report", &arguments).await
        }
        "nexus_sast_scan" => {
            dispatch_catalog_tool(db, user_id, project_id, "sast_scan", &arguments).await
        }
        "nexus_deps_audit" => {
            dispatch_catalog_tool(db, user_id, project_id, "deps_audit", &arguments).await
        }
        "nexus_rename_symbol" => {
            dispatch_catalog_tool(db, user_id, project_id, "rename_symbol", &arguments).await
        }
        "nexus_extract_function" => {
            dispatch_catalog_tool(db, user_id, project_id, "extract_function", &arguments).await
        }
        "nexus_api_docs" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_docs", &arguments).await
        }
        "nexus_deps_tree" => {
            dispatch_catalog_tool(db, user_id, project_id, "deps_tree", &arguments).await
        }
        "nexus_build_project" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_project", &arguments).await
        }
        "nexus_gh_pr_create" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_create", &arguments).await
        }
        "nexus_profile_run" => {
            dispatch_catalog_tool(db, user_id, project_id, "profile_run", &arguments).await
        }
        "nexus_bench_run" => {
            dispatch_catalog_tool(db, user_id, project_id, "bench_run", &arguments).await
        }
        "nexus_db_schema_inspect" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_schema_inspect", &arguments).await
        }
        "nexus_db_query_explain" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_query_explain", &arguments).await
        }
        "nexus_openapi_validate" => {
            dispatch_catalog_tool(db, user_id, project_id, "openapi_validate", &arguments).await
        }
        "nexus_ruvector_insert" => {
            dispatch_catalog_tool(db, user_id, project_id, "ruvector_insert", &arguments).await
        }
        "nexus_ruvector_search" => {
            dispatch_catalog_tool(db, user_id, project_id, "ruvector_search", &arguments).await
        }
        "nexus_ruvector_stats" => {
            dispatch_catalog_tool(db, user_id, project_id, "ruvector_stats", &arguments).await
        }
        "nexus_consensus_vote" => {
            dispatch_catalog_tool(db, user_id, project_id, "consensus_vote", &arguments).await
        }
        // ── Fase 9F: Utility batch ──────────────────────────────────────
        "nexus_fs_read" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_read", &arguments).await
        }
        "nexus_fs_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_list", &arguments).await
        }
        "nexus_fs_grep" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_grep", &arguments).await
        }
        "nexus_fs_tree" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_tree", &arguments).await
        }
        "nexus_json_parse" => {
            dispatch_catalog_tool(db, user_id, project_id, "json_parse", &arguments).await
        }
        "nexus_json_get" => {
            dispatch_catalog_tool(db, user_id, project_id, "json_get", &arguments).await
        }
        "nexus_base64_encode" => {
            dispatch_catalog_tool(db, user_id, project_id, "base64_encode", &arguments).await
        }
        "nexus_base64_decode" => {
            dispatch_catalog_tool(db, user_id, project_id, "base64_decode", &arguments).await
        }
        "nexus_hash_content" => {
            dispatch_catalog_tool(db, user_id, project_id, "hash_content", &arguments).await
        }
        "nexus_uuid_generate" => {
            dispatch_catalog_tool(db, user_id, project_id, "uuid_generate", &arguments).await
        }
        // ── Fase 9F: VCS batch ──────────────────────────────────────────
        "nexus_git_branch_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_branch_list", &arguments).await
        }
        "nexus_git_remote_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_remote_list", &arguments).await
        }
        "nexus_git_show" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_show", &arguments).await
        }
        "nexus_git_tag_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_tag_list", &arguments).await
        }
        // ── Fase 9F: GitHub batch ───────────────────────────────────────
        "nexus_gh_workflow_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_workflow_list", &arguments).await
        }
        "nexus_gh_run_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_run_list", &arguments).await
        }
        "nexus_gh_release_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_release_list", &arguments).await
        }
        // ── Fase 9F: CodeAnalysis / Quality batch ───────────────────────
        "nexus_count_loc" => {
            dispatch_catalog_tool(db, user_id, project_id, "count_loc", &arguments).await
        }
        "nexus_find_todos" => {
            dispatch_catalog_tool(db, user_id, project_id, "find_todos", &arguments).await
        }
        "nexus_cargo_fmt_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_fmt_check", &arguments).await
        }
        // ── Fase 9G: Utility batch ──────────────────────────────────────
        "nexus_fs_write" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_write", &arguments).await
        }
        "nexus_fs_stat" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_stat", &arguments).await
        }
        "nexus_fs_glob" => {
            dispatch_catalog_tool(db, user_id, project_id, "fs_glob", &arguments).await
        }
        "nexus_env_get" => {
            dispatch_catalog_tool(db, user_id, project_id, "env_get", &arguments).await
        }
        "nexus_time_now" => {
            dispatch_catalog_tool(db, user_id, project_id, "time_now", &arguments).await
        }
        "nexus_regex_replace" => {
            dispatch_catalog_tool(db, user_id, project_id, "regex_replace", &arguments).await
        }
        "nexus_text_diff" => {
            dispatch_catalog_tool(db, user_id, project_id, "text_diff", &arguments).await
        }
        "nexus_uuid_parse" => {
            dispatch_catalog_tool(db, user_id, project_id, "uuid_parse", &arguments).await
        }
        // ── Fase 9G: VCS batch ──────────────────────────────────────────
        "nexus_git_stash_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_stash_list", &arguments).await
        }
        "nexus_git_grep" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_grep", &arguments).await
        }
        "nexus_git_describe" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_describe", &arguments).await
        }
        "nexus_git_shortlog" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_shortlog", &arguments).await
        }
        // ── Fase 9G: GitHub batch ───────────────────────────────────────
        "nexus_gh_pr_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_list", &arguments).await
        }
        "nexus_gh_pr_view" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_view", &arguments).await
        }
        "nexus_gh_repo_view" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_repo_view", &arguments).await
        }
        // ── Fase 9G: Cargo / Build batch ────────────────────────────────
        "nexus_cargo_doc" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_doc", &arguments).await
        }
        "nexus_cargo_locate_project" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_locate_project", &arguments)
                .await
        }
        "nexus_cargo_pkgid" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_pkgid", &arguments).await
        }
        // ── Fase 9G: CodeAnalysis batch ─────────────────────────────────
        "nexus_find_unsafe" => {
            dispatch_catalog_tool(db, user_id, project_id, "find_unsafe", &arguments).await
        }
        "nexus_find_pubapi" => {
            dispatch_catalog_tool(db, user_id, project_id, "find_pubapi", &arguments).await
        }
        // ── Fase 9H: Cargo extras (20) ──────────────────────────────────
        "nexus_cargo_run" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_run", &arguments).await
        }
        "nexus_cargo_install_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_install_list", &arguments).await
        }
        "nexus_cargo_search" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_search", &arguments).await
        }
        "nexus_cargo_publish_dry" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_publish_dry", &arguments).await
        }
        "nexus_cargo_check_release" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_check_release", &arguments).await
        }
        "nexus_cargo_check_all_features" => {
            dispatch_catalog_tool(
                db,
                user_id,
                project_id,
                "cargo_check_all_features",
                &arguments,
            )
            .await
        }
        "nexus_cargo_test_doc" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_test_doc", &arguments).await
        }
        "nexus_cargo_test_lib" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_test_lib", &arguments).await
        }
        "nexus_cargo_features_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_features_list", &arguments).await
        }
        "nexus_cargo_targets_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_targets_list", &arguments).await
        }
        "nexus_cargo_workspace_members" => {
            dispatch_catalog_tool(
                db,
                user_id,
                project_id,
                "cargo_workspace_members",
                &arguments,
            )
            .await
        }
        "nexus_cargo_dep_versions" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_dep_versions", &arguments).await
        }
        "nexus_cargo_lockfile_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_lockfile_check", &arguments)
                .await
        }
        "nexus_cargo_msrv_detect" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_msrv_detect", &arguments).await
        }
        "nexus_cargo_edition_detect" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_edition_detect", &arguments)
                .await
        }
        "nexus_cargo_env_overrides" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_env_overrides", &arguments).await
        }
        "nexus_cargo_build_artifact_check" => {
            dispatch_catalog_tool(
                db,
                user_id,
                project_id,
                "cargo_build_artifact_check",
                &arguments,
            )
            .await
        }
        "nexus_cargo_clean_dry" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_clean_dry", &arguments).await
        }
        "nexus_cargo_size_estimate" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_size_estimate", &arguments).await
        }
        "nexus_cargo_doc_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "cargo_doc_check", &arguments).await
        }
        // ── Fase 9I: Git extras (20) ────────────────────────────────────
        "nexus_git_rev_parse" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_rev_parse", &arguments).await
        }
        "nexus_git_count_objects" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_count_objects", &arguments).await
        }
        "nexus_git_reflog" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_reflog", &arguments).await
        }
        "nexus_git_clean_dry" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_clean_dry", &arguments).await
        }
        "nexus_git_check_ignore" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_check_ignore", &arguments).await
        }
        "nexus_git_ls_files" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_ls_files", &arguments).await
        }
        "nexus_git_ls_tree" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_ls_tree", &arguments).await
        }
        "nexus_git_cat_file" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_cat_file", &arguments).await
        }
        "nexus_git_for_each_ref" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_for_each_ref", &arguments).await
        }
        "nexus_git_merge_base" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_merge_base", &arguments).await
        }
        "nexus_git_diff_stat" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_diff_stat", &arguments).await
        }
        "nexus_git_log_graph" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_log_graph", &arguments).await
        }
        "nexus_git_show_branch" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_show_branch", &arguments).await
        }
        "nexus_git_archive_dry" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_archive_dry", &arguments).await
        }
        "nexus_git_bundle_verify" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_bundle_verify", &arguments).await
        }
        "nexus_git_fsck" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_fsck", &arguments).await
        }
        "nexus_git_gc_dry" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_gc_dry", &arguments).await
        }
        "nexus_git_config_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_config_list", &arguments).await
        }
        "nexus_git_worktree_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_worktree_list", &arguments).await
        }
        "nexus_git_submodule_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "git_submodule_list", &arguments).await
        }
        // ── Fase 9J: GitHub extras (20) ─────────────────────────────────
        "nexus_gh_issue_view" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_issue_view", &arguments).await
        }
        "nexus_gh_issue_create" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_issue_create", &arguments).await
        }
        "nexus_gh_issue_close" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_issue_close", &arguments).await
        }
        "nexus_gh_issue_comment" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_issue_comment", &arguments).await
        }
        "nexus_gh_pr_close" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_close", &arguments).await
        }
        "nexus_gh_pr_merge" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_merge", &arguments).await
        }
        "nexus_gh_pr_review" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_review", &arguments).await
        }
        "nexus_gh_pr_diff" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_diff", &arguments).await
        }
        "nexus_gh_pr_checks" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_checks", &arguments).await
        }
        "nexus_gh_pr_files" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_pr_files", &arguments).await
        }
        "nexus_gh_workflow_view" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_workflow_view", &arguments).await
        }
        "nexus_gh_workflow_run" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_workflow_run", &arguments).await
        }
        "nexus_gh_run_view" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_run_view", &arguments).await
        }
        "nexus_gh_run_logs" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_run_logs", &arguments).await
        }
        "nexus_gh_run_cancel" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_run_cancel", &arguments).await
        }
        "nexus_gh_release_view" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_release_view", &arguments).await
        }
        "nexus_gh_release_create" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_release_create", &arguments).await
        }
        "nexus_gh_repo_clone_url" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_repo_clone_url", &arguments).await
        }
        "nexus_gh_repo_fork_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_repo_fork_list", &arguments).await
        }
        "nexus_gh_label_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "gh_label_list", &arguments).await
        }

        // ── Fase 9K: Database extras (20) ───────────────────────────────
        "nexus_db_ping" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_ping", &arguments).await
        }
        "nexus_db_table_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_table_list", &arguments).await
        }
        "nexus_db_table_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_table_count", &arguments).await
        }
        "nexus_db_index_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_index_list", &arguments).await
        }
        "nexus_db_view_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_view_list", &arguments).await
        }
        "nexus_db_role_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_role_list", &arguments).await
        }
        "nexus_db_extension_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_extension_list", &arguments).await
        }
        "nexus_db_size" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_size", &arguments).await
        }
        "nexus_db_connection_info" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_connection_info", &arguments).await
        }
        "nexus_db_migration_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_migration_list", &arguments).await
        }
        "nexus_db_seq_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_seq_list", &arguments).await
        }
        "nexus_db_foreign_keys" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_foreign_keys", &arguments).await
        }
        "nexus_db_unused_indexes" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_unused_indexes", &arguments).await
        }
        "nexus_db_dead_tuples" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_dead_tuples", &arguments).await
        }
        "nexus_db_bloat_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_bloat_check", &arguments).await
        }
        "nexus_db_table_size" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_table_size", &arguments).await
        }
        "nexus_db_constraint_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_constraint_list", &arguments).await
        }
        "nexus_db_lock_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_lock_list", &arguments).await
        }
        "nexus_db_active_queries" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_active_queries", &arguments).await
        }
        "nexus_db_replication_status" => {
            dispatch_catalog_tool(db, user_id, project_id, "db_replication_status", &arguments).await
        }

        // ── Fase 9L: Documentation extras (20) ──────────────────────────
        "nexus_doc_readme_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_readme_check", &arguments).await
        }
        "nexus_doc_changelog_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_changelog_check", &arguments).await
        }
        "nexus_doc_license_detect" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_license_detect", &arguments).await
        }
        "nexus_doc_codeowners_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_codeowners_check", &arguments).await
        }
        "nexus_doc_contributing_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_contributing_check", &arguments).await
        }
        "nexus_doc_security_md_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_security_md_check", &arguments).await
        }
        "nexus_doc_toc_extract" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_toc_extract", &arguments).await
        }
        "nexus_doc_links_extract" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_links_extract", &arguments).await
        }
        "nexus_doc_word_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_word_count", &arguments).await
        }
        "nexus_doc_link_check_local" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_link_check_local", &arguments).await
        }
        "nexus_doc_image_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_image_list", &arguments).await
        }
        "nexus_doc_frontmatter_parse" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_frontmatter_parse", &arguments).await
        }
        "nexus_doc_md_lint" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_md_lint", &arguments).await
        }
        "nexus_doc_orphan_md" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_orphan_md", &arguments).await
        }
        "nexus_doc_size_report" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_size_report", &arguments).await
        }
        "nexus_doc_heading_depth" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_heading_depth", &arguments).await
        }
        "nexus_doc_codeblocks_extract" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_codeblocks_extract", &arguments).await
        }
        "nexus_doc_codeblocks_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_codeblocks_count", &arguments).await
        }
        "nexus_doc_api_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_api_list", &arguments).await
        }
        "nexus_doc_examples_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "doc_examples_list", &arguments).await
        }

        // ── Fase 9M: Performance extras (20) ────────────────────────────
        "nexus_perf_cargo_build_time" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_cargo_build_time", &arguments).await
        }
        "nexus_perf_binary_size" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_binary_size", &arguments).await
        }
        "nexus_perf_cargo_bloat" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_cargo_bloat", &arguments).await
        }
        "nexus_perf_target_dir_size" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_target_dir_size", &arguments).await
        }
        "nexus_perf_largest_files" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_largest_files", &arguments).await
        }
        "nexus_perf_loc_per_crate" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_loc_per_crate", &arguments).await
        }
        "nexus_perf_unused_deps" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_unused_deps", &arguments).await
        }
        "nexus_perf_test_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_test_count", &arguments).await
        }
        "nexus_perf_async_funcs" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_async_funcs", &arguments).await
        }
        "nexus_perf_unsafe_blocks" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_unsafe_blocks", &arguments).await
        }
        "nexus_perf_panic_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_panic_count", &arguments).await
        }
        "nexus_perf_clone_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_clone_count", &arguments).await
        }
        "nexus_perf_string_alloc" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_string_alloc", &arguments).await
        }
        "nexus_perf_box_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_box_count", &arguments).await
        }
        "nexus_perf_arc_mutex" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_arc_mutex", &arguments).await
        }
        "nexus_perf_dep_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_dep_count", &arguments).await
        }
        "nexus_perf_compile_units" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_compile_units", &arguments).await
        }
        "nexus_perf_optimization_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_optimization_check", &arguments).await
        }
        "nexus_perf_lto_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_lto_check", &arguments).await
        }
        "nexus_perf_codegen_units" => {
            dispatch_catalog_tool(db, user_id, project_id, "perf_codegen_units", &arguments).await
        }

        // ── Fase 9N: Testing extras (20) ────────────────────────────────
        "nexus_test_run_unit" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_run_unit", &arguments).await
        }
        "nexus_test_run_integration" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_run_integration", &arguments).await
        }
        "nexus_test_run_quiet" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_run_quiet", &arguments).await
        }
        "nexus_test_run_workspace" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_run_workspace", &arguments).await
        }
        "nexus_test_count_files" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_count_files", &arguments).await
        }
        "nexus_test_ignored_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_ignored_count", &arguments).await
        }
        "nexus_test_should_panic_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_should_panic_count", &arguments).await
        }
        "nexus_test_module_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_module_count", &arguments).await
        }
        "nexus_test_assert_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_assert_count", &arguments).await
        }
        "nexus_test_proptest_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_proptest_count", &arguments).await
        }
        "nexus_test_quickcheck_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_quickcheck_count", &arguments).await
        }
        "nexus_test_mock_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_mock_count", &arguments).await
        }
        "nexus_test_bench_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_bench_count", &arguments).await
        }
        "nexus_test_doc_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_doc_count", &arguments).await
        }
        "nexus_test_fixtures_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_fixtures_list", &arguments).await
        }
        "nexus_test_snapshots_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_snapshots_list", &arguments).await
        }
        "nexus_test_stale_snapshots" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_stale_snapshots", &arguments).await
        }
        "nexus_test_coverage_summary" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_coverage_summary", &arguments).await
        }
        "nexus_test_failed_log" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_failed_log", &arguments).await
        }
        "nexus_test_workflow_files" => {
            dispatch_catalog_tool(db, user_id, project_id, "test_workflow_files", &arguments).await
        }

        // ── Fase 9O: Security extras (20) ───────────────────────────────
        "nexus_sec_secret_patterns" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_secret_patterns", &arguments).await
        }
        "nexus_sec_unwrap_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_unwrap_count", &arguments).await
        }
        "nexus_sec_panic_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_panic_count", &arguments).await
        }
        "nexus_sec_env_var_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_env_var_check", &arguments).await
        }
        "nexus_sec_http_url_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_http_url_count", &arguments).await
        }
        "nexus_sec_localhost_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_localhost_count", &arguments).await
        }
        "nexus_sec_eval_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_eval_check", &arguments).await
        }
        "nexus_sec_sql_injection_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_sql_injection_check", &arguments).await
        }
        "nexus_sec_cmd_injection_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_cmd_injection_check", &arguments).await
        }
        "nexus_sec_dependency_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_dependency_count", &arguments).await
        }
        "nexus_sec_git_secrets_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_git_secrets_check", &arguments).await
        }
        "nexus_sec_env_files_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_env_files_check", &arguments).await
        }
        "nexus_sec_dockerfile_user_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_dockerfile_user_check", &arguments).await
        }
        "nexus_sec_workflow_perms_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_workflow_perms_check", &arguments).await
        }
        "nexus_sec_cors_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_cors_check", &arguments).await
        }
        "nexus_sec_jwt_secret_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_jwt_secret_check", &arguments).await
        }
        "nexus_sec_md5_sha1_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_md5_sha1_check", &arguments).await
        }
        "nexus_sec_random_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_random_check", &arguments).await
        }
        "nexus_sec_tls_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_tls_check", &arguments).await
        }
        "nexus_sec_audit_summary" => {
            dispatch_catalog_tool(db, user_id, project_id, "sec_audit_summary", &arguments).await
        }

        // ── Fase 9P: Code Analysis extras (20) ──────────────────────────
        "nexus_ca_struct_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_struct_count", &arguments).await
        }
        "nexus_ca_enum_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_enum_count", &arguments).await
        }
        "nexus_ca_trait_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_trait_count", &arguments).await
        }
        "nexus_ca_impl_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_impl_count", &arguments).await
        }
        "nexus_ca_fn_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_fn_count", &arguments).await
        }
        "nexus_ca_pub_fn_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_pub_fn_count", &arguments).await
        }
        "nexus_ca_macro_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_macro_count", &arguments).await
        }
        "nexus_ca_use_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_use_count", &arguments).await
        }
        "nexus_ca_mod_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_mod_count", &arguments).await
        }
        "nexus_ca_lifetime_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_lifetime_count", &arguments).await
        }
        "nexus_ca_generic_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_generic_count", &arguments).await
        }
        "nexus_ca_derive_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_derive_count", &arguments).await
        }
        "nexus_ca_attr_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_attr_count", &arguments).await
        }
        "nexus_ca_doc_comment_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_doc_comment_count", &arguments).await
        }
        "nexus_ca_inline_comment_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_inline_comment_count", &arguments).await
        }
        "nexus_ca_todo_fixme_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_todo_fixme_count", &arguments).await
        }
        "nexus_ca_match_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_match_count", &arguments).await
        }
        "nexus_ca_if_let_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_if_let_count", &arguments).await
        }
        "nexus_ca_while_let_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_while_let_count", &arguments).await
        }
        "nexus_ca_complexity_estimate" => {
            dispatch_catalog_tool(db, user_id, project_id, "ca_complexity_estimate", &arguments).await
        }

        // ── Fase 9Q: Build / Deploy (21) ────────────────────────────────
        "nexus_build_target_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_target_list", &arguments).await
        }
        "nexus_build_artifact_age" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_artifact_age", &arguments).await
        }
        "nexus_build_release_size" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_release_size", &arguments).await
        }
        "nexus_build_debug_size" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_debug_size", &arguments).await
        }
        "nexus_build_incremental_dir" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_incremental_dir", &arguments).await
        }
        "nexus_build_lockfile_age" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_lockfile_age", &arguments).await
        }
        "nexus_build_log_tail" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_log_tail", &arguments).await
        }
        "nexus_build_rerun_checks" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_rerun_checks", &arguments).await
        }
        "nexus_build_script_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_script_count", &arguments).await
        }
        "nexus_build_workspace_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_workspace_check", &arguments).await
        }
        "nexus_build_profile_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "build_profile_list", &arguments).await
        }
        "nexus_deploy_dockerfile_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_dockerfile_count", &arguments).await
        }
        "nexus_deploy_compose_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_compose_check", &arguments).await
        }
        "nexus_deploy_k8s_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_k8s_check", &arguments).await
        }
        "nexus_deploy_helm_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_helm_check", &arguments).await
        }
        "nexus_deploy_terraform_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_terraform_check", &arguments).await
        }
        "nexus_deploy_ansible_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_ansible_check", &arguments).await
        }
        "nexus_deploy_systemd_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_systemd_check", &arguments).await
        }
        "nexus_deploy_nginx_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_nginx_check", &arguments).await
        }
        "nexus_deploy_env_files_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_env_files_count", &arguments).await
        }
        "nexus_deploy_release_artifacts" => {
            dispatch_catalog_tool(db, user_id, project_id, "deploy_release_artifacts", &arguments).await
        }

        // ── Fase 9R — API / Memory / Other (20) ───────────────────────────
        "nexus_api_openapi_files" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_openapi_files", &arguments).await
        }
        "nexus_api_route_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_route_count", &arguments).await
        }
        "nexus_api_handler_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_handler_count", &arguments).await
        }
        "nexus_api_endpoint_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_endpoint_list", &arguments).await
        }
        "nexus_api_graphql_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_graphql_check", &arguments).await
        }
        "nexus_api_grpc_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_grpc_check", &arguments).await
        }
        "nexus_api_postman_check" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_postman_check", &arguments).await
        }
        "nexus_api_middleware_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "api_middleware_count", &arguments).await
        }
        "nexus_memory_namespace_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_namespace_count", &arguments).await
        }
        "nexus_memory_size_estimate" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_size_estimate", &arguments).await
        }
        "nexus_memory_pattern_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_pattern_list", &arguments).await
        }
        "nexus_memory_recent_writes" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_recent_writes", &arguments).await
        }
        "nexus_memory_topkeys" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_topkeys", &arguments).await
        }
        "nexus_memory_evict_stats" => {
            dispatch_catalog_tool(db, user_id, project_id, "memory_evict_stats", &arguments).await
        }
        "nexus_util_disk_free" => {
            dispatch_catalog_tool(db, user_id, project_id, "util_disk_free", &arguments).await
        }
        "nexus_util_uptime" => {
            dispatch_catalog_tool(db, user_id, project_id, "util_uptime", &arguments).await
        }
        "nexus_util_hostname" => {
            dispatch_catalog_tool(db, user_id, project_id, "util_hostname", &arguments).await
        }
        "nexus_util_cpu_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "util_cpu_count", &arguments).await
        }
        "nexus_util_now_iso" => {
            dispatch_catalog_tool(db, user_id, project_id, "util_now_iso", &arguments).await
        }
        "nexus_util_pid" => {
            dispatch_catalog_tool(db, user_id, project_id, "util_pid", &arguments).await
        }

        // ── Fase 9S — Final meta tools (5) ────────────────────────────────
        "nexus_meta_catalog_count" => {
            dispatch_catalog_tool(db, user_id, project_id, "meta_catalog_count", &arguments).await
        }
        "nexus_meta_categories_list" => {
            dispatch_catalog_tool(db, user_id, project_id, "meta_categories_list", &arguments).await
        }
        "nexus_meta_version_info" => {
            dispatch_catalog_tool(db, user_id, project_id, "meta_version_info", &arguments).await
        }
        "nexus_meta_health_summary" => {
            dispatch_catalog_tool(db, user_id, project_id, "meta_health_summary", &arguments).await
        }
        "nexus_meta_self_test" => {
            dispatch_catalog_tool(db, user_id, project_id, "meta_self_test", &arguments).await
        }

        "nexus_shell_exec" => {
            dispatch_catalog_tool(db, user_id, project_id, "shell_exec", &arguments).await
        }

        // ── service_control ───────────────────────────────────────────────
        "nexus_service_status" => handle_service_status(db, project_id).await,
        "nexus_service_control" => handle_service_control(db, project_id, &arguments).await,

        _ => {
            let _ = (user_id, project_id);
            format!("[Nexus Builtin] Tool '{}' non riconosciuto.", tool_name)
        }
    }
}

// ---------------------------------------------------------------------------
// Variante con NeuralCoreClient per ricerca semantica tool MCP
// ---------------------------------------------------------------------------

/// Come `execute()`, ma passa `neural` a nexus_mcp_tool_search e nexus_mcp_tool_reindex
/// per abilitare la ricerca semantica Qdrant e l'indicizzazione embedding.
pub async fn execute_with_neural(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    user_role: &str,
    neural: &crate::orchestrator::NeuralCoreClient,
    tool_name: &str,
    arguments: Value,
) -> String {
    match tool_name {
        "nexus_mcp_tool_search" => {
            handle_mcp_tool_search_with_neural(db, neural, user_id, project_id, &arguments).await
        }
        "nexus_mcp_tool_reindex" => {
            if user_role != "admin" {
                return "[Accesso negato] nexus_mcp_tool_reindex richiede ruolo admin.".to_string();
            }
            handle_mcp_tool_reindex(db, Some(neural), &arguments).await
        }
        // Tutti gli altri tool non usano neural: delega a execute()
        other => execute(db, user_id, project_id, user_role, other, arguments).await,
    }
}

// ---------------------------------------------------------------------------
// Bridge: MCP dispatcher → NexusToolCatalog
// ---------------------------------------------------------------------------
//
// Traduce una chiamata MCP (tool_name, arguments JSON, project_id) in un
// `NexusToolContext` e inoltra al catalog globale. Il risultato viene
// serializzato come stringa JSON per rispettare il contratto `execute()`
// che ritorna `String` al chiamante (agent_tools).
//
// Errori:
// - `get_project_root` fallisce (project non trovato) → messaggio di errore
// - `NexusToolCatalog::global()` non inizializzato → messaggio fallback
// - `catalog.execute` ritorna `NexusToolError` → renderizzato come JSON
//   `{"error": "..."}` così l'agente può interpretare il failure.
async fn dispatch_catalog_tool(
    db: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    catalog_name: &str,
    arguments: &Value,
) -> String {
    use crate::nexus_tool_catalog::NexusToolCatalog;
    use crate::nexus_tools::NexusToolContext;
    use std::path::PathBuf;

    // 1. Resolve project root (assoluto sul FS)
    let project_root = match get_project_root(db, project_id).await {
        Ok(path) => PathBuf::from(path),
        Err(e) => {
            return json!({
                "error": format!("project_root resolution failed: {}", e),
                "tool": catalog_name,
            })
            .to_string();
        }
    };

    // 2. Recupera catalog singleton
    let catalog = match NexusToolCatalog::global() {
        Some(c) => c,
        None => {
            return json!({
                "error": "NexusToolCatalog non inizializzato",
                "tool": catalog_name,
            })
            .to_string();
        }
    };

    // 3. Costruisci context (120s timeout di default, coerente con preset)
    let ctx = NexusToolContext::new(project_root, project_id, user_id);

    // 4. Esegui via catalog e rendi il Result uniforme
    match catalog.execute(catalog_name, &ctx, arguments).await {
        Ok(value) => format_json(&value),
        Err(e) => json!({
            "error": e.to_string(),
            "tool": catalog_name,
        })
        .to_string(),
    }
}

/// Handler per `nexus_open_file_in_editor`: chiede al frontend di aprire un file
/// nell'editor del web-ide.
///
/// Il tool non esegue azioni sul filesystem direttamente: ritorna un JSON con
/// `_ui_action: "open_file"` che il frontend (ChatPanel) intercetta nel
/// tool_result e usa per dispatchare l'evento `nexus:editor:open-file`.
///
/// Sicurezza: il `path` deve essere relativo alla root del progetto e non
/// contenere `..` per evitare directory traversal. Verifichiamo anche che il
/// file esista realmente nel workspace del progetto.
async fn handle_open_file_in_editor(
    db: &PgPool,
    project_id: Uuid,
    arguments: &Value,
) -> String {
    let path = arguments.get("path").and_then(Value::as_str).unwrap_or("").trim();
    if path.is_empty() {
        return json!({
            "ok": false,
            "error": "Parametro 'path' mancante o vuoto",
        }).to_string();
    }
    // Security: rifiuta path assoluti o con ".." per traversal.
    if path.starts_with('/') || path.starts_with('\\') {
        return json!({
            "ok": false,
            "error": format!("Path '{path}' deve essere relativo alla root del progetto, non assoluto"),
        }).to_string();
    }
    if path.split('/').any(|seg| seg == "..") {
        return json!({
            "ok": false,
            "error": format!("Path '{path}' contiene '..', non ammesso"),
        }).to_string();
    }
    // Verifica esistenza file nel workspace del progetto.
    let root_path = match get_project_root(db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            return json!({
                "ok": false,
                "error": format!("Workspace del progetto non disponibile: {e}"),
            }).to_string();
        }
    };
    let full_path = std::path::Path::new(&root_path).join(path);
    if !full_path.exists() {
        return json!({
            "ok": false,
            "error": format!("File '{path}' non esiste nel workspace ({root_path})"),
            "_ui_action": "open_file",
            "path": path,
        }).to_string();
    }
    if !full_path.is_file() {
        return json!({
            "ok": false,
            "error": format!("Path '{path}' esiste ma non e' un file"),
        }).to_string();
    }
    let line = arguments.get("line").and_then(Value::as_i64);
    // Risposta strutturata: il frontend intercetta `_ui_action: "open_file"` nel
    // tool_result e dispatcha l'evento `nexus:editor:open-file` con `path` (e
    // opzionalmente `line`). L'editor del web-ide apre il file nel gruppo
    // attivo, riusando una tab esistente se gia' aperta.
    json!({
        "ok": true,
        "message": format!("Apertura {path} richiesta all'editor"),
        "_ui_action": "open_file",
        "path": path,
        "line": line,
    }).to_string()
}

/// M13.2 — nexus_impact_brief: dato un seed (file modificati), ritorna impact
/// set strutturale + note KB pertinenti + test che lo coprono. Consultivo.
/// arguments: { "paths": ["src/a.rs", ...] } oppure { "path": "src/a.rs" }.
async fn handle_impact_brief(db: &PgPool, project_id: Uuid, arguments: &Value) -> String {
    let mut seed_paths: Vec<String> = Vec::new();
    if let Some(arr) = arguments.get("paths").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    seed_paths.push(s.to_string());
                }
            }
        }
    }
    if let Some(p) = arguments.get("path").and_then(|v| v.as_str()) {
        if !p.is_empty() && !seed_paths.iter().any(|e| e == p) {
            seed_paths.push(p.to_string());
        }
    }
    if seed_paths.is_empty() {
        return json!({
            "error": "nexus_impact_brief richiede 'paths' (array) o 'path' (stringa) con i file seed."
        })
        .to_string();
    }
    crate::knowledge::impact::impact_brief(db, project_id, &seed_paths)
        .await
        .to_string()
}
