//! Fix M11 (versione minimal — snapshot poll-based invece di SSE).
//!
//! GET /api/projects/:id/fs-events?since_fingerprint=<u64>
//!
//! Ritorna un JSON snapshot del filesystem del progetto target:
//! - file_count, last_modified_iso, fingerprint
//! - changed=true se fingerprint != since_fingerprint passato
//!
//! Il frontend EXPLORER fa polling ogni 5-10s su questo endpoint passando
//! il fingerprint dell'ultimo scan; se changed=true invalida la cache del tree.
//! Versione SSE/WebSocket vera richiede async_stream crate (non disponibile in mcp-core),
//! da consolidare in PR successiva.

use super::*;
use std::path::PathBuf;
use std::time::SystemTime;

const SCAN_DEPTH: usize = 4;
const EXCLUDE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    ".next",
    ".turbo",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    ".cache",
    ".pnpm-store",
];

#[derive(Default)]
struct FsSnapshot {
    file_count: usize,
    last_modified_iso: Option<String>,
    fingerprint: u64,
}

async fn scan_root(root: &PathBuf) -> FsSnapshot {
    let mut file_count: usize = 0;
    let mut latest: Option<SystemTime> = None;
    let mut fingerprint: u64 = 0;

    fn is_excluded(name: &str) -> bool {
        EXCLUDE_DIRS.iter().any(|e| *e == name)
    }

    let mut queue: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        if depth > SCAN_DEPTH {
            continue;
        }
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        loop {
            let next = match entries.next_entry().await {
                Ok(Some(e)) => e,
                _ => break,
            };
            let name = next.file_name().to_string_lossy().to_string();
            if is_excluded(&name) || name.starts_with('.') {
                continue;
            }
            let path = next.path();
            let meta = match next.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                queue.push((path, depth + 1));
            } else {
                file_count += 1;
                if let Ok(mtime) = meta.modified() {
                    if let Ok(elapsed) = mtime.duration_since(SystemTime::UNIX_EPOCH) {
                        fingerprint = fingerprint.wrapping_add(elapsed.as_secs());
                    }
                    if latest.map_or(true, |l| mtime > l) {
                        latest = Some(mtime);
                    }
                }
            }
        }
    }

    let last_modified_iso = latest.and_then(|t| {
        t.duration_since(SystemTime::UNIX_EPOCH).ok().and_then(|d| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                .map(|dt| dt.to_rfc3339())
        })
    });

    FsSnapshot {
        file_count,
        last_modified_iso,
        fingerprint,
    }
}

pub async fn fs_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;
    let context = load_project_context(&state.db, project_id, user_id).await?;

    let since: Option<u64> = params.get("since_fingerprint").and_then(|s| s.parse().ok());

    let snap = scan_root(&context.root_path).await;
    let changed = since.map_or(true, |s| s != snap.fingerprint);

    Ok(Json(json!({
        "ok": true,
        "changed": changed,
        "fingerprint": snap.fingerprint,
        "file_count": snap.file_count,
        "last_modified_iso": snap.last_modified_iso,
        "since_fingerprint": since,
    })))
}
