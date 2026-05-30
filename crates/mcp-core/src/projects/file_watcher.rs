/// File watcher per il re-indicizzazione semantica automatica.
///
/// Usa il crate `notify` (inotify su Linux, kqueue su macOS, ReadDirectoryChangesW
/// su Windows) per rilevare modifiche esterne ai file di codice sorgente.
/// Quando un file viene creato o modificato, viene re-indicizzato in Qdrant via
/// `reindex_single_file` (stessa funzione usata dagli agent tool `write_file` e
/// `edit_file`).
///
/// Il watcher gira in background per tutta la durata del processo. Un `DashSet`
/// in `AppState` impedisce di avviare watcher duplicati sullo stesso progetto.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::AppState;

/// Estensioni monitorate per la RE-INDICIZZAZIONE semantica (stessa lista di
/// `reindex_single_file`): solo file di codice "pesante" che vale la pena
/// rievaluare nell'indice vettoriale.
const CODE_EXTENSIONS: &[&str] = &["tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue"];

/// Estensioni che, oltre al codice, vanno NOTIFICATE al frontend tramite
/// `FileChanged` per far refreshare il pannello explorer. Lista piu' larga
/// perche' include manifest/config/markup/docs che NON serve indicizzare nel
/// vector store ma servono visibilita' immediata nella UI (es. README.md,
/// package.json, Cargo.toml creati da un sub-agent implementer).
const NOTIFY_EXTENSIONS: &[&str] = &[
    // codice (gia' in CODE_EXTENSIONS, ripetuto per chiarezza)
    "tsx", "jsx", "ts", "js", "rs", "py", "cs", "go", "vue",
    // markup / styling
    "html", "htm", "css", "scss", "sass", "less", "svg",
    // dati / config
    "json", "yaml", "yml", "toml", "xml", "ini", "env",
    // documentazione / testo
    "md", "mdx", "txt", "rst", "adoc",
    // shell / db / docker
    "sh", "bash", "zsh", "ps1", "sql", "dockerfile", "lock",
    // build / package manager
    "cjs", "mjs", "d.ts",
];

/// Directory da ignorare.
const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".next",
    "dist",
    "coverage",
    ".turbo",
    "__pycache__",
];

/// Avvia un file watcher per `root` associato al progetto `project_id`.
///
/// Se il progetto e' gia' monitorato (campo `watching_projects` in `AppState`),
/// la funzione ritorna immediatamente senza lanciare nulla.
/// Il watcher gira in background con debounce di 500 ms.
pub fn spawn_file_watcher(state: &AppState, project_id: Uuid, root: PathBuf) {
    // Gate: se gia' attivo, niente da fare.
    if state.watching_projects.contains(&project_id) {
        return;
    }
    if !root.exists() {
        tracing::warn!(
            "spawn_file_watcher: root non esiste, skip project={project_id} root={}",
            root.display()
        );
        return;
    }

    state.watching_projects.insert(project_id);
    tracing::info!(
        "spawn_file_watcher: avvio watcher project={project_id} root={}",
        root.display()
    );

    let db = state.db.clone();
    let neural = state.orchestrator.neural.clone();
    let channels = state.project_channels.clone();
    let watching_projects = Arc::clone(&state.watching_projects);

    tokio::spawn(async move {
        let result = run_watcher(db, neural, project_id, root, channels).await;
        // Rimuove il progetto dal set cosi' si puo' riavviare se necessario.
        watching_projects.remove(&project_id);
        if let Err(e) = result {
            tracing::warn!(
                "spawn_file_watcher: watcher terminato con errore project={project_id}: {e}"
            );
        }
    });
}

/// Ciclo principale del watcher. Ritorna solo in caso di errore grave.
async fn run_watcher(
    db: sqlx::PgPool,
    neural: crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    root: PathBuf,
    channels: nexus_events::ProjectChannels,
) -> anyhow::Result<()> {
    // Canale tokio per ricevere gli eventi `notify` dal thread OS.
    let (tx, mut rx) = mpsc::channel::<notify::Result<Event>>(512);

    // `notify` usa un thread interno di sistema (non tokio); usiamo un
    // `std::sync::mpsc` wrapped in un `tx` asincrono tramite closure sincrona.
    let tx_sync = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res| {
        // Ignora l'errore di invio se il ricevitore e' gia' chiuso (shutdown).
        let _ = tx_sync.blocking_send(res);
    })?;

    watcher.watch(&root, RecursiveMode::Recursive)?;
    tracing::info!(
        "spawn_file_watcher: watching attivo su {} (project={project_id})",
        root.display()
    );

    // Debounce: raggruppa eventi ravvicinati entro 500 ms.
    let debounce = Duration::from_millis(500);
    let mut pending: Vec<PathBuf> = Vec::new();
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        // Calcola timeout per il select: se ci sono eventi in attesa aspettiamo
        // fino alla scadenza del debounce, altrimenti aspettiamo a lungo.
        let timeout = deadline.map(|d| {
            let now = tokio::time::Instant::now();
            if d > now { d - now } else { Duration::ZERO }
        });

        let recv_fut = rx.recv();

        // Due rami: ricezione nuovo evento oppure scadenza debounce.
        if let Some(timeout_dur) = timeout {
            tokio::select! {
                maybe_event = recv_fut => {
                    match maybe_event {
                        None => break, // canale chiuso
                        Some(Ok(event)) => {
                            enqueue_paths(&mut pending, &event, &root);
                            deadline = Some(tokio::time::Instant::now() + debounce);
                        }
                        Some(Err(e)) => {
                            tracing::debug!("spawn_file_watcher: notify error: {e}");
                        }
                    }
                }
                _ = tokio::time::sleep(timeout_dur) => {
                    // Debounce scaduto: re-indicizza i file accumulati.
                    flush_pending(&db, &neural, project_id, &root, &mut pending, &channels).await;
                    deadline = None;
                }
            }
        } else {
            match recv_fut.await {
                None => break,
                Some(Ok(event)) => {
                    enqueue_paths(&mut pending, &event, &root);
                    deadline = Some(tokio::time::Instant::now() + debounce);
                }
                Some(Err(e)) => {
                    tracing::debug!("spawn_file_watcher: notify error: {e}");
                }
            }
        }
    }

    Ok(())
}

/// Aggiunge i path dell'evento alla coda, filtrando per estensione e dir escluse.
fn enqueue_paths(pending: &mut Vec<PathBuf>, event: &Event, root: &Path) {
    // Solo eventi di creazione o modifica contenuto.
    match &event.kind {
        EventKind::Create(_) | EventKind::Modify(notify::event::ModifyKind::Data(_)) => {}
        EventKind::Modify(notify::event::ModifyKind::Any) => {}
        _ => return,
    }

    for path in &event.paths {
        // Filtro NOTIFY_EXTENSIONS (lista larga): include manifest/markup/docs
        // necessari per il refresh del pannello explorer. La reindex semantica
        // resta limitata a CODE_EXTENSIONS via `is_code_file` in flush_pending.
        if !is_notify_file(path) {
            continue;
        }
        if is_in_excluded_dir(path, root) {
            continue;
        }
        if !pending.contains(path) {
            pending.push(path.clone());
        }
    }
}

fn is_notify_file(path: &Path) -> bool {
    // Match per estensione case-insensitive.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    if let Some(ext) = ext {
        if NOTIFY_EXTENSIONS.iter().any(|e| *e == ext.as_str()) {
            return true;
        }
    }
    // File senza estensione comuni in progetti (Dockerfile, Makefile, README).
    let name_lower = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    matches!(
        name_lower.as_str(),
        "dockerfile" | "makefile" | "readme" | "license" | "changelog" | "authors" | "contributing"
    )
}

/// Re-indicizza tutti i path in coda e svuota il vettore.
async fn flush_pending(
    db: &sqlx::PgPool,
    neural: &crate::orchestrator::NeuralCoreClient,
    project_id: Uuid,
    root: &Path,
    pending: &mut Vec<PathBuf>,
    channels: &nexus_events::ProjectChannels,
) {
    if pending.is_empty() {
        return;
    }
    let files: Vec<PathBuf> = std::mem::take(pending);
    tracing::info!(
        "spawn_file_watcher: re-indicizzazione {} file project={project_id}",
        files.len()
    );
    // La reindex semantica vale solo per file di codice. Manifest/markup/docs
    // vanno notificati al frontend ma NON indicizzati nel vector store.
    for path in &files {
        if !is_code_file(path) {
            continue;
        }
        match crate::projects::reindex_single_file(db, neural, project_id, root, path).await {
            Ok(chunks) if chunks > 0 => {
                tracing::info!(
                    "spawn_file_watcher: re-indicizzato {} chunk(s) da {}",
                    chunks,
                    path.display()
                );
            }
            Ok(_) => {} // 0 chunk: file invariato o non indicizzabile
            Err(e) => {
                tracing::warn!(
                    "spawn_file_watcher: errore re-indicizzazione {}: {e}",
                    path.display()
                );
            }
        }
    }
    for path in &files {
        nexus_events::dispatcher::emit(
            channels,
            project_id,
            nexus_events::ProjectEvent::FileChanged {
                path: path.display().to_string(),
                op: "write".into(),
            },
        );
    }
}

fn is_code_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| CODE_EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}

fn is_in_excluded_dir(path: &Path, root: &Path) -> bool {
    let relative = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => path,
    };
    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy();
        if EXCLUDED_DIRS.contains(&name.as_ref()) {
            return true;
        }
    }
    false
}
