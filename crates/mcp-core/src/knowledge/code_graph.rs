//! Code graph: parser degli import intra-progetto (M13.1 del piano impact).
//!
//! Estrae le dipendenze di import a livello file per Rust, Python e TS/JS, per
//! popolare `project_code_edges(edge_kind='import')`. Parser regex leggero (non
//! AST): copre >90% degli import diretti; le relazioni mancate sono recuperate
//! dallo strato semantico Qdrant (vedi impact.rs). Per ridurre i falsi edge
//! verso librerie esterne si estraggono SOLO gli import sicuramente
//! intra-progetto:
//!   - Rust: `use crate::` / `use self::` / `use super::`, `mod nome;`
//!   - Python: import relativi `from .x import` / `from ..x import`
//!   - TS/JS: path relativi `import ... from './x'` / `require('../x')`
//!
//! Le funzioni qui sono pure (nessun I/O): testabili con `cargo test`. La
//! risoluzione degli specificatori a path di file reali e la persistenza su DB
//! avvengono nel popolamento (indexing) usando l'albero dei file del progetto.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use uuid::Uuid;

/// Linguaggio riconosciuto per il parsing degli import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeLang {
    Rust,
    Python,
    TypeScript,
}

impl CodeLang {
    /// Etichetta breve per la colonna `project_code_nodes.lang`.
    pub fn as_str(self) -> &'static str {
        match self {
            CodeLang::Rust => "rust",
            CodeLang::Python => "python",
            CodeLang::TypeScript => "ts",
        }
    }
}

/// Deduce il linguaggio dall'estensione del file. `None` se non supportato.
pub fn detect_lang(file_path: &str) -> Option<CodeLang> {
    let lower = file_path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Some(CodeLang::Rust),
        "py" | "pyi" => Some(CodeLang::Python),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(CodeLang::TypeScript),
        _ => None,
    }
}

fn rust_use_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*use\s+((?:crate|self|super)(?:::[A-Za-z0-9_]+)+)").unwrap()
    })
}

fn rust_mod_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `mod nome;` (dichiarazione di sottomodulo), non `mod nome {` (inline).
    RE.get_or_init(|| Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+([A-Za-z0-9_]+)\s*;").unwrap())
}

fn py_from_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Solo import relativi: `from .x import`, `from ..pkg.mod import`.
    RE.get_or_init(|| Regex::new(r"(?m)^\s*from\s+(\.+[A-Za-z0-9_.]*)\s+import\b").unwrap())
}

fn ts_import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `import ... from './x'` / `export ... from '../x'`, virgolette singole o doppie.
    RE.get_or_init(|| Regex::new(r#"(?m)\bfrom\s+["'](\.[^"']+)["']"#).unwrap())
}

fn ts_require_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\brequire\(\s*["'](\.[^"']+)["']\s*\)"#).unwrap())
}

/// Estrae gli specificatori di import intra-progetto dal contenuto del file.
///
/// Ritorna i raw specifier (es. `crate::knowledge::mod`, `.models`, `./utils`),
/// deduplicati preservando l'ordine di prima apparizione. La risoluzione a path
/// di file reali avviene nel popolamento del grafo.
pub fn extract_imports(file_path: &str, content: &str) -> Vec<String> {
    let lang = match detect_lang(file_path) {
        Some(l) => l,
        None => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String, out: &mut Vec<String>| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    match lang {
        CodeLang::Rust => {
            for c in rust_use_re().captures_iter(content) {
                push(c[1].to_string(), &mut out);
            }
            for c in rust_mod_re().captures_iter(content) {
                push(format!("mod::{}", &c[1]), &mut out);
            }
        }
        CodeLang::Python => {
            for c in py_from_re().captures_iter(content) {
                push(c[1].to_string(), &mut out);
            }
        }
        CodeLang::TypeScript => {
            for c in ts_import_re().captures_iter(content) {
                push(c[1].to_string(), &mut out);
            }
            for c in ts_require_re().captures_iter(content) {
                push(c[1].to_string(), &mut out);
            }
        }
    }
    out
}

/// True se il path e un file di test (convenzioni di naming multi-linguaggio).
/// Usato per popolare project_code_tests con method='naming' (M13.3).
pub fn is_test_file(file_path: &str) -> bool {
    let name = file_path.rsplit('/').next().unwrap_or(file_path);
    let lower = name.to_ascii_lowercase();
    lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.starts_with("test_")
        || lower.ends_with("_test.py")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
        || name.ends_with("Test.java")
        || file_path.contains("/tests/")
        || file_path.contains("/__tests__/")
}

/// Dato un file di test, deduce per naming il path del file coperto (stessa dir).
/// None se non deducibile. Confidence associata: 'naming' (0.6).
pub fn naming_target(test_path: &str) -> Option<String> {
    let (dir, name) = match test_path.rsplit_once('/') {
        Some((d, n)) => (format!("{d}/"), n.to_string()),
        None => (String::new(), test_path.to_string()),
    };
    // X.test.ext / X.spec.ext -> X.ext
    for marker in [".test.", ".spec."] {
        if let Some(idx) = name.find(marker) {
            let base = &name[..idx];
            let ext = &name[idx + marker.len()..];
            return Some(format!("{dir}{base}.{ext}"));
        }
    }
    // test_X.py -> X.py
    if let Some(rest) = name.strip_prefix("test_") {
        return Some(format!("{dir}{rest}"));
    }
    // X_test.py / X_test.go / X_test.rs -> X.ext
    for ext in ["py", "go", "rs"] {
        let suffix = format!("_test.{ext}");
        if let Some(base) = name.strip_suffix(suffix.as_str()) {
            return Some(format!("{dir}{base}.{ext}"));
        }
    }
    None
}

/// Normalizza un path relativo risolvendo `.` e `..` sui segmenti.
fn normalize_rel(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

fn dir_of(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((d, _)) => d.to_string(),
        None => String::new(),
    }
}

const TS_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

/// Path candidati (relativi-progetto, con estensione/index) per uno specifier di
/// import relativo, da provare contro il filesystem. Funzione pura (no I/O).
pub fn import_candidates(lang: CodeLang, from_rel: &str, spec: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    match lang {
        CodeLang::TypeScript => {
            if !spec.starts_with('.') {
                return out;
            }
            let base = dir_of(from_rel);
            let joined = normalize_rel(&format!("{base}/{spec}"));
            if joined.is_empty() {
                return out;
            }
            for e in TS_EXTS {
                out.push(format!("{joined}.{e}"));
            }
            for e in TS_EXTS {
                out.push(format!("{joined}/index.{e}"));
            }
        }
        CodeLang::Python => {
            if !spec.starts_with('.') {
                return out;
            }
            let dots = spec.chars().take_while(|c| *c == '.').count();
            let rest = &spec[dots..]; // es. "providers.base" o ""
                                      // dots=1 -> stessa dir; dots=N -> sali N-1 livelli
            let mut base = dir_of(from_rel);
            for _ in 0..dots.saturating_sub(1) {
                base = dir_of(&base);
            }
            let rest_path = rest.replace('.', "/");
            let joined = if rest_path.is_empty() {
                normalize_rel(&base)
            } else if base.is_empty() {
                normalize_rel(&rest_path)
            } else {
                normalize_rel(&format!("{base}/{rest_path}"))
            };
            if joined.is_empty() {
                return out;
            }
            out.push(format!("{joined}.py"));
            out.push(format!("{joined}/__init__.py"));
        }
        CodeLang::Rust => {
            // La risoluzione crate::/mod richiede la mappa modulo->file: rimandata.
        }
    }
    out
}

/// Popola project_code_nodes/edges/tests per un singolo file gia letto.
/// Idempotente (upsert + delete-then-insert degli edge strutturali del file).
/// Best-effort: errori loggati, mai propagati (non deve rompere l'indicizzazione).
pub async fn persist_code_graph(
    db: &sqlx::PgPool,
    project_id: Uuid,
    root: &Path,
    relative_path: &str,
    content: &str,
    content_hash: &str,
) {
    let lang = match detect_lang(relative_path) {
        Some(l) => l,
        None => return,
    };

    // Nodo
    if let Err(e) = sqlx::query(
        "INSERT INTO project_code_nodes (project_id, file_path, lang, content_hash, last_seen_at)
         VALUES ($1,$2,$3,$4,NOW())
         ON CONFLICT (project_id, file_path)
         DO UPDATE SET lang=EXCLUDED.lang, content_hash=EXCLUDED.content_hash, last_seen_at=NOW()",
    )
    .bind(project_id)
    .bind(relative_path)
    .bind(lang.as_str())
    .bind(content_hash)
    .execute(db)
    .await
    {
        tracing::warn!("persist_code_graph: upsert node {relative_path} fallito: {e}");
        return;
    }

    // Edge import (structural): ricostruisci da zero per questo file
    let _ = sqlx::query(
        "DELETE FROM project_code_edges WHERE project_id=$1 AND from_path=$2 AND source='structural'",
    )
    .bind(project_id)
    .bind(relative_path)
    .execute(db)
    .await;

    for spec in extract_imports(relative_path, content) {
        // Solo specifier relativi risolvibili a un file reale diventano edge.
        for cand in import_candidates(lang, relative_path, &spec) {
            if root.join(&cand).exists() {
                let _ = sqlx::query(
                    "INSERT INTO project_code_edges (project_id, from_path, to_path, edge_kind, weight, source)
                     VALUES ($1,$2,$3,'import',1.0,'structural')
                     ON CONFLICT (project_id, from_path, to_path, edge_kind) DO NOTHING",
                )
                .bind(project_id)
                .bind(relative_path)
                .bind(&cand)
                .execute(db)
                .await;
                break; // primo candidato esistente
            }
        }
    }

    // Test mapping per naming
    if is_test_file(relative_path) {
        if let Some(target) = naming_target(relative_path) {
            if root.join(&target).exists() {
                let _ = sqlx::query(
                    "INSERT INTO project_code_tests (project_id, test_path, covers_path, method, confidence)
                     VALUES ($1,$2,$3,'naming',0.6)
                     ON CONFLICT (project_id, test_path, covers_path) DO NOTHING",
                )
                .bind(project_id)
                .bind(relative_path)
                .bind(&target)
                .execute(db)
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_lang_by_extension() {
        assert_eq!(detect_lang("src/a.rs"), Some(CodeLang::Rust));
        assert_eq!(detect_lang("b.py"), Some(CodeLang::Python));
        assert_eq!(detect_lang("c.tsx"), Some(CodeLang::TypeScript));
        assert_eq!(detect_lang("d.go"), None);
        assert_eq!(detect_lang("README.md"), None);
    }

    #[test]
    fn rust_imports() {
        let src = r#"
            use crate::knowledge::code_graph;
            use super::routes::handler;
            use std::sync::OnceLock;   // esterno: ignorato
            use serde_json::Value;     // esterno: ignorato
            pub mod impact;
            mod helpers;
            fn f() { let _ = 1; }
        "#;
        let imps = extract_imports("src/knowledge/mod.rs", src);
        assert!(
            imps.contains(&"crate::knowledge::code_graph".to_string()),
            "{imps:?}"
        );
        assert!(
            imps.contains(&"super::routes::handler".to_string()),
            "{imps:?}"
        );
        assert!(imps.contains(&"mod::impact".to_string()), "{imps:?}");
        assert!(imps.contains(&"mod::helpers".to_string()), "{imps:?}");
        assert!(
            !imps.iter().any(|i| i.contains("std")),
            "stdlib non intra-progetto: {imps:?}"
        );
        assert!(
            !imps.iter().any(|i| i.contains("serde")),
            "crate esterno escluso: {imps:?}"
        );
    }

    #[test]
    fn python_relative_imports_only() {
        let src = "\
from .models import Foo\n\
from ..providers.base import Bar\n\
from os import path        # stdlib assoluto: ignorato\n\
import json                # assoluto: ignorato\n\
";
        let imps = extract_imports("brain/agents/x.py", src);
        assert!(imps.contains(&".models".to_string()), "{imps:?}");
        assert!(imps.contains(&"..providers.base".to_string()), "{imps:?}");
        assert!(!imps.iter().any(|i| i.contains("os")), "{imps:?}");
        assert!(!imps.iter().any(|i| i == "json"), "{imps:?}");
    }

    #[test]
    fn ts_relative_imports_and_require() {
        let src = r#"
            import { A } from './utils';
            import B from "../lib/b";
            import React from 'react';          // pacchetto: ignorato
            const c = require('./local-mod');
            const d = require('lodash');        // pacchetto: ignorato
        "#;
        let imps = extract_imports("apps/web-ide/x.tsx", src);
        assert!(imps.contains(&"./utils".to_string()), "{imps:?}");
        assert!(imps.contains(&"../lib/b".to_string()), "{imps:?}");
        assert!(imps.contains(&"./local-mod".to_string()), "{imps:?}");
        assert!(!imps.iter().any(|i| i.contains("react")), "{imps:?}");
        assert!(!imps.iter().any(|i| i.contains("lodash")), "{imps:?}");
    }

    #[test]
    fn dedup_preserves_order() {
        let src = "use crate::a::b;\nuse crate::a::b;\nuse crate::c::d;\n";
        let imps = extract_imports("x.rs", src);
        assert_eq!(
            imps,
            vec!["crate::a::b".to_string(), "crate::c::d".to_string()]
        );
    }

    #[test]
    fn unsupported_extension_returns_empty() {
        assert!(extract_imports("x.go", "import \"fmt\"").is_empty());
    }

    #[test]
    fn test_file_detection() {
        assert!(is_test_file("a/b.test.ts"));
        assert!(is_test_file("x.spec.tsx"));
        assert!(is_test_file("test_foo.py"));
        assert!(is_test_file("foo_test.go"));
        assert!(is_test_file("src/tests/integration.rs"));
        assert!(is_test_file("app/__tests__/x.jsx"));
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("brain/agents/nodes.py"));
    }

    #[test]
    fn naming_target_mapping() {
        assert_eq!(naming_target("a/b.test.ts").as_deref(), Some("a/b.ts"));
        assert_eq!(naming_target("comp.spec.tsx").as_deref(), Some("comp.tsx"));
        assert_eq!(
            naming_target("pkg/test_utils.py").as_deref(),
            Some("pkg/utils.py")
        );
        assert_eq!(naming_target("m_test.go").as_deref(), Some("m.go"));
        assert_eq!(
            naming_target("src/parser_test.rs").as_deref(),
            Some("src/parser.rs")
        );
        assert_eq!(naming_target("main.rs"), None);
    }

    #[test]
    fn normalize_resolves_dotdot() {
        assert_eq!(normalize_rel("a/b/../c"), "a/c");
        assert_eq!(normalize_rel("a/./b"), "a/b");
        assert_eq!(normalize_rel("a/b/../../c"), "c");
    }

    #[test]
    fn ts_import_candidates() {
        let c = import_candidates(CodeLang::TypeScript, "apps/web/src/a.tsx", "./utils");
        assert!(c.contains(&"apps/web/src/utils.ts".to_string()), "{c:?}");
        assert!(
            c.contains(&"apps/web/src/utils/index.tsx".to_string()),
            "{c:?}"
        );
        let up = import_candidates(CodeLang::TypeScript, "apps/web/src/comp/x.ts", "../lib/b");
        assert!(up.contains(&"apps/web/src/lib/b.ts".to_string()), "{up:?}");
        // import non relativo (pacchetto) -> nessun candidato
        assert!(import_candidates(CodeLang::TypeScript, "a/x.ts", "react").is_empty());
    }

    #[test]
    fn python_import_candidates() {
        let c = import_candidates(CodeLang::Python, "brain/agents/x.py", ".models");
        assert!(c.contains(&"brain/agents/models.py".to_string()), "{c:?}");
        assert!(
            c.contains(&"brain/agents/models/__init__.py".to_string()),
            "{c:?}"
        );
        let up = import_candidates(CodeLang::Python, "brain/agents/x.py", "..providers.base");
        assert!(
            up.contains(&"brain/providers/base.py".to_string()),
            "{up:?}"
        );
    }
}
