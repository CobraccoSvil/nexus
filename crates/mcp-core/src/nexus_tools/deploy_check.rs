//! `deployment::deploy_check` — pre-deploy readiness checks.
//!
//! Controlla che la repository contenga i file necessari per un deploy
//! pulito e segnala eventuali problemi prima di shippare. È un tool
//! read-only: non tocca file né esegue deploy.
//!
//! Check eseguiti:
//! 1. **Uncommitted changes** — `git status --porcelain=v1` vuoto?
//! 2. **Unpushed commits** — HEAD uguale a `@{u}` (upstream)?
//! 3. **Deploy files presenti** — almeno uno tra: Dockerfile, docker-compose.yml,
//!    deploy/*.sh, .github/workflows/*.yml
//! 4. **Env sample** — se esiste `.env` deve esistere anche `.env.example`
//!    (evita di droppare credenziali segrete nel repo per errore)
//! 5. **Lockfile commit** — se esiste Cargo.lock/package-lock.json/yarn.lock
//!    deve essere tracked (non gitignored)
//!
//! Output:
//! ```json
//! {
//!   "ready": false,
//!   "checks": [
//!     {"name": "uncommitted_changes", "ok": true, "detail": "clean"},
//!     {"name": "unpushed_commits", "ok": false, "detail": "3 commits ahead"},
//!     ...
//!   ],
//!   "warnings": ["env file without sample"],
//!   "blockers": ["unpushed commits"]
//! }
//! ```

use super::exec::run_cmd;
use super::{NexusToolContext, NexusToolError, NexusToolHandler, NexusToolSafety};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

pub struct DeployCheckTool;

#[async_trait]
impl NexusToolHandler for DeployCheckTool {
    async fn execute(
        &self,
        ctx: &NexusToolContext,
        _args: &Value,
    ) -> Result<Value, NexusToolError> {
        let mut checks: Vec<Value> = Vec::new();
        let mut blockers: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        // 1. Uncommitted changes
        let porcelain =
            run_cmd("git", &["status", "--porcelain=v1"], &ctx.project_root, 30).await?;
        let uncommitted_lines = porcelain.stdout.lines().filter(|l| !l.is_empty()).count();
        let uncommitted_ok = uncommitted_lines == 0;
        checks.push(json!({
            "name": "uncommitted_changes",
            "ok": uncommitted_ok,
            "detail": if uncommitted_ok { "clean".to_string() } else { format!("{} file(s) pendenti", uncommitted_lines) }
        }));
        if !uncommitted_ok {
            blockers.push(format!("{} uncommitted changes", uncommitted_lines));
        }

        // 2. Unpushed commits (confronto HEAD vs @{u})
        let rev_head = run_cmd("git", &["rev-parse", "HEAD"], &ctx.project_root, 15).await;
        let rev_ups = run_cmd("git", &["rev-parse", "@{u}"], &ctx.project_root, 15).await;
        match (rev_head, rev_ups) {
            (Ok(h), Ok(u)) if h.success() && u.success() => {
                let head_sha = h.stdout.trim();
                let ups_sha = u.stdout.trim();
                let synced = head_sha == ups_sha;
                checks.push(json!({
                    "name": "unpushed_commits",
                    "ok": synced,
                    "detail": if synced { "in sync with upstream".to_string() } else { format!("HEAD {} vs upstream {}", &head_sha[..7.min(head_sha.len())], &ups_sha[..7.min(ups_sha.len())]) }
                }));
                if !synced {
                    warnings
                        .push("HEAD diverge da upstream — ricordati push prima del deploy".into());
                }
            }
            _ => {
                checks.push(json!({
                    "name": "unpushed_commits",
                    "ok": false,
                    "detail": "impossibile leggere HEAD o upstream (branch senza tracking?)"
                }));
                warnings.push("nessun upstream configurato sul branch corrente".into());
            }
        }

        // 3. Deploy files presenti
        let deploy_signals = [
            "Dockerfile",
            "docker-compose.yml",
            "deploy",
            ".github/workflows",
        ];
        let has_deploy_file = deploy_signals
            .iter()
            .any(|sig| ctx.project_root.join(sig).exists());
        checks.push(json!({
            "name": "deploy_artifacts",
            "ok": has_deploy_file,
            "detail": if has_deploy_file { "found deploy markers".to_string() } else { "no Dockerfile/docker-compose/deploy/.github/workflows".to_string() }
        }));
        if !has_deploy_file {
            blockers.push("nessun artefatto di deploy (Dockerfile, compose, workflow)".into());
        }

        // 4. Env sample
        let has_env = ctx.project_root.join(".env").exists();
        let has_env_sample = ctx.project_root.join(".env.example").exists()
            || ctx.project_root.join(".env.sample").exists();
        let env_ok = !has_env || has_env_sample;
        checks.push(json!({
            "name": "env_sample",
            "ok": env_ok,
            "detail": match (has_env, has_env_sample) {
                (false, _) => "no .env file".to_string(),
                (true, true) => ".env + .env.example present".to_string(),
                (true, false) => ".env presente senza .env.example".to_string(),
            }
        }));
        if !env_ok {
            warnings.push(".env senza .env.example corrispondente".into());
        }

        // 5. Lockfile tracking
        let lockfiles = [
            "Cargo.lock",
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
        ];
        let mut lockfile_issues: Vec<String> = Vec::new();
        for lf in &lockfiles {
            let p = ctx.project_root.join(lf);
            if p.exists() {
                // Verifica se è tracked con git check-ignore
                let check = run_cmd("git", &["check-ignore", lf], &ctx.project_root, 10)
                    .await
                    .ok();
                // check-ignore exit=0 significa "è ignorato" → problema
                if let Some(c) = check {
                    if c.exit_code == 0 {
                        lockfile_issues.push(format!("{} è gitignored ma esiste sul FS", lf));
                    }
                }
            }
        }
        let lockfile_ok = lockfile_issues.is_empty();
        checks.push(json!({
            "name": "lockfiles_tracked",
            "ok": lockfile_ok,
            "detail": if lockfile_ok { "ok".to_string() } else { lockfile_issues.join("; ") }
        }));
        if !lockfile_ok {
            blockers.extend(lockfile_issues);
        }

        let ready = blockers.is_empty();
        Ok(json!({
            "ready": ready,
            "checks": checks,
            "warnings": warnings,
            "blockers": blockers,
        }))
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    fn safety(&self) -> NexusToolSafety {
        // Legge FS e spawna git read-only.
        NexusToolSafety::read_only_subproc()
    }
}

/// Helper pure function per testing: presenza di deploy markers in una root data.
#[cfg(test)]
fn has_any_deploy_marker(root: &Path) -> bool {
    let deploy_signals = [
        "Dockerfile",
        "docker-compose.yml",
        "deploy",
        ".github/workflows",
    ];
    deploy_signals.iter().any(|sig| root.join(sig).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Crea una dir temporanea unica e la ritorna insieme al path.
    /// Cleanup best-effort a fine test via `std::fs::remove_dir_all`.
    struct TmpDir(std::path::PathBuf);
    impl TmpDir {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("nexus-deploycheck-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_has_any_deploy_marker_finds_dockerfile() {
        let tmp = TmpDir::new();
        std::fs::write(tmp.0.join("Dockerfile"), "FROM rust").unwrap();
        assert!(has_any_deploy_marker(&tmp.0));
    }

    #[test]
    fn test_has_any_deploy_marker_empty_dir() {
        let tmp = TmpDir::new();
        assert!(!has_any_deploy_marker(&tmp.0));
    }

    #[test]
    fn test_safety_is_readonly_subproc() {
        let s = DeployCheckTool.safety();
        assert!(s.read_only);
        assert!(s.can_execute_subproc);
    }
}
