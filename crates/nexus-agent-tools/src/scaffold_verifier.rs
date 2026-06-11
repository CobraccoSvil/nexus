//! Tool `nexus_verify_scaffold`: verifica la completezza di un progetto
//! scaffolded (tipicamente da `nexus_extract_figma_code`) prima del primo
//! `npm start`. Identifica file critici mancanti, import path inconsistenti
//! e suggerisce fix concreti.
//!
//! Scopo: evitare il loop iterativo "avvia → 404/import error → diagnose →
//! fix → riavvia" eliminando i bug noti PRIMA del primo run.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;

use super::ToolContextCore;

/// File critici che un progetto Vite+React+TS deve avere per partire.
struct VerifyResult {
    project_kind: String,
    missing_files: Vec<MissingFile>,
    inconsistent_imports: Vec<InconsistentImport>,
    package_json_issues: Vec<String>,
    suggested_fixes: Vec<Value>,
}

struct MissingFile {
    path: String,
    purpose: String,
    template_id: Option<&'static str>,
}

struct InconsistentImport {
    file: String,
    import_path: String,
    reason: String,
    suggested_path: Option<String>,
}

pub async fn tool_nexus_verify_scaffold(ctx: &ToolContextCore, input: &Value) -> String {
    let target_rel = input
        .get("target_dir")
        .and_then(Value::as_str)
        .map(|s| s.trim().trim_start_matches('/'))
        .unwrap_or(".");
    if target_rel.contains("..") {
        return json!({"error": "target_dir non puo' contenere '..'"}).to_string();
    }
    let target = ctx.root_path.join(target_rel);

    if !target.exists() {
        return json!({
            "error": format!("target_dir '{}' non esiste", target.display())
        })
        .to_string();
    }

    // Rileva project kind (oggi: vite+react+ts; estendibile)
    let pkg_json_path = target.join("package.json");
    if !pkg_json_path.exists() {
        return json!({
            "project_kind": "unknown",
            "missing_files": [{"path": "package.json", "purpose": "manifest npm", "template_id": null}],
            "inconsistent_imports": [],
            "package_json_issues": ["package.json mancante"],
            "suggested_fixes": [{
                "type": "blocker",
                "message": "Non posso verificare scaffolding senza package.json. Crealo prima."
            }]
        })
        .to_string();
    }

    let mut result = VerifyResult {
        project_kind: "vite-react-ts".into(),
        missing_files: Vec::new(),
        inconsistent_imports: Vec::new(),
        package_json_issues: Vec::new(),
        suggested_fixes: Vec::new(),
    };

    // ── 1. Check file critici ───────────────────────────────────────────────
    let critical: &[(&str, &str, Option<&'static str>)] = &[
        (
            "index.html",
            "entry point HTML per Vite",
            Some("vite_basic_index_html"),
        ),
        (
            "vite.config.ts",
            "config Vite (server, plugins, alias)",
            Some("vite_basic_config"),
        ),
        (
            "src/main.tsx",
            "entry point React (createRoot)",
            Some("vite_basic_main_tsx"),
        ),
    ];
    for (path, purpose, tmpl) in critical {
        let full = target.join(path);
        if !full.exists() {
            result.missing_files.push(MissingFile {
                path: path.to_string(),
                purpose: purpose.to_string(),
                template_id: *tmpl,
            });
        }
    }

    // ── 2. Lettura package.json ────────────────────────────────────────────
    let pkg_content = fs::read_to_string(&pkg_json_path).await.unwrap_or_default();
    let pkg: Value = serde_json::from_str(&pkg_content).unwrap_or(json!({}));
    let scripts = pkg.get("scripts").cloned().unwrap_or(json!({}));
    let has_dev = scripts.get("dev").is_some();
    let has_start = scripts.get("start").is_some();
    if !has_dev && !has_start {
        result.package_json_issues.push(
            "Nessuno script 'dev' o 'start' in package.json: vite non parte con npm run dev/start"
                .into(),
        );
        result.suggested_fixes.push(json!({
            "type": "edit_package_json",
            "field": "scripts.dev",
            "value": "vite",
            "note": "Aggiunge 'dev' come alias di vite. Oppure aggiungi 'start': 'vite'."
        }));
    }

    let deps = pkg.get("dependencies").cloned().unwrap_or(json!({}));
    let dev_deps = pkg.get("devDependencies").cloned().unwrap_or(json!({}));
    let all_deps: std::collections::HashSet<String> = deps
        .as_object()
        .into_iter()
        .chain(dev_deps.as_object())
        .flat_map(|m| m.keys().cloned())
        .collect();

    // ── 3. Check main.tsx → import esistano ────────────────────────────────
    let main_tsx = target.join("src/main.tsx");
    if main_tsx.exists() {
        if let Ok(content) = fs::read_to_string(&main_tsx).await {
            for cap in import_regex().captures_iter(&content) {
                let path = cap
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                if path.starts_with("./") || path.starts_with("../") {
                    // import relativo: verifica file
                    if let Some(suggested) =
                        resolve_relative_import(&main_tsx, &path, &target).await
                    {
                        if suggested != path {
                            result.inconsistent_imports.push(InconsistentImport {
                                file: "src/main.tsx".into(),
                                import_path: path.clone(),
                                reason: "file non trovato al path indicato".into(),
                                suggested_path: Some(suggested),
                            });
                        }
                    } else {
                        result.inconsistent_imports.push(InconsistentImport {
                            file: "src/main.tsx".into(),
                            import_path: path.clone(),
                            reason: "file non trovato neanche con resolve fallback".into(),
                            suggested_path: None,
                        });
                    }
                } else if !path.starts_with('/') {
                    // pkg npm
                    let pkg_root = path.split('/').next().unwrap_or(&path).to_string();
                    let pkg_root = if pkg_root.starts_with('@') {
                        // scoped: @scope/name
                        let mut it = path.splitn(3, '/');
                        let scope = it.next().unwrap_or("");
                        let name = it.next().unwrap_or("");
                        format!("{}/{}", scope, name)
                    } else {
                        pkg_root
                    };
                    if !all_deps.contains(&pkg_root) {
                        result.inconsistent_imports.push(InconsistentImport {
                            file: "src/main.tsx".into(),
                            import_path: path.clone(),
                            reason: format!(
                                "pkg '{}' non in dependencies/devDependencies",
                                pkg_root
                            ),
                            suggested_path: Some(format!("npm install {}", pkg_root)),
                        });
                    }
                }
            }
        }
    }

    // ── 4. Costruisci suggested_fixes finali ────────────────────────────────
    for mf in &result.missing_files {
        if let Some(tmpl) = mf.template_id {
            let body = template_content(tmpl).unwrap_or("");
            result.suggested_fixes.push(json!({
                "type": "write_file",
                "path": format!("{}/{}", target_rel.trim_end_matches('/'), mf.path),
                "content": body,
                "note": format!("Crea '{}' ({}) dal template '{}'.", mf.path, mf.purpose, tmpl),
            }));
        } else {
            result.suggested_fixes.push(json!({
                "type": "blocker",
                "message": format!("File '{}' mancante ({}). Crealo manualmente.", mf.path, mf.purpose),
            }));
        }
    }
    for ii in &result.inconsistent_imports {
        if let Some(sug) = &ii.suggested_path {
            if sug.starts_with("npm install ") {
                result.suggested_fixes.push(json!({
                    "type": "run_command",
                    "command": sug,
                    "note": format!("Install pkg per import in {}", ii.file),
                }));
            } else {
                result.suggested_fixes.push(json!({
                    "type": "edit_file",
                    "file": ii.file,
                    "from": format!("from \"{}\"", ii.import_path),
                    "to": format!("from \"{}\"", sug),
                    "note": format!("Reason: {}", ii.reason),
                }));
            }
        } else {
            result.suggested_fixes.push(json!({
                "type": "manual_review",
                "file": ii.file,
                "import_path": ii.import_path,
                "reason": ii.reason,
            }));
        }
    }

    let ok = result.missing_files.is_empty()
        && result.inconsistent_imports.is_empty()
        && result.package_json_issues.is_empty();

    json!({
        "ok": ok,
        "project_kind": result.project_kind,
        "target_dir": target_rel,
        "missing_files": result.missing_files.iter().map(|m| json!({
            "path": m.path, "purpose": m.purpose, "template_id": m.template_id
        })).collect::<Vec<_>>(),
        "inconsistent_imports": result.inconsistent_imports.iter().map(|i| json!({
            "file": i.file, "import_path": i.import_path,
            "reason": i.reason, "suggested_path": i.suggested_path
        })).collect::<Vec<_>>(),
        "package_json_issues": result.package_json_issues,
        "suggested_fixes": result.suggested_fixes,
        "next_step_hint": if ok {
            "Scaffolding consistente. Puoi avviare con npm run dev/start senza errori noti."
        } else {
            "Applica i suggested_fixes in ordine (write_file/edit_file/run_command), poi ri-chiama questo tool per verifica residui."
        }
    })
    .to_string()
}

/// Regex per estrarre import: `import X from "Y"` / `import "Y"`.
fn import_regex() -> regex::Regex {
    // Capture group 1 = path
    regex::Regex::new(r#"import\s+(?:[^"']+\s+from\s+)?["']([^"']+)["']"#).unwrap()
}

/// Tenta di risolvere un import relativo: cerca varianti con `.tsx`, `.ts`,
/// `/index.tsx`, e in sottocartelle frequenti (`app/`).
async fn resolve_relative_import(
    importing_file: &Path,
    import_path: &str,
    target_root: &Path,
) -> Option<String> {
    let base_dir = importing_file.parent()?;
    // Resolve base path
    let mut try_paths: Vec<PathBuf> = Vec::new();
    let candidate = base_dir.join(import_path);
    try_paths.push(candidate.with_extension("tsx"));
    try_paths.push(candidate.with_extension("ts"));
    try_paths.push(candidate.with_extension("jsx"));
    try_paths.push(candidate.with_extension("js"));
    try_paths.push(candidate.join("index.tsx"));
    try_paths.push(candidate.join("index.ts"));
    try_paths.push(candidate.clone());

    for p in &try_paths {
        if fs::metadata(p).await.is_ok() {
            return Some(import_path.to_string()); // path originale OK
        }
    }

    // Fallback: cerca nelle sottocartelle frequenti
    let file_name = Path::new(import_path).file_name()?.to_str()?;
    for ext in &["tsx", "ts"] {
        for subdir in &["app", "components", "pages"] {
            let try_path = base_dir.join(subdir).join(format!("{}.{}", file_name, ext));
            if fs::metadata(&try_path).await.is_ok() {
                // Suggerisci import relativo nuovo
                if let Ok(rel) = try_path.strip_prefix(base_dir) {
                    let mut s = format!("./{}", rel.with_extension("").to_string_lossy());
                    s = s.replace('\\', "/");
                    return Some(s);
                }
            }
        }
    }

    // Cerca anche fuori da base_dir, dentro target_root/src/
    let _ = target_root; // reserved per future ricerche larghe
    None
}

/// Template per file mancanti (Vite+React+TS standard).
fn template_content(id: &str) -> Option<&'static str> {
    match id {
        "vite_basic_index_html" => Some(VITE_INDEX_HTML),
        "vite_basic_config" => Some(VITE_CONFIG_TS),
        "vite_basic_main_tsx" => Some(VITE_MAIN_TSX),
        _ => None,
    }
}

const VITE_INDEX_HTML: &str = r#"<!doctype html>
<html lang="it">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>App</title>
</head>
<body>
  <div id="root"></div>
  <script type="module" src="/src/main.tsx"></script>
</body>
</html>
"#;

const VITE_CONFIG_TS: &str = r#"import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "0.0.0.0",
    strictPort: true,
  },
});
"#;

const VITE_MAIN_TSX: &str = r#"import React from "react";
import { createRoot } from "react-dom/client";
import App from "./app/App";
import "./index.css";

const container = document.getElementById("root");
if (!container) throw new Error("Root element #root mancante in index.html");
createRoot(container).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
"#;
