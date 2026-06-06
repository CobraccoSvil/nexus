// ═══════════════════════════════════════════════════════════════════════════
// wiki/code_graph.rs — ADR 0017 v2 TODO 5 — code-graph triple su wiki_concept_triples.
//
// Reimplementa il vecchio `knowledge::code_graph::persist_code_graph` sopra
// lo schema unificato (mig 0295 + mig 0302):
//
//   - Per ogni file di codice indicizzato, crea (o riusa) un wiki_doc
//     placeholder con `scope='project'`, `kind='code'`,
//     `vault_file_path = relative_path`.
//   - Parse degli import intra-progetto via regex leggera (Rust/Python/TS-JS).
//   - INSERT idempotente su `wiki_concept_triples` con `predicate='imports'`,
//     `source='static_analysis'`, `obj_text = specifier dell'import`.
//
// Politica:
//   - Niente AST: regex copre >90% dei casi (port del parser legacy
//     `knowledge/code_graph.rs` rimosso dalla F8).
//   - Niente nuovi indici UNIQUE: dedup tramite DELETE + INSERT del set di
//     edge "static_analysis" per quel file (idempotente, sopporta rinomine
//     e rimozioni di import senza accumulare triple orfane).
//   - Best-effort: errori loggati a WARN, mai propagati (non deve rompere il
//     reindex dei chunk vettoriali).
// ═══════════════════════════════════════════════════════════════════════════

use crate::wiki::vault::{sha256_hex, slugify};
use regex::Regex;
use sqlx::PgPool;
use std::sync::OnceLock;
use uuid::Uuid;

/// Linguaggio riconosciuto per il parsing degli import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeLang {
    Rust,
    Python,
    TypeScript,
}

/// Deduce il linguaggio dall'estensione. `None` se non supportato.
fn detect_lang(file_path: &str) -> Option<CodeLang> {
    let lower = file_path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Some(CodeLang::Rust),
        "py" | "pyi" => Some(CodeLang::Python),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(CodeLang::TypeScript),
        _ => None,
    }
}

// ── Regex (compilate una sola volta via OnceLock) ────────────────────────────

fn rust_use_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Catturiamo qualunque `use X::Y...;` (anche crate esterni). Tronchiamo
        // sui caratteri di blocco/raggruppamento (`{`, ` as`, `;`).
        Regex::new(r"(?m)^\s*(?:pub\s+)?use\s+([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)").unwrap()
    })
}

fn rust_mod_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+([A-Za-z0-9_]+)\s*;").unwrap())
}

fn py_import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `import X[.Y]` semplice. NON cattura `import x as y` con alias completo,
    // ma la radice e' corretta.
    RE.get_or_init(|| Regex::new(r"(?m)^\s*import\s+([A-Za-z_][A-Za-z0-9_.]*)").unwrap())
}

fn py_from_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `from X[.Y] import ...` (relativo o assoluto: catturiamo entrambi).
    RE.get_or_init(|| Regex::new(r"(?m)^\s*from\s+(\.*[A-Za-z_][A-Za-z0-9_.]*|\.+)\s+import\b").unwrap())
}

fn ts_import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `import ... from "X"` / `export ... from 'X'`. Cattura anche bare specifier
    // (pacchetti npm) — utile per il knowledge graph.
    RE.get_or_init(|| Regex::new(r#"(?m)\bfrom\s+["']([^"']+)["']"#).unwrap())
}

fn ts_require_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\brequire\(\s*["']([^"']+)["']\s*\)"#).unwrap())
}

/// Estrae gli specificatori di import dal contenuto. Ritorna lista deduplicata
/// preservando l'ordine. Tetto a 200 specifier per file (sanity bound).
fn extract_imports(file_path: &str, content: &str) -> Vec<String> {
    const MAX_IMPORTS_PER_FILE: usize = 200;
    let lang = match detect_lang(file_path) {
        Some(l) => l,
        None => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String, out: &mut Vec<String>| {
        let trimmed = s.trim().to_string();
        if !trimmed.is_empty() && !out.contains(&trimmed) && out.len() < MAX_IMPORTS_PER_FILE {
            out.push(trimmed);
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
            for c in py_import_re().captures_iter(content) {
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

/// Garantisce l'esistenza di un wiki_doc placeholder (scope=project, kind='code')
/// per il file specificato. Idempotente via UNIQUE su (scope, project_id, slug).
/// Ritorna l'id del doc esistente o appena creato.
pub(crate) async fn ensure_code_doc(
    db: &PgPool,
    project_id: Uuid,
    relative_path: &str,
) -> anyhow::Result<Uuid> {
    // Slug deterministico dal path (sicurezza unique + leggibilita').
    let raw_slug = format!("code/{relative_path}");
    let slug = slugify(&raw_slug);
    if slug.is_empty() {
        anyhow::bail!("slug vuoto per path {relative_path}");
    }
    let title = relative_path.to_string();
    let body_md = format!(
        "Placeholder doc per il file di codice `{relative_path}`. \
         Generato automaticamente dal code-graph reindex (ADR 0017 v2)."
    );
    let body_hash = sha256_hex(&body_md);

    let row: (Uuid,) = sqlx::query_as::<_, (Uuid,)>(
        r#"
        INSERT INTO wiki_docs (
            scope, project_id, slug, title, body_md, body_hash,
            kind, tags, vault_file_path,
            edit_lock, protected_sections, manually_edited,
            generated_hash, edited_hash,
            current_version, auto_generated, public_read
        ) VALUES (
            'project', $1, $2, $3, $4, $5,
            'code', ARRAY['code','auto']::text[], $6,
            'none', '{}', FALSE,
            $5, NULL,
            1, TRUE, FALSE
        )
        ON CONFLICT (scope, COALESCE(project_id::text,''), slug) DO UPDATE SET
            vault_file_path = EXCLUDED.vault_file_path,
            updated_at      = NOW()
        RETURNING id
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind(&title)
    .bind(&body_md)
    .bind(&body_hash)
    .bind(relative_path)
    .fetch_one(db)
    .await?;

    Ok(row.0)
}

/// Entry-point chiamato da `reindex_single_file`. Best-effort: assorbe gli
/// errori e logga a WARN. Ritorna il numero di triple effettivamente upserted.
pub async fn persist_code_graph_for_file(
    db: &PgPool,
    project_id: Uuid,
    relative_path: &str,
    content: &str,
) -> usize {
    // Punto unico (regola L): la presenza di un file nella knowledge base come
    // wiki_doc kind='code' segue CODE_EXTENSIONS — lo stesso filtro usato dal RAG
    // (`projects::CODE_EXTENSIONS`) — NON il sottoinsieme `detect_lang`. Cosi' i
    // file HTML/markup e quelli senza import compaiono comunque come scheda nella
    // KB, non solo i linguaggi per cui esiste un parser di import.
    let ext = relative_path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if !crate::projects::CODE_EXTENSIONS.contains(&ext.as_str()) {
        return 0;
    }

    // Garantisce SEMPRE il doc placeholder per il file indicizzato. L'enricher
    // (mig 0331) lo trasforma poi in scheda descrittiva via LLM. L'assenza di
    // import non nasconde piu' il file dalla knowledge base.
    let subj_doc_id = match ensure_code_doc(db, project_id, relative_path).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                file = relative_path,
                error = %e,
                "wiki.code_graph: ensure_code_doc fallito (skip)"
            );
            return 0;
        }
    };

    // Triple `imports`: solo per i linguaggi con parser. `extract_imports` usa
    // `detect_lang` internamente e ritorna vuoto per HTML/markup; il doc esiste
    // comunque gia'.
    let specifiers = extract_imports(relative_path, content);
    if specifiers.is_empty() {
        return 0;
    }

    // Strategia idempotente: cancella le triple `imports` precedenti generate
    // da static_analysis per questo soggetto, poi reinserisce il set corrente.
    // Cosi' rinomine/cancellazioni di import non lasciano edge orfani.
    if let Err(e) = sqlx::query(
        "DELETE FROM wiki_concept_triples \
         WHERE subj_doc_id = $1 AND predicate = 'imports' AND source = 'static_analysis'",
    )
    .bind(subj_doc_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            project_id = %project_id,
            file = relative_path,
            error = %e,
            "wiki.code_graph: DELETE pregresso fallito (proseguo con INSERT)"
        );
    }

    let mut inserted = 0usize;
    for spec in &specifiers {
        let res = sqlx::query(
            r#"
            INSERT INTO wiki_concept_triples
                (subj_doc_id, predicate, obj_text, source, confidence, evidence)
            VALUES ($1, 'imports', $2, 'static_analysis', 1.0, $3)
            "#,
        )
        .bind(subj_doc_id)
        .bind(spec)
        .bind(relative_path)
        .execute(db)
        .await;
        match res {
            Ok(_) => {
                inserted += 1;
            }
            Err(e) => {
                tracing::debug!(
                    project_id = %project_id,
                    file = relative_path,
                    spec = %spec,
                    error = %e,
                    "wiki.code_graph: INSERT tripla fallito (skip)"
                );
            }
        }
    }

    if inserted > 0 {
        tracing::info!(
            project_id = %project_id,
            file = relative_path,
            count = inserted,
            "wiki.code_graph: import edge inserito"
        );
    }
    inserted
}

// ───────────────────────────────────────────────────────────────────────────
// Tests (puri, niente DB)
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_lang_basic() {
        assert_eq!(detect_lang("a.rs"), Some(CodeLang::Rust));
        assert_eq!(detect_lang("b.py"), Some(CodeLang::Python));
        assert_eq!(detect_lang("c.tsx"), Some(CodeLang::TypeScript));
        assert_eq!(detect_lang("d.md"), None);
    }

    #[test]
    fn extract_rust_use_and_mod() {
        let src = r#"
            use crate::foo::bar;
            use std::collections::HashMap;
            pub use serde::Serialize;
            mod helper;
            pub mod public_one;
        "#;
        let v = extract_imports("foo/bar.rs", src);
        assert!(v.contains(&"crate::foo::bar".to_string()));
        assert!(v.contains(&"std::collections::HashMap".to_string()));
        assert!(v.contains(&"serde::Serialize".to_string()));
        assert!(v.contains(&"mod::helper".to_string()));
        assert!(v.contains(&"mod::public_one".to_string()));
    }

    #[test]
    fn extract_python_imports() {
        let src = r#"
            import os
            import sys
            from collections import OrderedDict
            from .relative import thing
            from ..pkg.sub import other
        "#;
        let v = extract_imports("app/foo.py", src);
        assert!(v.contains(&"os".to_string()));
        assert!(v.contains(&"sys".to_string()));
        assert!(v.contains(&"collections".to_string()));
        assert!(v.contains(&".relative".to_string()));
        assert!(v.contains(&"..pkg.sub".to_string()));
    }

    #[test]
    fn extract_ts_imports() {
        let src = r#"
            import { foo } from "./local";
            import bar from '../other';
            import * as React from 'react';
            const x = require("./util");
            export { y } from "@scope/pkg";
        "#;
        let v = extract_imports("src/foo.ts", src);
        assert!(v.contains(&"./local".to_string()));
        assert!(v.contains(&"../other".to_string()));
        assert!(v.contains(&"react".to_string()));
        assert!(v.contains(&"./util".to_string()));
        assert!(v.contains(&"@scope/pkg".to_string()));
    }

    #[test]
    fn dedup_preserves_order() {
        let src = "use crate::a;\nuse crate::a;\nuse crate::b;";
        let v = extract_imports("x.rs", src);
        assert_eq!(v, vec!["crate::a".to_string(), "crate::b".to_string()]);
    }
}
