//! Nexus Tools — handler eseguibili per i tool registrati nel NexusToolCatalog.
//!
//! Fase 9A: introduce il trait `NexusToolHandler` e la struttura di runtime
//! (context, errori, flag di sicurezza) che permette ai tool del catalogo di
//! essere effettivamente eseguiti anziché restare semplici descrittori.
//!
//! I 27 stub caricati da `NexusToolCatalog::with_builtins()` restano presenti
//! come spec; i tool "veri" di Fase 9 vengono registrati in aggiunta tramite
//! `register_with_handler` e contati in `implemented_count()`.
//!
//! Architettura:
//! - `NexusToolHandler` è un trait `async` (via `async-trait`) con un solo
//!   metodo `execute(ctx, args) -> Result<Value, NexusToolError>`.
//! - `NexusToolContext` trasporta il contesto di progetto necessario a ogni
//!   invocazione: project_root assoluto, project_id, user_id, e un timeout
//!   di default per eventuali subprocess.
//! - `NexusToolSafety` è un plain struct di flag booleani — volutamente
//!   semplice per evitare nuove dipendenze (es. bitflags). Il dispatcher nel
//!   catalog può usarli per enforcement pre-run (es. negare
//!   `CAN_WRITE_FILESYSTEM` se l'utente è in modalità readonly).
//!
//! I sottomoduli `exec`, `cargo_check`, `git_status`, ... contengono
//! rispettivamente l'helper `run_cmd` e i 20 handler previsti dal piano.

pub mod exec;
pub mod parse_ndjson;

/// Valida che `path` non contenga `..` (path traversal) e che, joined con
/// `project_root`, resti dentro la root. Punto unico (regola L, S59) per i
/// check di path traversal duplicati in piu' tool.
pub fn validate_no_path_traversal(
    project_root: &std::path::Path,
    path: &str,
) -> Result<std::path::PathBuf, NexusToolError> {
    use std::path::Component;
    let pb = std::path::PathBuf::from(path);
    if pb.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(NexusToolError::BadInput("path traversal denied".into()));
    }
    let full = project_root.join(&pb);
    if !full.starts_with(project_root) {
        return Err(NexusToolError::BadInput("path traversal denied".into()));
    }
    Ok(full)
}

/// Esito di una esecuzione `pg_dump`: success/duration/size/stderr + exit_code.
pub struct PgDumpOutcome {
    pub success: bool,
    pub duration_ms: u64,
    pub size_bytes: u64,
    pub stderr_truncated: String,
    pub exit_code: i32,
}

/// Esegue `pg_dump` con `PGPASSWORD` impostato e stdin chiuso. Misura la
/// durata, raccoglie size del file di backup e stderr troncato. Punto unico
/// (regola L, S74) per il pattern duplicato fra `project_db_backup` e
/// `project_db_dump_schema`.
pub async fn run_pg_dump(
    args: &[&str],
    password: &str,
    current_dir: Option<&std::path::Path>,
    backup_path: &std::path::Path,
) -> Result<PgDumpOutcome, NexusToolError> {
    let start = std::time::Instant::now();
    let mut cmd = tokio::process::Command::new("pg_dump");
    cmd.args(args)
        .env("PGPASSWORD", password)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    let child = cmd
        .output()
        .await
        .map_err(|e| NexusToolError::BadInput(format!("pg_dump: {}", e)))?;
    let duration_ms = start.elapsed().as_millis() as u64;
    let success = child.status.success();
    let size_bytes = if success {
        tokio::fs::metadata(backup_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    let stderr_truncated = String::from_utf8_lossy(&child.stderr)
        .chars()
        .take(2000)
        .collect();
    Ok(PgDumpOutcome {
        success,
        duration_ms,
        size_bytes,
        stderr_truncated,
        exit_code: child.status.code().unwrap_or(-1),
    })
}

/// Esegue `npm run <script_name>` nel project_root e ritorna il JSON canonico
/// `{ok, stack: "node", exit_code, duration_ms, stdout, stderr}`. Punto unico
/// (regola L, S65) per i tool che lanciano script npm (es. `bench_run`,
/// `coverage_report`) e condividono il pattern run_cmd+response.
pub async fn run_npm_script_node_stack(
    ctx: &NexusToolContext,
    script_name: &str,
) -> Result<serde_json::Value, NexusToolError> {
    let out = exec::run_cmd(
        "npm",
        &["run", script_name],
        &ctx.project_root,
        ctx.timeout_secs,
    )
    .await?;
    Ok(serde_json::json!({
        "ok": out.success(),
        "stack": "node",
        "exit_code": out.exit_code,
        "duration_ms": out.duration_ms,
        "stdout": out.stdout,
        "stderr": out.stderr,
    }))
}

/// Esegue `cargo test <subset_flag>` (es. `--lib`, `--doc`, `--bins`) e ritorna
/// il JSON canonico `{ok, exit_code, passed, failed, stdout_preview, duration_ms}`.
/// Punto unico (regola L, S53) per il pattern duplicato fra
/// cargo_test_lib/cargo_test_doc/cargo_test_bins.
pub async fn run_cargo_test_subset(
    ctx: &NexusToolContext,
    subset_flag: &str,
) -> Result<serde_json::Value, NexusToolError> {
    let out = exec::run_cmd(
        "cargo",
        &["test", subset_flag],
        &ctx.project_root,
        ctx.timeout_secs.max(300),
    )
    .await?;
    let passed = out.stdout.lines().filter(|l| l.contains(" ... ok")).count();
    let failed = out
        .stdout
        .lines()
        .filter(|l| l.contains(" ... FAILED"))
        .count();
    Ok(serde_json::json!({
        "ok": out.success(),
        "exit_code": out.exit_code,
        "passed": passed,
        "failed": failed,
        "stdout_preview": out.stdout.chars().take(2000).collect::<String>(),
        "duration_ms": out.duration_ms,
    }))
}

/// Directory che vengono SEMPRE saltate dai tool che scansionano il filesystem
/// (find_todos, fs_grep, ecc.). Punto unico (regola L, S24): prima ogni walk
/// aveva la sua catena di `name == "node_modules" || name == "target" || ...`.
pub fn is_skipped_dir(name: &str) -> bool {
    name.starts_with('.')
        || name == "node_modules"
        || name == "target"
        || name == "dist"
        || name == "build"
}

// ── Rust toolchain ────────────────────────────────────────────────────────
pub mod rustc_explain;
pub mod rustc_version;

// ── Cargo lifecycle ───────────────────────────────────────────────────────
pub mod cargo_audit;
pub mod cargo_bench;
pub mod cargo_build;
pub mod cargo_check;
pub mod cargo_clean;
pub mod cargo_metadata;
pub mod cargo_outdated;
pub mod cargo_test;
pub mod cargo_tree;
pub mod cargo_update;

// ── Quality / Security ────────────────────────────────────────────────────
pub mod clippy_lint;
pub mod format_code;
pub mod license_check;
pub mod lint_run;
pub mod sast_scan;
pub mod secret_scan;
pub mod test_coverage;

// ── Git / VCS ─────────────────────────────────────────────────────────────
pub mod git_blame;
pub mod git_diff;
pub mod git_log;
pub mod git_status;

// ── Deployment / GitHub ───────────────────────────────────────────────────
pub mod deploy_check;
pub mod gh_issue_list;
pub mod gh_pr_create;

// ── Memory / Utility ──────────────────────────────────────────────────────
pub mod memory_ns;
pub mod regex_match;

// ── Fase 9E: RuVector + Consensus runtime wiring ──────────────────────────
pub mod consensus_vote;
pub mod ruvector_insert;
pub mod ruvector_search;
pub mod ruvector_stats;

// ── Fase 9F: Utility batch (fs/json/base64/hash/uuid) ─────────────────────
pub mod base64_decode;
pub mod base64_encode;
pub mod fs_grep;
pub mod fs_list;
pub mod fs_read;
pub mod fs_tree;
pub mod hash_content;
pub mod json_get;
pub mod json_parse;
pub mod uuid_generate;

// ── Fase 9F: VCS batch ────────────────────────────────────────────────────
pub mod git_branch_list;
pub mod git_remote_list;
pub mod git_show;
pub mod git_tag_list;

// ── Fase 9F: GitHub batch ─────────────────────────────────────────────────
pub mod gh_release_list;
pub mod gh_run_list;
pub mod gh_workflow_list;

// ── Fase 9F: CodeAnalysis / Quality batch ─────────────────────────────────
pub mod cargo_fmt_check;
pub mod count_loc;
pub mod find_todos;

// ── Fase 9G: Utility batch (8) ────────────────────────────────────────────
pub mod env_get;
pub mod fs_glob;
pub mod fs_stat;
pub mod fs_write;
pub mod regex_replace;
pub mod text_diff;
pub mod time_now;
pub mod uuid_parse;

// ── Fase 9G: VCS batch (4) ────────────────────────────────────────────────
pub mod git_describe;
pub mod git_grep;
pub mod git_shortlog;
pub mod git_stash_list;

// ── Fase 9G: GitHub batch (3) ─────────────────────────────────────────────
pub mod gh_pr_list;
pub mod gh_pr_view;
pub mod gh_repo_view;

// ── Fase 9G: Cargo / Build batch (3) ──────────────────────────────────────
pub mod cargo_doc;
pub mod cargo_locate_project;
pub mod cargo_pkgid;

// ── Fase 9G: CodeAnalysis batch (2) ───────────────────────────────────────
pub mod find_pubapi;
pub mod find_unsafe;

// ── Fase 9J: GitHub extras (20) ───────────────────────────────────────────
pub mod gh_issue_close;
pub mod gh_issue_comment;
pub mod gh_issue_create;
pub mod gh_issue_view;
pub mod gh_label_list;
pub mod gh_pr_checks;
pub mod gh_pr_close;
pub mod gh_pr_diff;
pub mod gh_pr_files;
pub mod gh_pr_merge;
pub mod gh_pr_review;
pub mod gh_release_create;
pub mod gh_release_view;
pub mod gh_repo_clone_url;
pub mod gh_repo_fork_list;
pub mod gh_run_cancel;
pub mod gh_run_logs;
pub mod gh_run_view;
pub mod gh_workflow_run;
pub mod gh_workflow_view;

// ── Fase 9I: Git extras (20) ──────────────────────────────────────────────
pub mod git_archive_dry;
pub mod git_bundle_verify;
pub mod git_cat_file;
pub mod git_check_ignore;
pub mod git_clean_dry;
pub mod git_config_list;
pub mod git_count_objects;
pub mod git_diff_stat;
pub mod git_for_each_ref;
pub mod git_fsck;
pub mod git_gc_dry;
pub mod git_log_graph;
pub mod git_ls_files;
pub mod git_ls_tree;
pub mod git_merge_base;
pub mod git_reflog;
pub mod git_rev_parse;
pub mod git_show_branch;
pub mod git_submodule_list;
pub mod git_worktree_list;

// ── Fase 9H: Cargo extras (20) ────────────────────────────────────────────
pub mod cargo_build_artifact_check;
pub mod cargo_check_all_features;
pub mod cargo_check_release;
pub mod cargo_clean_dry;
pub mod cargo_dep_versions;
pub mod cargo_doc_check;
pub mod cargo_edition_detect;
pub mod cargo_env_overrides;
pub mod cargo_features_list;
pub mod cargo_install_list;
pub mod cargo_lockfile_check;
pub mod cargo_msrv_detect;
pub mod cargo_publish_dry;
pub mod cargo_run;
pub mod cargo_search;
pub mod cargo_size_estimate;
pub mod cargo_targets_list;
pub mod cargo_test_doc;
pub mod cargo_test_lib;
pub mod cargo_workspace_members;
pub mod shell_exec;

// ── Fase 9D: 18 stub handlers implementati ────────────────────────────────
// Code analysis
pub mod ast_parse;
pub mod ast_query;
// Testing
pub mod coverage_report;
pub mod test_generate;
// Dependencies / Build / Performance
pub mod bench_run;
pub mod build_project;
pub mod deps_audit;
pub mod deps_tree;
pub mod profile_run;
// Refactoring / Documentation
pub mod doc_generate;
pub mod extract_function;
pub mod rename_symbol;
// Database / API
pub mod db_query_explain;
pub mod db_schema_inspect;
pub mod openapi_validate;

// ── Fase 9N: Testing extras (20) ──────────────────────────────────────────
pub mod test_assert_count;
pub mod test_bench_count;
pub mod test_count_files;
pub mod test_coverage_summary;
pub mod test_doc_count;
pub mod test_failed_log;
pub mod test_fixtures_list;
pub mod test_ignored_count;
pub mod test_mock_count;
pub mod test_module_count;
pub mod test_playwright;
pub mod test_proptest_count;
pub mod test_quickcheck_count;
pub mod test_run_integration;
pub mod test_run_quiet;
pub mod test_run_unit;
pub mod test_run_workspace;
pub mod test_should_panic_count;
pub mod test_snapshots_list;
pub mod test_stale_snapshots;
pub mod test_workflow_files;

// ── Fase 9O: Security extras (20) ─────────────────────────────────────────
pub mod sec_audit_summary;
pub mod sec_cmd_injection_check;
pub mod sec_cors_check;
pub mod sec_dependency_count;
pub mod sec_dockerfile_user_check;
pub mod sec_env_files_check;
pub mod sec_env_var_check;
pub mod sec_eval_check;
pub mod sec_git_secrets_check;
pub mod sec_http_url_count;
pub mod sec_jwt_secret_check;
pub mod sec_localhost_count;
pub mod sec_md5_sha1_check;
pub mod sec_panic_count;
pub mod sec_random_check;
pub mod sec_secret_patterns;
pub mod sec_sql_injection_check;
pub mod sec_tls_check;
pub mod sec_unwrap_count;
pub mod sec_workflow_perms_check;

// ── Fase 9P: Code Analysis extras (20) ────────────────────────────────────
pub mod ca_attr_count;
pub mod ca_complexity_estimate;
pub mod ca_derive_count;
pub mod ca_doc_comment_count;
pub mod ca_enum_count;
pub mod ca_fn_count;
pub mod ca_generic_count;
pub mod ca_if_let_count;
pub mod ca_impl_count;
pub mod ca_inline_comment_count;
pub mod ca_lifetime_count;
pub mod ca_macro_count;
pub mod ca_match_count;
pub mod ca_mod_count;
pub mod ca_pub_fn_count;
pub mod ca_struct_count;
pub mod ca_todo_fixme_count;
pub mod ca_trait_count;
pub mod ca_use_count;
pub mod ca_while_let_count;

// ── Fase 9Q: Build / Deploy (20) ──────────────────────────────────────────
pub mod build_artifact_age;
pub mod build_debug_size;
pub mod build_incremental_dir;
pub mod build_lockfile_age;
pub mod build_log_tail;
pub mod build_profile_list;
pub mod build_release_size;
pub mod build_rerun_checks;
pub mod build_script_count;
pub mod build_target_list;
pub mod build_workspace_check;
pub mod deploy_ansible_check;
pub mod deploy_compose_check;
pub mod deploy_dockerfile_count;
pub mod deploy_env_files_count;
pub mod deploy_helm_check;
pub mod deploy_k8s_check;
pub mod deploy_nginx_check;
pub mod deploy_release_artifacts;
pub mod deploy_systemd_check;
pub mod deploy_terraform_check;

// ── Fase 9R: API / Memory / Other (20) ────────────────────────────────────
pub mod api_endpoint_list;
pub mod api_graphql_check;
pub mod api_grpc_check;
pub mod api_handler_count;
pub mod api_middleware_count;
pub mod api_openapi_files;
pub mod api_postman_check;
pub mod api_route_count;
pub mod memory_evict_stats;
pub mod memory_namespace_count;
pub mod memory_pattern_list;
pub mod memory_recent_writes;
pub mod memory_size_estimate;
pub mod memory_topkeys;
pub mod util_cpu_count;
pub mod util_disk_free;
pub mod util_hostname;
pub mod util_now_iso;
pub mod util_pid;
pub mod util_uptime;

// ── Fase 9S: Final meta tools (5) ─────────────────────────────────────────
pub mod meta_catalog_count;
pub mod meta_categories_list;
pub mod meta_health_summary;
pub mod meta_self_test;
pub mod meta_version_info;

// ── Fase 9M: Performance extras (20) ──────────────────────────────────────
pub mod perf_arc_mutex;
pub mod perf_async_funcs;
pub mod perf_binary_size;
pub mod perf_box_count;
pub mod perf_cargo_bloat;
pub mod perf_cargo_build_time;
pub mod perf_clone_count;
pub mod perf_codegen_units;
pub mod perf_compile_units;
pub mod perf_dep_count;
pub mod perf_largest_files;
pub mod perf_loc_per_crate;
pub mod perf_lto_check;
pub mod perf_optimization_check;
pub mod perf_panic_count;
pub mod perf_scan;
pub mod perf_string_alloc;
pub mod perf_target_dir_size;
pub mod perf_test_count;
pub mod perf_unsafe_blocks;
pub mod perf_unused_deps;

// ── Fase 9L: Documentation extras (20) ────────────────────────────────────
pub mod doc_api_list;
pub mod doc_changelog_check;
pub mod doc_codeblocks_count;
pub mod doc_codeblocks_extract;
pub mod doc_codeowners_check;
pub mod doc_contributing_check;
pub mod doc_examples_list;
pub mod doc_frontmatter_parse;
pub mod doc_heading_depth;
pub mod doc_image_list;
pub mod doc_license_detect;
pub mod doc_link_check_local;
pub mod doc_links_extract;
pub mod doc_md_lint;
pub mod doc_orphan_md;
pub mod doc_readme_check;
pub mod doc_security_md_check;
pub mod doc_size_report;
pub mod doc_toc_extract;
pub mod doc_word_count;

// ── Fase 9K: Database extras (20) ─────────────────────────────────────────
pub mod db_active_queries;
pub mod db_bloat_check;
pub mod db_connection_info;
pub mod db_constraint_list;
pub mod db_dead_tuples;
pub mod db_extension_list;
pub mod db_foreign_keys;
pub mod db_helper;
pub mod db_index_list;
pub mod db_lock_list;
pub mod db_migration_list;
pub mod db_ping;
pub mod db_replication_status;
pub mod db_role_list;
pub mod db_seq_list;
pub mod db_size;
pub mod db_table_count;
pub mod db_table_list;
pub mod db_table_size;
pub mod db_unused_indexes;
pub mod db_view_list;
pub mod http_request;
pub mod project_db_apply_migration;
pub mod project_db_connections;
pub mod project_db_create_migration;
pub mod project_db_query;
pub mod project_db_rollback;
pub mod project_db_schema;
pub mod project_db_set_connection;
pub mod project_db_status;
pub mod project_db_tables;
pub mod project_info;
pub mod project_run_configs;
pub mod service_healthcheck;

// ── Fase 4: Bootstrap progetto ────────────────────────────────────────────
pub mod project_delete;
pub mod project_register_existing_dir;
pub mod project_register_from_git;
pub mod project_set_default_branch;
pub mod project_workspace_init;

// ── Fase 6: Operazioni DB avanzate ─────────────────────────────────────────
pub mod project_db_analyze;
pub mod project_db_backup;
pub mod project_db_diff_schema;
pub mod project_db_dump_schema;
pub mod project_db_helpers;
pub mod project_db_kill_query;
pub mod project_db_reindex;
pub mod project_db_restore;
pub mod project_db_vacuum;

// ── Fase 5: Docker / Container ────────────────────────────────────────────
pub mod docker_build;
pub mod docker_compose_down;
pub mod docker_compose_up;
pub mod docker_helpers;
pub mod docker_logs;
pub mod docker_ps;
pub mod docker_rm;
pub mod docker_run;
pub mod docker_stop;

use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

/// Flag di sicurezza dichiarati da ogni handler. Usati dal dispatcher per
/// enforcement pre-run e per documentazione al chiamante.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct NexusToolSafety {
    /// Il tool non modifica lo stato del filesystem del progetto.
    pub read_only: bool,
    /// Il tool può scrivere sul filesystem (es. cargo test in target/).
    pub can_write_filesystem: bool,
    /// Il tool spawna subprocess esterni (binario + argomenti).
    pub can_execute_subproc: bool,
    /// Il tool può fare egress di rete (es. cargo audit scarica advisory DB).
    pub network_egress: bool,
}

#[allow(dead_code)]
impl NexusToolSafety {
    /// Preset: read-only puro (nessuna scrittura FS, nessun subprocess).
    pub const fn read_only() -> Self {
        Self {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: false,
            network_egress: false,
        }
    }

    /// Preset: lancia un subprocess read-only (es. git status, rustc --version).
    pub const fn read_only_subproc() -> Self {
        Self {
            read_only: true,
            can_write_filesystem: false,
            can_execute_subproc: true,
            network_egress: false,
        }
    }

    /// Preset: lancia un subprocess che può scrivere sul FS (es. cargo test).
    pub const fn write_subproc() -> Self {
        Self {
            read_only: false,
            can_write_filesystem: true,
            can_execute_subproc: true,
            network_egress: false,
        }
    }
}

/// Contesto passato a ogni invocazione di un handler. Contiene le
/// informazioni minime per eseguire un tool sul progetto corrente.
#[derive(Debug, Clone)]
pub struct NexusToolContext {
    /// Path assoluto della root del progetto (es. /opt/ai-orchestrator).
    pub project_root: PathBuf,
    /// UUID del progetto nella tabella `projects`.
    pub project_id: Uuid,
    /// UUID dell'utente che ha invocato il tool.
    #[allow(dead_code)]
    pub user_id: Uuid,
    /// Timeout di default per eventuali subprocess (secondi).
    pub timeout_secs: u64,
}

impl NexusToolContext {
    pub fn new(project_root: PathBuf, project_id: Uuid, user_id: Uuid) -> Self {
        Self {
            project_root,
            project_id,
            user_id,
            timeout_secs: 120,
        }
    }

    #[allow(dead_code)]
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

/// Errore standard ritornato da ogni handler.
#[derive(Debug, thiserror::Error)]
pub enum NexusToolError {
    /// Il binario richiesto non è disponibile nel PATH del runtime.
    #[error("binario non disponibile: {0}")]
    BinaryMissing(&'static str),

    /// Il subprocess ha sforato il timeout configurato.
    #[error("timeout dopo {0}s")]
    Timeout(u64),

    /// Errore di I/O generico (spawn, read stdout, ecc.).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Il subprocess è terminato con exit code non-zero.
    #[error("exec fallita (exit={exit_code}): {stderr}")]
    Exec { exit_code: i32, stderr: String },

    /// L'input JSON del chiamante non è valido.
    #[error("input invalido: {0}")]
    BadInput(String),

    /// Fallimento nella serializzazione dell'output.
    #[error("serializzazione output: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Trait implementato da ogni handler eseguibile del NexusToolCatalog.
///
/// L'implementazione deve essere `Send + Sync` perché gli handler vengono
/// condivisi come `Arc<dyn NexusToolHandler>` attraverso il singleton del
/// catalog.
#[async_trait]
pub trait NexusToolHandler: Send + Sync {
    /// Esegue il tool con il contesto e gli argomenti forniti.
    ///
    /// Il dispatcher valida il safety flag prima di chiamare questo metodo,
    /// quindi l'implementazione può assumere che l'enforcement sia già stato
    /// applicato. L'output deve essere JSON serializzabile; eventuali errori
    /// strutturati passano attraverso `NexusToolError`.
    async fn execute(&self, ctx: &NexusToolContext, args: &Value) -> Result<Value, NexusToolError>;

    /// Schema JSON dell'input atteso. Usato per discovery / documentazione.
    /// Default: oggetto vuoto (il tool non richiede argomenti).
    #[allow(dead_code)]
    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    /// Flag di sicurezza del tool. Default: read_only.
    #[allow(dead_code)]
    fn safety(&self) -> NexusToolSafety {
        NexusToolSafety::read_only()
    }
}

/// Alias per il tipo del puntatore condiviso a un handler. Usato dal catalog
/// per evitare di ripetere `Arc<dyn NexusToolHandler>` ovunque.
pub type SharedHandler = Arc<dyn NexusToolHandler>;

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;

    #[async_trait]
    impl NexusToolHandler for Noop {
        async fn execute(
            &self,
            _ctx: &NexusToolContext,
            _args: &Value,
        ) -> Result<Value, NexusToolError> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    #[tokio::test]
    async fn test_handler_trait_object_works() {
        let h: SharedHandler = Arc::new(Noop);
        let ctx = NexusToolContext::new(PathBuf::from("/tmp"), Uuid::nil(), Uuid::nil());
        let out = h.execute(&ctx, &serde_json::json!({})).await.unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn test_safety_presets() {
        let ro = NexusToolSafety::read_only();
        assert!(ro.read_only && !ro.can_write_filesystem);
        let ros = NexusToolSafety::read_only_subproc();
        assert!(ros.read_only && ros.can_execute_subproc);
        let ws = NexusToolSafety::write_subproc();
        assert!(ws.can_write_filesystem && ws.can_execute_subproc);
    }

    #[test]
    fn test_context_with_timeout() {
        let ctx = NexusToolContext::new(PathBuf::from("/tmp"), Uuid::nil(), Uuid::nil())
            .with_timeout(300);
        assert_eq!(ctx.timeout_secs, 300);
    }
}
