// ═══════════════════════════════════════════════════════════════════════════
// wiki/watcher.rs — Watcher bidirezionale vault -> DB (ADR 0017 v2 TODO 1).
//
// Quando un utente modifica un file `.md` direttamente in:
//   - `docs/.nexus-vault/` (meta-vault Nexus)
//   - `<project_root>/.nexus-vault/` (vault per progetto)
//
// questo worker osserva il filesystem via `notify` (inotify su Linux), debouncea
// gli eventi e chiama `wiki::reingest::reingest_path` per il singolo file. Cosi'
// l'editing diretto da Obsidian/CLI si propaga su `wiki_docs` + Qdrant senza
// passare dalla UI.
//
// Settings DB-driven (mig 0301):
//   - agent.wiki.watcher_enabled            (default true)
//   - agent.wiki.watcher_debounce_ms        (default 500)
//   - agent.wiki.watcher_poll_interval_secs (default 60)
//
// TODO post-MVP: hot-reload dei progetti. Attualmente lo snapshot dei progetti
// e' calcolato una sola volta all'avvio; nuovi progetti registrati post-startup
// (o `.nexus-vault/` creati dopo il primo scan) richiedono il restart di
// mcp-core per essere osservati. Un loop di rescan periodico ridurrebbe la
// finestra ma non e' implementato qui per non complicare il path: notify
// supporta `watcher.watch(path, ...)` runtime ma serve riconciliare lo stato
// con la lista corrente.
// ═══════════════════════════════════════════════════════════════════════════

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::model::WikiScope;
use crate::reingest::reingest_path;
use crate::vault::vault_root_for_scope;
use crate::deps::WikiDeps;

/// File di config che, se modificati, invalidano il build graph (ADR 0020).
/// Match esatto sul nome file (case-insensitive).
const BUILD_GRAPH_CONFIG_FILES: &[&str] = &[
    "tsconfig.json",
    "tsconfig.app.json",
    "tsconfig.build.json",
    "tsconfig.node.json",
    "jsconfig.json",
    "package.json",
    "pnpm-workspace.yaml",
    "cargo.toml",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "go.mod",
    "go.work",
];

/// Mappatura root progetto (repository_root_path) → project_id, per
/// l'invalidazione della cache build graph (ADR 0020).
#[derive(Debug, Clone)]
struct ProjectRootMap {
    path: PathBuf,
    project_id: Uuid,
}

/// True se il nome file appartiene ai config che invalidano il build graph.
fn is_build_graph_config(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    BUILD_GRAPH_CONFIG_FILES
        .iter()
        .any(|c| c.to_ascii_lowercase() == lower)
}

/// Settings caricate al boot (con safe defaults se il DB non li ha ancora).
#[derive(Debug, Clone, Copy)]
struct WatcherSettings {
    enabled: bool,
    debounce_ms: u64,
}

impl WatcherSettings {
    const fn safe_defaults() -> Self {
        Self {
            enabled: true,
            debounce_ms: 500,
        }
    }
}

async fn load_settings(db: &PgPool) -> WatcherSettings {
    let mut out = WatcherSettings::safe_defaults();
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM settings WHERE key IN \
         ('agent.wiki.watcher_enabled','agent.wiki.watcher_debounce_ms')",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    for (k, v) in rows {
        match k.as_str() {
            "agent.wiki.watcher_enabled" => {
                out.enabled = !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off");
            }
            "agent.wiki.watcher_debounce_ms" => {
                if let Ok(n) = v.trim().parse::<u64>() {
                    if (50..=10_000).contains(&n) {
                        out.debounce_ms = n;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Mappatura di una root osservata al suo scope + project_id.
#[derive(Debug, Clone)]
struct WatchedRoot {
    path: PathBuf,
    scope: WikiScope,
    project_id: Option<Uuid>,
}

/// Risolve `path` (assoluto, da evento notify) verso una delle root osservate.
/// Ritorna la root proprietaria col path piu' lungo che e' prefisso (per
/// gestire correttamente vault innestati, sebbene improbabili).
fn resolve_root<'a>(path: &Path, roots: &'a [WatchedRoot]) -> Option<&'a WatchedRoot> {
    let mut best: Option<&WatchedRoot> = None;
    for r in roots {
        if path.starts_with(&r.path) {
            match best {
                None => best = Some(r),
                Some(prev) if r.path.as_os_str().len() > prev.path.as_os_str().len() => {
                    best = Some(r)
                }
                _ => {}
            }
        }
    }
    best
}

/// Avvia il watcher in background. Idempotente: chiamata multipla, ignora le
/// successive (gate sul flag in `AppState.watching_projects` rifratto in una
/// const dedicata? Per ora il chiamante deve invocarla una volta sola in main).
pub fn start_wiki_watcher(state: Arc<WikiDeps>) {
    tokio::spawn(async move {
        if let Err(e) = run(state).await {
            tracing::error!(error = %e, "wiki.watcher: terminato con errore");
        }
    });
}

async fn run(state: Arc<WikiDeps>) -> anyhow::Result<()> {
    let settings = load_settings(&state.db).await;
    if !settings.enabled {
        tracing::info!("wiki.watcher: disabilitato via settings, no-op");
        return Ok(());
    }

    // ── Costruzione lista root da osservare ──────────────────────────────
    let mut roots: Vec<WatchedRoot> = Vec::new();

    // Meta-vault (sempre, se directory esiste).
    match vault_root_for_scope(&state, WikiScope::Meta, None).await {
        Ok(p) => {
            let path = PathBuf::from(&p);
            if path.is_dir() {
                roots.push(WatchedRoot {
                    path,
                    scope: WikiScope::Meta,
                    project_id: None,
                });
            } else {
                tracing::warn!(path = %p, "wiki.watcher: vault meta non esiste, skip");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "wiki.watcher: vault_root_for_scope(meta) fallito");
        }
    }

    // Vault per progetto + root progetto (per ADR 0020 build graph config).
    let project_rows: Vec<(Uuid, Option<String>)> = sqlx::query_as::<_, (Uuid, Option<String>)>(
        "SELECT id, repository_root_path FROM projects \
         WHERE repository_root_path IS NOT NULL AND repository_root_path <> ''",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Mappa root → project_id per gli eventi di config (ADR 0020).
    let mut project_root_map: Vec<ProjectRootMap> = Vec::new();

    for (pid, repo_root) in project_rows {
        match vault_root_for_scope(&state, WikiScope::Project, Some(pid)).await {
            Ok(p) => {
                let path = PathBuf::from(&p);
                if path.is_dir() {
                    roots.push(WatchedRoot {
                        path,
                        scope: WikiScope::Project,
                        project_id: Some(pid),
                    });
                }
            }
            Err(e) => {
                tracing::debug!(project_id = %pid, error = %e, "wiki.watcher: vault_root_for_scope(project) skip");
            }
        }
        // Registra root del repository per gli eventi di config (ADR 0020).
        if let Some(r) = repo_root {
            let p = PathBuf::from(&r);
            if p.is_dir() {
                project_root_map.push(ProjectRootMap {
                    path: p,
                    project_id: pid,
                });
            }
        }
    }

    if roots.is_empty() {
        tracing::info!("wiki.watcher: nessuna root da osservare, exit");
        return Ok(());
    }

    // ── Avvio notify watcher ─────────────────────────────────────────────
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(1024);
    let tx_sync = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx_sync.blocking_send(res);
    })?;

    for r in &roots {
        match watcher.watch(&r.path, RecursiveMode::Recursive) {
            Ok(_) => tracing::info!(
                scope = r.scope.as_str(),
                project_id = ?r.project_id,
                path = %r.path.display(),
                "wiki.watcher: avviato"
            ),
            Err(e) => tracing::warn!(
                scope = r.scope.as_str(),
                path = %r.path.display(),
                error = %e,
                "wiki.watcher: watch fallita su root"
            ),
        }
    }

    // Watch root progetto (ADR 0020: invalidazione build graph cache su
    // change config). Profondita' limitata = non-ricorsivo per evitare di
    // duplicare gli eventi dei vault gia' osservati e ridurre il rumore
    // (i config sono al root del progetto, non in sub-dir profonde).
    for m in &project_root_map {
        match watcher.watch(&m.path, RecursiveMode::NonRecursive) {
            Ok(_) => tracing::info!(
                project_id = %m.project_id,
                path = %m.path.display(),
                "wiki.watcher: avviato (root progetto per config build graph)"
            ),
            Err(e) => tracing::debug!(
                project_id = %m.project_id,
                path = %m.path.display(),
                error = %e,
                "wiki.watcher: watch project root skip"
            ),
        }
    }

    // ── Loop di debounce ──────────────────────────────────────────────────
    // Buffer di path pendenti -> ultimo Instant in cui sono stati toccati.
    // Quando passa `debounce_ms` senza nuovi eventi, flush.
    let debounce = Duration::from_millis(settings.debounce_ms);
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();

    loop {
        // Calcola il prossimo deadline = min(pending.values) + debounce.
        let timeout = pending
            .values()
            .min()
            .copied()
            .map(|earliest| {
                let target = earliest + debounce;
                let now = Instant::now();
                if target > now {
                    target - now
                } else {
                    Duration::ZERO
                }
            })
            // Se non c'e' niente in coda, dormiamo "molto" (1h) — la recv ci
            // sveglia al prossimo evento.
            .unwrap_or_else(|| Duration::from_secs(3600));

        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    None => break, // canale chiuso (shutdown)
                    Some(Ok(event)) => {
                        enqueue_event(&mut pending, &event, &roots);
                        // ADR 0020: se l'evento tocca un file di config build
                        // graph dentro una project root, invalida la cache.
                        maybe_invalidate_build_graph(&event, &project_root_map).await;
                    }
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "wiki.watcher: notify error");
                    }
                }
            }
            _ = tokio::time::sleep(timeout) => {
                if !pending.is_empty() {
                    flush(&state, &roots, &mut pending, debounce).await;
                }
            }
        }
    }

    Ok(())
}

/// Aggiunge i path dell'evento al buffer (solo .md, solo Create/Modify).
/// Il filtro delle cartelle escluse delega al punto unico
/// `nexus_tool_kit::is_in_skipped_dir` per COMPONENTE relativo al vault root
/// (regola L, S24): il vecchio filtro substring con '/' non matchava mai i
/// separatori '\' di Windows e il watcher reingeriva i .md di
/// .git/node_modules/.venv interi (incidente stack overflow 20/07).
fn enqueue_event(pending: &mut HashMap<PathBuf, Instant>, event: &Event, roots: &[WatchedRoot]) {
    match &event.kind {
        EventKind::Create(_) => {}
        EventKind::Modify(notify::event::ModifyKind::Data(_)) => {}
        EventKind::Modify(notify::event::ModifyKind::Any) => {}
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => {}
        _ => return,
    }
    let now = Instant::now();
    for p in &event.paths {
        if p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("md"))
            .unwrap_or(false)
        {
            // Path fuori da ogni vault root (es. i watch non-ricorsivi sulle
            // project root per ADR 0020): mai reingeribili, scarta subito —
            // il flush li avrebbe comunque scartati in resolve_root.
            let Some(root) = resolve_root(p, roots) else {
                continue;
            };
            if nexus_tool_kit::is_in_skipped_dir(p, &root.path) {
                continue;
            }
            pending.insert(p.clone(), now);
        }
    }
}

/// ADR 0020: se l'evento riguarda un file di config build graph dentro una
/// project root, invalida la cache del progetto. Eventi multipli sullo stesso
/// progetto sono idempotenti (la prossima `get_or_compute` riparsera' i config).
async fn maybe_invalidate_build_graph(event: &Event, roots: &[ProjectRootMap]) {
    if roots.is_empty() {
        return;
    }
    // Filtra solo Create/Modify (i config rimangono validi su Access/Remove parziali).
    match &event.kind {
        EventKind::Create(_) => {}
        EventKind::Modify(_) => {}
        EventKind::Remove(_) => {}
        _ => return,
    }
    let Some(cache) = nexus_build_graph::BuildGraphCache::global() else {
        return;
    };
    let mut already_invalidated: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for p in &event.paths {
        let Some(file_name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_build_graph_config(file_name) {
            continue;
        }
        // Risolvi project_id: la project root col piu' lungo prefisso che e'
        // ancestor del path.
        let mut best: Option<&ProjectRootMap> = None;
        for r in roots {
            if p.starts_with(&r.path) {
                match best {
                    None => best = Some(r),
                    Some(prev) if r.path.as_os_str().len() > prev.path.as_os_str().len() => {
                        best = Some(r)
                    }
                    _ => {}
                }
            }
        }
        if let Some(owner) = best {
            if already_invalidated.insert(owner.project_id) {
                cache.invalidate(owner.project_id).await;
                tracing::info!(
                    project_id = %owner.project_id,
                    path = %p.display(),
                    "build_graph: cache invalidata (watcher: config modificato)"
                );
            }
        }
    }
}

/// Esegue il reingest dei path il cui ultimo evento e' piu' vecchio di
/// `debounce`. I path piu' freschi restano in coda per il prossimo giro.
async fn flush(
    state: &Arc<WikiDeps>,
    roots: &[WatchedRoot],
    pending: &mut HashMap<PathBuf, Instant>,
    debounce: Duration,
) {
    let now = Instant::now();
    let ready: Vec<PathBuf> = pending
        .iter()
        .filter_map(|(p, t)| {
            if now.duration_since(*t) >= debounce {
                Some(p.clone())
            } else {
                None
            }
        })
        .collect();
    for p in &ready {
        pending.remove(p);
    }

    for abs_path in ready {
        let Some(root) = resolve_root(&abs_path, roots) else {
            tracing::debug!(path = %abs_path.display(), "wiki.watcher: nessuna root proprietaria, skip");
            continue;
        };
        let started = Instant::now();
        match reingest_path(state, root.scope, root.project_id, &abs_path, &root.path).await {
            Ok(true) => {
                tracing::info!(
                    scope = root.scope.as_str(),
                    project_id = ?root.project_id,
                    path = %abs_path.display(),
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "wiki.reingest: doc aggiornato (watcher)"
                );
            }
            Ok(false) => {
                tracing::debug!(
                    path = %abs_path.display(),
                    "wiki.watcher: file ignorato dal reingest (skip)"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %abs_path.display(),
                    error = %e,
                    "wiki.watcher: reingest fallito"
                );
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_root_picks_longest_prefix() {
        let roots = vec![
            WatchedRoot {
                path: PathBuf::from("/tmp/a"),
                scope: WikiScope::Meta,
                project_id: None,
            },
            WatchedRoot {
                path: PathBuf::from("/tmp/a/sub"),
                scope: WikiScope::Project,
                project_id: Some(Uuid::nil()),
            },
        ];
        let r = resolve_root(Path::new("/tmp/a/sub/file.md"), &roots).unwrap();
        assert_eq!(r.path, PathBuf::from("/tmp/a/sub"));
        let r2 = resolve_root(Path::new("/tmp/a/other.md"), &roots).unwrap();
        assert_eq!(r2.path, PathBuf::from("/tmp/a"));
        assert!(resolve_root(Path::new("/elsewhere/x.md"), &roots).is_none());
    }

    fn roots_di_test(vault: &str) -> Vec<WatchedRoot> {
        vec![WatchedRoot {
            path: PathBuf::from(vault),
            scope: WikiScope::Meta,
            project_id: None,
        }]
    }

    #[test]
    fn enqueue_event_filters_non_md_and_excluded() {
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let event = Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![
                PathBuf::from("/tmp/vault/a.md"),
                PathBuf::from("/tmp/vault/b.txt"),
                PathBuf::from("/tmp/vault/.git/c.md"),
                PathBuf::from("/tmp/vault/sub/.obsidian/d.md"),
                PathBuf::from("/tmp/vault/node_modules/pkg/e.md"),
                PathBuf::from("/tmp/vault/api/.venv/lib/site-packages/f.md"),
                PathBuf::from("/elsewhere/fuori-da-ogni-root.md"),
            ],
            attrs: Default::default(),
        };
        enqueue_event(&mut pending, &event, &roots_di_test("/tmp/vault"));
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&PathBuf::from("/tmp/vault/a.md")));
    }

    #[cfg(windows)]
    #[test]
    fn enqueue_event_regressione_backslash_windows() {
        // Incidente 20/07: il filtro substring "/.git/" ecc. non matchava mai
        // i path Windows (separatore '\') e il watcher accodava i .md di
        // node_modules e site-packages (migliaia per virtualenv). Il vault
        // root e' esso stesso una dot-dir (.nexus-vault) e NON deve essere
        // scartato: si valutano solo i componenti relativi al root.
        let roots = roots_di_test(r"D:\IDEAI\docs\.nexus-vault");
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let event = Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![
                PathBuf::from(r"D:\IDEAI\docs\.nexus-vault\adr\0010-porte.md"),
                PathBuf::from(r"D:\IDEAI\docs\.nexus-vault\node_modules\pkg\readme.md"),
                PathBuf::from(r"D:\IDEAI\docs\.nexus-vault\.venv\Lib\site-packages\pkg\doc.md"),
                PathBuf::from(r"D:\IDEAI\docs\.nexus-vault\.git\x.md"),
                PathBuf::from(r"D:\IDEAI\docs\.nexus-vault\target\doc\y.md"),
            ],
            attrs: Default::default(),
        };
        enqueue_event(&mut pending, &event, &roots);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&PathBuf::from(
            r"D:\IDEAI\docs\.nexus-vault\adr\0010-porte.md"
        )));
    }

    #[test]
    fn detects_build_graph_config_filenames() {
        assert!(is_build_graph_config("tsconfig.json"));
        assert!(is_build_graph_config("Cargo.toml"));
        assert!(is_build_graph_config("CARGO.TOML"));
        assert!(is_build_graph_config("pyproject.toml"));
        assert!(is_build_graph_config("go.mod"));
        assert!(is_build_graph_config("pnpm-workspace.yaml"));
        assert!(!is_build_graph_config("random.md"));
        assert!(!is_build_graph_config("App.tsx"));
    }

    #[test]
    fn enqueue_event_ignores_non_create_modify() {
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let event = Event {
            kind: EventKind::Access(notify::event::AccessKind::Read),
            paths: vec![PathBuf::from("/tmp/vault/a.md")],
            attrs: Default::default(),
        };
        enqueue_event(&mut pending, &event, &roots_di_test("/tmp/vault"));
        assert!(pending.is_empty());
    }
}
