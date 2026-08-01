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
    let target = match resolve_target_dir(&ctx.root_path, target_rel) {
        Ok(t) => t,
        Err(err_json) => return err_json,
    };

    // Rileva project kind (oggi: vite+react+ts; estendibile)
    let pkg_json_path = target.join("package.json");
    if !pkg_json_path.exists() {
        return missing_package_json_report();
    }

    let mut result = VerifyResult {
        project_kind: "vite-react-ts".into(),
        missing_files: check_critical_files(&target),
        inconsistent_imports: Vec::new(),
        package_json_issues: Vec::new(),
        suggested_fixes: Vec::new(),
    };

    let pkg_content = fs::read_to_string(&pkg_json_path).await.unwrap_or_default();
    let pkg: Value = serde_json::from_str(&pkg_content).unwrap_or(json!({}));
    check_package_scripts(&pkg, &mut result);
    let all_deps = collect_all_deps(&pkg);

    let main_tsx = target.join("src/main.tsx");
    check_main_tsx_imports(&main_tsx, &target, &all_deps, &mut result).await;

    let scan = ScanCtx::new(target.as_path(), target_rel, &all_deps);
    let app_uses_router_provider = scan_source_files(&scan, &mut result).await;
    check_double_router(&main_tsx, target_rel, app_uses_router_provider, &mut result).await;

    build_suggested_fixes(target_rel, &mut result);

    let apply = input.get("apply").and_then(Value::as_bool).unwrap_or(true);
    let (applied, apply_errors) = run_auto_apply(&ctx.root_path, &target, &result, apply).await;
    render_report(&result, target_rel, &applied, &apply_errors, apply)
}

/// Risolve `target_dir` dentro la root del workspace. Punto unico (regola L):
/// de-duplica la root se l'agente l'ha inclusa nel path e blocca il traversal
/// ".." (normalize_into_root). `Err` contiene il JSON d'errore gia' serializzato,
/// pronto per essere ritornato al chiamante.
fn resolve_target_dir(root_path: &Path, target_rel: &str) -> Result<PathBuf, String> {
    let clean = match nexus_types::workspace_paths::normalize_into_root(root_path, target_rel) {
        Ok(clean) => clean,
        Err(e) => {
            return Err(crate::errore_json(format!(
                "target_dir non valido: {}",
                e.message()
            )))
        }
    };
    let target = root_path.join(&clean);
    if !target.exists() {
        return Err(crate::errore_json(format!("target_dir '{}' non esiste", target.display())));
    }
    Ok(target)
}

/// Report di blocco: senza package.json non c'e' nulla da verificare.
fn missing_package_json_report() -> String {
    json!({
        "project_kind": "unknown",
        "missing_files": [{"path": "package.json", "purpose": "manifest npm", "template_id": null}],
        "inconsistent_imports": [],
        "package_json_issues": ["package.json mancante"],
        "suggested_fixes": [{
            "type": "blocker",
            "message": "Non posso verificare scaffolding senza package.json. Crealo prima."
        }]
    })
    .to_string()
}

/// File critici che un progetto Vite+React+TS deve avere per partire: quelli
/// assenti diventano `MissingFile` con il template che li sa ricreare.
fn check_critical_files(target: &Path) -> Vec<MissingFile> {
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
    let mut missing = Vec::new();
    for (path, purpose, tmpl) in critical {
        let full = target.join(path);
        if !full.exists() {
            missing.push(MissingFile {
                path: path.to_string(),
                purpose: purpose.to_string(),
                template_id: *tmpl,
            });
        }
    }
    missing
}

/// Senza script 'dev' ne' 'start' vite non parte con `npm run dev/start`.
fn check_package_scripts(pkg: &Value, result: &mut VerifyResult) {
    let scripts = pkg.get("scripts").cloned().unwrap_or(json!({}));
    if scripts.get("dev").is_some() || scripts.get("start").is_some() {
        return;
    }
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

/// Unione dei nomi in dependencies + devDependencies dichiarati in package.json.
fn collect_all_deps(pkg: &Value) -> std::collections::HashSet<String> {
    let deps = pkg.get("dependencies").cloned().unwrap_or(json!({}));
    let dev_deps = pkg.get("devDependencies").cloned().unwrap_or(json!({}));
    deps.as_object()
        .into_iter()
        .chain(dev_deps.as_object())
        .flat_map(|m| m.keys().cloned())
        .collect()
}

/// Radice del pacchetto npm di un import: `@scope/name` per gli scoped, il primo
/// segmento per tutti gli altri.
fn npm_package_root(path: &str) -> String {
    let first = path.split('/').next().unwrap_or(path);
    if !first.starts_with('@') {
        return first.to_string();
    }
    // scoped: @scope/name
    let mut it = path.splitn(3, '/');
    let scope = it.next().unwrap_or("");
    let name = it.next().unwrap_or("");
    format!("{}/{}", scope, name)
}

/// Classifica un singolo import di `src/main.tsx`: relativo non risolvibile (con
/// l'eventuale path corretto) oppure pacchetto npm assente dalle dependencies.
/// `None` = import coerente, niente da segnalare.
async fn classify_main_import(
    path: &str,
    main_tsx: &Path,
    target: &Path,
    all_deps: &std::collections::HashSet<String>,
) -> Option<InconsistentImport> {
    if path.starts_with("./") || path.starts_with("../") {
        // import relativo: verifica file
        let Some(suggested) = resolve_relative_import(main_tsx, path, target).await else {
            return Some(InconsistentImport {
                file: "src/main.tsx".into(),
                import_path: path.to_string(),
                reason: "file non trovato neanche con resolve fallback".into(),
                suggested_path: None,
            });
        };
        if suggested == path {
            return None;
        }
        return Some(InconsistentImport {
            file: "src/main.tsx".into(),
            import_path: path.to_string(),
            reason: "file non trovato al path indicato".into(),
            suggested_path: Some(suggested),
        });
    }
    if path.starts_with('/') {
        return None;
    }
    // pkg npm
    let pkg_root = npm_package_root(path);
    if all_deps.contains(&pkg_root) {
        return None;
    }
    Some(InconsistentImport {
        file: "src/main.tsx".into(),
        import_path: path.to_string(),
        reason: format!("pkg '{}' non in dependencies/devDependencies", pkg_root),
        suggested_path: Some(format!("npm install {}", pkg_root)),
    })
}

/// Verifica che gli import di `src/main.tsx` esistano davvero (file relativi) o
/// siano dichiarati (pacchetti npm).
async fn check_main_tsx_imports(
    main_tsx: &Path,
    target: &Path,
    all_deps: &std::collections::HashSet<String>,
    result: &mut VerifyResult,
) {
    if !main_tsx.exists() {
        return;
    }
    let Ok(content) = fs::read_to_string(main_tsx).await else {
        return;
    };
    for cap in import_regex().captures_iter(&content) {
        let path = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if let Some(issue) = classify_main_import(&path, main_tsx, target, all_deps).await {
            result.inconsistent_imports.push(issue);
        }
    }
}

/// Contesto della scansione dei sorgenti: root del progetto, path relativo con
/// cui l'agente l'ha chiamata e quale dei due pacchetti router e' installato.
struct ScanCtx<'a> {
    target: &'a Path,
    target_rel: &'a str,
    has_rr: bool,
    has_rr_dom: bool,
}

impl<'a> ScanCtx<'a> {
    fn new(
        target: &'a Path,
        target_rel: &'a str,
        all_deps: &std::collections::HashSet<String>,
    ) -> Self {
        Self {
            target,
            target_rel,
            has_rr: all_deps.contains("react-router"),
            has_rr_dom: all_deps.contains("react-router-dom"),
        }
    }
}

/// Import dei simboli router dal pacchetto "react-router" (esatto, non -dom): se
/// NON e' in dependencies ma react-router-dom si', va normalizzato a v6, altrimenti
/// il build fallisce con "createBrowserRouter is not exported by react-router".
fn router_import_issue(content: &str, rel: &str, scan: &ScanCtx<'_>) -> Option<InconsistentImport> {
    let imports_v7 =
        content.contains("from \"react-router\"") || content.contains("from 'react-router'");
    if !imports_v7 || scan.has_rr || !scan.has_rr_dom {
        return None;
    }
    Some(InconsistentImport {
        file: rel.to_string(),
        import_path: "react-router".into(),
        reason: "import da 'react-router' (v7) non presente in dependencies; usa 'react-router-dom' (v6 installato), che esporta createBrowserRouter/RouterProvider".into(),
        suggested_path: Some("react-router-dom".into()),
    })
}

/// Path (relativo alla root del progetto) dello stub sonner da generare.
fn sonner_stub_rel(file: &Path, import_path: &str, target: &Path) -> Option<String> {
    let parent = file.parent()?;
    parent
        .join(format!("{}.tsx", import_path))
        .strip_prefix(target)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .ok()
}

/// Wrapper UI 'sonner' importato ma assente -> registra il file mancante e il fix
/// che ne genera lo stub (re-export del Toaster).
async fn check_sonner_stub(
    file: &Path,
    content: &str,
    scan: &ScanCtx<'_>,
    result: &mut VerifyResult,
) {
    for cap in import_regex().captures_iter(content) {
        let path = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let is_sonner = (path.starts_with("./") || path.starts_with("../"))
            && (path.ends_with("/ui/sonner") || path.ends_with("/sonner"));
        if !is_sonner || resolve_relative_import(file, &path, scan.target).await.is_some() {
            continue;
        }
        let Some(stub_rel) = sonner_stub_rel(file, &path, scan.target) else {
            continue;
        };
        result.missing_files.push(MissingFile {
            path: stub_rel.clone(),
            purpose: "wrapper Toaster (re-export da 'sonner')".into(),
            template_id: None,
        });
        result.suggested_fixes.push(json!({
            "type": "write_file",
            "path": format!("{}/{}", scan.target_rel.trim_end_matches('/'), stub_rel),
            "content": "// Stub generato da nexus_verify_scaffold: re-export Toaster da 'sonner'.\nexport { Toaster } from \"sonner\";\n",
            "note": format!("'{}' e' importato ma il file non esiste -> genera lo stub (re-export del Toaster di sonner).", path),
        }));
    }
}

/// Router consistency (causa #1 del build-loop sugli export Figma).
/// L'export Figma spesso importa i simboli router da "react-router" (v7) o
/// avvolge App in <BrowserRouter> (v6) mentre App usa <RouterProvider>
/// (data-router v6.4): import non risolto oppure doppio router (App NON monta ->
/// schermo bianco). Si scansionano TUTTI i .tsx sotto src/, non solo main.tsx:
/// l'export sparge i bug del router in App.tsx/routes.tsx.
/// Ritorna true se almeno un sorgente monta <RouterProvider>.
async fn scan_source_files(scan: &ScanCtx<'_>, result: &mut VerifyResult) -> bool {
    let source_files = collect_source_files(&scan.target.join("src")).await;
    let mut app_uses_router_provider = false;
    for f in &source_files {
        let Ok(content) = fs::read_to_string(f).await else {
            continue;
        };
        let rel = f
            .strip_prefix(scan.target)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| f.to_string_lossy().to_string());
        if content.contains("RouterProvider") {
            app_uses_router_provider = true;
        }
        if let Some(issue) = router_import_issue(&content, &rel, scan) {
            result.inconsistent_imports.push(issue);
        }
        check_sonner_stub(f, &content, scan, result).await;
    }
    app_uses_router_provider
}

/// main.tsx avvolge App in <BrowserRouter> mentre l'app usa <RouterProvider> ->
/// doppio router: l'app non monta. Sostituisci main.tsx col template canonico
/// (rende solo <App />, il routing lo gestisce RouterProvider dentro App).
async fn check_double_router(
    main_tsx: &Path,
    target_rel: &str,
    app_uses_router_provider: bool,
    result: &mut VerifyResult,
) {
    if !main_tsx.exists() || !app_uses_router_provider {
        return;
    }
    let Ok(main_content) = fs::read_to_string(main_tsx).await else {
        return;
    };
    if !main_content.contains("BrowserRouter") {
        return;
    }
    result.package_json_issues.push(
        "main.tsx avvolge App in <BrowserRouter> (v6) mentre l'app usa <RouterProvider> (data-router): doppio router -> App NON monta (schermo bianco)".into(),
    );
    result.suggested_fixes.push(json!({
        "type": "write_file",
        "path": format!("{}/src/main.tsx", target_rel.trim_end_matches('/')),
        "content": VITE_MAIN_TSX,
        "note": "Sostituisci main.tsx col template canonico che rende solo <App /> SENZA <BrowserRouter>: il routing e' gia' gestito da <RouterProvider> dentro App. Cosi' l'app monta invece di restare bianca.",
    }));
}

/// Fix per i file mancanti: write_file dal template quando esiste, altrimenti
/// blocker (creazione manuale).
fn fixes_for_missing_files(target_rel: &str, missing: &[MissingFile]) -> Vec<Value> {
    let mut fixes = Vec::new();
    for mf in missing {
        if let Some(tmpl) = mf.template_id {
            let body = template_content(tmpl).unwrap_or("");
            fixes.push(json!({
                "type": "write_file",
                "path": format!("{}/{}", target_rel.trim_end_matches('/'), mf.path),
                "content": body,
                "note": format!("Crea '{}' ({}) dal template '{}'.", mf.path, mf.purpose, tmpl),
            }));
        } else {
            fixes.push(json!({
                "type": "blocker",
                "message": format!("File '{}' mancante ({}). Crealo manualmente.", mf.path, mf.purpose),
            }));
        }
    }
    fixes
}

/// Fix per gli import incoerenti: install del pacchetto, riscrittura del path o
/// revisione manuale quando non c'e' un suggerimento.
fn fixes_for_inconsistent_imports(imports: &[InconsistentImport]) -> Vec<Value> {
    let mut fixes = Vec::new();
    for ii in imports {
        let Some(sug) = &ii.suggested_path else {
            fixes.push(json!({
                "type": "manual_review",
                "file": ii.file,
                "import_path": ii.import_path,
                "reason": ii.reason,
            }));
            continue;
        };
        if sug.starts_with("npm install ") {
            fixes.push(json!({
                "type": "run_command",
                "command": sug,
                "note": format!("Install pkg per import in {}", ii.file),
            }));
        } else {
            fixes.push(json!({
                "type": "edit_file",
                "file": ii.file,
                "from": format!("from \"{}\"", ii.import_path),
                "to": format!("from \"{}\"", sug),
                "note": format!("Reason: {}", ii.reason),
            }));
        }
    }
    fixes
}

/// Traduce i problemi rilevati nei fix concreti finali, nell'ordine storico:
/// prima i file mancanti, poi gli import incoerenti.
fn build_suggested_fixes(target_rel: &str, result: &mut VerifyResult) {
    let from_missing = fixes_for_missing_files(target_rel, &result.missing_files);
    let from_imports = fixes_for_inconsistent_imports(&result.inconsistent_imports);
    result.suggested_fixes.extend(from_missing);
    result.suggested_fixes.extend(from_imports);
}

/// Auto-apply dei fix deterministici (regola H). Il verifier non si limita a
/// SUGGERIRE: APPLICA i fix sicuri e idempotenti (write_file di template/stub,
/// edit_file di normalizzazione import router). Motivo: il bug del doppio router
/// NON rompe il build (vite compila lo stesso), quindi l'agente vede "build OK" e
/// conclude il turno lasciando l'app a schermo bianco. Applicare lato verifier
/// toglie la dipendenza dalla convergenza dell'agente nel loop diagnose->fix.
/// run_command/blocker/manual_review NON sono auto-applicati (richiedono comandi
/// o giudizio). Disattivabile con apply=false per sola ispezione.
async fn run_auto_apply(
    root_path: &Path,
    target: &Path,
    result: &VerifyResult,
    apply: bool,
) -> (Vec<Value>, Vec<String>) {
    if !apply {
        return (Vec::new(), Vec::new());
    }
    apply_fixes(root_path, target, &result.suggested_fixes).await
}

/// Fix che restano a carico dell'agente: richiedono esecuzione di comandi o
/// giudizio, quindi il verifier non li auto-applica.
fn count_manual_fixes(fixes: &[Value]) -> usize {
    fixes
        .iter()
        .filter(|f| {
            matches!(
                f.get("type").and_then(Value::as_str).unwrap_or(""),
                "run_command" | "blocker" | "manual_review" | "edit_package_json"
            )
        })
        .count()
}

/// Segnali che scelgono il suggerimento di prossimo passo, in ordine di priorita'.
struct HintSignals {
    apply: bool,
    has_apply_errors: bool,
    manual_remaining: usize,
    has_applied: bool,
    ok: bool,
}

/// Prossimo passo suggerito all'agente, dal caso piu' bloccante al piu' sereno.
fn next_step_hint(s: &HintSignals) -> &'static str {
    if !s.apply {
        "apply=false: sola ispezione. Applica i suggested_fixes (write_file/edit_file/run_command), poi ri-chiama."
    } else if s.has_apply_errors {
        "Alcuni fix automatici sono FALLITI (vedi apply_errors): risolvili a mano, poi build."
    } else if s.manual_remaining > 0 {
        "Fix deterministici applicati automaticamente. Restano azioni manuali (run_command/blocker) in suggested_fixes: eseguile, poi build."
    } else if s.has_applied {
        "Fix applicati automaticamente: scaffold riparato (router/import/template). Avvia/build: niente schermo bianco da doppio router."
    } else if s.ok {
        "Scaffolding consistente. Puoi avviare con npm run dev/start senza errori noti."
    } else {
        "Nessun fix auto-applicabile rilevato; vedi suggested_fixes."
    }
}

/// Serializza il report finale del verifier.
fn render_report(
    result: &VerifyResult,
    target_rel: &str,
    applied: &[Value],
    apply_errors: &[String],
    apply: bool,
) -> String {
    let ok = result.missing_files.is_empty()
        && result.inconsistent_imports.is_empty()
        && result.package_json_issues.is_empty();
    let hint = next_step_hint(&HintSignals {
        apply,
        has_apply_errors: !apply_errors.is_empty(),
        manual_remaining: count_manual_fixes(&result.suggested_fixes),
        has_applied: !applied.is_empty(),
        ok,
    });
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
        "applied": applied,
        "apply_errors": apply_errors,
        "next_step_hint": hint,
    })
    .to_string()
}

/// Applica i fix deterministici e idempotenti prodotti dalla verifica:
/// `write_file` (template/stub) e `edit_file` (normalizzazione import). NON
/// applica `run_command`/`blocker`/`manual_review`/`edit_package_json`, che
/// richiedono esecuzione comandi o giudizio. Idempotente: ri-applicare e' sicuro
/// (write_file riscrive identico; edit_file salta se il pattern `from` non e'
/// piu' presente). Estratto come punto unico testabile (regola L).
async fn apply_fixes(
    root_path: &Path,
    target: &Path,
    fixes: &[Value],
) -> (Vec<Value>, Vec<String>) {
    let mut applied: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for fix in fixes {
        match fix.get("type").and_then(Value::as_str).unwrap_or("") {
            "write_file" => {
                let path = fix.get("path").and_then(Value::as_str).unwrap_or("");
                let content = fix.get("content").and_then(Value::as_str).unwrap_or("");
                match nexus_types::workspace_paths::normalize_into_root(
                    root_path,
                    path.trim_start_matches("./"),
                ) {
                    Ok(clean) => {
                        let abs = root_path.join(&clean);
                        if let Some(parent) = abs.parent() {
                            let _ = fs::create_dir_all(parent).await;
                        }
                        match fs::write(&abs, content).await {
                            Ok(_) => applied.push(json!({"type": "write_file", "path": path})),
                            Err(e) => errors.push(format!("write_file {}: {}", path, e)),
                        }
                    }
                    Err(e) => {
                        errors.push(format!("write_file {} path invalido: {}", path, e.message()))
                    }
                }
            }
            "edit_file" => {
                let file = fix.get("file").and_then(Value::as_str).unwrap_or("");
                let from = fix.get("from").and_then(Value::as_str).unwrap_or("");
                let to = fix.get("to").and_then(Value::as_str).unwrap_or("");
                let abs = target.join(file);
                match fs::read_to_string(&abs).await {
                    Ok(c) if c.contains(from) => {
                        match fs::write(&abs, c.replace(from, to)).await {
                            Ok(_) => applied.push(json!({"type": "edit_file", "file": file})),
                            Err(e) => errors.push(format!("edit_file {}: {}", file, e)),
                        }
                    }
                    // `from` assente: gia' applicato (idempotente), non un errore.
                    Ok(_) => {}
                    Err(e) => errors.push(format!("edit_file read {}: {}", file, e)),
                }
            }
            _ => {}
        }
    }
    (applied, errors)
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

/// Raccoglie ricorsivamente i file .tsx/.ts sotto `dir`, saltando node_modules e
/// le cartelle nascoste. Usato per verificare gli import in TUTTI i sorgenti, non
/// solo main.tsx: l'export Figma sparge i bug del router (react-router v7,
/// BrowserRouter+RouterProvider) e gli import UI mancanti in App.tsx/routes.tsx.
async fn collect_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut rd = match fs::read_dir(&d).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            let p = entry.path();
            match entry.file_type().await {
                Ok(ft) if ft.is_dir() => stack.push(p),
                Ok(ft) if ft.is_file() && (name.ends_with(".tsx") || name.ends_with(".ts")) => {
                    out.push(p)
                }
                _ => {}
            }
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn apply_fixes_ripara_router_e_main_tsx() {
        // Replica il bug Beauty-Book: App.tsx importa da "react-router" (v7, non
        // in deps) e usa RouterProvider; main.tsx avvolge App in <BrowserRouter>
        // (doppio router -> schermo bianco). I fix: edit_file import + write_file
        // main.tsx canonico. run_command NON deve essere auto-applicato.
        let root =
            std::env::temp_dir().join(format!("scaffold_apply_{}", uuid::Uuid::new_v4()));
        let app_dir = root.join("src/app");
        fs::create_dir_all(&app_dir).await.unwrap();
        let app_tsx = app_dir.join("App.tsx");
        fs::write(
            &app_tsx,
            "import { RouterProvider } from \"react-router\";\nexport default function App() { return <RouterProvider router={router} />; }\n",
        )
        .await
        .unwrap();
        let main_tsx = root.join("src/main.tsx");
        fs::write(
            &main_tsx,
            "import { BrowserRouter } from 'react-router-dom';\n// vecchio main con doppio router\n",
        )
        .await
        .unwrap();

        let fixes = vec![
            json!({
                "type": "edit_file",
                "file": "src/app/App.tsx",
                "from": "from \"react-router\"",
                "to": "from \"react-router-dom\"",
            }),
            json!({
                "type": "write_file",
                "path": "./src/main.tsx",
                "content": VITE_MAIN_TSX,
            }),
            json!({"type": "run_command", "command": "npm install foo"}),
        ];

        let (applied, errors) = apply_fixes(&root, &root, &fixes).await;
        assert!(errors.is_empty(), "errori inattesi: {:?}", errors);
        assert_eq!(applied.len(), 2, "solo write_file + edit_file auto-applicati");

        let app_after = fs::read_to_string(&app_tsx).await.unwrap();
        assert!(
            app_after.contains("from \"react-router-dom\""),
            "import non normalizzato: {app_after}"
        );
        let main_after = fs::read_to_string(&main_tsx).await.unwrap();
        assert_eq!(main_after, VITE_MAIN_TSX, "main.tsx non sostituito col canonico");
        assert!(
            !main_after.contains("BrowserRouter"),
            "BrowserRouter ancora presente in main.tsx"
        );

        // Idempotenza: al 2o giro edit_file salta (pattern assente), write_file
        // riscrive identico -> nessun errore, un solo applied.
        let (applied2, errors2) = apply_fixes(&root, &root, &fixes).await;
        assert!(errors2.is_empty(), "errori al secondo apply: {:?}", errors2);
        assert_eq!(applied2.len(), 1, "al 2o giro solo write_file (edit_file gia' fatto)");

        let _ = fs::remove_dir_all(&root).await;
    }
}
