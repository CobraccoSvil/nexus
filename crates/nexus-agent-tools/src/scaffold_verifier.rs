//! Tool `nexus_verify_scaffold`: verifica la completezza di un progetto
//! scaffolded (tipicamente da `nexus_extract_figma_code`) prima del primo
//! `npm start`. Identifica file critici mancanti, import path inconsistenti
//! e suggerisce fix concreti.
//!
//! Scopo: evitare il loop iterativo "avvia → 404/import error → diagnose →
//! fix → riavvia" eliminando i bug noti PRIMA del primo run.
//!
//! DIVERGENZA CHIUSA: l'handler leggeva anche un campo `apply` (default true)
//! per la sola ispezione senza scritture, ma quel campo non e' mai stato
//! dichiarato ne' nel contratto d'ingresso ne' nel catalogo che il modello
//! legge — nessun chiamante poteva impostarlo, e il ramo "sola ispezione" era
//! irraggiungibile per costruzione. Ora l'handler legge esattamente cio' che il
//! contratto dichiara. Ridare quella capacita' significa DICHIARARLA (contratto
//! in `tool_inputs.rs` piu' catalogo in `tool_schema.rs`), non rileggerla di
//! nascosto dall'input grezzo aggirando il contratto.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::fs;

use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

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

pub async fn tool_nexus_verify_scaffold(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    use crate::{input_contract::InputTool, tool_inputs::NexusVerifyScaffoldInput};

    let params = match NexusVerifyScaffoldInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // `filter` PRIMA di `unwrap_or`, non dopo: con `target_dir: "/"` (o "" o soli
    // spazi) il trim lascia una stringa VUOTA, e il ripiego non scattava perche'
    // il campo era `Some`. Il percorso composto piu' avanti diventava allora
    // `/index.html` — assoluto — che `normalize_into_root` respinge come fuori
    // dalla radice: l'agente riceveva `DelSistema` («cambia strada») per un
    // parametro che gli bastava riscrivere.
    let target_rel = params
        .target_dir
        .as_deref()
        .map(|s| s.trim().trim_start_matches('/'))
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let target = match resolve_target_dir(&ctx.root_path, target_rel) {
        Ok(t) => t,
        Err(risposta) => return risposta,
    };

    // Rileva project kind (oggi: vite+react+ts; estendibile)
    let pkg = match carica_package_json(&target.join("package.json")).await {
        Ok(pkg) => pkg,
        Err(risposta) => return risposta,
    };

    let mut result = VerifyResult {
        project_kind: "vite-react-ts".into(),
        missing_files: check_critical_files(&target),
        inconsistent_imports: Vec::new(),
        package_json_issues: Vec::new(),
        suggested_fixes: Vec::new(),
    };
    check_package_scripts(&pkg, &mut result);
    let all_deps = collect_all_deps(&pkg);

    let main_tsx = target.join("src/main.tsx");
    check_main_tsx_imports(&main_tsx, &target, &all_deps, &mut result).await;

    let scan = ScanCtx::new(target.as_path(), target_rel, &all_deps);
    let app_uses_router_provider = scan_source_files(&scan, &mut result).await;
    check_double_router(&main_tsx, target_rel, app_uses_router_provider, &mut result).await;

    build_suggested_fixes(target_rel, &mut result);

    // Il verifier non si limita a SUGGERIRE: applica i fix deterministici e
    // idempotenti (regola H). Motivo: il bug del doppio router NON rompe il
    // build (vite compila lo stesso), quindi l'agente vede "build OK" e chiude
    // il turno lasciando l'app a schermo bianco. Applicare qui toglie la
    // dipendenza dalla convergenza dell'agente nel loop diagnose->fix.
    let (applied, apply_errors) =
        apply_fixes(&ctx.root_path, &target, &result.suggested_fixes).await;
    render_report(&result, target_rel, &applied, &apply_errors)
}

/// Risolve `target_dir` dentro la root del workspace. Punto unico (regola L):
/// de-duplica la root se l'agente l'ha inclusa nel path e blocca il traversal
/// ".." (normalize_into_root). `Err` porta gia' il fallimento da restituire al
/// chiamante, con la natura nel campo invece che nel testo (regola Q).
///
/// Entrambi i fallimenti sono `Rimediabile` perche' nascono da cio' che l'agente
/// ha CHIESTO, e il messaggio nomina il campo da correggere: dire «rimediabile»
/// senza dire come sarebbe una promessa non mantenuta.
fn resolve_target_dir(root_path: &Path, target_rel: &str) -> Result<PathBuf, RispostaTool> {
    let clean = match nexus_types::workspace_paths::normalize_into_root(root_path, target_rel) {
        Ok(clean) => clean,
        Err(e) => {
            let messaggio = format!(
                "target_dir '{target_rel}' non valido: {}. Passa un path RELATIVO \
                 alla root del progetto (default '.').",
                e.message()
            );
            return Err(crate::errore_tool(messaggio, NaturaFallimento::Rimediabile));
        }
    };
    let target = root_path.join(&clean);
    if !target.exists() {
        let messaggio = format!(
            "target_dir '{}' non esiste. Elenca le sottocartelle con list_files e \
             ri-chiama nexus_verify_scaffold col path giusto.",
            target.display()
        );
        return Err(crate::errore_tool(messaggio, NaturaFallimento::Rimediabile));
    }
    Ok(target)
}

/// Legge e interpreta il manifest npm del progetto.
///
/// ERRORE INGHIOTTITO CHIUSO: la lettura era `unwrap_or_default()` e il parse
/// `unwrap_or(json!({}))`, quindi un package.json illeggibile o malformato
/// diventava un manifest VUOTO — e il report che ne usciva accusava il progetto
/// di non avere ne' lo script `dev` ne' alcuna dipendenza, cioe' inventava
/// problemi che non esistono a partire da un errore che nessuno dichiarava.
/// I tre esiti ora sono distinti, e nessuno di loro e' «manifest vuoto».
async fn carica_package_json(path: &Path) -> Result<Value, RispostaTool> {
    let contenuto = match fs::read_to_string(path).await {
        Ok(c) => c,
        // Il caso «assente» resta il blocco storico: senza manifest non c'e'
        // nulla da verificare. Chiesto alla read invece che a un `exists()`
        // precedente, cosi' la domanda al filesystem e' una sola.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(manifest_assente()),
        Err(e) => {
            // La natura la legge il `ErrorKind` (regola M): il messaggio del
            // sistema operativo e' localizzato e cambia fra Windows e Linux.
            let natura = NaturaFallimento::da_errore_io(&e);
            let messaggio = format!("package.json '{}' non leggibile: {e}", path.display());
            return Err(crate::errore_tool(messaggio, natura));
        }
    };
    serde_json::from_str(&contenuto).map_err(|e| {
        let messaggio = format!(
            "package.json '{}' non e' JSON valido ({e}): correggilo con edit_file, \
             poi ri-chiama nexus_verify_scaffold.",
            path.display()
        );
        crate::errore_tool(messaggio, NaturaFallimento::Rimediabile)
    })
}

/// Blocco: senza package.json non c'e' nulla da verificare.
///
/// RAMO NUDO CHIUSO: il corpo dichiarava il blocco nel TESTO (`"type":
/// "blocker"`) e usciva come SUCCESSO, quindi il sistema leggeva una verifica
/// mai eseguita come una verifica superata. La natura e' `Rimediabile` perche'
/// l'agente ha due uscite e il messaggio le nomina entrambe: creare il manifest,
/// oppure puntare `target_dir` alla sottocartella che lo contiene.
fn manifest_assente() -> RispostaTool {
    let rimedio = "Non posso verificare lo scaffolding senza package.json: crealo con \
                   write_file, oppure passa target_dir con la sottocartella che lo contiene.";
    let corpo = json!({
        "error": rimedio,
        "project_kind": "unknown",
        "missing_files": [{"path": "package.json", "purpose": "manifest npm", "template_id": null}],
        "inconsistent_imports": [],
        "package_json_issues": ["package.json mancante"],
        "suggested_fixes": [{"type": "blocker", "message": rimedio}],
    });
    crate::errore_tool_con_dettagli(corpo, NaturaFallimento::Rimediabile)
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
    has_apply_errors: bool,
    manual_remaining: usize,
    has_applied: bool,
    ok: bool,
}

/// Prossimo passo suggerito all'agente, dal caso piu' bloccante al piu' sereno.
fn next_step_hint(s: &HintSignals) -> &'static str {
    if s.has_apply_errors {
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

/// Serializza il report finale del verifier e ne DICHIARA l'esito.
///
/// Il report in se' non e' un fallimento nemmeno quando elenca problemi: quelli
/// sono il suo prodotto, e trovarli significa che il tool ha funzionato. A
/// fallire e' il caso in cui il verifier ha PROMESSO una riparazione e non e'
/// riuscito a scriverla (`apply_errors`): li' lo scaffold resta rotto, e senza
/// una dichiarazione nel campo l'agente prosegue verso il primo `npm start`
/// credendo il contrario.
fn render_report(
    result: &VerifyResult,
    target_rel: &str,
    applied: &[Value],
    apply_errors: &[ErroreApply],
) -> RispostaTool {
    let ok = result.missing_files.is_empty()
        && result.inconsistent_imports.is_empty()
        && result.package_json_issues.is_empty();
    let hint = next_step_hint(&HintSignals {
        has_apply_errors: !apply_errors.is_empty(),
        manual_remaining: count_manual_fixes(&result.suggested_fixes),
        has_applied: !applied.is_empty(),
        ok,
    });
    let messaggi: Vec<&str> = apply_errors.iter().map(|e| e.messaggio.as_str()).collect();
    let corpo = json!({
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
        "apply_errors": messaggi,
        "next_step_hint": hint,
    });
    esito_report(corpo, apply_errors)
}

/// L'esito del report: riuscito, oppure fallito con la natura che governa i fix
/// non applicati. Separata da [`render_report`] perche' li' la composizione del
/// corpo e la dichiarazione dell'esito sono due lavori distinti, e il secondo
/// deve poter aggiungere al corpo il campo `error` che il primo non ha.
fn esito_report(mut corpo: Value, apply_errors: &[ErroreApply]) -> RispostaTool {
    let Some(natura) = natura_peggiore(apply_errors) else {
        return RispostaTool::riuscito(corpo.to_string());
    };
    let avviso = format!(
        "{} fix automatici NON applicati: lo scaffold non e' riparato del tutto \
         (vedi apply_errors), non avviarlo prima di averli sistemati.",
        apply_errors.len()
    );
    if let Some(oggetto) = corpo.as_object_mut() {
        oggetto.insert("error".to_string(), Value::String(avviso));
    }
    crate::errore_tool_con_dettagli(corpo, natura)
}

/// La natura che governa l'esito quando piu' fix falliscono insieme: prevale
/// quella che chiude piu' strade. `DelSistema` per prima (l'agente deve cercare
/// un'altra via), poi `Transitorio` (ritentare identico e' corretto), infine
/// `Rimediabile` (l'agente corregge e ri-chiama). `None` = nessun fix fallito.
fn natura_peggiore(errori: &[ErroreApply]) -> Option<NaturaFallimento> {
    [
        NaturaFallimento::DelSistema,
        NaturaFallimento::Transitorio,
        NaturaFallimento::Rimediabile,
    ]
    .into_iter()
    .find(|natura| errori.iter().any(|e| e.natura == *natura))
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
) -> (Vec<Value>, Vec<ErroreApply>) {
    let mut applied: Vec<Value> = Vec::new();
    let mut errors: Vec<ErroreApply> = Vec::new();
    for fix in fixes {
        let esito = match fix.get("type").and_then(Value::as_str).unwrap_or("") {
            "write_file" => applica_write_file(root_path, fix).await,
            "edit_file" => applica_edit_file(target, fix).await,
            _ => continue,
        };
        match esito {
            Ok(Some(v)) => applied.push(v),
            // Niente da fare: il fix era gia' applicato (idempotenza).
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }
    (applied, errors)
}

/// Un fix auto-applicabile che NON e' andato a buon fine: il messaggio per
/// l'umano e la natura del fallimento, che viaggia in un CAMPO fino all'esito
/// del tool invece di restare implicita nella stringa (regola Q).
#[derive(Debug)]
struct ErroreApply {
    messaggio: String,
    natura: NaturaFallimento,
}

/// Scrive il file di un fix `write_file` (template o stub). Ritorna sempre
/// `Some` quando riesce: una scrittura o avviene o fallisce.
async fn applica_write_file(root_path: &Path, fix: &Value) -> Result<Option<Value>, ErroreApply> {
    let path = fix.get("path").and_then(Value::as_str).unwrap_or("");
    let content = fix.get("content").and_then(Value::as_str).unwrap_or("");
    let grezzo = path.trim_start_matches("./");
    let clean = nexus_types::workspace_paths::normalize_into_root(root_path, grezzo).map_err(
        |e| ErroreApply {
            messaggio: format!("write_file {path} path invalido: {}", e.message()),
            // Il path lo compone il verifier dai propri template, non l'agente:
            // se e' invalido non c'e' nessun parametro che l'agente possa
            // correggere, ed e' il tool a essere da riparare.
            natura: NaturaFallimento::DelSistema,
        },
    )?;
    let abs = root_path.join(&clean);
    if let Some(parent) = abs.parent() {
        let _ = fs::create_dir_all(parent).await;
    }
    // La natura viene dal `ErrorKind` (regola M): permesso negato e disco pieno
    // non sono la stessa cosa di un percorso inesistente, e il messaggio del
    // sistema operativo che li distingue e' localizzato.
    fs::write(&abs, content).await.map_err(|e| ErroreApply {
        messaggio: format!("write_file {path}: {e}"),
        natura: NaturaFallimento::da_errore_io(&e),
    })?;
    Ok(Some(json!({"type": "write_file", "path": path})))
}

/// Sostituisce il pattern di un fix `edit_file`. `Ok(None)` = pattern assente,
/// cioe' fix gia' applicato: e' l'idempotenza, non un errore.
async fn applica_edit_file(target: &Path, fix: &Value) -> Result<Option<Value>, ErroreApply> {
    let file = fix.get("file").and_then(Value::as_str).unwrap_or("");
    let from = fix.get("from").and_then(Value::as_str).unwrap_or("");
    let to = fix.get("to").and_then(Value::as_str).unwrap_or("");
    let abs = target.join(file);
    let contenuto = fs::read_to_string(&abs).await.map_err(|e| ErroreApply {
        messaggio: format!("edit_file read {file}: {e}"),
        natura: NaturaFallimento::da_errore_io(&e),
    })?;
    if !contenuto.contains(from) {
        return Ok(None);
    }
    let nuovo = contenuto.replace(from, to);
    fs::write(&abs, nuovo).await.map_err(|e| ErroreApply {
        messaggio: format!("edit_file {file}: {e}"),
        natura: NaturaFallimento::da_errore_io(&e),
    })?;
    Ok(Some(json!({"type": "edit_file", "file": file})))
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

    /// Il blocco per manifest assente e' un FALLIMENTO dichiarato nel campo, non
    /// un report che esce come successo.
    ///
    /// MUTAZIONE: riportando `manifest_assente` a `RispostaTool::riuscito` col
    /// solo corpo JSON, questo test rosseggia — ed e' la firma del difetto
    /// originale, dove una verifica MAI eseguita arrivava all'agente come
    /// verifica superata.
    #[test]
    fn senza_manifest_il_tool_dichiara_il_blocco_nel_campo() {
        let risposta = manifest_assente();
        assert!(risposta.esito.e_fallito(), "e' un blocco: {risposta:?}");
        assert_eq!(
            risposta.natura,
            Some(NaturaFallimento::Rimediabile),
            "creare package.json o correggere target_dir e' cosa che l'agente puo' fare"
        );
        assert!(
            risposta.testo.contains("write_file") && risposta.testo.contains("target_dir"),
            "il messaggio nomina entrambe le uscite: {}",
            risposta.testo
        );
    }

    /// Un fix promesso e non scritto e' un fallimento, e la natura che governa
    /// l'esito e' quella che chiude piu' strade.
    ///
    /// MUTAZIONE: facendo ritornare a `esito_report` sempre `riuscito`, oppure
    /// invertendo l'ordine in `natura_peggiore`, questo test rosseggia — e senza
    /// di lui l'agente prosegue verso `npm start` con lo scaffold ancora rotto.
    #[test]
    fn un_fix_non_scritto_non_esce_come_successo() {
        let corpo = json!({"ok": false, "apply_errors": ["write_file x: accesso negato"]});
        let errori = vec![
            ErroreApply {
                messaggio: "edit_file y: non trovato".into(),
                natura: NaturaFallimento::Rimediabile,
            },
            ErroreApply {
                messaggio: "write_file x: accesso negato".into(),
                natura: NaturaFallimento::DelSistema,
            },
        ];
        let risposta = esito_report(corpo.clone(), &errori);
        assert!(risposta.esito.e_fallito(), "scaffold non riparato: {risposta:?}");
        assert_eq!(
            risposta.natura,
            Some(NaturaFallimento::DelSistema),
            "un permesso negato non lo rimedia l'agente ripetendo la chiamata"
        );
        assert!(
            risposta.testo.contains("apply_errors"),
            "il corpo resta il report, col rimando ai fix falliti: {}",
            risposta.testo
        );

        // Senza fix falliti il report resta un successo, anche se elenca
        // problemi: trovarli e' il prodotto del tool, non il suo fallimento.
        let sereno = esito_report(corpo, &[]);
        assert!(!sereno.esito.e_fallito(), "un report con problemi e' comunque un report");
    }
}
