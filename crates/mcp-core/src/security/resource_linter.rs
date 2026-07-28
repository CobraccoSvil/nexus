//! Linter periodico dei sorgenti progetto per violazioni di governance risorse
//! (porte hardcoded fuori bucket / nel bucket non allocate, URL interni
//! hardcoded). Copre i canali che l'enforcement in scrittura non intercetta:
//! sed/heredoc via run_command, file preesistenti, repository importati.
//!
//! Sola RILEVAZIONE: ogni finding apre una diagnosi `policy_violation`
//! (pannello Problemi) via `resource_governance::open_resource_violation` e
//! viene auditato (`outcome='detected'`); la riparazione e' demandata a
//! `project_workspace::resource_violation_remediation`.
//!
//! Config DB-driven (regola G): settings `agent.resource_violation.linter_*`
//! (mig 0398) + opt-out per progetto `projects.port_lint_enabled` (il
//! meta-progetto Nexus e' escluso alla radice). Parsing delegato ai punti unici
//! `agent_tools::port_scanner` / `agent_tools::url_scanner` (regola L).

use std::collections::HashSet;
use std::path::Path;

use sqlx::PgPool;
use uuid::Uuid;

/// Directory escluse dal walk (generati, dipendenze, VCS).
const LINT_EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "out",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    "coverage",
    ".cache",
    "figma_export",
];

/// Estensioni incluse (sorgenti/config). File senza estensione: solo nomi noti.
const LINT_INCLUDED_EXTS: &[&str] = &[
    "js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "rb", "go", "rs", "java", "cs", "php", "sh",
    "yml", "yaml", "json", "toml", "conf", "ini",
];
const LINT_INCLUDED_BARE_NAMES: &[&str] = &["Dockerfile", "Procfile", "Makefile"];

/// Cap difensivi: file enormi (lock/minificati) e walk runaway.
const MAX_FILE_BYTES: u64 = 262_144;
const MAX_FILES_PER_SCAN: usize = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceViolationKind {
    PortOutOfBucket,
    PortBucketNotAllocated,
    InternalUrlHardcoded,
}

impl ResourceViolationKind {
    pub fn rule(&self) -> (&'static str, &'static str) {
        match self {
            Self::PortOutOfBucket => ("port", "enforce_hardcode"),
            Self::PortBucketNotAllocated => ("port", "require_allocation"),
            Self::InternalUrlHardcoded => ("network", "no_hardcoded_internal"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResourceLintFinding {
    pub rel_path: String,
    pub line: usize,
    /// Porta (per le violazioni porte) o 0 per gli URL.
    pub port: u32,
    /// Valore leggibile (porta o URL) per detail/firma.
    pub value: String,
    pub kind: ResourceViolationKind,
    pub snippet: String,
}

/// Lint di un singolo contenuto (funzione pura, testabile). Delega ai punti
/// unici di parsing; `allocated` = porte allocate del progetto.
pub fn lint_file_content(
    rel_path: &str,
    content: &str,
    allocated: &HashSet<u32>,
) -> Vec<ResourceLintFinding> {
    // I file .env* sono il posto canonico delle porte/URL come variabili:
    // skip totale (coerente con scan_content del port_scanner).
    let file_name = Path::new(rel_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(rel_path);
    if file_name.to_lowercase().starts_with(".env") {
        return Vec::new();
    }

    let mut findings = Vec::new();

    for f in crate::agent_tools::port_scanner::collect_out_of_bucket_ports(content) {
        findings.push(ResourceLintFinding {
            rel_path: rel_path.to_string(),
            line: f.line,
            port: f.port,
            value: f.port.to_string(),
            kind: ResourceViolationKind::PortOutOfBucket,
            snippet: f.snippet.clone(),
        });
    }
    for f in crate::agent_tools::port_scanner::collect_bucket_ports(content) {
        if !allocated.contains(&f.port) {
            findings.push(ResourceLintFinding {
                rel_path: rel_path.to_string(),
                line: f.line,
                port: f.port,
                value: f.port.to_string(),
                kind: ResourceViolationKind::PortBucketNotAllocated,
                snippet: f.snippet.clone(),
            });
        }
    }
    for f in crate::agent_tools::url_scanner::collect_internal_urls(rel_path, content) {
        findings.push(ResourceLintFinding {
            rel_path: rel_path.to_string(),
            line: f.line,
            port: 0,
            value: f.url.clone(),
            kind: ResourceViolationKind::InternalUrlHardcoded,
            snippet: f.snippet.clone(),
        });
    }

    findings
}

/// True se lo snippet e' un fallback con porta letterale: lettura da env PIU' un
/// operatore di default, es. `process.env.PORT || 21950`, `${PORT:-21950}`,
/// `os.environ.get("PORT", 21950)`. Euristica usata SOLO per comporre il
/// messaggio d'azione: la decisione di violazione resta strutturale a monte
/// (`port_scanner`), qui si raffina solo il consiglio di fix.
fn snippet_has_env_fallback(snippet: &str) -> bool {
    let lower = snippet.to_lowercase();
    let has_env_hint = [
        "process.env",
        "os.environ",
        "env::var",
        "getenv",
        "import.meta.env",
    ]
    .iter()
    .any(|h| lower.contains(h))
        || lower.contains("${");
    let has_fallback_op = snippet.contains("||")
        || snippet.contains("??")
        || snippet.contains(":-")
        || lower.contains(".get(");
    has_env_hint && has_fallback_op
}

/// Suffisso AZIONABILE per il `detail` di una violazione. Il pannello Problemi e
/// il prompt di remediation costruiscono il messaggio dal `detail` del linter:
/// senza un'azione esplicita l'agente/utente deve indovinare il fix, e una stessa
/// violazione produce diagnosi divergenti (uno cambia il numero, l'altro rimuove
/// il fallback). Punto unico (regola L) del testo d'azione lato linter; distingue
/// il fallback env hardcoded - dove il fix e' allocare e usare il valore OPPURE
/// rimuovere il fallback - dal numero hardcoded puro.
fn violation_action_hint(kind: &ResourceViolationKind, snippet: &str) -> &'static str {
    let env_fallback = snippet_has_env_fallback(snippet);
    match kind {
        ResourceViolationKind::PortBucketNotAllocated if env_fallback => {
            "Azione: il fallback usa una porta nel range Nexus non allocata. Chiama \
             request_port(label=\"<servizio>\") e usa il valore ritornato come default del \
             fallback, OPPURE rimuovi il fallback numerico lasciando solo la lettura da env. \
             Vedi ADR 0010."
        }
        ResourceViolationKind::PortBucketNotAllocated => {
            "Azione: porta nel range Nexus (20000-39999) non allocata a questo progetto. Chiama \
             request_port(label=\"<servizio>\") e usa la porta ritornata, mai sceglierla a mano. \
             Vedi ADR 0010."
        }
        ResourceViolationKind::PortOutOfBucket if env_fallback => {
            "Azione: il fallback usa una porta fuori dal range Nexus (20000-39999). Chiama \
             request_port(label=\"<servizio>\") e usa la porta allocata come default, oppure \
             rimuovi il fallback. Vedi ADR 0010."
        }
        ResourceViolationKind::PortOutOfBucket => {
            "Azione: porta hardcoded fuori dal range Nexus (20000-39999). Chiama \
             request_port(label=\"<servizio>\") e usa la porta allocata. Vedi ADR 0010."
        }
        ResourceViolationKind::InternalUrlHardcoded => {
            "Azione: URL interno hardcoded. Leggi host e porta da variabile d'ambiente del \
             servizio invece di scriverli nel sorgente. Vedi ADR 0010."
        }
    }
}

fn should_lint_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if LINT_INCLUDED_BARE_NAMES
        .iter()
        .any(|n| name == *n || name.starts_with(&format!("{n}.")))
    {
        return true;
    }
    if name.starts_with("docker-compose") {
        return true;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => LINT_INCLUDED_EXTS.contains(&ext.to_lowercase().as_str()),
        None => false,
    }
}

/// Esito di un walk: i finding E se l'albero e' stato attraversato per intero.
///
/// La completezza non e' un dettaglio diagnostico: e' la premessa senza la quale
/// la lista non significa nulla per chi deve CHIUDERE le violazioni rientrate
/// (regola O). Uno scan fermato al cap produce una lista indistinguibile da una
/// completa; usarla per decidere le chiusure chiuderebbe le violazioni dei file
/// mai visitati. Il tipo obbliga il chiamante a rispondere alla domanda "ho
/// visto tutto?" prima di trarne conclusioni.
pub struct TreeLint {
    pub findings: Vec<ResourceLintFinding>,
    pub complete: bool,
}

/// Walk sincrono della project_root (da chiamare in `spawn_blocking`).
pub fn lint_tree(root: &Path, allocated: &HashSet<u32>) -> TreeLint {
    let mut findings = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned >= MAX_FILES_PER_SCAN {
                tracing::warn!(
                    "resource_linter: cap {MAX_FILES_PER_SCAN} file raggiunto, scan parziale"
                );
                return TreeLint {
                    findings,
                    complete: false,
                };
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !LINT_EXCLUDED_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                    stack.push(path);
                }
                continue;
            }
            if !should_lint_file(&path) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.len() > MAX_FILE_BYTES {
                    continue;
                }
            }
            scanned += 1;
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string_lossy().to_string());
            findings.extend(lint_file_content(&rel, &content, allocated));
        }
    }
    // Uscita naturale del walk: lo stack e' vuoto, l'albero e' stato percorso
    // tutto. L'unica uscita parziale e' quella al cap, sopra.
    TreeLint {
        findings,
        complete: true,
    }
}

/// Variante mirata per la catena del kill runtime: solo finding sulla porta data.
pub fn lint_tree_for_port(
    root: &Path,
    allocated: &HashSet<u32>,
    target: u32,
) -> Vec<ResourceLintFinding> {
    // Qui la completezza non serve: si cerca UNA porta gia' osservata a runtime,
    // non si decide la chiusura di violazioni.
    lint_tree(root, allocated)
        .findings
        .into_iter()
        .filter(|f| f.port == target)
        .collect()
}

/// Porte che valgono come allocazione LEGITTIMA del progetto: registrate in
/// `nexus_port_allocations` E autorizzate (nel bucket del progetto, oppure
/// allocate a mano).
///
/// Il registro da solo non basta: una riga puo' esistere per una porta del
/// bucket di un ALTRO progetto (accadeva quando il rilevamento porta-da-output
/// registrava qualunque porta del range globale). Trattarla come prova di
/// legittimita' chiudeva da sola la violazione che l'aveva prodotta - il linter
/// taceva proprio sul caso peggiore, e piu' il sistema sbagliava meno lo
/// segnalava. Il criterio di autorizzazione toglie al registro quel potere di
/// autoassoluzione: la riga resta, ma non fa piu' da prova.
///
/// Nell'altro verso, una `manual` fuori bucket e' una decisione presa da una
/// persona: il sorgente che usa quella porta non e' in violazione, e segnalarlo
/// darebbe una diagnosi che nessuno puo' chiudere.
pub async fn legitimate_ports_for_project(db: &PgPool, project_id: Uuid) -> HashSet<u32> {
    sqlx::query_as::<_, (i32, String)>(
        "SELECT port::int, allocation_mode FROM nexus_port_allocations WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(p, mode)| u16::try_from(p).ok().map(|p| (p, mode)))
    // Criterio dal punto unico (regola L): qui non si ricalcola nulla.
    .filter(|(p, mode)| nexus_tool_kit::ports::allocation_authorizes_port(&project_id, *p, mode))
    .map(|(p, _)| p as u32)
    .collect()
}

/// Esegue il lint di UN progetto, apre le diagnosi e audita i finding nuovi.
/// Ritorna il numero di violazioni aperte (nuove).
pub async fn lint_project(state: &crate::AppState, project_id: Uuid, root_path: &str) -> usize {
    let db = &state.db;
    let allocated = legitimate_ports_for_project(db, project_id).await;
    let root = std::path::PathBuf::from(root_path);
    if !root.is_dir() {
        return 0;
    }
    let alloc_clone = allocated.clone();
    let scan = match tokio::task::spawn_blocking(move || lint_tree(&root, &alloc_clone)).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "resource_linter: walk fallito");
            return 0;
        }
    };
    let findings = scan.findings;

    let mut opened = 0usize;
    for f in &findings {
        if open_lint_finding(db, project_id, f).await {
            opened += 1;
        }
    }

    // Simmetria fra apertura e chiusura (regola H, niente fantasmi eterni nel
    // pannello Problemi). Il walk ha appena prodotto l'insieme COMPLETO dei
    // finding del progetto: le diagnosi per-file la cui firma non compare piu'
    // sono rientrate, anche quando a cambiare non e' stato il file ma le porte
    // allocate al progetto — il caso che la ri-validazione per-file non puo'
    // vedere, perche' la innesca la scrittura del file.
    //
    // Solo su scan COMPLETO: fermarsi al cap dei file e chiudere sarebbe
    // dichiarare rientrate le violazioni dei file mai visitati.
    if scan.complete {
        let present = present_signatures(project_id, &findings);
        let resolved = resolve_absent_file_violations(db, project_id, None, &present).await;
        if !resolved.is_empty() {
            tracing::info!(
                project_id = %project_id,
                resolved = resolved.len(),
                "resource_linter: violazioni non piu' presenti nei sorgenti risolte"
            );
        }
    }

    if opened > 0 {
        tracing::warn!(
            project_id = %project_id,
            opened,
            total = findings.len(),
            "resource_linter: violazioni nuove aperte"
        );
        // Realtime: badge problemi + pannello si aggiornano subito.
        nexus_events::dispatcher::emit_global(
            project_id,
            nexus_events::event::ProjectEvent::Notification {
                severity: "error".to_string(),
                message: format!(
                    "Rilevate {opened} violazione/i di governance risorse nei sorgenti (vedi pannello Problemi)"
                ),
                panel: Some("problems".to_string()),
                ttl_ms: Some(15_000),
                run_id: None,
            },
        );
    }
    opened
}

/// Punto unico (regola L) di APERTURA di una diagnosi da un finding del linter:
/// firma + detail azionabile + open_resource_violation + audit. Usato sia dal
/// lint completo (`lint_project`) sia dalla ri-validazione per-file
/// (`revalidate_file_violations`). Ritorna true se ha aperto una nuova diagnosi
/// (dedup per firma a monte: non riapre violazioni gia' note).
async fn open_lint_finding(db: &PgPool, project_id: Uuid, f: &ResourceLintFinding) -> bool {
    let (kind, rule) = f.kind.rule();
    let sig = finding_signature(project_id, f);
    let detail = format!(
        "{}:{} {} ({}/{}) | {}\n-> {}",
        f.rel_path,
        f.line,
        f.value,
        kind,
        rule,
        f.snippet.chars().take(160).collect::<String>(),
        violation_action_hint(&f.kind, &f.snippet),
    );
    let opened_id = crate::security::resource_governance::open_resource_violation(
        db,
        project_id,
        kind,
        rule,
        f.port as f64,
        Some(&f.rel_path),
        &detail,
        &sig,
    )
    .await;
    if opened_id.is_some() {
        let entry = crate::security::AuditEntry {
            project_id,
            actor: "system",
            actor_user_id: None,
            actor_session_id: None,
            action: "resource_lint_violation".to_string(),
            resource_kind: if f.port > 0 { "port" } else { "network" },
            resource_id: Some(f.value.clone()),
            outcome: "detected",
            details: serde_json::json!({
                "file": f.rel_path,
                "line": f.line,
                "rule": format!("{kind}/{rule}"),
            }),
        };
        crate::security::record_audit(entry);
        true
    } else {
        false
    }
}

/// Ri-valida le violazioni di governance risorse su UN file appena modificato
/// (write/edit). Controparte per-file di `lint_project`: copre il caso "l'utente
/// o l'agente corregge il file" che il linter periodico vede solo dopo minuti.
///
/// - apre eventuali violazioni NUOVE introdotte dall'edit (riuso `open_lint_finding`);
/// - chiude (status='resolved') le diagnosi `policy_violation` aperte su QUESTO
///   file che NON sono piu' presenti nel contenuto corrente, confrontando per
///   firma (metric + file_path + value) — la stessa identita' usata in apertura.
///
/// Emette `FindingsUpdated` con gli id risolti cosi' il pannello Problemi si
/// aggiorna in tempo reale. `abs_path` deve essere il path assoluto del file
/// modificato; viene risolto in path relativo alla root del progetto per
/// allineare la firma a quella prodotta dal lint completo.
pub async fn revalidate_file_violations(
    db: &PgPool,
    project_id: Uuid,
    root_path: &str,
    abs_path: &Path,
) {
    // Flag DB-driven coerente con il linter periodico (regola G): opt-out per progetto.
    let lint_on =
        sqlx::query_scalar::<_, bool>("SELECT port_lint_enabled FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .unwrap_or(true);
    if !lint_on {
        return;
    }

    let root = Path::new(root_path);
    // Path relativo alla root: deve combaciare con il rel_path del lint completo,
    // altrimenti la firma non coincide e non chiuderemmo la diagnosi giusta.
    let rel_path = match abs_path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        // File fuori dalla root del progetto: non e' soggetto al lint progetto.
        Err(_) => return,
    };
    if !should_lint_file(abs_path) {
        return;
    }

    let allocated = legitimate_ports_for_project(db, project_id).await;
    // Contenuto corrente del file (se illeggibile/cancellato: nessun finding,
    // quindi tutte le diagnosi aperte sul file vengono risolte).
    let current = tokio::fs::read_to_string(abs_path)
        .await
        .unwrap_or_default();
    let findings = lint_file_content(&rel_path, &current, &allocated);

    // 1) Apre eventuali violazioni nuove (dedup a monte: non riapre le note).
    for f in &findings {
        let _ = open_lint_finding(db, project_id, f).await;
    }

    // 2) Chiusura delle violazioni non piu' presenti su QUESTO file.
    let present_sigs = present_signatures(project_id, &findings);
    let resolved =
        resolve_absent_file_violations(db, project_id, Some(&rel_path), &present_sigs).await;
    if !resolved.is_empty() {
        tracing::info!(
            project_id = %project_id,
            file = %rel_path,
            resolved = resolved.len(),
            "resource_linter: violazioni risorse risolte dopo edit del file"
        );
    }
}

/// Firma di UN finding: PUNTO UNICO (regola L) condiviso da chi apre le
/// diagnosi e da chi le chiude. Se le due strade la calcolassero ciascuna per
/// conto proprio, una divergenza qualunque (il path relativo, l'ordine di
/// kind/rule) non romperebbe nulla in modo visibile: semplicemente nessuna
/// diagnosi verrebbe mai piu' chiusa, e il pannello Problemi accumulerebbe
/// fantasmi in silenzio.
fn finding_signature(project_id: Uuid, f: &ResourceLintFinding) -> String {
    let (kind, rule) = f.kind.rule();
    crate::security::resource_governance::violation_signature(
        project_id,
        Some(&f.rel_path),
        &f.value,
        &format!("{kind}/{rule}"),
    )
}

/// Firme delle violazioni presenti in un insieme di finding.
fn present_signatures(
    project_id: Uuid,
    findings: &[ResourceLintFinding],
) -> std::collections::HashSet<String> {
    findings
        .iter()
        .map(|f| finding_signature(project_id, f))
        .collect()
}

/// PUNTO UNICO (regola L) della CHIUSURA delle violazioni per-file rientrate:
/// una diagnosi resta aperta solo finche' la sua firma compare ancora fra i
/// finding correnti. `file_scope` sceglie l'ampiezza: `Some(rel_path)` dopo
/// l'edit di un file, `None` per l'intero progetto dopo il lint completo.
/// Ritorna gli id risolti (vuoto se non c'era nulla da chiudere).
///
/// Perche' serve anche lo scope progetto (regola H, niente fantasmi eterni nel
/// pannello Problemi): una violazione di porta dipende da DUE fatti — il
/// contenuto del file E l'insieme delle porte allocate al progetto. La
/// ri-validazione per-file copre solo il primo, perche' e' innescata dalla
/// scrittura del file. Quando e' il secondo a cambiare — la porta viene
/// allocata DOPO che il file e' stato scritto — nessuno rivaluta e la diagnosi
/// resta aperta per sempre su un problema che non esiste piu'. Caso reale del
/// 26/07/2026: `scripts/setup-crud.sh` scritto alle 13:36 UTC con la porta
/// 32987 non ancora allocata, porta poi allocata (`adopted`) alle 15:04 UTC,
/// diagnosi ancora `open` a fine giornata; e una gemella sulla 33276 aperta dal
/// 23/07. Il lint completo attraversa gia' tutto l'albero e conosce l'insieme
/// dei finding correnti: gli mancava solo la simmetria fra aprire e chiudere.
///
/// Le diagnosi con `file_path IS NULL` sono escluse di proposito: appartengono
/// alle violazioni rilevate a runtime, il cui ciclo di vita e' gestito da
/// `resolve_stale_runtime_port_violations` (port_enforcer).
async fn resolve_absent_file_violations(
    db: &PgPool,
    project_id: Uuid,
    file_scope: Option<&str>,
    present_sigs: &std::collections::HashSet<String>,
) -> Vec<Uuid> {
    let open_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, error_signature_hash FROM service_diagnoses \
          WHERE project_id = $1 AND signal_kind = 'policy_violation' \
            AND file_path IS NOT NULL \
            AND ($2::text IS NULL OR file_path = $2) \
            AND status IN ('open', 'diagnosing', 'failed_remediation')",
    )
    .bind(project_id)
    .bind(file_scope)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let resolved_ids: Vec<Uuid> = open_rows
        .into_iter()
        .filter(|(_, sig)| !present_sigs.contains(sig))
        .map(|(id, _)| id)
        .collect();

    if resolved_ids.is_empty() {
        return Vec::new();
    }

    let _ = sqlx::query(
        "UPDATE service_diagnoses SET status = 'resolved', resolved_at = NOW() \
          WHERE id = ANY($1)",
    )
    .bind(&resolved_ids)
    .execute(db)
    .await;

    // Realtime: il pannello Problemi ascolta FindingsUpdated e ri-fetcha.
    // total/critical/warnings restano 0: il pannello Problemi non li usa per
    // i policy_violation (refetch via get_project_problems), ma resolved_ids
    // permette al frontend di marcare in-place i problemi spariti.
    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::event::ProjectEvent::FindingsUpdated {
            scan_id: None,
            total: 0,
            critical: 0,
            warnings: 0,
            resolved_ids: resolved_ids.clone(),
        },
    );
    resolved_ids
}

/// Worker periodico: linta tutti i progetti con `port_lint_enabled=true` e
/// innesca la riparazione delle violazioni aperte. Flag e cadenza DB-driven
/// letti a ogni ciclo (regola G).
pub fn spawn_resource_linter(state: crate::AppState) {
    tokio::spawn(async move {
        // Primo giro ritardato: lascia partire i worker di base.
        tokio::time::sleep(std::time::Duration::from_secs(90)).await;
        loop {
            let enabled =
                crate::settings::get_setting(&state.db, "agent.resource_violation.linter_enabled")
                    .await
                    .ok()
                    .flatten()
                    .map(|v| {
                        !matches!(
                            v.trim().to_lowercase().as_str(),
                            "false" | "0" | "off" | "no"
                        )
                    })
                    .unwrap_or(true);
            let interval_s = crate::settings::get_setting(
                &state.db,
                "agent.resource_violation.linter_interval_seconds",
            )
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s >= 60)
            .unwrap_or(300);

            if enabled {
                let projects: Vec<(Uuid, String)> = sqlx::query_as(
                    "SELECT id, repository_root_path FROM projects \
                      WHERE COALESCE(repository_root_path, '') <> '' \
                        AND port_lint_enabled = TRUE \
                      ORDER BY created_at DESC LIMIT 50",
                )
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();
                for (pid, root) in projects {
                    let opened = lint_project(&state, pid, &root).await;
                    if opened > 0 {
                        crate::project_workspace::resource_violation_remediation::process_open_violations(
                            &state, pid,
                        )
                        .await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(interval_s)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_rileva_porta_fuori_bucket_e_url() {
        let allocated = HashSet::new();
        let f = lint_file_content(
            "server.js",
            "const port = process.env.PORT || 5000;\nfetch('http://localhost:3000/api');\n",
            &allocated,
        );
        assert!(f
            .iter()
            .any(|x| x.kind == ResourceViolationKind::PortOutOfBucket && x.port == 5000));
        assert!(f
            .iter()
            .any(|x| x.kind == ResourceViolationKind::InternalUrlHardcoded));
        // 3000 dentro l'URL non e' una porta "di listen": il port_scanner non
        // la cattura (pattern listen/bind/PORT=), e' coperta dal finding URL.
    }

    #[test]
    fn lint_bucket_non_allocata_vs_allocata() {
        let mut allocated = HashSet::new();
        let f = lint_file_content("app.py", "PORT = 21970\n", &allocated);
        assert!(f
            .iter()
            .any(|x| x.kind == ResourceViolationKind::PortBucketNotAllocated));
        allocated.insert(21970);
        let f = lint_file_content("app.py", "PORT = 21970\n", &allocated);
        assert!(f.is_empty());
    }

    #[test]
    fn lint_skippa_env() {
        let allocated = HashSet::new();
        let f = lint_file_content(".env", "PORT=5000\n", &allocated);
        assert!(f.is_empty());
    }

    /// Il walk vero su un albero vero: la lista dei finding, da sola, non dice
    /// se e' completa, e chi decide le chiusure deve saperlo prima di trarne
    /// conclusioni (regola O).
    #[test]
    fn lint_tree_dichiara_se_ha_visto_tutto_l_albero() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("app.py"), "PORT = 21970\n").expect("write");
        let scan = lint_tree(dir.path(), &HashSet::new());
        assert!(
            scan.complete,
            "albero di un file: lo scan non puo' essere parziale"
        );
        assert!(scan.findings.iter().any(|f| f.port == 21970));
    }

    /// La chiusura filtra le diagnosi per firma: se la firma non distinguesse
    /// file e porta, chiudere una violazione rientrata ne chiuderebbe altre
    /// ancora vive. Caso reale del 26/07: `scripts/setup-crud.sh` con la 32987.
    #[test]
    fn la_firma_distingue_file_e_porta_ed_e_deterministica() {
        let project_id = Uuid::nil();
        let findings = lint_file_content("scripts/setup-crud.sh", "PORT=32987\n", &HashSet::new());
        let f = findings
            .first()
            .expect("una porta del bucket non allocata e' una violazione");
        assert_eq!(f.kind, ResourceViolationKind::PortBucketNotAllocated);

        let presenti = present_signatures(project_id, &findings);
        assert!(
            presenti.contains(&finding_signature(project_id, f)),
            "una violazione ancora presente non deve risultare assente: verrebbe chiusa da viva"
        );
        // Determinismo: due calcoli della stessa firma coincidono, altrimenti
        // nessuna diagnosi verrebbe mai richiusa.
        assert_eq!(
            finding_signature(project_id, f),
            finding_signature(project_id, f)
        );

        let altro_file = ResourceLintFinding {
            rel_path: "scripts/altro.sh".into(),
            ..f.clone()
        };
        let altra_porta = ResourceLintFinding {
            value: "32988".into(),
            port: 32988,
            ..f.clone()
        };
        assert!(!presenti.contains(&finding_signature(project_id, &altro_file)));
        assert!(!presenti.contains(&finding_signature(project_id, &altra_porta)));
    }

    /// Dalla riga nel registro alla conseguenza (regola O): il caso reale di
    /// "gestione-spese". L'agente aveva scritto a mano `process.env.PORT || 20001`
    /// e il rilevamento porta-da-output aveva registrato 20001 come allocazione
    /// del progetto, benche' quella porta appartenga al bucket di un ALTRO. Da
    /// quel momento il linter taceva: la porta risultava "allocata", cioe' la
    /// violazione produceva da se' la prova della propria legittimita'.
    ///
    /// Il test non asserisce un predicato: parte dalla riga in
    /// `nexus_port_allocations` sullo schema META REALE e arriva a cio' che quella
    /// riga deve o non deve poter fare - autorizzare il sorgente che l'ha causata.
    ///
    /// Mutazione che rende rosso: togliere il filtro sul bucket da
    /// `legitimate_ports_for_project` -> `fuori` rientra fra le porte legittime, il
    /// finding sparisce e le ultime due asserzioni cadono.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn riga_registrata_fuori_bucket_non_autorizza_il_sorgente(pool: PgPool) {
        let (_user, project_id) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let (bucket_start, bucket_end) =
            crate::project_workspace::services::project_bucket_range(&project_id);

        // La porta dell'incidente (20001, bucket 20000-20049 di un altro progetto),
        // a meno che il progetto seminato abbia proprio quel bucket: allora si
        // prende la prima porta subito dopo il proprio.
        let fuori: i32 = if bucket_start == crate::project_workspace::services::PROJECT_PORT_RANGE_START
        {
            (bucket_end + 1) as i32
        } else {
            20001
        };
        let dentro: i32 = bucket_start as i32;

        // Le due righe come le scriveva il rilevamento: allocation_mode 'auto',
        // label del servizio.
        for (port, label) in [(fuori, "backend"), (dentro, "frontend")] {
            sqlx::query(
                "INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode) \
                 VALUES ($1, $2, $3, 'auto')",
            )
            .bind(project_id)
            .bind(port)
            .bind(label)
            .execute(&pool)
            .await
            .expect("registrazione della porta rilevata");
        }

        let legittime = legitimate_ports_for_project(&pool, project_id).await;
        assert!(
            legittime.contains(&(dentro as u32)),
            "la porta del proprio bucket resta un'allocazione valida"
        );
        assert!(
            !legittime.contains(&(fuori as u32)),
            "la porta {fuori} e' registrata ma sta nel bucket di un altro progetto \
             ({bucket_start}-{bucket_end}): il registro non puo' farla passare per legittima"
        );

        // La conseguenza: il sorgente che ha causato la registrazione resta una
        // violazione, quindi la diagnosi si apre e la remediation parte.
        let sorgente = format!("const PORT = process.env.PORT || {fuori};\n");
        let findings = lint_file_content("backend/src/server.js", &sorgente, &legittime);
        assert!(
            findings
                .iter()
                .any(|f| f.port == fuori as u32
                    && f.kind == ResourceViolationKind::PortBucketNotAllocated),
            "la porta fuori bucket deve restare segnalata: {findings:?}"
        );

        // Controprova: la porta legittima non produce rumore, altrimenti il fix
        // avrebbe solo spostato il difetto dall'altra parte.
        let sano = format!("const PORT = process.env.PORT || {dentro};\n");
        assert!(
            lint_file_content("backend/src/server.js", &sano, &legittime).is_empty(),
            "una porta allocata nel proprio bucket non e' una violazione"
        );
    }

    #[test]
    fn should_lint_filtra_estensioni() {
        assert!(should_lint_file(Path::new("src/server.js")));
        assert!(should_lint_file(Path::new("docker-compose.yml")));
        assert!(should_lint_file(Path::new("Dockerfile")));
        assert!(!should_lint_file(Path::new("README.md")));
        assert!(!should_lint_file(Path::new("logo.png")));
    }

    #[test]
    fn env_fallback_riconosciuto_dallo_snippet() {
        // Il caso che ha confuso i run: fallback env con porta letterale.
        assert!(snippet_has_env_fallback(
            "port: parseInt(process.env.PORT || '21950', 10),"
        ));
        assert!(snippet_has_env_fallback("PORT=${PORT_BACKEND:-21950}"));
        assert!(snippet_has_env_fallback(
            "port = os.environ.get(\"PORT\", 21950)"
        ));
        // Hardcode puro: nessun fallback env.
        assert!(!snippet_has_env_fallback("PORT = 21970"));
        assert!(!snippet_has_env_fallback("app.listen(21950)"));
    }

    #[test]
    fn action_hint_distingue_fallback_da_hardcode() {
        // Fallback env -> il consiglio include la rimozione del fallback.
        let env = violation_action_hint(
            &ResourceViolationKind::PortBucketNotAllocated,
            "process.env.PORT || '21950'",
        );
        assert!(env.contains("rimuovi il fallback"));
        assert!(env.contains("request_port"));
        // Hardcode puro -> consiglio di allocazione, senza "rimuovi il fallback".
        let pure = violation_action_hint(
            &ResourceViolationKind::PortBucketNotAllocated,
            "PORT = 21950",
        );
        assert!(pure.contains("request_port"));
        assert!(!pure.contains("rimuovi il fallback"));
        // Ogni kind produce un'azione che cita ADR 0010.
        for kind in [
            ResourceViolationKind::PortOutOfBucket,
            ResourceViolationKind::PortBucketNotAllocated,
            ResourceViolationKind::InternalUrlHardcoded,
        ] {
            assert!(violation_action_hint(&kind, "x").contains("ADR 0010"));
        }
    }
}
