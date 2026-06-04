//! Cache in-memory + persistenza DB del build graph per progetto.
//!
//! Architettura:
//! - Singleton globale via `once_cell::sync::OnceCell`. Inizializzato in
//!   `main.rs` dopo la creazione del pool DB.
//! - In-memory: `HashMap<Uuid, (BuildGraphInfo, Instant)>` con TTL (default
//!   600s, configurabile via `agent.build_graph.default_ttl_secs`).
//! - DB persistente: tabella `nexus_project_build_graph` (mig 0312). Al
//!   cache miss ricalcoliamo dai resolver, persistiamo, ripopoliamo la map.
//! - Invalidazione: `invalidate(project_id)` chiamato dal `wiki::watcher`
//!   quando un file di config (tsconfig.json, Cargo.toml, ...) cambia.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::OnceCell;
use sqlx::{PgPool, Row};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::model::BuildGraphInfo;
use super::{resolver_go, resolver_python, resolver_rust, resolver_typescript};

static GLOBAL_CACHE: OnceCell<Arc<BuildGraphCache>> = OnceCell::new();

pub struct BuildGraphCache {
    db: PgPool,
    inner: RwLock<HashMap<Uuid, (BuildGraphInfo, Instant)>>,
    ttl: Duration,
}

impl BuildGraphCache {
    /// Inizializza il singleton globale. Idempotente: chiamate successive sono no-op.
    pub async fn init_global(db: PgPool) {
        let ttl_secs = load_ttl_setting(&db).await;
        let cache = Arc::new(Self {
            db,
            inner: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        });
        let _ = GLOBAL_CACHE.set(cache);
    }

    pub fn global() -> Option<Arc<Self>> {
        GLOBAL_CACHE.get().cloned()
    }

    /// Ritorna il `BuildGraphInfo` cached (se fresh) o lo ricalcola dai resolver
    /// e lo persiste. Errori dei resolver vengono propagati al chiamante.
    pub async fn get_or_compute(&self, project_id: Uuid) -> anyhow::Result<BuildGraphInfo> {
        // Read-only check cache.
        {
            let r = self.inner.read().await;
            if let Some((info, computed_at)) = r.get(&project_id) {
                if Instant::now().duration_since(*computed_at) < self.ttl {
                    return Ok(info.clone());
                }
            }
        }
        let info = self.compute(project_id).await?;
        self.persist(&info).await;
        {
            let mut w = self.inner.write().await;
            w.insert(project_id, (info.clone(), Instant::now()));
        }
        Ok(info)
    }

    /// Invalida (rimuove dalla cache) il build graph di un progetto.
    /// Chiamato dal watcher al cambio di un file di config.
    pub async fn invalidate(&self, project_id: Uuid) {
        let removed = {
            let mut w = self.inner.write().await;
            w.remove(&project_id).is_some()
        };
        if removed {
            tracing::info!(
                project_id = %project_id,
                "build_graph.cache: invalidato (watcher trigger)"
            );
        }
    }

    /// Riferimento DB (per riuso da `tool.rs`).
    pub fn db(&self) -> &PgPool {
        &self.db
    }

    async fn compute(&self, project_id: Uuid) -> anyhow::Result<BuildGraphInfo> {
        let root = load_project_root(&self.db, project_id).await?;
        let root_path = PathBuf::from(&root);
        if !root_path.is_dir() {
            anyhow::bail!(
                "repository_root_path '{}' non e' una directory accessibile",
                root
            );
        }

        // Rilevamento linguaggio per priorita'. Un progetto puo' avere piu' di
        // un config (es. Rust + node frontend); scegliamo quello principale
        // in base alla "verticalita'" della build.
        let has_cargo = root_path.join("Cargo.toml").is_file();
        let has_go = root_path.join("go.mod").is_file() || root_path.join("go.work").is_file();
        let has_ts = root_path.join("tsconfig.json").is_file()
            || root_path.join("tsconfig.app.json").is_file()
            || root_path.join("jsconfig.json").is_file();
        let has_py = root_path.join("pyproject.toml").is_file()
            || root_path.join("setup.py").is_file()
            || root_path.join("setup.cfg").is_file();
        let has_pkg = root_path.join("package.json").is_file();

        // Ordine: Rust > Go > TypeScript (anche solo package.json) > Python.
        // Rationale: Cargo.toml e' il segnale piu' forte (un repo Rust raramente
        // ne ha altri); TS prevale su Python perche' molti progetti Python in
        // monorepo hanno anche tooling JS (eslint/prettier) ma il codice base e' Python.
        if has_cargo {
            return resolver_rust::resolve_rust(project_id, &root_path).await;
        }
        if has_go {
            return resolver_go::resolve_go(project_id, &root_path).await;
        }
        if has_ts || has_pkg {
            return resolver_typescript::resolve_typescript(project_id, &root_path).await;
        }
        if has_py {
            return resolver_python::resolve_python(project_id, &root_path).await;
        }
        // Niente di riconosciuto: ritorna "unknown" con include "**/*" (best-effort).
        tracing::debug!(
            project_id = %project_id,
            root = %root_path.display(),
            "build_graph: nessun config riconosciuto, ritorno unknown"
        );
        Ok(BuildGraphInfo::unknown(project_id))
    }

    async fn persist(&self, info: &BuildGraphInfo) {
        let res = sqlx::query(
            r#"INSERT INTO nexus_project_build_graph
                (project_id, language, include_globs, exclude_globs, entry_points,
                 monorepo_members, generated_dirs, sources, computed_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
                ON CONFLICT (project_id) DO UPDATE SET
                    language = EXCLUDED.language,
                    include_globs = EXCLUDED.include_globs,
                    exclude_globs = EXCLUDED.exclude_globs,
                    entry_points = EXCLUDED.entry_points,
                    monorepo_members = EXCLUDED.monorepo_members,
                    generated_dirs = EXCLUDED.generated_dirs,
                    sources = EXCLUDED.sources,
                    computed_at = NOW()"#,
        )
        .bind(info.project_id)
        .bind(&info.language)
        .bind(serde_json::to_value(&info.include_globs).unwrap_or_default())
        .bind(serde_json::to_value(&info.exclude_globs).unwrap_or_default())
        .bind(serde_json::to_value(&info.entry_points).unwrap_or_default())
        .bind(serde_json::to_value(&info.monorepo_members).unwrap_or_default())
        .bind(serde_json::to_value(&info.generated_dirs).unwrap_or_default())
        .bind(serde_json::to_value(&info.sources).unwrap_or_default())
        .execute(&self.db)
        .await;
        if let Err(e) = res {
            tracing::warn!(
                project_id = %info.project_id,
                error = %e,
                "build_graph: persist fallita (proseguo, solo cache in-memory)"
            );
        }
    }
}

async fn load_ttl_setting(db: &PgPool) -> u64 {
    let row = sqlx::query(
        "SELECT value FROM settings WHERE key = 'agent.build_graph.default_ttl_secs' LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    row.and_then(|r| r.try_get::<String, _>("value").ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| (10..=86_400).contains(n))
        .unwrap_or(600)
}

async fn load_project_root(db: &PgPool, project_id: Uuid) -> anyhow::Result<String> {
    let row = sqlx::query(
        "SELECT repository_root_path FROM projects WHERE id = $1 LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("project_id {} non trovato", project_id))?;
    let root: Option<String> = row.try_get("repository_root_path").ok();
    let root = root.unwrap_or_default();
    if root.trim().is_empty() {
        anyhow::bail!("repository_root_path vuoto per project {}", project_id);
    }
    Ok(root)
}
