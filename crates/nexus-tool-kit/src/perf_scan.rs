//! Helper condiviso per i tool `performance::*`: scansiona ricorsivamente i
//! file `.rs` sotto `src/` e `crates/` (escludendo `target/` e dot-dirs) e
//! conta occorrenze di pattern testuali (substring match, no regex).
use std::path::{Path, PathBuf};

pub fn scan_substrings(root: &Path, patterns: &[&str]) -> (Vec<usize>, usize) {
    let mut counts = vec![0usize; patterns.len()];
    let mut files = 0usize;
    let candidates: [PathBuf; 2] = [root.join("src"), root.join("crates")];
    for c in &candidates {
        if c.is_dir() {
            walk(c, &mut counts, patterns, &mut files, 0);
        }
    }
    (counts, files)
}

fn walk(dir: &Path, counts: &mut [usize], patterns: &[&str], files: &mut usize, depth: usize) {
    if depth > 8 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "target" || name.starts_with('.') || name == "node_modules" {
                continue;
            }
            if p.is_dir() {
                walk(&p, counts, patterns, files, depth + 1);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    *files += 1;
                    for (i, pat) in patterns.iter().enumerate() {
                        counts[i] += content.matches(pat).count();
                    }
                }
            }
        }
    }
}
