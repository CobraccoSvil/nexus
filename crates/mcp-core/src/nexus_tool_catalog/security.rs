//! Registrazione handler dominio: security
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        cargo_audit::CargoAuditTool, deps_audit::DepsAuditTool, license_check::LicenseCheckTool,
        sast_scan::SastScanTool, sec_audit_summary::SecAuditSummaryTool,
        sec_cmd_injection_check::SecCmdInjectionCheckTool, sec_cors_check::SecCorsCheckTool,
        sec_dependency_count::SecDependencyCountTool,
        sec_dockerfile_user_check::SecDockerfileUserCheckTool,
        sec_env_files_check::SecEnvFilesCheckTool, sec_env_var_check::SecEnvVarCheckTool,
        sec_eval_check::SecEvalCheckTool, sec_git_secrets_check::SecGitSecretsCheckTool,
        sec_http_url_count::SecHttpUrlCountTool, sec_jwt_secret_check::SecJwtSecretCheckTool,
        sec_localhost_count::SecLocalhostCountTool, sec_md5_sha1_check::SecMd5Sha1CheckTool,
        sec_panic_count::SecPanicCountTool, sec_random_check::SecRandomCheckTool,
        sec_secret_patterns::SecSecretPatternsTool,
        sec_sql_injection_check::SecSqlInjectionCheckTool, sec_tls_check::SecTlsCheckTool,
        sec_unwrap_count::SecUnwrapCountTool, sec_workflow_perms_check::SecWorkflowPermsCheckTool,
        secret_scan::SecretScanTool,
    };

    // Security
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_audit",
            NexusToolCategory::Security,
            "Run `cargo audit --json` and summarize RUSTSEC advisories",
        ),
        Arc::new(CargoAuditTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "secret_scan",
            NexusToolCategory::Security,
            "Scan project files for hardcoded secrets (regex-based)",
        ),
        Arc::new(SecretScanTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "license_check",
            NexusToolCategory::Security,
            "Analyze package licenses from cargo metadata",
        ),
        Arc::new(LicenseCheckTool),
    );

    // Security (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "sast_scan",
            NexusToolCategory::Security,
            "SAST scan via semgrep if available, else built-in regex rules",
        ),
        Arc::new(SastScanTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "deps_audit",
            NexusToolCategory::Security,
            "Multi-stack dependency audit (cargo audit / npm audit / pip-audit)",
        ),
        Arc::new(DepsAuditTool),
    );

    // Security extras (Fase 9O, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_secret_patterns",
            NexusToolCategory::Security,
            "Heuristic scan for hardcoded secrets in source",
        ),
        Arc::new(SecSecretPatternsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_unwrap_count",
            NexusToolCategory::Security,
            "Count `.unwrap()` and `.expect(` (panic surface)",
        ),
        Arc::new(SecUnwrapCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_panic_count",
            NexusToolCategory::Security,
            "Count panic!/todo!/unimplemented!/unreachable!",
        ),
        Arc::new(SecPanicCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_env_var_check",
            NexusToolCategory::Security,
            "Count `std::env::var` and default fallbacks",
        ),
        Arc::new(SecEnvVarCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_http_url_count",
            NexusToolCategory::Security,
            "Count plaintext http:// vs https:// URLs",
        ),
        Arc::new(SecHttpUrlCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_localhost_count",
            NexusToolCategory::Security,
            "Count localhost / 127.0.0.1 / 0.0.0.0 references",
        ),
        Arc::new(SecLocalhostCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_eval_check",
            NexusToolCategory::Security,
            "Heuristic scan for eval-like / sandbox patterns",
        ),
        Arc::new(SecEvalCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_sql_injection_check",
            NexusToolCategory::Security,
            "Find string interpolation in SQL queries",
        ),
        Arc::new(SecSqlInjectionCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_cmd_injection_check",
            NexusToolCategory::Security,
            "Find Command::new + shell -c patterns",
        ),
        Arc::new(SecCmdInjectionCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_dependency_count",
            NexusToolCategory::Security,
            "Count dependencies across all Cargo.toml",
        ),
        Arc::new(SecDependencyCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_git_secrets_check",
            NexusToolCategory::Security,
            "Scan .git/config for credentials in URLs",
        ),
        Arc::new(SecGitSecretsCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_env_files_check",
            NexusToolCategory::Security,
            "Find .env* files and check .gitignore coverage",
        ),
        Arc::new(SecEnvFilesCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_dockerfile_user_check",
            NexusToolCategory::Security,
            "Check Dockerfile USER directive (non-root)",
        ),
        Arc::new(SecDockerfileUserCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_workflow_perms_check",
            NexusToolCategory::Security,
            "Check workflows for permissions: blocks",
        ),
        Arc::new(SecWorkflowPermsCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_cors_check",
            NexusToolCategory::Security,
            "Find permissive CORS patterns",
        ),
        Arc::new(SecCorsCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_jwt_secret_check",
            NexusToolCategory::Security,
            "Find hardcoded JWT secrets and weak algorithms",
        ),
        Arc::new(SecJwtSecretCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_md5_sha1_check",
            NexusToolCategory::Security,
            "Find weak hash algorithm usage (md5/sha1)",
        ),
        Arc::new(SecMd5Sha1CheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_random_check",
            NexusToolCategory::Security,
            "Find non-secure RNG usage",
        ),
        Arc::new(SecRandomCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_tls_check",
            NexusToolCategory::Security,
            "Find TLS verify=false / accept_invalid_certs",
        ),
        Arc::new(SecTlsCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "sec_audit_summary",
            NexusToolCategory::Security,
            "High-level audit overview combining several scans",
        ),
        Arc::new(SecAuditSummaryTool),
    );
}
