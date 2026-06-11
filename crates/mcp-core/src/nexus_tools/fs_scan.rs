//! Scansione filesystem condivisa per i tool che camminano il progetto
//! (deploy_*, api_*, sec_*, doc_*, find_todos, fs_grep). Punto unico
//! (regola L): prima ogni tool aveva la propria copia ricorsiva di `walk`.
//! La skip-list canonica resta `super::is_skipped_dir` (S24).

use super::is_skipped_dir;
use serde_json::Value;
use std::path::Path;

/// Skip-list ridotta per i tool che cercano dotfile (es. `.env*`): non si
/// puo' scartare ogni nome che inizia con '.', ma `.git` resta escluso.
pub fn is_skipped_dir_keep_dotfiles(name: &str) -> bool {
    name == ".git" || name == "node_modules" || name == "target"
}

/// Motore generico: visita ricorsivamente i file sotto `root` fino a
/// `max_depth` livelli, saltando i nomi (directory E file) per cui `skip`
/// e' vero. Per ogni file chiama `visit(path, nome_file)`.
pub fn walk_project_with(
    root: &Path,
    max_depth: usize,
    skip: &dyn Fn(&str) -> bool,
    visit: &mut dyn FnMut(&Path, &str),
) {
    walk_rec(root, 0, max_depth, skip, visit);
}

fn walk_rec(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    skip: &dyn Fn(&str) -> bool,
    visit: &mut dyn FnMut(&Path, &str),
) {
    if depth > max_depth {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if skip(&name) {
                continue;
            }
            if p.is_dir() {
                walk_rec(&p, depth + 1, max_depth, skip, visit);
            } else {
                visit(&p, &name);
            }
        }
    }
}

/// Variante standard: skip-list canonica `is_skipped_dir`, raccoglie i nomi
/// dei file per cui `matcher(name)` e' vero. I tool delegano passando solo
/// il predicato di match sull'estensione/nome.
pub fn walk_project_files(
    root: &Path,
    max_depth: usize,
    matcher: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    walk_project_with(root, max_depth, &is_skipped_dir, &mut |_p, name| {
        if matcher(name) {
            out.push(name.to_string());
        }
    });
    out
}

/// Come `walk_project_files` ma con la skip-list che NON scarta i dotfile
/// (per deploy_env_files_count / sec_env_files_check).
pub fn walk_project_files_keep_dotfiles(
    root: &Path,
    max_depth: usize,
    matcher: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    walk_project_with(
        root,
        max_depth,
        &is_skipped_dir_keep_dotfiles,
        &mut |_p, name| {
            if matcher(name) {
                out.push(name.to_string());
            }
        },
    );
    out
}

/// Profondita' massima per le scansioni con tetto risultati (find_todos, fs_grep).
const SCAN_MAX_DEPTH: usize = 8;

/// Scansione per-linea con tetto risultati: cammina da `start_dir`, filtra i
/// file con `file_filter(nome, path)` e dimensione <= `max_file_bytes`; per
/// ogni riga chiama `project(rel_path, numero_riga_1based, riga)` e accumula
/// i `Some(..)` fino a `limit`. Punto unico per find_todos + fs_grep.
pub fn scan_file_lines(
    root: &Path,
    start_dir: &Path,
    max_file_bytes: u64,
    limit: usize,
    file_filter: &dyn Fn(&str, &Path) -> bool,
    project: &mut dyn FnMut(&str, usize, &str) -> Option<Value>,
) -> Vec<Value> {
    let mut out = Vec::new();
    scan_rec(
        root,
        start_dir,
        max_file_bytes,
        limit,
        file_filter,
        project,
        &mut out,
        0,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn scan_rec(
    root: &Path,
    dir: &Path,
    max_file_bytes: u64,
    limit: usize,
    file_filter: &dyn Fn(&str, &Path) -> bool,
    project: &mut dyn FnMut(&str, usize, &str) -> Option<Value>,
    out: &mut Vec<Value>,
    depth: usize,
) {
    if out.len() >= limit || depth > SCAN_MAX_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= limit {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_skipped_dir(&name) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            scan_rec(
                root,
                &path,
                max_file_bytes,
                limit,
                file_filter,
                project,
                out,
                depth + 1,
            );
            continue;
        }
        if !file_filter(&name, &path) {
            continue;
        }
        if meta.len() > max_file_bytes {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.clone());
        for (i, line) in content.lines().enumerate() {
            if out.len() >= limit {
                break;
            }
            if let Some(item) = project(&rel, i + 1, line) {
                out.push(item);
            }
        }
    }
}
