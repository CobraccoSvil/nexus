//! Nexus Tool Catalog — registro estendibile dei tool Ruflo (target: 314 tool).
//!
//! Scaffold di Fase 6: definisce la struttura per descrivere i tool che Ruflo
//! espone nativi (AST analysis, security scanning, dependency audit, ecc.) e
//! li organizza per categoria. I tool NON sono ancora implementati, ma il
//! catalogo fornisce il registry e le categorie canoniche su cui appoggiarsi
//! nelle fasi successive.
//!
//! Uso:
//! - `NexusToolCatalog::new()` crea un catalogo vuoto
//! - `NexusToolCatalog::with_builtins()` carica il seed iniziale (categorie note)
//! - `register(spec)` aggiunge un tool
//! - `list_by_category(cat)` filtra per categoria
//! - `len()` / `is_empty()` per statistiche
//!
//! Il catalogo è thread-safe (DashMap) ed esponibile come singleton tramite
//! `init_global` / `global`, sullo stesso pattern di `NexusBridge`.

use crate::nexus_tools::{NexusToolContext, NexusToolError, SharedHandler};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

/// Categorie canoniche per i 314 tool Ruflo.
///
/// Sono raggruppate per dominio funzionale. Numeri indicativi per riferimento
/// alla spec Ruflo v3.5 — i count effettivi verranno popolati nelle fasi
/// successive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NexusToolCategory {
    /// Analisi AST, parsing, semantic analysis
    CodeAnalysis,
    /// Formatting, linting, style enforcement
    CodeQuality,
    /// Test generation, coverage, mutation testing
    Testing,
    /// SAST, secret scanning, dependency audit
    Security,
    /// Refactoring, code transformation
    Refactoring,
    /// Documentation generation
    Documentation,
    /// Dependency management
    Dependencies,
    /// Build, compilation, packaging
    Build,
    /// Git/VCS operations
    Vcs,
    /// GitHub/GitLab integration
    GitHub,
    /// Profiling, benchmarking, metrics
    Performance,
    /// Database schema, queries, migrations
    Database,
    /// API design, OpenAPI, GraphQL
    Api,
    /// Deployment, CI/CD
    Deployment,
    /// Memory/namespace operations
    Memory,
    /// Generic utilities (string ops, regex, ecc.)
    Utility,
    /// Altri
    Other,
}

impl NexusToolCategory {
    pub fn name(&self) -> &'static str {
        match self {
            Self::CodeAnalysis => "code_analysis",
            Self::CodeQuality => "code_quality",
            Self::Testing => "testing",
            Self::Security => "security",
            Self::Refactoring => "refactoring",
            Self::Documentation => "documentation",
            Self::Dependencies => "dependencies",
            Self::Build => "build",
            Self::Vcs => "vcs",
            Self::GitHub => "github",
            Self::Performance => "performance",
            Self::Database => "database",
            Self::Api => "api",
            Self::Deployment => "deployment",
            Self::Memory => "memory",
            Self::Utility => "utility",
            Self::Other => "other",
        }
    }

    /// Tutte le categorie per iterazione
    pub fn all() -> &'static [NexusToolCategory] {
        &[
            Self::CodeAnalysis,
            Self::CodeQuality,
            Self::Testing,
            Self::Security,
            Self::Refactoring,
            Self::Documentation,
            Self::Dependencies,
            Self::Build,
            Self::Vcs,
            Self::GitHub,
            Self::Performance,
            Self::Database,
            Self::Api,
            Self::Deployment,
            Self::Memory,
            Self::Utility,
            Self::Other,
        ]
    }
}

/// Specifica di un tool registrato nel catalogo
#[derive(Debug, Clone)]
pub struct NexusToolSpec {
    /// Nome univoco (es. "ast_parse_rust", "audit_deps_cargo")
    pub name: String,
    /// Categoria
    pub category: NexusToolCategory,
    /// Breve descrizione per LLM / UI
    #[allow(dead_code)]
    pub description: String,
    /// Tool implementato (true) o scaffold stub (false)
    pub implemented: bool,
}

impl NexusToolSpec {
    pub fn new(
        name: impl Into<String>,
        category: NexusToolCategory,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            category,
            description: description.into(),
            implemented: false,
        }
    }

    #[allow(dead_code)]
    pub fn implemented(mut self) -> Self {
        self.implemented = true;
        self
    }
}

/// Entry interna del catalog: avvolge la spec con un handler opzionale.
///
/// Fase 9A: i tool con `handler: Some(...)` sono eseguibili via
/// `NexusToolCatalog::execute`; quelli con `handler: None` restano scaffold
/// descrittivi (i 27 stub del seed). L'API pubblica `get(name)` continua a
/// ritornare solo la spec, per retrocompatibilità con l'endpoint
/// `/nexus/tools` e con i test esistenti.
#[derive(Clone)]
struct NexusToolEntry {
    spec: NexusToolSpec,
    handler: Option<SharedHandler>,
}

impl std::fmt::Debug for NexusToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NexusToolEntry")
            .field("spec", &self.spec)
            .field("has_handler", &self.handler.is_some())
            .finish()
    }
}

/// Catalogo dei tool nexus. Thread-safe via DashMap.
pub struct NexusToolCatalog {
    tools: DashMap<String, NexusToolEntry>,
}

impl NexusToolCatalog {
    pub fn new() -> Self {
        Self {
            tools: DashMap::new(),
        }
    }

    /// Inizializza con il seed canonico di categorie (scaffold — tool stub
    /// rappresentativi per ogni categoria). Le implementazioni reali verranno
    /// aggiunte nelle fasi successive.
    pub fn with_builtins() -> Self {
        let catalog = Self::new();

        // Seed rappresentativo — ~1-2 tool stub per categoria.
        // Questi nomi sono placeholder coerenti con Ruflo v3.5 spec.
        let seed: Vec<NexusToolSpec> = vec![
            // Code analysis
            NexusToolSpec::new(
                "ast_parse",
                NexusToolCategory::CodeAnalysis,
                "Parse source into AST (Rust/TS/Python)",
            ),
            NexusToolSpec::new(
                "ast_query",
                NexusToolCategory::CodeAnalysis,
                "Query AST nodes by type/name",
            ),
            // Code quality
            NexusToolSpec::new(
                "lint_run",
                NexusToolCategory::CodeQuality,
                "Run linters (clippy, eslint, ruff)",
            ),
            NexusToolSpec::new(
                "format_code",
                NexusToolCategory::CodeQuality,
                "Apply formatter (rustfmt, prettier, black)",
            ),
            // Testing
            NexusToolSpec::new(
                "test_generate",
                NexusToolCategory::Testing,
                "Generate test cases from function signature",
            ),
            NexusToolSpec::new(
                "coverage_report",
                NexusToolCategory::Testing,
                "Compute code coverage",
            ),
            // Security
            NexusToolSpec::new(
                "sast_scan",
                NexusToolCategory::Security,
                "Static security analysis",
            ),
            NexusToolSpec::new(
                "secret_scan",
                NexusToolCategory::Security,
                "Detect secrets / credentials",
            ),
            NexusToolSpec::new(
                "deps_audit",
                NexusToolCategory::Security,
                "Audit dependencies for known CVEs",
            ),
            // Refactoring
            NexusToolSpec::new(
                "rename_symbol",
                NexusToolCategory::Refactoring,
                "Rename a symbol across the project",
            ),
            NexusToolSpec::new(
                "extract_function",
                NexusToolCategory::Refactoring,
                "Extract selection into new function",
            ),
            // Documentation
            NexusToolSpec::new(
                "doc_generate",
                NexusToolCategory::Documentation,
                "Generate docstrings from code",
            ),
            // Dependencies
            NexusToolSpec::new(
                "deps_tree",
                NexusToolCategory::Dependencies,
                "Show dependency tree",
            ),
            // Build
            NexusToolSpec::new(
                "build_project",
                NexusToolCategory::Build,
                "Run the project build pipeline",
            ),
            // VCS
            NexusToolSpec::new(
                "git_diff",
                NexusToolCategory::Vcs,
                "Show git diff",
            ),
            NexusToolSpec::new(
                "git_blame",
                NexusToolCategory::Vcs,
                "Show git blame for a file",
            ),
            // GitHub
            NexusToolSpec::new(
                "gh_pr_create",
                NexusToolCategory::GitHub,
                "Create a GitHub pull request",
            ),
            NexusToolSpec::new(
                "gh_issue_list",
                NexusToolCategory::GitHub,
                "List GitHub issues",
            ),
            // Performance
            NexusToolSpec::new(
                "profile_run",
                NexusToolCategory::Performance,
                "Profile a function / endpoint",
            ),
            NexusToolSpec::new(
                "bench_run",
                NexusToolCategory::Performance,
                "Run benchmarks",
            ),
            // Database
            NexusToolSpec::new(
                "db_schema_inspect",
                NexusToolCategory::Database,
                "Inspect database schema",
            ),
            NexusToolSpec::new(
                "db_query_explain",
                NexusToolCategory::Database,
                "Explain query plan",
            ),
            // API
            NexusToolSpec::new(
                "openapi_validate",
                NexusToolCategory::Api,
                "Validate OpenAPI spec",
            ),
            // Deployment
            NexusToolSpec::new(
                "deploy_check",
                NexusToolCategory::Deployment,
                "Pre-deploy readiness checks",
            ),
            // Memory
            NexusToolSpec::new(
                "memory_ns_read",
                NexusToolCategory::Memory,
                "Read from a memory namespace",
            ),
            NexusToolSpec::new(
                "memory_ns_write",
                NexusToolCategory::Memory,
                "Write into a memory namespace",
            ),
            // Utility
            NexusToolSpec::new(
                "regex_match",
                NexusToolCategory::Utility,
                "Run a regex over text",
            ),
        ];

        for spec in seed {
            catalog.register(spec);
        }

        catalog
    }

    /// Registra un tool SENZA handler (scaffold stub). Idempotente.
    pub fn register(&self, spec: NexusToolSpec) {
        let name = spec.name.clone();
        self.tools.insert(
            name,
            NexusToolEntry {
                spec,
                handler: None,
            },
        );
    }

    /// Registra un tool CON handler eseguibile. Marca automaticamente
    /// `spec.implemented = true`.
    ///
    /// Fase 9A: questa è l'API usata dai 20 handler reali. Sovrascrive
    /// eventuali stub pre-esistenti con lo stesso nome, unificando la
    /// rappresentazione (prima scaffold, ora eseguibile).
    pub fn register_with_handler(&self, mut spec: NexusToolSpec, handler: SharedHandler) {
        spec.implemented = true;
        let name = spec.name.clone();
        self.tools.insert(
            name,
            NexusToolEntry {
                spec,
                handler: Some(handler),
            },
        );
    }

    /// Recupera la spec di un tool per nome (compat. con versione Fase 6).
    #[allow(dead_code)]
    pub fn get(&self, name: &str) -> Option<NexusToolSpec> {
        self.tools.get(name).map(|r| r.spec.clone())
    }

    /// Recupera l'handler eseguibile di un tool, se presente.
    ///
    /// Ritorna `None` se il tool non esiste o è ancora un scaffold stub.
    pub fn get_handler(&self, name: &str) -> Option<SharedHandler> {
        self.tools
            .get(name)
            .and_then(|r| r.handler.as_ref().cloned())
    }

    /// Esegue un tool per nome. Facciata sul trait `NexusToolHandler`.
    ///
    /// Errori specifici:
    /// - `NexusToolError::BadInput("unknown tool: ...")` se il nome non
    ///   corrisponde a nessun tool registrato;
    /// - `NexusToolError::BadInput("tool is stub-only: ...")` se il tool
    ///   esiste come spec ma non ha handler associato.
    pub async fn execute(
        &self,
        name: &str,
        ctx: &NexusToolContext,
        args: &Value,
    ) -> Result<Value, NexusToolError> {
        let handler = match self.get_handler(name) {
            Some(h) => h,
            None => {
                // Distinguo tool inesistente vs stub-only per diagnostica
                if self.tools.contains_key(name) {
                    return Err(NexusToolError::BadInput(format!(
                        "tool is stub-only: {}",
                        name
                    )));
                } else {
                    return Err(NexusToolError::BadInput(format!("unknown tool: {}", name)));
                }
            }
        };
        handler.execute(ctx, args).await
    }

    /// Lista tool di una specifica categoria
    pub fn list_by_category(&self, category: NexusToolCategory) -> Vec<NexusToolSpec> {
        self.tools
            .iter()
            .filter(|r| r.spec.category == category)
            .map(|r| r.spec.clone())
            .collect()
    }

    /// Numero totale di tool registrati
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Numero di tool effettivamente implementati.
    ///
    /// Un tool è considerato implementato se ha un handler associato
    /// (registrato tramite `register_with_handler`). Il vecchio flag
    /// `spec.implemented` è mantenuto per retrocompatibilità ma il
    /// conteggio di verità arriva dalla presenza dell'handler.
    pub fn implemented_count(&self) -> usize {
        self.tools.iter().filter(|r| r.handler.is_some()).count()
    }

    /// Breakdown per categoria — utile per diagnostica
    pub fn breakdown(&self) -> Vec<(NexusToolCategory, usize)> {
        NexusToolCategory::all()
            .iter()
            .map(|c| (*c, self.list_by_category(*c).len()))
            .collect()
    }
}

impl Default for NexusToolCatalog {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Singleton globale del catalogo
static NEXUS_TOOL_CATALOG: OnceLock<Arc<NexusToolCatalog>> = OnceLock::new();

impl NexusToolCatalog {
    /// Inizializza il singleton globale (idempotent).
    ///
    /// Fase 9A: dopo aver caricato i 27 stub del seed, registra anche gli
    /// handler eseguibili del blocco "prototype" (cargo_check + git_status).
    /// Gli stub pre-esistenti con lo stesso nome vengono sovrascritti con la
    /// versione eseguibile tramite `register_with_handler`.
    pub fn init_global() {
        NEXUS_TOOL_CATALOG.get_or_init(|| {
            let catalog = Self::with_builtins();
            catalog.register_fase9_handlers();
            Arc::new(catalog)
        });
    }

    /// Accesso al singleton
    pub fn global() -> Option<Arc<Self>> {
        NEXUS_TOOL_CATALOG.get().cloned()
    }

    /// Registra gli handler eseguibili di Fase 9A.
    ///
    /// Questo è il punto di estensione per promuovere stub a tool reali:
    /// ogni handler definisce il proprio `NexusToolSpec` con `name` allineato
    /// (quando presente) a quello del seed, in modo che il conteggio totale
    /// resti stabile e l'API `/nexus/tools` mostri il tool una volta sola.
    ///
    /// Fase 9A totale: **20 tool eseguibili** distribuiti su 7 categorie.
    /// Fase 9C: **+6 handler** (format_code, deploy_check, gh_issue_list,
    /// memory_ns_read/write, regex_match).
    /// Fase 9D: **+18 handler** (ast_parse, ast_query, lint_run,
    /// test_generate, coverage_report, sast_scan, deps_audit, rename_symbol,
    /// extract_function, doc_generate, deps_tree, build_project, gh_pr_create,
    /// profile_run, bench_run, db_schema_inspect, db_query_explain,
    /// openapi_validate). Totale: **44** implementati su 44 stub del seed.
    /// Fase 9E: **+4 handler nuovi** (ruvector_insert, ruvector_search,
    /// ruvector_stats, consensus_vote) — non mappano su stub preesistenti,
    /// portano il totale a **48** tool eseguibili.
    /// Fase 9F: **+20 handler nuovi** (fs_read, fs_list, fs_grep, fs_tree,
    /// json_parse, json_get, base64_encode, base64_decode, hash_content,
    /// uuid_generate, git_branch_list, git_remote_list, git_show,
    /// git_tag_list, gh_workflow_list, gh_run_list, gh_release_list,
    /// count_loc, find_todos, cargo_fmt_check). Totale a **68**.
    /// Fase 9G: **+20 handler nuovi** — Utility (fs_write, fs_stat, fs_glob,
    /// env_get, time_now, regex_replace, text_diff, uuid_parse), VCS
    /// (git_stash_list, git_grep, git_describe, git_shortlog), GitHub
    /// (gh_pr_list, gh_pr_view, gh_repo_view), Cargo (cargo_doc,
    /// cargo_locate_project, cargo_pkgid), CodeAnalysis (find_unsafe,
    /// find_pubapi). Totale a **88**.
    /// Fase 9H: **+20 handler Cargo extras** — cargo_run, cargo_install_list,
    /// cargo_search, cargo_publish_dry, cargo_check_release,
    /// cargo_check_all_features, cargo_test_doc, cargo_test_lib,
    /// cargo_features_list, cargo_targets_list, cargo_workspace_members,
    /// cargo_dep_versions, cargo_lockfile_check, cargo_msrv_detect,
    /// cargo_edition_detect, cargo_env_overrides, cargo_build_artifact_check,
    /// cargo_clean_dry, cargo_size_estimate, cargo_doc_check. Totale a **108**.
    /// Fase 9I: **+20 handler Git extras** — git_rev_parse, git_count_objects,
    /// git_reflog, git_clean_dry, git_check_ignore, git_ls_files, git_ls_tree,
    /// git_cat_file, git_for_each_ref, git_merge_base, git_diff_stat,
    /// git_log_graph, git_show_branch, git_archive_dry, git_bundle_verify,
    /// git_fsck, git_gc_dry, git_config_list, git_worktree_list,
    /// git_submodule_list. Totale a **128**.
    /// Fase 9J: **+20 handler GitHub extras** — gh_issue_view, gh_issue_create,
    /// gh_issue_close, gh_issue_comment, gh_pr_close, gh_pr_merge, gh_pr_review,
    /// gh_pr_diff, gh_pr_checks, gh_pr_files, gh_workflow_view, gh_workflow_run,
    /// gh_run_view, gh_run_logs, gh_run_cancel, gh_release_view,
    /// gh_release_create, gh_repo_clone_url, gh_repo_fork_list, gh_label_list.
    /// Totale a **148**.
    /// Fase 9K: **+20 handler Database extras** — db_ping, db_table_list,
    /// db_table_count, db_index_list, db_view_list, db_role_list,
    /// db_extension_list, db_size, db_connection_info, db_migration_list,
    /// db_seq_list, db_foreign_keys, db_unused_indexes, db_dead_tuples,
    /// db_bloat_check, db_table_size, db_constraint_list, db_lock_list,
    /// db_active_queries, db_replication_status. Totale a **168**.
    /// Fase 9L: **+20 handler Documentation extras** — doc_readme_check,
    /// doc_changelog_check, doc_license_detect, doc_codeowners_check,
    /// doc_contributing_check, doc_security_md_check, doc_toc_extract,
    /// doc_links_extract, doc_word_count, doc_link_check_local,
    /// doc_image_list, doc_frontmatter_parse, doc_md_lint, doc_orphan_md,
    /// doc_size_report, doc_heading_depth, doc_codeblocks_extract,
    /// doc_codeblocks_count, doc_api_list, doc_examples_list. Totale a **188**.
    /// Fase 9M: **+20 handler Performance extras** — perf_cargo_build_time,
    /// perf_binary_size, perf_cargo_bloat, perf_target_dir_size,
    /// perf_largest_files, perf_loc_per_crate, perf_unused_deps,
    /// perf_test_count, perf_async_funcs, perf_unsafe_blocks,
    /// perf_panic_count, perf_clone_count, perf_string_alloc, perf_box_count,
    /// perf_arc_mutex, perf_dep_count, perf_compile_units,
    /// perf_optimization_check, perf_lto_check, perf_codegen_units. Totale a **208**.
    /// Fase 9N: **+20 handler Testing extras** — test_run_unit, test_run_integration,
    /// test_run_quiet, test_run_workspace, test_count_files, test_ignored_count,
    /// test_should_panic_count, test_module_count, test_assert_count,
    /// test_proptest_count, test_quickcheck_count, test_mock_count,
    /// test_bench_count, test_doc_count, test_fixtures_list, test_snapshots_list,
    /// test_stale_snapshots, test_coverage_summary, test_failed_log,
    /// test_workflow_files. Totale a **228**.
    /// Fase 9O: **+20 handler Security extras** — sec_secret_patterns,
    /// sec_unwrap_count, sec_panic_count, sec_env_var_check,
    /// sec_http_url_count, sec_localhost_count, sec_eval_check,
    /// sec_sql_injection_check, sec_cmd_injection_check, sec_dependency_count,
    /// sec_git_secrets_check, sec_env_files_check, sec_dockerfile_user_check,
    /// sec_workflow_perms_check, sec_cors_check, sec_jwt_secret_check,
    /// sec_md5_sha1_check, sec_random_check, sec_tls_check,
    /// sec_audit_summary. Totale a **248**.
    /// Fase 9P: **+20 handler Code Analysis extras** — ca_struct_count,
    /// ca_enum_count, ca_trait_count, ca_impl_count, ca_fn_count,
    /// ca_pub_fn_count, ca_macro_count, ca_use_count, ca_mod_count,
    /// ca_lifetime_count, ca_generic_count, ca_derive_count, ca_attr_count,
    /// ca_doc_comment_count, ca_inline_comment_count, ca_todo_fixme_count,
    /// ca_match_count, ca_if_let_count, ca_while_let_count,
    /// ca_complexity_estimate. Totale a **268**.
    /// Fase 9Q: **+21 handler Build / Deploy** — build_target_list,
    /// build_artifact_age, build_release_size, build_debug_size,
    /// build_incremental_dir, build_lockfile_age, build_log_tail,
    /// build_rerun_checks, build_script_count, build_workspace_check,
    /// build_profile_list, deploy_dockerfile_count, deploy_compose_check,
    /// deploy_k8s_check, deploy_helm_check, deploy_terraform_check,
    /// deploy_ansible_check, deploy_systemd_check, deploy_nginx_check,
    /// deploy_env_files_count, deploy_release_artifacts. Totale a **289**.
    /// Fase 9R: **+20 handler API / Memory / Other** — api_openapi_files,
    /// api_route_count, api_handler_count, api_endpoint_list, api_graphql_check,
    /// api_grpc_check, api_postman_check, api_middleware_count,
    /// memory_namespace_count, memory_size_estimate, memory_pattern_list,
    /// memory_recent_writes, memory_topkeys, memory_evict_stats,
    /// util_disk_free, util_uptime, util_hostname, util_cpu_count,
    /// util_now_iso, util_pid. Totale a **309**.
    /// Fase 9S: **+5 handler Final meta** — meta_catalog_count,
    /// meta_categories_list, meta_version_info, meta_health_summary,
    /// meta_self_test. Totale a **314** (target finale).
    fn register_fase9_handlers(&self) {
        use crate::nexus_tools::{
            api_endpoint_list::ApiEndpointListTool,
            api_graphql_check::ApiGraphqlCheckTool,
            api_grpc_check::ApiGrpcCheckTool,
            api_handler_count::ApiHandlerCountTool,
            api_middleware_count::ApiMiddlewareCountTool,
            api_openapi_files::ApiOpenapiFilesTool,
            api_postman_check::ApiPostmanCheckTool,
            api_route_count::ApiRouteCountTool,
            ast_parse::AstParseTool, ast_query::AstQueryTool,
            base64_decode::Base64DecodeTool, base64_encode::Base64EncodeTool,
            bench_run::BenchRunTool,
            build_artifact_age::BuildArtifactAgeTool,
            build_debug_size::BuildDebugSizeTool,
            build_incremental_dir::BuildIncrementalDirTool,
            build_lockfile_age::BuildLockfileAgeTool,
            build_log_tail::BuildLogTailTool,
            build_profile_list::BuildProfileListTool,
            build_project::BuildProjectTool,
            build_release_size::BuildReleaseSizeTool,
            build_rerun_checks::BuildRerunChecksTool,
            build_script_count::BuildScriptCountTool,
            build_target_list::BuildTargetListTool,
            build_workspace_check::BuildWorkspaceCheckTool,
            ca_attr_count::CaAttrCountTool,
            ca_complexity_estimate::CaComplexityEstimateTool,
            ca_derive_count::CaDeriveCountTool,
            ca_doc_comment_count::CaDocCommentCountTool,
            ca_enum_count::CaEnumCountTool,
            ca_fn_count::CaFnCountTool,
            ca_generic_count::CaGenericCountTool,
            ca_if_let_count::CaIfLetCountTool,
            ca_impl_count::CaImplCountTool,
            ca_inline_comment_count::CaInlineCommentCountTool,
            ca_lifetime_count::CaLifetimeCountTool,
            ca_macro_count::CaMacroCountTool,
            ca_match_count::CaMatchCountTool,
            ca_mod_count::CaModCountTool,
            ca_pub_fn_count::CaPubFnCountTool,
            ca_struct_count::CaStructCountTool,
            ca_todo_fixme_count::CaTodoFixmeCountTool,
            ca_trait_count::CaTraitCountTool,
            ca_use_count::CaUseCountTool,
            ca_while_let_count::CaWhileLetCountTool,
            cargo_audit::CargoAuditTool, cargo_bench::CargoBenchTool,
            cargo_build::CargoBuildTool,
            cargo_build_artifact_check::CargoBuildArtifactCheckTool,
            cargo_check::CargoCheckTool,
            cargo_check_all_features::CargoCheckAllFeaturesTool,
            cargo_check_release::CargoCheckReleaseTool,
            cargo_clean::CargoCleanTool, cargo_clean_dry::CargoCleanDryTool,
            cargo_dep_versions::CargoDepVersionsTool,
            cargo_doc::CargoDocTool, cargo_doc_check::CargoDocCheckTool,
            cargo_edition_detect::CargoEditionDetectTool,
            cargo_env_overrides::CargoEnvOverridesTool,
            cargo_features_list::CargoFeaturesListTool,
            cargo_fmt_check::CargoFmtCheckTool,
            cargo_install_list::CargoInstallListTool,
            cargo_locate_project::CargoLocateProjectTool,
            cargo_lockfile_check::CargoLockfileCheckTool,
            cargo_metadata::CargoMetadataTool,
            cargo_msrv_detect::CargoMsrvDetectTool,
            cargo_outdated::CargoOutdatedTool, cargo_pkgid::CargoPkgidTool,
            cargo_publish_dry::CargoPublishDryTool,
            cargo_run::CargoRunTool, cargo_search::CargoSearchTool, shell_exec::ShellExecTool,
            cargo_size_estimate::CargoSizeEstimateTool,
            cargo_targets_list::CargoTargetsListTool,
            cargo_test::CargoTestTool,
            cargo_test_doc::CargoTestDocTool,
            cargo_test_lib::CargoTestLibTool,
            cargo_tree::CargoTreeTool,
            cargo_update::CargoUpdateTool,
            cargo_workspace_members::CargoWorkspaceMembersTool,
            clippy_lint::ClippyLintTool,
            consensus_vote::ConsensusVoteTool, count_loc::CountLocTool,
            coverage_report::CoverageReportTool,
            db_active_queries::DbActiveQueriesTool,
            db_bloat_check::DbBloatCheckTool,
            db_connection_info::DbConnectionInfoTool,
            db_constraint_list::DbConstraintListTool,
            db_dead_tuples::DbDeadTuplesTool,
            db_extension_list::DbExtensionListTool,
            db_foreign_keys::DbForeignKeysTool,
            db_index_list::DbIndexListTool,
            db_lock_list::DbLockListTool,
            db_migration_list::DbMigrationListTool,
            db_ping::DbPingTool,
            db_query_explain::DbQueryExplainTool,
            db_replication_status::DbReplicationStatusTool,
            db_role_list::DbRoleListTool,
            db_schema_inspect::DbSchemaInspectTool,
            db_seq_list::DbSeqListTool,
            db_size::DbSizeTool,
            db_table_count::DbTableCountTool,
            db_table_list::DbTableListTool,
            db_table_size::DbTableSizeTool,
            db_unused_indexes::DbUnusedIndexesTool,
            db_view_list::DbViewListTool,
            doc_api_list::DocApiListTool,
            doc_changelog_check::DocChangelogCheckTool,
            doc_codeblocks_count::DocCodeblocksCountTool,
            doc_codeblocks_extract::DocCodeblocksExtractTool,
            doc_codeowners_check::DocCodeownersCheckTool,
            doc_contributing_check::DocContributingCheckTool,
            doc_examples_list::DocExamplesListTool,
            doc_frontmatter_parse::DocFrontmatterParseTool,
            doc_heading_depth::DocHeadingDepthTool,
            doc_image_list::DocImageListTool,
            doc_license_detect::DocLicenseDetectTool,
            doc_link_check_local::DocLinkCheckLocalTool,
            doc_links_extract::DocLinksExtractTool,
            doc_md_lint::DocMdLintTool,
            doc_orphan_md::DocOrphanMdTool,
            doc_readme_check::DocReadmeCheckTool,
            doc_security_md_check::DocSecurityMdCheckTool,
            doc_size_report::DocSizeReportTool,
            doc_toc_extract::DocTocExtractTool,
            doc_word_count::DocWordCountTool,
            perf_arc_mutex::PerfArcMutexTool,
            perf_async_funcs::PerfAsyncFuncsTool,
            perf_binary_size::PerfBinarySizeTool,
            perf_box_count::PerfBoxCountTool,
            perf_cargo_bloat::PerfCargoBloatTool,
            perf_cargo_build_time::PerfCargoBuildTimeTool,
            perf_clone_count::PerfCloneCountTool,
            perf_codegen_units::PerfCodegenUnitsTool,
            perf_compile_units::PerfCompileUnitsTool,
            perf_dep_count::PerfDepCountTool,
            perf_largest_files::PerfLargestFilesTool,
            perf_loc_per_crate::PerfLocPerCrateTool,
            perf_lto_check::PerfLtoCheckTool,
            perf_optimization_check::PerfOptimizationCheckTool,
            perf_panic_count::PerfPanicCountTool,
            perf_string_alloc::PerfStringAllocTool,
            perf_target_dir_size::PerfTargetDirSizeTool,
            perf_test_count::PerfTestCountTool,
            perf_unsafe_blocks::PerfUnsafeBlocksTool,
            perf_unused_deps::PerfUnusedDepsTool,
            deploy_ansible_check::DeployAnsibleCheckTool,
            deploy_check::DeployCheckTool,
            deploy_compose_check::DeployComposeCheckTool,
            deploy_dockerfile_count::DeployDockerfileCountTool,
            deploy_env_files_count::DeployEnvFilesCountTool,
            deploy_helm_check::DeployHelmCheckTool,
            deploy_k8s_check::DeployK8sCheckTool,
            deploy_nginx_check::DeployNginxCheckTool,
            deploy_release_artifacts::DeployReleaseArtifactsTool,
            deploy_systemd_check::DeploySystemdCheckTool,
            deploy_terraform_check::DeployTerraformCheckTool,
            deps_audit::DepsAuditTool,
            deps_tree::DepsTreeTool, doc_generate::DocGenerateTool,
            env_get::EnvGetTool, extract_function::ExtractFunctionTool,
            find_pubapi::FindPubApiTool, find_todos::FindTodosTool,
            find_unsafe::FindUnsafeTool, format_code::FormatCodeTool,
            fs_glob::FsGlobTool, fs_grep::FsGrepTool, fs_list::FsListTool,
            fs_read::FsReadTool, fs_stat::FsStatTool, fs_tree::FsTreeTool,
            fs_write::FsWriteTool,
            gh_issue_close::GhIssueCloseTool,
            gh_issue_comment::GhIssueCommentTool,
            gh_issue_create::GhIssueCreateTool,
            gh_issue_list::GhIssueListTool,
            gh_issue_view::GhIssueViewTool,
            gh_label_list::GhLabelListTool,
            gh_pr_checks::GhPrChecksTool,
            gh_pr_close::GhPrCloseTool,
            gh_pr_create::GhPrCreateTool,
            gh_pr_diff::GhPrDiffTool,
            gh_pr_files::GhPrFilesTool,
            gh_pr_list::GhPrListTool,
            gh_pr_merge::GhPrMergeTool,
            gh_pr_review::GhPrReviewTool,
            gh_pr_view::GhPrViewTool,
            gh_release_create::GhReleaseCreateTool,
            gh_release_list::GhReleaseListTool,
            gh_release_view::GhReleaseViewTool,
            gh_repo_clone_url::GhRepoCloneUrlTool,
            gh_repo_fork_list::GhRepoForkListTool,
            gh_repo_view::GhRepoViewTool,
            gh_run_cancel::GhRunCancelTool,
            gh_run_list::GhRunListTool,
            gh_run_logs::GhRunLogsTool,
            gh_run_view::GhRunViewTool,
            gh_workflow_list::GhWorkflowListTool,
            gh_workflow_run::GhWorkflowRunTool,
            gh_workflow_view::GhWorkflowViewTool,
            git_archive_dry::GitArchiveDryTool,
            git_blame::GitBlameTool, git_branch_list::GitBranchListTool,
            git_bundle_verify::GitBundleVerifyTool,
            git_cat_file::GitCatFileTool,
            git_check_ignore::GitCheckIgnoreTool,
            git_clean_dry::GitCleanDryTool,
            git_config_list::GitConfigListTool,
            git_count_objects::GitCountObjectsTool,
            git_describe::GitDescribeTool, git_diff::GitDiffTool,
            git_diff_stat::GitDiffStatTool,
            git_for_each_ref::GitForEachRefTool,
            git_fsck::GitFsckTool,
            git_gc_dry::GitGcDryTool,
            git_grep::GitGrepTool, git_log::GitLogTool,
            git_log_graph::GitLogGraphTool,
            git_ls_files::GitLsFilesTool,
            git_ls_tree::GitLsTreeTool,
            git_merge_base::GitMergeBaseTool,
            git_reflog::GitReflogTool,
            git_remote_list::GitRemoteListTool,
            git_rev_parse::GitRevParseTool,
            git_shortlog::GitShortlogTool,
            git_show::GitShowTool, git_show_branch::GitShowBranchTool,
            git_stash_list::GitStashListTool,
            git_status::GitStatusTool,
            git_submodule_list::GitSubmoduleListTool,
            git_tag_list::GitTagListTool,
            git_worktree_list::GitWorktreeListTool,
            hash_content::HashContentTool, json_get::JsonGetTool,
            json_parse::JsonParseTool, license_check::LicenseCheckTool,
            lint_run::LintRunTool,
            memory_evict_stats::MemoryEvictStatsTool,
            memory_namespace_count::MemoryNamespaceCountTool,
            meta_catalog_count::MetaCatalogCountTool,
            meta_categories_list::MetaCategoriesListTool,
            meta_health_summary::MetaHealthSummaryTool,
            meta_self_test::MetaSelfTestTool,
            meta_version_info::MetaVersionInfoTool,
            memory_ns::{MemoryNsReadTool, MemoryNsWriteTool},
            memory_pattern_list::MemoryPatternListTool,
            memory_recent_writes::MemoryRecentWritesTool,
            memory_size_estimate::MemorySizeEstimateTool,
            memory_topkeys::MemoryTopkeysTool,
            openapi_validate::OpenApiValidateTool,
            profile_run::ProfileRunTool, regex_match::RegexMatchTool,
            regex_replace::RegexReplaceTool,
            rename_symbol::RenameSymbolTool,
            ruvector_insert::RuVectorInsertTool,
            ruvector_search::RuVectorSearchTool,
            ruvector_stats::RuVectorStatsTool,
            rustc_explain::RustcExplainTool,
            rustc_version::RustcVersionTool, sast_scan::SastScanTool,
            sec_audit_summary::SecAuditSummaryTool,
            sec_cmd_injection_check::SecCmdInjectionCheckTool,
            sec_cors_check::SecCorsCheckTool,
            sec_dependency_count::SecDependencyCountTool,
            sec_dockerfile_user_check::SecDockerfileUserCheckTool,
            sec_env_files_check::SecEnvFilesCheckTool,
            sec_env_var_check::SecEnvVarCheckTool,
            sec_eval_check::SecEvalCheckTool,
            sec_git_secrets_check::SecGitSecretsCheckTool,
            sec_http_url_count::SecHttpUrlCountTool,
            sec_jwt_secret_check::SecJwtSecretCheckTool,
            sec_localhost_count::SecLocalhostCountTool,
            sec_md5_sha1_check::SecMd5Sha1CheckTool,
            sec_panic_count::SecPanicCountTool,
            sec_random_check::SecRandomCheckTool,
            sec_secret_patterns::SecSecretPatternsTool,
            sec_sql_injection_check::SecSqlInjectionCheckTool,
            sec_tls_check::SecTlsCheckTool,
            sec_unwrap_count::SecUnwrapCountTool,
            sec_workflow_perms_check::SecWorkflowPermsCheckTool,
            secret_scan::SecretScanTool,
            test_assert_count::TestAssertCountTool,
            test_bench_count::TestBenchCountTool,
            test_count_files::TestCountFilesTool,
            test_coverage::TestCoverageTool,
            test_coverage_summary::TestCoverageSummaryTool,
            test_doc_count::TestDocCountTool,
            test_failed_log::TestFailedLogTool,
            test_fixtures_list::TestFixturesListTool,
            test_generate::TestGenerateTool,
            test_ignored_count::TestIgnoredCountTool,
            test_mock_count::TestMockCountTool,
            test_module_count::TestModuleCountTool,
            test_proptest_count::TestProptestCountTool,
            test_quickcheck_count::TestQuickcheckCountTool,
            test_playwright::TestPlaywrightTool,
            test_run_integration::TestRunIntegrationTool,
            test_run_quiet::TestRunQuietTool,
            test_run_unit::TestRunUnitTool,
            test_run_workspace::TestRunWorkspaceTool,
            test_should_panic_count::TestShouldPanicCountTool,
            test_snapshots_list::TestSnapshotsListTool,
            test_stale_snapshots::TestStaleSnapshotsTool,
            test_workflow_files::TestWorkflowFilesTool,
            text_diff::TextDiffTool,
            time_now::TimeNowTool,
            util_cpu_count::UtilCpuCountTool,
            util_disk_free::UtilDiskFreeTool,
            util_hostname::UtilHostnameTool,
            util_now_iso::UtilNowIsoTool,
            util_pid::UtilPidTool,
            util_uptime::UtilUptimeTool,
            uuid_generate::UuidGenerateTool,
            uuid_parse::UuidParseTool,
        };

        // ── CodeAnalysis ─────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_check",
                NexusToolCategory::CodeAnalysis,
                "Run `cargo check --message-format=json` and parse errors/warnings",
            ),
            Arc::new(CargoCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_metadata",
                NexusToolCategory::CodeAnalysis,
                "Run `cargo metadata --format-version=1` and return workspace graph",
            ),
            Arc::new(CargoMetadataTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "rustc_version",
                NexusToolCategory::CodeAnalysis,
                "Run `rustc --version --verbose` and parse toolchain info",
            ),
            Arc::new(RustcVersionTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "rustc_explain",
                NexusToolCategory::CodeAnalysis,
                "Run `rustc --explain Exxxx` for a given error code",
            ),
            Arc::new(RustcExplainTool),
        );

        // ── CodeQuality ──────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "clippy_lint",
                NexusToolCategory::CodeQuality,
                "Run `cargo clippy --message-format=json` and parse lints",
            ),
            Arc::new(ClippyLintTool),
        );

        // ── Testing ──────────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_test",
                NexusToolCategory::Testing,
                "Run `cargo test --no-fail-fast` and parse pass/fail counts",
            ),
            Arc::new(CargoTestTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "test_coverage",
                NexusToolCategory::Testing,
                "Run `cargo llvm-cov --json --summary-only` and summarize coverage",
            ),
            Arc::new(TestCoverageTool),
        );

        // ── Security ─────────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_audit",
                NexusToolCategory::Security,
                "Run `cargo audit --json` and summarize RUSTSEC advisories",
            ),
            Arc::new(CargoAuditTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "secret_scan",
                NexusToolCategory::Security,
                "Scan project files for hardcoded secrets (regex-based)",
            ),
            Arc::new(SecretScanTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "license_check",
                NexusToolCategory::Security,
                "Analyze package licenses from cargo metadata",
            ),
            Arc::new(LicenseCheckTool),
        );

        // ── Dependencies ─────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_tree",
                NexusToolCategory::Dependencies,
                "Run `cargo tree` and return dependency tree",
            ),
            Arc::new(CargoTreeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_outdated",
                NexusToolCategory::Dependencies,
                "Run `cargo outdated --format json` and return outdated deps",
            ),
            Arc::new(CargoOutdatedTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_update",
                NexusToolCategory::Dependencies,
                "Run `cargo update` to refresh Cargo.lock",
            ),
            Arc::new(CargoUpdateTool),
        );

        // ── Build ────────────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_build",
                NexusToolCategory::Build,
                "Run `cargo build --message-format=json` and parse diagnostics",
            ),
            Arc::new(CargoBuildTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_clean",
                NexusToolCategory::Build,
                "Run `cargo clean` to remove target directory",
            ),
            Arc::new(CargoCleanTool),
        );

        // ── Performance ──────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_bench",
                NexusToolCategory::Performance,
                "Run `cargo bench` and count benchmark entries",
            ),
            Arc::new(CargoBenchTool),
        );

        // ── Vcs ──────────────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "git_status",
                NexusToolCategory::Vcs,
                "Run `git status --porcelain=v2 --branch` and return structured state",
            ),
            Arc::new(GitStatusTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_log",
                NexusToolCategory::Vcs,
                "Run `git log` with structured format and parse commits",
            ),
            Arc::new(GitLogTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_diff",
                NexusToolCategory::Vcs,
                "Run `git diff` with --stat and return structured diff",
            ),
            Arc::new(GitDiffTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_blame",
                NexusToolCategory::Vcs,
                "Run `git blame --porcelain` and parse per-line authorship",
            ),
            Arc::new(GitBlameTool),
        );

        // ── CodeQuality (Fase 9C) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "format_code",
                NexusToolCategory::CodeQuality,
                "Run `cargo fmt [--check]` and list files changed",
            ),
            Arc::new(FormatCodeTool),
        );

        // ── Deployment (Fase 9C) ─────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "deploy_check",
                NexusToolCategory::Deployment,
                "Pre-deploy readiness audit (uncommitted, upstream, deploy files, env, lockfiles)",
            ),
            Arc::new(DeployCheckTool),
        );

        // ── GitHub (Fase 9C) ─────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_issue_list",
                NexusToolCategory::GitHub,
                "Run `gh issue list --json` and return parsed issues",
            ),
            Arc::new(GhIssueListTool),
        );

        // ── Memory (Fase 9C) ─────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "memory_ns_read",
                NexusToolCategory::Memory,
                "Read a key from the project-scoped NexusBridge memory namespace",
            ),
            Arc::new(MemoryNsReadTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "memory_ns_write",
                NexusToolCategory::Memory,
                "Write a JSON value into the project-scoped NexusBridge memory namespace",
            ),
            Arc::new(MemoryNsWriteTool),
        );

        // ── Utility (Fase 9C) ────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "regex_match",
                NexusToolCategory::Utility,
                "Run a regex over inline text or a project file and return matches",
            ),
            Arc::new(RegexMatchTool),
        );

        // ══════════════════════════════════════════════════════════════════
        //                         FASE 9D — 18 stub handlers
        // ══════════════════════════════════════════════════════════════════

        // ── CodeAnalysis (Fase 9D) ───────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "ast_parse",
                NexusToolCategory::CodeAnalysis,
                "Parse source into AST via mcp-ast (Rust/TS/JS/Python/Go/Java)",
            ),
            Arc::new(AstParseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "ast_query",
                NexusToolCategory::CodeAnalysis,
                "Query AST symbols by kind/name_pattern/visibility",
            ),
            Arc::new(AstQueryTool),
        );

        // ── CodeQuality (Fase 9D) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "lint_run",
                NexusToolCategory::CodeQuality,
                "Multi-language linter dispatcher (clippy / eslint / ruff / flake8)",
            ),
            Arc::new(LintRunTool),
        );

        // ── Testing (Fase 9D) ────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "test_generate",
                NexusToolCategory::Testing,
                "Scaffold unit tests from function signatures (mcp-ast based)",
            ),
            Arc::new(TestGenerateTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "coverage_report",
                NexusToolCategory::Testing,
                "Multi-stack coverage dispatcher (cargo llvm-cov / npm coverage)",
            ),
            Arc::new(CoverageReportTool),
        );

        // ── Security (Fase 9D) ───────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "sast_scan",
                NexusToolCategory::Security,
                "SAST scan via semgrep if available, else built-in regex rules",
            ),
            Arc::new(SastScanTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "deps_audit",
                NexusToolCategory::Security,
                "Multi-stack dependency audit (cargo audit / npm audit / pip-audit)",
            ),
            Arc::new(DepsAuditTool),
        );

        // ── Refactoring (Fase 9D) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "rename_symbol",
                NexusToolCategory::Refactoring,
                "Rename a symbol within a single file (word-boundary regex)",
            ),
            Arc::new(RenameSymbolTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "extract_function",
                NexusToolCategory::Refactoring,
                "Mechanical extract-function scaffold for Rust/TS/JS/Python",
            ),
            Arc::new(ExtractFunctionTool),
        );

        // ── Documentation (Fase 9D) ──────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "api_docs",
                NexusToolCategory::Documentation,
                "Generate project API docs (cargo doc / npm docs / sphinx)",
            ),
            Arc::new(DocGenerateTool),
        );

        // ── Dependencies (Fase 9D) ───────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "deps_tree",
                NexusToolCategory::Dependencies,
                "Multi-stack dep tree (cargo tree / npm list / pipdeptree)",
            ),
            Arc::new(DepsTreeTool),
        );

        // ── Build (Fase 9D) ──────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "build_project",
                NexusToolCategory::Build,
                "Multi-stack build dispatcher (cargo / npm run build / make / python -m build)",
            ),
            Arc::new(BuildProjectTool),
        );

        // ── GitHub (Fase 9D) ─────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_pr_create",
                NexusToolCategory::GitHub,
                "Create a GitHub pull request via `gh pr create`",
            ),
            Arc::new(GhPrCreateTool),
        );

        // ── Performance (Fase 9D) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "profile_run",
                NexusToolCategory::Performance,
                "Wall-clock profiling with N runs and mean/min/max/p95 stats",
            ),
            Arc::new(ProfileRunTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "bench_run",
                NexusToolCategory::Performance,
                "Benchmark dispatcher (cargo bench / npm run bench)",
            ),
            Arc::new(BenchRunTool),
        );

        // ── Database (Fase 9D) ───────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "db_schema_inspect",
                NexusToolCategory::Database,
                "Inspect PostgreSQL schema via information_schema",
            ),
            Arc::new(DbSchemaInspectTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "db_query_explain",
                NexusToolCategory::Database,
                "EXPLAIN (VERBOSE, FORMAT JSON) for SELECT/WITH queries",
            ),
            Arc::new(DbQueryExplainTool),
        );

        // ── Database extras (Fase 9K, 20 new) ─────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("db_ping", NexusToolCategory::Database, "SELECT 1 connectivity test against DATABASE_URL"),
            Arc::new(DbPingTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_table_list", NexusToolCategory::Database, "List tables in a schema (default public)"),
            Arc::new(DbTableListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_table_count", NexusToolCategory::Database, "SELECT COUNT(*) for a specific table"),
            Arc::new(DbTableCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_index_list", NexusToolCategory::Database, "List indexes in a schema from pg_indexes"),
            Arc::new(DbIndexListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_view_list", NexusToolCategory::Database, "List views in a schema from pg_views"),
            Arc::new(DbViewListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_role_list", NexusToolCategory::Database, "List roles from pg_roles"),
            Arc::new(DbRoleListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_extension_list", NexusToolCategory::Database, "List installed extensions from pg_extension"),
            Arc::new(DbExtensionListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_size", NexusToolCategory::Database, "Total size of the current database (pg_database_size)"),
            Arc::new(DbSizeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_connection_info", NexusToolCategory::Database, "Current connection info (user, db, host, version)"),
            Arc::new(DbConnectionInfoTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_migration_list", NexusToolCategory::Database, "List .sql migration files under db/migrations or migrations"),
            Arc::new(DbMigrationListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_seq_list", NexusToolCategory::Database, "List sequences in a schema"),
            Arc::new(DbSeqListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_foreign_keys", NexusToolCategory::Database, "List foreign keys in a schema with referenced table/column"),
            Arc::new(DbForeignKeysTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_unused_indexes", NexusToolCategory::Database, "Indexes never scanned (idx_scan = 0) from pg_stat_user_indexes"),
            Arc::new(DbUnusedIndexesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_dead_tuples", NexusToolCategory::Database, "Top tables by dead tuples from pg_stat_user_tables"),
            Arc::new(DbDeadTuplesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_bloat_check", NexusToolCategory::Database, "Quick bloat estimate via dead/live ratio"),
            Arc::new(DbBloatCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_table_size", NexusToolCategory::Database, "Total + heap size for a specific table"),
            Arc::new(DbTableSizeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_constraint_list", NexusToolCategory::Database, "List constraints in a schema with type"),
            Arc::new(DbConstraintListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_lock_list", NexusToolCategory::Database, "Active locks from pg_locks joined with pg_stat_activity"),
            Arc::new(DbLockListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_active_queries", NexusToolCategory::Database, "Non-idle queries from pg_stat_activity"),
            Arc::new(DbActiveQueriesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("db_replication_status", NexusToolCategory::Database, "Replication status from pg_stat_replication"),
            Arc::new(DbReplicationStatusTool),
        );

        // ── Documentation extras (Fase 9L, 20 new) ────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("doc_readme_check", NexusToolCategory::Documentation, "Check README.md presence and minimal sections"),
            Arc::new(DocReadmeCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_changelog_check", NexusToolCategory::Documentation, "Check CHANGELOG.md presence and release count"),
            Arc::new(DocChangelogCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_license_detect", NexusToolCategory::Documentation, "Detect LICENSE file and license type"),
            Arc::new(DocLicenseDetectTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_codeowners_check", NexusToolCategory::Documentation, "Check CODEOWNERS file presence"),
            Arc::new(DocCodeownersCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_contributing_check", NexusToolCategory::Documentation, "Check CONTRIBUTING.md presence"),
            Arc::new(DocContributingCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_security_md_check", NexusToolCategory::Documentation, "Check SECURITY.md presence with contact/disclosure"),
            Arc::new(DocSecurityMdCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_toc_extract", NexusToolCategory::Documentation, "Extract markdown headings (table of contents)"),
            Arc::new(DocTocExtractTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_links_extract", NexusToolCategory::Documentation, "Extract markdown links from a file"),
            Arc::new(DocLinksExtractTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_word_count", NexusToolCategory::Documentation, "Count words/lines/chars in a markdown file"),
            Arc::new(DocWordCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_link_check_local", NexusToolCategory::Documentation, "Check that local links in a .md exist on disk"),
            Arc::new(DocLinkCheckLocalTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_image_list", NexusToolCategory::Documentation, "List images referenced from a .md"),
            Arc::new(DocImageListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_frontmatter_parse", NexusToolCategory::Documentation, "Parse YAML frontmatter from a .md"),
            Arc::new(DocFrontmatterParseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_md_lint", NexusToolCategory::Documentation, "Basic markdown lint (long lines, trailing spaces, tabs)"),
            Arc::new(DocMdLintTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_orphan_md", NexusToolCategory::Documentation, "Markdown files not referenced from README.md"),
            Arc::new(DocOrphanMdTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_size_report", NexusToolCategory::Documentation, "Total .md file count and bytes in project"),
            Arc::new(DocSizeReportTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_heading_depth", NexusToolCategory::Documentation, "Max heading depth and per-level distribution"),
            Arc::new(DocHeadingDepthTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_codeblocks_extract", NexusToolCategory::Documentation, "Extract fenced code blocks with language"),
            Arc::new(DocCodeblocksExtractTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_codeblocks_count", NexusToolCategory::Documentation, "Count fenced code blocks per language"),
            Arc::new(DocCodeblocksCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_api_list", NexusToolCategory::Documentation, "List .md files under docs/api"),
            Arc::new(DocApiListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("doc_examples_list", NexusToolCategory::Documentation, "List entries under examples/"),
            Arc::new(DocExamplesListTool),
        );

        // ── Performance extras (Fase 9M, 20 new) ──────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("perf_cargo_build_time", NexusToolCategory::Performance, "Run `cargo build --timings` and report duration"),
            Arc::new(PerfCargoBuildTimeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_binary_size", NexusToolCategory::Performance, "Sizes of binaries in target/release"),
            Arc::new(PerfBinarySizeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_cargo_bloat", NexusToolCategory::Performance, "`cargo bloat --release --crates -n 20`"),
            Arc::new(PerfCargoBloatTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_target_dir_size", NexusToolCategory::Performance, "Total size of target/ directory"),
            Arc::new(PerfTargetDirSizeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_largest_files", NexusToolCategory::Performance, "Top N .rs files by byte size"),
            Arc::new(PerfLargestFilesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_loc_per_crate", NexusToolCategory::Performance, "Lines of Rust code per workspace crate"),
            Arc::new(PerfLocPerCrateTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_unused_deps", NexusToolCategory::Performance, "Heuristic: deps in Cargo.toml not referenced in src/"),
            Arc::new(PerfUnusedDepsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_test_count", NexusToolCategory::Performance, "Count #[test] / #[tokio::test] attributes"),
            Arc::new(PerfTestCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_async_funcs", NexusToolCategory::Performance, "Count `async fn` and `.await` usages"),
            Arc::new(PerfAsyncFuncsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_unsafe_blocks", NexusToolCategory::Performance, "Count `unsafe {`, `unsafe fn`, `unsafe impl`"),
            Arc::new(PerfUnsafeBlocksTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_panic_count", NexusToolCategory::Performance, "Count `panic!`/`unwrap()`/`expect(`/`todo!`/`unimplemented!`"),
            Arc::new(PerfPanicCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_clone_count", NexusToolCategory::Performance, "Count `.clone()` and `.to_owned()`"),
            Arc::new(PerfCloneCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_string_alloc", NexusToolCategory::Performance, "Count common String allocation patterns"),
            Arc::new(PerfStringAllocTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_box_count", NexusToolCategory::Performance, "Count `Box<dyn` and `Box::new`"),
            Arc::new(PerfBoxCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_arc_mutex", NexusToolCategory::Performance, "Count Arc<Mutex/RwLock> patterns"),
            Arc::new(PerfArcMutexTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_dep_count", NexusToolCategory::Performance, "Count deps/dev-deps/build-deps in Cargo.toml"),
            Arc::new(PerfDepCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_compile_units", NexusToolCategory::Performance, "Workspace package count via `cargo metadata`"),
            Arc::new(PerfCompileUnitsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_optimization_check", NexusToolCategory::Performance, "Inspect [profile.release] optimization keys"),
            Arc::new(PerfOptimizationCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_lto_check", NexusToolCategory::Performance, "Check LTO setting in [profile.release]"),
            Arc::new(PerfLtoCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("perf_codegen_units", NexusToolCategory::Performance, "Check codegen-units in [profile.release]"),
            Arc::new(PerfCodegenUnitsTool),
        );

        // ── Testing extras (Fase 9N, 20 new) ──────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("test_run_unit", NexusToolCategory::Testing, "Run `cargo test --lib --quiet`"),
            Arc::new(TestRunUnitTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_run_integration", NexusToolCategory::Testing, "Run `cargo test --tests --quiet`"),
            Arc::new(TestRunIntegrationTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_run_quiet", NexusToolCategory::Testing, "Run `cargo test --quiet` with optional filter"),
            Arc::new(TestRunQuietTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_run_workspace", NexusToolCategory::Testing, "Run `cargo test --workspace --quiet`"),
            Arc::new(TestRunWorkspaceTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_count_files", NexusToolCategory::Testing, "Count *_test.rs and tests/*.rs files"),
            Arc::new(TestCountFilesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_ignored_count", NexusToolCategory::Testing, "Count `#[ignore]` attributes in source"),
            Arc::new(TestIgnoredCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_should_panic_count", NexusToolCategory::Testing, "Count `#[should_panic` attributes"),
            Arc::new(TestShouldPanicCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_module_count", NexusToolCategory::Testing, "Count test modules (`mod tests`, `#[cfg(test)]`)"),
            Arc::new(TestModuleCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_assert_count", NexusToolCategory::Testing, "Count assert!/assert_eq!/assert_ne!/debug_assert"),
            Arc::new(TestAssertCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_proptest_count", NexusToolCategory::Testing, "Count proptest!/prop_assert/use proptest"),
            Arc::new(TestProptestCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_quickcheck_count", NexusToolCategory::Testing, "Count #[quickcheck]/quickcheck! usages"),
            Arc::new(TestQuickcheckCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_mock_count", NexusToolCategory::Testing, "Count mockall/wiremock/MockServer usages"),
            Arc::new(TestMockCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_bench_count", NexusToolCategory::Testing, "Count #[bench]/criterion_group!/criterion_main!"),
            Arc::new(TestBenchCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_doc_count", NexusToolCategory::Testing, "Count doctest fences in /// comments"),
            Arc::new(TestDocCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_fixtures_list", NexusToolCategory::Testing, "List entries under tests/fixtures/"),
            Arc::new(TestFixturesListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_snapshots_list", NexusToolCategory::Testing, "Walk for `.snap` files (insta snapshots)"),
            Arc::new(TestSnapshotsListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_stale_snapshots", NexusToolCategory::Testing, "Walk for `.snap.new` files (unaccepted snapshots)"),
            Arc::new(TestStaleSnapshotsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_coverage_summary", NexusToolCategory::Testing, "Check for cobertura.xml/lcov.info/tarpaulin reports"),
            Arc::new(TestCoverageSummaryTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_failed_log", NexusToolCategory::Testing, "Run `cargo test --no-run --quiet` and parse compile errors"),
            Arc::new(TestFailedLogTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("test_workflow_files", NexusToolCategory::Testing, "List .github/workflows/*.yml with test mentions"),
            Arc::new(TestWorkflowFilesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "test_playwright",
                NexusToolCategory::Testing,
                "Run Playwright e2e test suite (`npx playwright test`) with pass/fail counts",
            ),
            Arc::new(TestPlaywrightTool),
        );

        // ── Security extras (Fase 9O, 20 new) ─────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("sec_secret_patterns", NexusToolCategory::Security, "Heuristic scan for hardcoded secrets in source"),
            Arc::new(SecSecretPatternsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_unwrap_count", NexusToolCategory::Security, "Count `.unwrap()` and `.expect(` (panic surface)"),
            Arc::new(SecUnwrapCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_panic_count", NexusToolCategory::Security, "Count panic!/todo!/unimplemented!/unreachable!"),
            Arc::new(SecPanicCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_env_var_check", NexusToolCategory::Security, "Count `std::env::var` and default fallbacks"),
            Arc::new(SecEnvVarCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_http_url_count", NexusToolCategory::Security, "Count plaintext http:// vs https:// URLs"),
            Arc::new(SecHttpUrlCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_localhost_count", NexusToolCategory::Security, "Count localhost / 127.0.0.1 / 0.0.0.0 references"),
            Arc::new(SecLocalhostCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_eval_check", NexusToolCategory::Security, "Heuristic scan for eval-like / sandbox patterns"),
            Arc::new(SecEvalCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_sql_injection_check", NexusToolCategory::Security, "Find string interpolation in SQL queries"),
            Arc::new(SecSqlInjectionCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_cmd_injection_check", NexusToolCategory::Security, "Find Command::new + shell -c patterns"),
            Arc::new(SecCmdInjectionCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_dependency_count", NexusToolCategory::Security, "Count dependencies across all Cargo.toml"),
            Arc::new(SecDependencyCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_git_secrets_check", NexusToolCategory::Security, "Scan .git/config for credentials in URLs"),
            Arc::new(SecGitSecretsCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_env_files_check", NexusToolCategory::Security, "Find .env* files and check .gitignore coverage"),
            Arc::new(SecEnvFilesCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_dockerfile_user_check", NexusToolCategory::Security, "Check Dockerfile USER directive (non-root)"),
            Arc::new(SecDockerfileUserCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_workflow_perms_check", NexusToolCategory::Security, "Check workflows for permissions: blocks"),
            Arc::new(SecWorkflowPermsCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_cors_check", NexusToolCategory::Security, "Find permissive CORS patterns"),
            Arc::new(SecCorsCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_jwt_secret_check", NexusToolCategory::Security, "Find hardcoded JWT secrets and weak algorithms"),
            Arc::new(SecJwtSecretCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_md5_sha1_check", NexusToolCategory::Security, "Find weak hash algorithm usage (md5/sha1)"),
            Arc::new(SecMd5Sha1CheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_random_check", NexusToolCategory::Security, "Find non-secure RNG usage"),
            Arc::new(SecRandomCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_tls_check", NexusToolCategory::Security, "Find TLS verify=false / accept_invalid_certs"),
            Arc::new(SecTlsCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("sec_audit_summary", NexusToolCategory::Security, "High-level audit overview combining several scans"),
            Arc::new(SecAuditSummaryTool),
        );

        // ── Code Analysis extras (Fase 9P, 20 new) ────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("ca_struct_count", NexusToolCategory::CodeAnalysis, "Count struct declarations"),
            Arc::new(CaStructCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_enum_count", NexusToolCategory::CodeAnalysis, "Count enum declarations"),
            Arc::new(CaEnumCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_trait_count", NexusToolCategory::CodeAnalysis, "Count trait declarations"),
            Arc::new(CaTraitCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_impl_count", NexusToolCategory::CodeAnalysis, "Count impl blocks"),
            Arc::new(CaImplCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_fn_count", NexusToolCategory::CodeAnalysis, "Count fn declarations (sync/async/const/unsafe)"),
            Arc::new(CaFnCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_pub_fn_count", NexusToolCategory::CodeAnalysis, "Count public function declarations"),
            Arc::new(CaPubFnCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_macro_count", NexusToolCategory::CodeAnalysis, "Count macro definitions (rules + proc)"),
            Arc::new(CaMacroCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_use_count", NexusToolCategory::CodeAnalysis, "Count `use` statements"),
            Arc::new(CaUseCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_mod_count", NexusToolCategory::CodeAnalysis, "Count module declarations"),
            Arc::new(CaModCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_lifetime_count", NexusToolCategory::CodeAnalysis, "Count lifetime annotations"),
            Arc::new(CaLifetimeCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_generic_count", NexusToolCategory::CodeAnalysis, "Count generic param usage"),
            Arc::new(CaGenericCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_derive_count", NexusToolCategory::CodeAnalysis, "Count derive macros"),
            Arc::new(CaDeriveCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_attr_count", NexusToolCategory::CodeAnalysis, "Count common attribute macros"),
            Arc::new(CaAttrCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_doc_comment_count", NexusToolCategory::CodeAnalysis, "Count doc comments (///, //!)"),
            Arc::new(CaDocCommentCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_inline_comment_count", NexusToolCategory::CodeAnalysis, "Count inline comments"),
            Arc::new(CaInlineCommentCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_todo_fixme_count", NexusToolCategory::CodeAnalysis, "Count TODO/FIXME/XXX/HACK markers"),
            Arc::new(CaTodoFixmeCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_match_count", NexusToolCategory::CodeAnalysis, "Count match expressions"),
            Arc::new(CaMatchCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_if_let_count", NexusToolCategory::CodeAnalysis, "Count if let / let Some / let Ok"),
            Arc::new(CaIfLetCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_while_let_count", NexusToolCategory::CodeAnalysis, "Count while let / for / loop"),
            Arc::new(CaWhileLetCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("ca_complexity_estimate", NexusToolCategory::CodeAnalysis, "Heuristic cyclomatic complexity estimate"),
            Arc::new(CaComplexityEstimateTool),
        );

        // ── Build / Deploy (Fase 9Q, 21 new) ──────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("build_target_list", NexusToolCategory::Build, "List subdirectories under target/"),
            Arc::new(BuildTargetListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_artifact_age", NexusToolCategory::Build, "Newest mtime under target/release"),
            Arc::new(BuildArtifactAgeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_release_size", NexusToolCategory::Build, "Sum binary sizes in target/release"),
            Arc::new(BuildReleaseSizeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_debug_size", NexusToolCategory::Build, "Sum binary sizes in target/debug"),
            Arc::new(BuildDebugSizeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_incremental_dir", NexusToolCategory::Build, "Check incremental compilation directory"),
            Arc::new(BuildIncrementalDirTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_lockfile_age", NexusToolCategory::Build, "Mtime/size of Cargo.lock"),
            Arc::new(BuildLockfileAgeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_log_tail", NexusToolCategory::Build, "Tail .rustc_info.json / fingerprint logs"),
            Arc::new(BuildLogTailTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_rerun_checks", NexusToolCategory::Build, "Count cargo:rerun-if- directives"),
            Arc::new(BuildRerunChecksTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_script_count", NexusToolCategory::Build, "Count build.rs files in workspace"),
            Arc::new(BuildScriptCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_workspace_check", NexusToolCategory::Build, "`cargo check --workspace --quiet`"),
            Arc::new(BuildWorkspaceCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("build_profile_list", NexusToolCategory::Build, "List [profile.*] sections in root Cargo.toml"),
            Arc::new(BuildProfileListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_dockerfile_count", NexusToolCategory::Deployment, "Count Dockerfile* files"),
            Arc::new(DeployDockerfileCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_compose_check", NexusToolCategory::Deployment, "Find docker-compose*.yml files"),
            Arc::new(DeployComposeCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_k8s_check", NexusToolCategory::Deployment, "Find kubernetes manifests"),
            Arc::new(DeployK8sCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_helm_check", NexusToolCategory::Deployment, "Find Chart.yaml/values.yaml files"),
            Arc::new(DeployHelmCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_terraform_check", NexusToolCategory::Deployment, "Find *.tf and tfstate files"),
            Arc::new(DeployTerraformCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_ansible_check", NexusToolCategory::Deployment, "Find ansible playbooks/configs"),
            Arc::new(DeployAnsibleCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_systemd_check", NexusToolCategory::Deployment, "Find systemd unit files"),
            Arc::new(DeploySystemdCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_nginx_check", NexusToolCategory::Deployment, "Find nginx*.conf files"),
            Arc::new(DeployNginxCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_env_files_count", NexusToolCategory::Deployment, "Count .env / .envrc files"),
            Arc::new(DeployEnvFilesCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("deploy_release_artifacts", NexusToolCategory::Deployment, "List common release artifact paths"),
            Arc::new(DeployReleaseArtifactsTool),
        );

        // ── API / Memory / Other (Fase 9R, 20 new) ────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("api_openapi_files", NexusToolCategory::Api, "Find openapi/swagger spec files"),
            Arc::new(ApiOpenapiFilesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_route_count", NexusToolCategory::Api, "Count axum/actix/warp/rocket route declarations"),
            Arc::new(ApiRouteCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_handler_count", NexusToolCategory::Api, "Count async fn handlers (heuristic)"),
            Arc::new(ApiHandlerCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_endpoint_list", NexusToolCategory::Api, "Extract endpoint paths from .route() literals"),
            Arc::new(ApiEndpointListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_graphql_check", NexusToolCategory::Api, "Detect GraphQL schemas/usages"),
            Arc::new(ApiGraphqlCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_grpc_check", NexusToolCategory::Api, "Detect gRPC/.proto/tonic usages"),
            Arc::new(ApiGrpcCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_postman_check", NexusToolCategory::Api, "Find postman collection files"),
            Arc::new(ApiPostmanCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("api_middleware_count", NexusToolCategory::Api, "Count tower/axum middleware layer registrations"),
            Arc::new(ApiMiddlewareCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("memory_namespace_count", NexusToolCategory::Memory, "Count distinct memory namespaces in DB"),
            Arc::new(MemoryNamespaceCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("memory_size_estimate", NexusToolCategory::Memory, "Estimate aggregate memory_namespace size"),
            Arc::new(MemorySizeEstimateTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("memory_pattern_list", NexusToolCategory::Memory, "List distinct memory keys/patterns"),
            Arc::new(MemoryPatternListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("memory_recent_writes", NexusToolCategory::Memory, "Recent memory_namespace writes"),
            Arc::new(MemoryRecentWritesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("memory_topkeys", NexusToolCategory::Memory, "Top namespaces by row count"),
            Arc::new(MemoryTopkeysTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("memory_evict_stats", NexusToolCategory::Memory, "Evictable rows older than TTL"),
            Arc::new(MemoryEvictStatsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("util_disk_free", NexusToolCategory::Utility, "Best-effort disk info at project_root"),
            Arc::new(UtilDiskFreeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("util_uptime", NexusToolCategory::Utility, "Process uptime in seconds since first call"),
            Arc::new(UtilUptimeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("util_hostname", NexusToolCategory::Utility, "Hostname/user from environment"),
            Arc::new(UtilHostnameTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("util_cpu_count", NexusToolCategory::Utility, "Logical CPU count via available_parallelism"),
            Arc::new(UtilCpuCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("util_now_iso", NexusToolCategory::Utility, "Current time as RFC3339 + epoch seconds"),
            Arc::new(UtilNowIsoTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("util_pid", NexusToolCategory::Utility, "Process id of running mcp-core"),
            Arc::new(UtilPidTool),
        );

        // ── Final meta tools (Fase 9S, 5 new — total 314) ─────────────────
        self.register_with_handler(
            NexusToolSpec::new("meta_catalog_count", NexusToolCategory::Other, "Total + implemented tool counts in catalog"),
            Arc::new(MetaCatalogCountTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("meta_categories_list", NexusToolCategory::Other, "List all NexusToolCategory variants with counts"),
            Arc::new(MetaCategoriesListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("meta_version_info", NexusToolCategory::Other, "Crate name/version + profile + os/arch"),
            Arc::new(MetaVersionInfoTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("meta_health_summary", NexusToolCategory::Other, "Basic health: project_root, db, catalog"),
            Arc::new(MetaHealthSummaryTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("meta_self_test", NexusToolCategory::Other, "Smoke-test a small set of read-only handlers"),
            Arc::new(MetaSelfTestTool),
        );

        // ── Project DB tools — gestione DB e migration per progetti utente ──
        {
            use crate::nexus_tools::{
                project_db_connections::ProjectDbConnectionsTool,
                project_db_status::ProjectDbStatusTool,
                project_db_create_migration::ProjectDbCreateMigrationTool,
                project_db_apply_migration::ProjectDbApplyMigrationTool,
                project_db_rollback::ProjectDbRollbackTool,
                project_db_query::ProjectDbQueryTool,
                project_db_schema::ProjectDbSchemaTool,
                project_db_tables::ProjectDbTablesTool,
                project_db_set_connection::ProjectDbSetConnectionTool,
            };
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_connections",
                    NexusToolCategory::Database,
                    "Restituisce le connessioni DB configurate per il progetto corrente (connection string, engine, ecc.)",
                ),
                Arc::new(ProjectDbConnectionsTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_status",
                    NexusToolCategory::Database,
                    "Stato DB e migration del progetto utente corrente (read-only)",
                ),
                Arc::new(ProjectDbStatusTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_create_migration",
                    NexusToolCategory::Database,
                    "Crea file migration timestampato per il DB del progetto. Blocca DDL diretto.",
                ),
                Arc::new(ProjectDbCreateMigrationTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_apply_migration",
                    NexusToolCategory::Database,
                    "Applica migration pending al DB del progetto utente.",
                ),
                Arc::new(ProjectDbApplyMigrationTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_rollback",
                    NexusToolCategory::Database,
                    "Annulla l'ultima migration applicata al DB del progetto utente.",
                ),
                Arc::new(ProjectDbRollbackTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_set_connection",
                    NexusToolCategory::Database,
                    "Configura la connessione al DB del progetto. Parametri: connection_string (DSN PostgreSQL), engine (postgres), hosting_mode (internal/external).",
                ),
                Arc::new(ProjectDbSetConnectionTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_query",
                    NexusToolCategory::Database,
                    "Esegue una query read-only (SELECT/WITH/EXPLAIN/SHOW) sul DB del progetto corrente. NON usare psql. DDL/DML scrittura sono rifiutati. Limit 100 righe.",
                ),
                Arc::new(ProjectDbQueryTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_schema",
                    NexusToolCategory::Database,
                    "Ispeziona lo schema del DB del progetto corrente: tabelle, colonne, tipi, nullable, default. Filtra per schema (default 'public') o singola tabella.",
                ),
                Arc::new(ProjectDbSchemaTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_tables",
                    NexusToolCategory::Database,
                    "Lista sintetica delle tabelle del DB del progetto corrente: nome, stima righe, dimensione. Piu' veloce di project_db_schema.",
                ),
                Arc::new(ProjectDbTablesTool),
            );
        }

        // ── HTTP + Healthcheck tools ─────────────────────────────────────
        {
            use crate::nexus_tools::{
                http_request::HttpRequestTool,
                service_healthcheck::ServiceHealthcheckTool,
            };
            self.register_with_handler(
                NexusToolSpec::new(
                    "http_request",
                    NexusToolCategory::Utility,
                    "Esegue una richiesta HTTP strutturata (GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS). Restituisce status, headers, body (JSON o testo), latenza. Ideale per testare endpoint del progetto.",
                ),
                Arc::new(HttpRequestTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "service_healthcheck",
                    NexusToolCategory::Utility,
                    "Verifica che un servizio sia raggiungibile tramite probe HTTP o TCP (tcp://host:port). Retry con backoff esponenziale. Restituisce ok, status, latenza.",
                ),
                Arc::new(ServiceHealthcheckTool),
            );
        }

        // ── Fase 4: Bootstrap progetto ──────────────────────────────────
        {
            use crate::nexus_tools::{
                project_delete::ProjectDeleteTool,
                project_register_existing_dir::ProjectRegisterExistingDirTool,
                project_register_from_git::ProjectRegisterFromGitTool,
                project_set_default_branch::ProjectSetDefaultBranchTool,
                project_workspace_init::ProjectWorkspaceInitTool,
            };
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_register_from_git",
                    NexusToolCategory::Utility,
                    "Clona un repository Git e lo registra come progetto Nexus. Esegue git clone --depth=1, inserisce in projects/workspaces/repositories con transazione atomica.",
                ),
                Arc::new(ProjectRegisterFromGitTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_register_existing_dir",
                    NexusToolCategory::Utility,
                    "Registra una directory gia' presente sul filesystem come progetto Nexus. Non esegue clone, rileva info Git, inserisce in DB con transazione.",
                ),
                Arc::new(ProjectRegisterExistingDirTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_delete",
                    NexusToolCategory::Utility,
                    "Soft-delete di un progetto dal DB. Rimuove righe da projects e tabelle dipendenti (CASCADE). Non cancella file dal disco. Richiede confirm:true.",
                ),
                Arc::new(ProjectDeleteTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_set_default_branch",
                    NexusToolCategory::Utility,
                    "Aggiorna il branch predefinito di un progetto (es. da develop a main).",
                ),
                Arc::new(ProjectSetDefaultBranchTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_workspace_init",
                    NexusToolCategory::Utility,
                    "Inizializza la riga workspaces per un progetto. Utile dopo clone manuale o registrazione incompleta. Idempotente: se il workspace esiste gia', ritorna l'ID esistente.",
                ),
                Arc::new(ProjectWorkspaceInitTool),
            );
        }

        // ── Fase 5: Docker / Container ──────────────────────────────────
        {
            use crate::nexus_tools::{
                docker_build::DockerBuildTool,
                docker_compose_down::DockerComposeDownTool,
                docker_compose_up::DockerComposeUpTool,
                docker_logs::DockerLogsTool,
                docker_ps::DockerPsTool,
                docker_rm::DockerRmTool,
                docker_run::DockerRunTool,
                docker_stop::DockerStopTool,
            };
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_build",
                    NexusToolCategory::Deployment,
                    "Costruisce un'immagine Docker dal progetto con auto-label. Il Dockerfile deve trovarsi dentro la project_root.",
                ),
                Arc::new(DockerBuildTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_run",
                    NexusToolCategory::Deployment,
                    "Esegue un container Docker con label progetto. Vieta nomi 'ideai-*' (infrastruttura Nexus). Supporta porte, env, volumi.",
                ),
                Arc::new(DockerRunTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_ps",
                    NexusToolCategory::Deployment,
                    "Lista container del progetto corrente (filtro per label). Non espone container ideai-* ne' di altri progetti.",
                ),
                Arc::new(DockerPsTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_logs",
                    NexusToolCategory::Deployment,
                    "Legge i log di un container del progetto. Verifica label prima dell'accesso. Supporta tail e timestamps.",
                ),
                Arc::new(DockerLogsTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_stop",
                    NexusToolCategory::Deployment,
                    "Ferma un singolo container del progetto. Verifica label progetto PRIMA dello stop. Container ideai-* sempre rifiutati.",
                ),
                Arc::new(DockerStopTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_rm",
                    NexusToolCategory::Deployment,
                    "Rimuove un container fermo del progetto. Verifica label progetto. Container ideai-* sempre rifiutati.",
                ),
                Arc::new(DockerRmTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_compose_up",
                    NexusToolCategory::Deployment,
                    "Avvia servizi con docker compose. OBBLIGATORIO specificare il file compose (mai compose globali). Supporta build e servizi specifici.",
                ),
                Arc::new(DockerComposeUpTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "docker_compose_down",
                    NexusToolCategory::Deployment,
                    "Ferma e rimuove servizi compose del progetto. OBBLIGATORIO il file compose. Opzione per rimuovere volumi e immagini.",
                ),
                Arc::new(DockerComposeDownTool),
            );
        }

        // ── Fase 6: Operazioni DB avanzate ──────────────────────────────
        {
            use crate::nexus_tools::{
                project_db_analyze::ProjectDbAnalyzeTool,
                project_db_backup::ProjectDbBackupTool,
                project_db_diff_schema::ProjectDbDiffSchemaTool,
                project_db_dump_schema::ProjectDbDumpSchemaTool,
                project_db_kill_query::ProjectDbKillQueryTool,
                project_db_reindex::ProjectDbReindexTool,
                project_db_restore::ProjectDbRestoreTool,
                project_db_vacuum::ProjectDbVacuumTool,
            };
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_backup",
                    NexusToolCategory::Database,
                    "Esegue pg_dump sul DB del progetto. Salva in .nexus/backups/. Supporta formato plain/custom, schema-only.",
                ),
                Arc::new(ProjectDbBackupTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_restore",
                    NexusToolCategory::Database,
                    "Ripristina un backup nel DB del progetto. Richiede confirm:true. Supporta plain SQL e formato custom.",
                ),
                Arc::new(ProjectDbRestoreTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_vacuum",
                    NexusToolCategory::Database,
                    "Esegue VACUUM sul DB del progetto. Supporta ANALYZE e FULL. Opera su tabella singola o intero database.",
                ),
                Arc::new(ProjectDbVacuumTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_analyze",
                    NexusToolCategory::Database,
                    "Esegue ANALYZE sul DB del progetto. Aggiorna le statistiche del query planner.",
                ),
                Arc::new(ProjectDbAnalyzeTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_reindex",
                    NexusToolCategory::Database,
                    "Esegue REINDEX su tabella/indice del DB progetto. Operazione bloccante.",
                ),
                Arc::new(ProjectDbReindexTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_dump_schema",
                    NexusToolCategory::Database,
                    "Esporta solo lo schema del DB progetto (pg_dump --schema-only). Snapshot pre-migration.",
                ),
                Arc::new(ProjectDbDumpSchemaTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_diff_schema",
                    NexusToolCategory::Database,
                    "Confronta lo schema DB attuale con un file SQL di riferimento. Utile per verifica post-migration.",
                ),
                Arc::new(ProjectDbDiffSchemaTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_db_kill_query",
                    NexusToolCategory::Database,
                    "Termina una query bloccante sul DB progetto. Usa pg_cancel_backend (graceful) o pg_terminate_backend (force).",
                ),
                Arc::new(ProjectDbKillQueryTool),
            );
        }

        // ── Project config tools — info progetto, run configs ────────────
        {
            use crate::nexus_tools::{
                project_info::ProjectInfoTool,
                project_run_configs::ProjectRunConfigsTool,
            };
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_info",
                    NexusToolCategory::Utility,
                    "Info generali del progetto: nome, root path, git branch, stack rilevato, istruzioni custom, sandbox config.",
                ),
                Arc::new(ProjectInfoTool),
            );
            self.register_with_handler(
                NexusToolSpec::new(
                    "project_run_configs",
                    NexusToolCategory::Utility,
                    "Configurazioni di esecuzione (comandi) disponibili per il progetto: label, tipo, comando, args, cwd, env.",
                ),
                Arc::new(ProjectRunConfigsTool),
            );
        }

        // ── Api (Fase 9D) ────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "openapi_validate",
                NexusToolCategory::Api,
                "Validate OpenAPI spec (JSON parse + structural checks)",
            ),
            Arc::new(OpenApiValidateTool),
        );

        // ── Fase 9E: RuVector + Consensus (4 new) ─────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "ruvector_insert",
                NexusToolCategory::Memory,
                "Embed and insert a text into the global HNSW vector database",
            ),
            Arc::new(RuVectorInsertTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "ruvector_search",
                NexusToolCategory::Memory,
                "k-NN semantic search over the global HNSW vector database",
            ),
            Arc::new(RuVectorSearchTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "ruvector_stats",
                NexusToolCategory::Memory,
                "Stats for the global HNSW vector database (nodes, fan-out, entry point)",
            ),
            Arc::new(RuVectorStatsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "consensus_vote",
                NexusToolCategory::Utility,
                "Evaluate multi-agent votes via ConsensusEngine (majority/supermajority/unanimous/weighted)",
            ),
            Arc::new(ConsensusVoteTool),
        );

        // ── Fase 9F: Utility batch (10) ───────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_read",
                NexusToolCategory::Utility,
                "Read a file from the project with optional line range",
            ),
            Arc::new(FsReadTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_list",
                NexusToolCategory::Utility,
                "List files in a project directory with regex filter",
            ),
            Arc::new(FsListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_grep",
                NexusToolCategory::Utility,
                "Recursive regex search across project files",
            ),
            Arc::new(FsGrepTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_tree",
                NexusToolCategory::Utility,
                "Project file tree as JSON",
            ),
            Arc::new(FsTreeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "json_parse",
                NexusToolCategory::Utility,
                "Parse and pretty-print JSON (inline or from file)",
            ),
            Arc::new(JsonParseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "json_get",
                NexusToolCategory::Utility,
                "Extract a value from JSON via dot-path query",
            ),
            Arc::new(JsonGetTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "base64_encode",
                NexusToolCategory::Utility,
                "Base64 encode (standard or url-safe)",
            ),
            Arc::new(Base64EncodeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "base64_decode",
                NexusToolCategory::Utility,
                "Base64 decode to UTF-8 string",
            ),
            Arc::new(Base64DecodeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "hash_content",
                NexusToolCategory::Utility,
                "SHA-256/SHA-512 hash of a string or file",
            ),
            Arc::new(HashContentTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "uuid_generate",
                NexusToolCategory::Utility,
                "Generate UUID v4 (batch, optional compact form)",
            ),
            Arc::new(UuidGenerateTool),
        );

        // ── Fase 9F: VCS batch (4) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "git_branch_list",
                NexusToolCategory::Vcs,
                "List local and remote git branches with upstream tracking",
            ),
            Arc::new(GitBranchListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_remote_list",
                NexusToolCategory::Vcs,
                "List git remotes with fetch and push URLs",
            ),
            Arc::new(GitRemoteListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_show",
                NexusToolCategory::Vcs,
                "Show a commit with numstat file changes",
            ),
            Arc::new(GitShowTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_tag_list",
                NexusToolCategory::Vcs,
                "List git tags sorted by creator date",
            ),
            Arc::new(GitTagListTool),
        );

        // ── Fase 9F: GitHub batch (3) ─────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_workflow_list",
                NexusToolCategory::GitHub,
                "List GitHub Actions workflows (gh workflow list)",
            ),
            Arc::new(GhWorkflowListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_run_list",
                NexusToolCategory::GitHub,
                "List GitHub Actions runs with success/failure counts",
            ),
            Arc::new(GhRunListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_release_list",
                NexusToolCategory::GitHub,
                "List GitHub releases",
            ),
            Arc::new(GhReleaseListTool),
        );

        // ── Fase 9F: CodeAnalysis / Quality batch (3) ─────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "count_loc",
                NexusToolCategory::CodeAnalysis,
                "Count lines of code per language (self-contained, no tokei)",
            ),
            Arc::new(CountLocTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "find_todos",
                NexusToolCategory::CodeAnalysis,
                "Find TODO/FIXME/HACK/XXX markers in source files",
            ),
            Arc::new(FindTodosTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_fmt_check",
                NexusToolCategory::CodeQuality,
                "Run `cargo fmt --check` and report files needing reformat",
            ),
            Arc::new(CargoFmtCheckTool),
        );

        // ── Fase 9G: Utility batch (8) ───────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_write",
                NexusToolCategory::Utility,
                "Write text to a file inside project_root (overwrite or append)",
            ),
            Arc::new(FsWriteTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_stat",
                NexusToolCategory::Utility,
                "File/dir metadata (size, mtime, type, readonly)",
            ),
            Arc::new(FsStatTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "fs_glob",
                NexusToolCategory::Utility,
                "Glob match (`*`, `?`) recursive over project files",
            ),
            Arc::new(FsGlobTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "env_get",
                NexusToolCategory::Utility,
                "Read environment variables (with secret masking by default)",
            ),
            Arc::new(EnvGetTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "time_now",
                NexusToolCategory::Utility,
                "Current UTC timestamp in unix/iso8601/rfc3339 formats",
            ),
            Arc::new(TimeNowTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "regex_replace",
                NexusToolCategory::Utility,
                "Regex replace on a string or file content (read-only, in-memory)",
            ),
            Arc::new(RegexReplaceTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "text_diff",
                NexusToolCategory::Utility,
                "Line-based LCS diff between two texts or files",
            ),
            Arc::new(TextDiffTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "uuid_parse",
                NexusToolCategory::Utility,
                "Validate and describe a UUID string (version, variant)",
            ),
            Arc::new(UuidParseTool),
        );

        // ── Fase 9G: VCS batch (4) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "git_stash_list",
                NexusToolCategory::Vcs,
                "List git stashes with index, branch and message",
            ),
            Arc::new(GitStashListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_grep",
                NexusToolCategory::Vcs,
                "`git grep -n -E` regex search across tracked files",
            ),
            Arc::new(GitGrepTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_describe",
                NexusToolCategory::Vcs,
                "`git describe --tags --long --dirty` parsed into tag/commits/sha",
            ),
            Arc::new(GitDescribeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "git_shortlog",
                NexusToolCategory::Vcs,
                "`git shortlog -sne` aggregating commits per author",
            ),
            Arc::new(GitShortlogTool),
        );

        // ── Fase 9G: GitHub batch (3) ─────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_pr_list",
                NexusToolCategory::GitHub,
                "`gh pr list --json` filtered by state/base",
            ),
            Arc::new(GhPrListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_pr_view",
                NexusToolCategory::GitHub,
                "`gh pr view <num> --json` full PR detail",
            ),
            Arc::new(GhPrViewTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "gh_repo_view",
                NexusToolCategory::GitHub,
                "`gh repo view --json` repository metadata",
            ),
            Arc::new(GhRepoViewTool),
        );

        // ── Fase 9G: Cargo / Build batch (3) ──────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_doc",
                NexusToolCategory::Documentation,
                "Run `cargo doc --no-deps` and count generated HTML pages",
            ),
            Arc::new(CargoDocTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_locate_project",
                NexusToolCategory::Build,
                "`cargo locate-project` (root + workspace manifest paths)",
            ),
            Arc::new(CargoLocateProjectTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_pkgid",
                NexusToolCategory::Build,
                "`cargo pkgid` resolved package URL (parsed name+version)",
            ),
            Arc::new(CargoPkgidTool),
        );

        // ── Fase 9G: CodeAnalysis batch (2) ───────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "find_unsafe",
                NexusToolCategory::CodeAnalysis,
                "Find `unsafe` blocks/fn/impl in Rust source files",
            ),
            Arc::new(FindUnsafeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "find_pubapi",
                NexusToolCategory::CodeAnalysis,
                "Count `pub` items per Rust file (top-files surface area)",
            ),
            Arc::new(FindPubApiTool),
        );

        // ══════════════════════════════════════════════════════════════════
        //                 FASE 9H — Cargo extras (20 new)
        // ══════════════════════════════════════════════════════════════════

        // ── Build (Fase 9H) ──────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_run",
                NexusToolCategory::Build,
                "`cargo run [--release] [--bin name]` execution wrapper",
            ),
            Arc::new(CargoRunTool),
        );
        self.register_with_handler(
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
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_publish_dry",
                NexusToolCategory::Build,
                "`cargo publish --dry-run --allow-dirty` rehearsal",
            ),
            Arc::new(CargoPublishDryTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_targets_list",
                NexusToolCategory::Build,
                "List targets (bin/lib/example/test/bench) via `cargo metadata`",
            ),
            Arc::new(CargoTargetsListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_workspace_members",
                NexusToolCategory::Build,
                "List workspace members via `cargo metadata`",
            ),
            Arc::new(CargoWorkspaceMembersTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_env_overrides",
                NexusToolCategory::Build,
                "Read CARGO_*/RUSTFLAGS/RUSTDOCFLAGS env vars affecting builds",
            ),
            Arc::new(CargoEnvOverridesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_build_artifact_check",
                NexusToolCategory::Build,
                "List binaries in target/<profile>/ with sizes",
            ),
            Arc::new(CargoBuildArtifactCheckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_clean_dry",
                NexusToolCategory::Build,
                "Compute target/ directory size without removing anything",
            ),
            Arc::new(CargoCleanDryTool),
        );

        // ── Dependencies (Fase 9H) ───────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_install_list",
                NexusToolCategory::Dependencies,
                "Parse `cargo install --list` into name/version pairs",
            ),
            Arc::new(CargoInstallListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_search",
                NexusToolCategory::Dependencies,
                "`cargo search <query> --limit N` (network egress)",
            ),
            Arc::new(CargoSearchTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_features_list",
                NexusToolCategory::Dependencies,
                "Parse `[features]` section of root Cargo.toml",
            ),
            Arc::new(CargoFeaturesListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_dep_versions",
                NexusToolCategory::Dependencies,
                "Detect duplicate packages (same name, multiple versions)",
            ),
            Arc::new(CargoDepVersionsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_lockfile_check",
                NexusToolCategory::Dependencies,
                "Verify Cargo.lock presence, version and package count",
            ),
            Arc::new(CargoLockfileCheckTool),
        );

        // ── Testing (Fase 9H) ────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_test_doc",
                NexusToolCategory::Testing,
                "`cargo test --doc` with passed/failed counts",
            ),
            Arc::new(CargoTestDocTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_test_lib",
                NexusToolCategory::Testing,
                "`cargo test --lib` with passed/failed counts",
            ),
            Arc::new(CargoTestLibTool),
        );

        // ── CodeAnalysis (Fase 9H) ───────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_check_release",
                NexusToolCategory::CodeAnalysis,
                "`cargo check --release` with warning/error counts",
            ),
            Arc::new(CargoCheckReleaseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_check_all_features",
                NexusToolCategory::CodeAnalysis,
                "`cargo check --all-features` with warning/error counts",
            ),
            Arc::new(CargoCheckAllFeaturesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_msrv_detect",
                NexusToolCategory::CodeAnalysis,
                "Walk workspace manifests to find `rust-version` (MSRV)",
            ),
            Arc::new(CargoMsrvDetectTool),
        );
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_edition_detect",
                NexusToolCategory::CodeAnalysis,
                "Walk workspace manifests grouping crates by `edition`",
            ),
            Arc::new(CargoEditionDetectTool),
        );

        // ── Performance (Fase 9H) ────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_size_estimate",
                NexusToolCategory::Performance,
                "Sum sizes of binaries in target/release/ (per-bin and total)",
            ),
            Arc::new(CargoSizeEstimateTool),
        );

        // ── Documentation (Fase 9H) ──────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new(
                "cargo_doc_check",
                NexusToolCategory::Documentation,
                "`cargo doc --no-deps --quiet` with warning/error counts",
            ),
            Arc::new(CargoDocCheckTool),
        );

        // ══════════════════════════════════════════════════════════════════
        //                  FASE 9I — Git extras (20 new)
        // ══════════════════════════════════════════════════════════════════

        // ── Vcs (Fase 9I) ────────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("git_rev_parse", NexusToolCategory::Vcs, "`git rev-parse <ref>` ref → SHA"),
            Arc::new(GitRevParseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_count_objects", NexusToolCategory::Vcs, "`git count-objects -v` repo size info"),
            Arc::new(GitCountObjectsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_reflog", NexusToolCategory::Vcs, "`git reflog -n N` reference log"),
            Arc::new(GitReflogTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_clean_dry", NexusToolCategory::Vcs, "`git clean -nd` dry-run"),
            Arc::new(GitCleanDryTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_check_ignore", NexusToolCategory::Vcs, "`git check-ignore -v` for given paths"),
            Arc::new(GitCheckIgnoreTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_ls_files", NexusToolCategory::Vcs, "`git ls-files` lista file tracciati"),
            Arc::new(GitLsFilesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_ls_tree", NexusToolCategory::Vcs, "`git ls-tree -r <ref>` lista in commit"),
            Arc::new(GitLsTreeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_cat_file", NexusToolCategory::Vcs, "`git cat-file -p <ref>` object content (preview)"),
            Arc::new(GitCatFileTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_for_each_ref", NexusToolCategory::Vcs, "`git for-each-ref` enumera tutte le ref"),
            Arc::new(GitForEachRefTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_merge_base", NexusToolCategory::Vcs, "`git merge-base a b` common ancestor"),
            Arc::new(GitMergeBaseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_diff_stat", NexusToolCategory::Vcs, "`git diff --shortstat <range>` summary"),
            Arc::new(GitDiffStatTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_log_graph", NexusToolCategory::Vcs, "`git log --oneline --graph -n N`"),
            Arc::new(GitLogGraphTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_show_branch", NexusToolCategory::Vcs, "`git show-branch --all`"),
            Arc::new(GitShowBranchTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_archive_dry", NexusToolCategory::Vcs, "Stima dimensione `git archive` (senza scrivere)"),
            Arc::new(GitArchiveDryTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_bundle_verify", NexusToolCategory::Vcs, "`git bundle verify <path>`"),
            Arc::new(GitBundleVerifyTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_fsck", NexusToolCategory::Vcs, "`git fsck --no-progress` repo integrity"),
            Arc::new(GitFsckTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_gc_dry", NexusToolCategory::Vcs, "Verifica se `git gc` è necessario (loose objects threshold)"),
            Arc::new(GitGcDryTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_config_list", NexusToolCategory::Vcs, "`git config --list --local` (sensitive masked)"),
            Arc::new(GitConfigListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_worktree_list", NexusToolCategory::Vcs, "`git worktree list --porcelain`"),
            Arc::new(GitWorktreeListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("git_submodule_list", NexusToolCategory::Vcs, "`git submodule status` lista submodule"),
            Arc::new(GitSubmoduleListTool),
        );

        // ══════════════════════════════════════════════════════════════════
        //                FASE 9J — GitHub extras (20 new)
        // ══════════════════════════════════════════════════════════════════

        // ── GitHub (Fase 9J) ─────────────────────────────────────────────
        self.register_with_handler(
            NexusToolSpec::new("gh_issue_view", NexusToolCategory::GitHub, "`gh issue view <num> --json`"),
            Arc::new(GhIssueViewTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_issue_create", NexusToolCategory::GitHub, "`gh issue create --title --body`"),
            Arc::new(GhIssueCreateTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_issue_close", NexusToolCategory::GitHub, "`gh issue close <num>`"),
            Arc::new(GhIssueCloseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_issue_comment", NexusToolCategory::GitHub, "`gh issue comment <num> --body`"),
            Arc::new(GhIssueCommentTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_pr_close", NexusToolCategory::GitHub, "`gh pr close <num>`"),
            Arc::new(GhPrCloseTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_pr_merge", NexusToolCategory::GitHub, "`gh pr merge <num> --squash|--merge|--rebase`"),
            Arc::new(GhPrMergeTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_pr_review", NexusToolCategory::GitHub, "`gh pr review <num>` approve/request-changes/comment"),
            Arc::new(GhPrReviewTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_pr_diff", NexusToolCategory::GitHub, "`gh pr diff <num>` con conteggio +/-"),
            Arc::new(GhPrDiffTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_pr_checks", NexusToolCategory::GitHub, "`gh pr checks <num>` pass/fail/pending"),
            Arc::new(GhPrChecksTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_pr_files", NexusToolCategory::GitHub, "`gh pr view <num> --json files`"),
            Arc::new(GhPrFilesTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_workflow_view", NexusToolCategory::GitHub, "`gh workflow view <name>`"),
            Arc::new(GhWorkflowViewTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_workflow_run", NexusToolCategory::GitHub, "`gh workflow run <name> --ref`"),
            Arc::new(GhWorkflowRunTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_run_view", NexusToolCategory::GitHub, "`gh run view <id> --json`"),
            Arc::new(GhRunViewTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_run_logs", NexusToolCategory::GitHub, "`gh run view <id> --log`"),
            Arc::new(GhRunLogsTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_run_cancel", NexusToolCategory::GitHub, "`gh run cancel <id>`"),
            Arc::new(GhRunCancelTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_release_view", NexusToolCategory::GitHub, "`gh release view <tag> --json`"),
            Arc::new(GhReleaseViewTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_release_create", NexusToolCategory::GitHub, "`gh release create <tag> --title --notes`"),
            Arc::new(GhReleaseCreateTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_repo_clone_url", NexusToolCategory::GitHub, "`gh repo view --json url,sshUrl`"),
            Arc::new(GhRepoCloneUrlTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_repo_fork_list", NexusToolCategory::GitHub, "`gh repo view --json forkCount,parent`"),
            Arc::new(GhRepoForkListTool),
        );
        self.register_with_handler(
            NexusToolSpec::new("gh_label_list", NexusToolCategory::GitHub, "`gh label list --json`"),
            Arc::new(GhLabelListTool),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_empty() {
        let c = NexusToolCatalog::new();
        assert_eq!(c.len(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn test_catalog_with_builtins() {
        let c = NexusToolCatalog::with_builtins();
        assert!(c.len() > 0);
        // Il seed dovrebbe coprire tutte le categorie principali
        let bd = c.breakdown();
        let covered: usize = bd.iter().filter(|(_, n)| *n > 0).count();
        assert!(covered >= 10, "Expected >= 10 categories covered, got {}", covered);
    }

    #[test]
    fn test_register_and_get() {
        let c = NexusToolCatalog::new();
        c.register(NexusToolSpec::new(
            "custom_tool",
            NexusToolCategory::Utility,
            "A test tool",
        ));
        let spec = c.get("custom_tool").unwrap();
        assert_eq!(spec.name, "custom_tool");
        assert_eq!(spec.category, NexusToolCategory::Utility);
        assert!(!spec.implemented);
    }

    #[test]
    fn test_list_by_category() {
        let c = NexusToolCatalog::with_builtins();
        let security = c.list_by_category(NexusToolCategory::Security);
        assert!(security.len() >= 2);
    }

    #[test]
    fn test_implemented_builder() {
        let spec = NexusToolSpec::new("x", NexusToolCategory::Other, "y").implemented();
        assert!(spec.implemented);
    }

    #[test]
    fn test_global_singleton() {
        NexusToolCatalog::init_global();
        assert!(NexusToolCatalog::global().is_some());
        let a = NexusToolCatalog::global().unwrap();
        let b = NexusToolCatalog::global().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
