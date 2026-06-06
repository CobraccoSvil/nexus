//! Definizioni statiche dei tool Nexus Builtin.
//!
//! Contiene la struct `ToolDef`, la costante `NEXUS_BUILTIN_SERVER_ID_STR`
//! e l'array statico `NEXUS_TOOLS` con tutti i descrittori dei tool
//! registrati nel server Nexus Builtin.

use uuid::Uuid;

/// UUID fisso del server Nexus Builtin (corrisponde alla migration 0044).
pub const NEXUS_BUILTIN_SERVER_ID_STR: &str = "00000000-0000-0000-0000-000000000001";

pub fn nexus_builtin_server_id() -> Uuid {
    Uuid::parse_str(NEXUS_BUILTIN_SERVER_ID_STR).expect("UUID builtin non valido")
}

// ---------------------------------------------------------------------------
// Definizioni statiche dei tool
// ---------------------------------------------------------------------------

pub(super) struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: &'static str,
}

pub(super) static NEXUS_TOOLS: &[ToolDef] = &[
    // ── run_config ────────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_run_config_list",
        description: "Elenca tutte le configurazioni di avvio salvate per un progetto Nexus (comandi come 'npm run dev', 'cargo run', ecc.)",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"}}}"#,
    },
    ToolDef {
        name: "nexus_run_config_detect",
        description: "Analizza il progetto e suggerisce automaticamente configurazioni di avvio (npm scripts, cargo targets, python entry points, .NET projects)",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"}}}"#,
    },
    ToolDef {
        name: "nexus_run_config_create",
        description: "Crea una nuova configurazione di avvio per un progetto. Usa per aggiungere comandi di start/build/test che appaiono nel pannello Run.",
        schema: r#"{"type":"object","required":["project_id","label","command"],"properties":{"project_id":{"type":"string"},"label":{"type":"string","description":"Nome visualizzato (es. 'Dev Server')"},"command":{"type":"string","description":"Comando da eseguire (es. 'npm')"},"args":{"type":"array","items":{"type":"string"},"description":"Argomenti (es. ['run','dev'])"},"cwd":{"type":"string","description":"Directory di lavoro (assoluta o relativa alla root progetto)"},"env":{"type":"object","description":"Variabili ambiente aggiuntive"},"kind":{"type":"string","enum":["shell","npm","cargo","python","node"],"description":"Tipo configurazione"}}}"#,
    },
    ToolDef {
        name: "nexus_run_config_update",
        description: "Aggiorna una configurazione di avvio esistente",
        schema: r#"{"type":"object","required":["project_id","config_id"],"properties":{"project_id":{"type":"string"},"config_id":{"type":"string","description":"UUID della configurazione"},"label":{"type":"string"},"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"},"env":{"type":"object"},"kind":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_run_config_delete",
        description: "Elimina una configurazione di avvio",
        schema: r#"{"type":"object","required":["project_id","config_id"],"properties":{"project_id":{"type":"string"},"config_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_run_config_launch",
        description: "Avvia una configurazione di avvio salvata. Ritorna il process_id per monitorare l'output.",
        schema: r#"{"type":"object","required":["project_id","config_id"],"properties":{"project_id":{"type":"string"},"config_id":{"type":"string"}}}"#,
    },
    // ── git_advanced ──────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_git_log",
        description: "Mostra la cronologia commit del repository git del progetto",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer","description":"Numero massimo di commit (default: 20)","default":20}}}"#,
    },
    ToolDef {
        name: "nexus_git_diff",
        description: "Mostra le modifiche non committate (diff) del progetto o di un file specifico",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"path":{"type":"string","description":"Percorso file specifico (opzionale, default: tutte le modifiche)"}}}"#,
    },
    ToolDef {
        name: "nexus_git_branches",
        description: "Elenca tutti i branch del repository git del progetto",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_checkout",
        description: "Effettua il checkout di un branch esistente nel progetto",
        schema: r#"{"type":"object","required":["project_id","branch"],"properties":{"project_id":{"type":"string"},"branch":{"type":"string","description":"Nome del branch (deve esistere)"}}}"#,
    },
    ToolDef {
        name: "nexus_git_create_branch",
        description: "Crea un nuovo branch nel repository git del progetto",
        schema: r#"{"type":"object","required":["project_id","branch_name"],"properties":{"project_id":{"type":"string"},"branch_name":{"type":"string","description":"Nome del nuovo branch"}}}"#,
    },
    // ── project ───────────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_project_list",
        description: "Elenca tutti i progetti Nexus dell'utente corrente",
        schema: r#"{"type":"object","properties":{}}"#,
    },
    ToolDef {
        name: "nexus_project_analyze",
        description: "Avvia l'analisi del progetto (rilevamento stack, dipendenze, indice semantico)",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_project_quality_scan",
        description: "Avvia una scansione di qualità del codice del progetto (complessità, naming, pattern errati, SQL injection, N+1 query, ecc.)",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_project_quality_findings",
        description: "Recupera i risultati dell'ultima scansione di qualità del progetto",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"severity":{"type":"string","enum":["error","warning","info","all"],"description":"Filtra per gravità (default: all)"}}}"#,
    },
    // ── profile ───────────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_profile_list",
        description: "Elenca i profili AI dell'utente corrente (profili specializzati con system prompt personalizzati)",
        schema: r#"{"type":"object","properties":{}}"#,
    },
    ToolDef {
        name: "nexus_profile_delete",
        description: "Elimina un profilo AI dell'utente",
        schema: r#"{"type":"object","required":["profile_id"],"properties":{"profile_id":{"type":"string","description":"UUID del profilo"}}}"#,
    },
    ToolDef {
        name: "nexus_profile_set_default",
        description: "Imposta un profilo AI come predefinito per l'utente",
        schema: r#"{"type":"object","required":["profile_id"],"properties":{"profile_id":{"type":"string"}}}"#,
    },
    // ── prompt_template ───────────────────────────────────────────────────
    ToolDef {
        name: "nexus_prompt_template_list",
        description: "Elenca i prompt template del sistema Nexus (system prompt, automation, quality rules, ecc.)",
        schema: r#"{"type":"object","properties":{"category":{"type":"string","description":"Filtra per categoria: system, automation, quality, profile, chat"}}}"#,
    },
    ToolDef {
        name: "nexus_prompt_template_update",
        description: "Aggiorna il contenuto di un prompt template nel database. Cambia il comportamento di Nexus senza rebuild.",
        schema: r#"{"type":"object","required":["key","content"],"properties":{"key":{"type":"string","description":"Chiave del template (es. 'system.nexus_base', 'chat.precheck_message')"},"content":{"type":"string","description":"Nuovo contenuto del prompt"},"change_note":{"type":"string","description":"Nota sul cambiamento (per la cronologia)"}}}"#,
    },
    // ── mcp_runtime (discovery + call) ────────────────────────────────────
    ToolDef {
        name: "nexus_mcp_tool_search",
        description: "Cerca tra TUTTI i tool MCP disponibili (builtin + esterni abilitati) e ritorna risultati con server_id e schema. Usalo per scoprire tool a runtime senza inviare tutte le definizioni al provider.",
        schema: r#"{"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Query testuale (nome tool, descrizione, keyword)"},"limit":{"type":"integer","description":"Max risultati (default: 10)","default":10}}}"#,
    },
    ToolDef {
        name: "nexus_mcp_tool_call",
        description: "Esegue un tool MCP (anche esterno) usando server_id + tool_name + arguments. Sicuro: verifica che il server sia accessibile e applica la policy del plugin se presente.",
        schema: r#"{"type":"object","required":["server_id","tool_name","arguments"],"properties":{"server_id":{"type":"string","description":"UUID del server MCP"},"tool_name":{"type":"string","description":"Nome tool originale (es. 'list_issues')"},"arguments":{"type":"object","description":"Argomenti JSON per il tool (secondo il suo input_schema)"}}}"#,
    },
    ToolDef {
        name: "nexus_mcp_tool_reindex",
        description: "Rigenera l'indice semantico Qdrant di tutti i tool MCP (o solo quelli non ancora indicizzati). Richiede ruolo admin. Utile dopo importazione massiva di server MCP o per forzare la risincronizzazione.",
        schema: r#"{"type":"object","properties":{"force":{"type":"boolean","description":"Se true, reindicizza tutti i tool anche se l'hash non è cambiato. Default: false."}}}"#,
    },
    // ── admin_settings ────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_admin_setting_get",
        description: "Legge il valore di una impostazione di sistema Nexus. Richiede ruolo admin.",
        schema: r#"{"type":"object","required":["key"],"properties":{"key":{"type":"string","description":"Chiave della impostazione"}}}"#,
    },
    ToolDef {
        name: "nexus_admin_setting_update",
        description: "Aggiorna il valore di una impostazione di sistema Nexus. Richiede ruolo admin.",
        schema: r#"{"type":"object","required":["key","value"],"properties":{"key":{"type":"string"},"value":{"type":"string","description":"Nuovo valore"}}}"#,
    },
    // ── documents ─────────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_doc_generate",
        description: "Genera un documento professionale .docx (Analisi Funzionale IEEE 830, Analisi Tecnica, Diagramma ER, Gestione Progetto, Release Notes). Compila il content_json con le sezioni del documento strutturate gerarchicamente.",
        schema: r#"{"type":"object","required":["project_id","doc_type","content_json"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"doc_type":{"type":"string","enum":["functional_analysis","technical_analysis","er_diagram","project_management","release_notes"],"description":"Tipo di documento"},"content_json":{"type":"object","description":"Contenuto strutturato con sections[{number,title,content,subsections}]"},"title":{"type":"string","description":"Titolo del documento (default: da template)"},"standard":{"type":"string","enum":["ieee830","iso29148","minimal"],"description":"Standard di riferimento (default: ieee830)"}}}"#,
    },
    ToolDef {
        name: "nexus_doc_update",
        description: "Aggiorna sezioni specifiche di un documento esistente. Incrementa la versione automaticamente.",
        schema: r#"{"type":"object","required":["project_id","document_id","sections"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"document_id":{"type":"string","description":"UUID del documento da aggiornare"},"sections":{"type":"array","items":{"type":"object","properties":{"number":{"type":"string"},"title":{"type":"string"},"content":{"type":"string"}}},"description":"Sezioni da aggiornare"},"bump":{"type":"string","enum":["patch","minor","major"],"description":"Tipo di incremento versione (default: patch)"}}}"#,
    },
    ToolDef {
        name: "nexus_doc_list",
        description: "Elenca tutti i documenti generati per un progetto, con tipo, versione e stato.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"doc_type":{"type":"string","description":"Filtra per tipo documento"},"status":{"type":"string","description":"Filtra per stato: draft, review, approved, outdated"}}}"#,
    },
    ToolDef {
        name: "nexus_doc_search",
        description: "Cerca nei documenti vettorializzati del progetto per trovare sezioni rilevanti. Utile per verificare se un requisito è già documentato o trovare sezioni outdated.",
        schema: r#"{"type":"object","required":["project_id","query"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"query":{"type":"string","description":"Query in linguaggio naturale"},"doc_type":{"type":"string","description":"Filtra per tipo documento"},"limit":{"type":"integer","description":"Numero massimo risultati (default: 5)"}}}"#,
    },
    ToolDef {
        name: "nexus_doc_status",
        description: "Cambia lo stato di un documento (draft, review, approved, outdated).",
        schema: r#"{"type":"object","required":["project_id","document_id","status"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"document_id":{"type":"string","description":"UUID del documento"},"status":{"type":"string","enum":["draft","review","approved","outdated"],"description":"Nuovo stato"}}}"#,
    },
    // ── file mutations / rollback ─────────────────────────────────────────
    ToolDef {
        name: "nexus_file_mutations_list",
        description: "Elenca le modifiche file recenti fatte dall'agente in questo progetto, ognuna con id, path, op (created/modified/deleted/reverted), dimensione before/after, timestamp e flag 'revertible'. Usalo quando l'utente chiede di vedere lo storico delle modifiche o vuole annullare un cambiamento.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"limit":{"type":"integer","description":"Numero massimo voci (default 50, max 500)"}}}"#,
    },
    ToolDef {
        name: "nexus_file_mutation_diff",
        description: "Carica i contenuti completi prima e dopo di una specifica mutazione file, per mostrare il diff all'utente prima di un eventuale ripristino. Usalo dopo nexus_file_mutations_list quando devi presentare il dettaglio di un cambiamento.",
        schema: r#"{"type":"object","required":["project_id","mutation_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"mutation_id":{"type":"integer","description":"ID della mutazione (dalla list)"}}}"#,
    },
    ToolDef {
        name: "nexus_session_branch_info",
        description: "Ritorna il branch git di auto-commit della sessione corrente (es. nexus/session/a1b2c3d4) e l'elenco dei comandi pronti all'uso per ispezionare/mergiare/scartare l'intera sessione. Utile quando l'utente chiede 'come vedo tutto cio' che ho fatto in questa chat?' o 'fai diff di tutta la sessione'.",
        schema: r#"{"type":"object","properties":{"project_id":{"type":"string","description":"UUID del progetto (default: progetto corrente)"}}}"#,
    },
    ToolDef {
        name: "nexus_file_revert",
        description: "Ripristina un file allo stato precedente a una mutazione specifica, oppure annulla l'ultima mutazione del progetto. Usalo quando l'utente dice 'annulla', 'torna indietro', 'ripristina', 'rimedia agli errori', dopo aver chiarito quale modifica annullare. Per sicurezza il revert fallisce se il file e' stato modificato dopo la mutazione (l'utente deve confermare con force=true).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"mutation_id":{"type":"integer","description":"ID specifico della mutazione da annullare. Se assente, annulla l'ULTIMA mutazione annullabile del progetto."},"force":{"type":"boolean","description":"Se true sovrascrive anche in caso di conflict (il file e' stato modificato dopo). Default false."}}}"#,
    },

    // ── editor UI ─────────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_open_file_in_editor",
        description: "Apre un file del progetto nell'editor del web-ide. Usalo quando l'utente chiede 'apri il file X' o quando ti riferisci a un file specifico nella risposta. Il frontend intercetta il tool_result e dispatcha l'evento di apertura.",
        schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"path":{"type":"string","description":"Path relativo alla root del progetto (es. 'src/main.rs' o 'README.md'). NON path assoluti."},"line":{"type":"integer","description":"Numero linea su cui posizionare il cursore (opzionale)"}}}"#,
    },
    // ── nexus_tool_catalog (Fase 9A) ──────────────────────────────────────
    // Questi tool sono eseguiti dal NexusToolCatalog (handler Rust nativi,
    // non dal match dispatcher). Il nome col prefisso `nexus_` viene
    // riconosciuto dal dispatcher e inoltrato al catalog strippando il
    // prefisso (es. `nexus_cargo_check` → catalog lookup `cargo_check`).
    ToolDef {
        name: "nexus_cargo_check",
        description: "Esegue `cargo check --message-format=json` sul progetto e ritorna errori/warning strutturati (file, line, message). Opzionale filtrare per workspace member.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"workspace_member":{"type":"string","description":"Package workspace da controllare (es. 'mcp-core'). Se assente, check l'intero workspace."},"release":{"type":"boolean","description":"Se true, check in release mode. Default: false."}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_build",
        description: "Esegue `cargo build --message-format=json` sul progetto e ritorna errori/warning. Può produrre artefatti in target/.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"release":{"type":"boolean"},"all_targets":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_test",
        description: "Esegue `cargo test --no-fail-fast` e ritorna contatori passed/failed/ignored con lista dei test falliti.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"filter":{"type":"string","description":"Filtro substring sul nome test"},"release":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_bench",
        description: "Esegue `cargo bench` e ritorna numero di benchmark e output testuale.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"filter":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_clean",
        description: "Esegue `cargo clean` per rimuovere la directory target/. Destructive sul filesystem.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"release":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_update",
        description: "Esegue `cargo update` per aggiornare Cargo.lock alle ultime versioni compatibili. Richiede rete verso crates.io.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"crate":{"type":"string","description":"Crate specifico da aggiornare"},"dry_run":{"type":"boolean"},"aggressive":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_tree",
        description: "Esegue `cargo tree` e ritorna l'albero delle dipendenze con conteggi.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"depth":{"type":"integer","minimum":0},"edges":{"type":"string","enum":["features","normal","build","dev","all"]}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_metadata",
        description: "Esegue `cargo metadata --format-version=1` e ritorna il grafo dei package del workspace.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"no_deps":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_audit",
        description: "Esegue `cargo audit --json` per scansionare vulnerabilità note (CVE/RUSTSEC) nelle dipendenze. Richiede cargo-audit installato.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"deny_warnings":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_outdated",
        description: "Esegue `cargo outdated --format json` per elencare dipendenze obsolete. Richiede cargo-outdated installato.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"workspace":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_clippy_lint",
        description: "Esegue `cargo clippy --message-format=json` e ritorna lint strutturati. Opzionalmente applica --fix o tratta warning come errori.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"},"all_targets":{"type":"boolean"},"fix":{"type":"boolean"},"deny_warnings":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_rustc_version",
        description: "Esegue `rustc --version --verbose` e ritorna info toolchain (versione, host triple, commit hash, LLVM version).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_rustc_explain",
        description: "Esegue `rustc --explain Exxxx` per ottenere la spiegazione completa di un error code rustc.",
        schema: r#"{"type":"object","required":["project_id","error_code"],"properties":{"project_id":{"type":"string"},"error_code":{"type":"string","description":"Codice errore in formato Exxxx (es. 'E0599')"}}}"#,
    },
    ToolDef {
        name: "nexus_test_coverage",
        description: "Esegue `cargo llvm-cov --json --summary-only` e ritorna coverage aggregata (lines/functions/regions). Richiede cargo-llvm-cov installato.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"workspace_member":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_secret_scan",
        description: "Scansione regex-based di credenziali hardcoded (AWS key, GitHub token, PEM key, JWT, Slack token, password) nei file del progetto.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"max_files":{"type":"integer","description":"Limite massimo file scansionati","default":2000}}}"#,
    },
    ToolDef {
        name: "nexus_license_check",
        description: "Analizza le licenze delle dipendenze (da cargo metadata) e categorizza in permissive/copyleft/unknown. Evidenzia licenze non permissive.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_status",
        description: "Esegue `git status --porcelain=v2 --branch` sul progetto e ritorna lo stato strutturato: branch, upstream, ahead/behind, modified/added/deleted/renamed/untracked/ignored.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"}}}"#,
    },
    ToolDef {
        name: "nexus_git_log_structured",
        description: "Esegue `git log` con formato strutturato e ritorna array di commit (hash, author, date, subject, body). Più potente di nexus_git_log testuale.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer","default":20},"path":{"type":"string"},"author":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_diff_structured",
        description: "Esegue `git diff --stat` + diff completo. Ritorna files_changed/insertions/deletions + diff testuale. Più ricco di nexus_git_diff.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"staged":{"type":"boolean"},"revision":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_blame",
        description: "Esegue `git blame --porcelain` per un file e ritorna array di righe con sha/author/summary/content.",
        schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string"},"path":{"type":"string","description":"Percorso file (relativo alla root progetto)"},"revision":{"type":"string"}}}"#,
    },
    // ── Fase 9C: 6 handler aggiuntivi ────────────────────────────────────
    ToolDef {
        name: "nexus_format_code",
        description: "Esegue `cargo fmt` (default --check, apply=true per modificare). Ritorna elenco file da formattare.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"apply":{"type":"boolean"},"workspace_member":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_deploy_check",
        description: "Pre-deploy readiness audit: uncommitted, upstream, deploy files, env sample, lockfile tracking.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_gh_issue_list",
        description: "Elenca issue GitHub via `gh issue list --json`. Richiede `gh` autenticato.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"repo":{"type":"string"},"state":{"type":"string","enum":["open","closed","all"]},"limit":{"type":"integer"},"label":{"type":"string"},"author":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_memory_ns_read",
        description: "Legge una chiave dal NexusBridge memory namespace (scoped per project).",
        schema: r#"{"type":"object","required":["project_id","key"],"properties":{"project_id":{"type":"string"},"key":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_memory_ns_write",
        description: "Scrive una coppia key/value nel NexusBridge memory namespace (scoped per project).",
        schema: r#"{"type":"object","required":["project_id","key","value"],"properties":{"project_id":{"type":"string"},"key":{"type":"string"},"value":{},"author":{"type":"string"},"ttl_seconds":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_regex_match",
        description: "Esegue una regex su testo inline o su un file del progetto e ritorna i match.",
        schema: r#"{"type":"object","required":["project_id","pattern"],"properties":{"project_id":{"type":"string"},"pattern":{"type":"string"},"text":{"type":"string"},"file":{"type":"string"},"case_insensitive":{"type":"boolean"},"multi_line":{"type":"boolean"},"max_matches":{"type":"integer"}}}"#,
    },
    // ── Fase 9D: 18 handler aggiuntivi ───────────────────────────────────
    ToolDef {
        name: "nexus_ast_parse",
        description: "Parsifica un file sorgente e ritorna AST index (simboli, imports, line_count) via mcp-ast.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"content":{"type":"string"},"language":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_ast_query",
        description: "Interroga i simboli di un file filtrando per kind/name_pattern/visibility.",
        schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"kind":{"type":"string"},"name_pattern":{"type":"string"},"visibility":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_lint_run",
        description: "Linter dispatcher multi-linguaggio (clippy / eslint / ruff / flake8).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_test_generate",
        description: "Genera scaffold di test unit a partire dalle funzioni di un file (Rust/TS/JS/Python).",
        schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"function":{"type":"string"},"max":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_coverage_report",
        description: "Coverage report multi-stack (cargo llvm-cov / npm run coverage).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_sast_scan",
        description: "SAST scan via semgrep (se disponibile) o regex builtin (eval/SQL/unsafe/password).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"prefer_semgrep":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_deps_audit",
        description: "Audit delle dipendenze multi-stack (cargo audit / npm audit / pip-audit).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_rename_symbol",
        description: "Rinomina un simbolo in un singolo file (word-boundary regex). Preview di default.",
        schema: r#"{"type":"object","required":["project_id","path","old_name","new_name"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"old_name":{"type":"string"},"new_name":{"type":"string"},"apply":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_extract_function",
        description: "Estrae un range di righe in una nuova funzione (Rust/TS/JS/Python) — scaffold meccanico.",
        schema: r#"{"type":"object","required":["project_id","path","start_line","end_line","new_name"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"start_line":{"type":"integer"},"end_line":{"type":"integer"},"new_name":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_api_docs",
        description: "Genera documentazione API del codice sorgente (cargo doc / npm run docs / sphinx). NON confondere con nexus_doc_generate che crea documenti .docx.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"include_deps":{"type":"boolean"},"open":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_deps_tree",
        description: "Dep tree multi-stack (cargo tree / npm list --json / pipdeptree --json).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"depth":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_build_project",
        description: "Build dispatcher multi-stack (cargo build / npm run build / make / python -m build).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"release":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_gh_pr_create",
        description: "Crea una pull request GitHub via `gh pr create`.",
        schema: r#"{"type":"object","required":["project_id","title"],"properties":{"project_id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"base":{"type":"string"},"head":{"type":"string"},"draft":{"type":"boolean"},"repo":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_profile_run",
        description: "Profiling wall-clock: esegue N volte un comando whitelistato e ritorna mean/min/max/p95.",
        schema: r#"{"type":"object","required":["project_id","command"],"properties":{"project_id":{"type":"string"},"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"runs":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_bench_run",
        description: "Benchmark dispatcher (cargo bench [filter] / npm run bench).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"filter":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_db_schema_inspect",
        description: "Introspezione schema PostgreSQL via information_schema (tables + columns).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"schema":{"type":"string"},"table":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_db_query_explain",
        description: "EXPLAIN (FORMAT JSON) di una query SELECT/WITH (analyze opt-in).",
        schema: r#"{"type":"object","required":["project_id","sql"],"properties":{"project_id":{"type":"string"},"sql":{"type":"string"},"analyze":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_openapi_validate",
        description: "Valida una spec OpenAPI (JSON parse + check strutturali minimi).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"content":{"type":"string"}}}"#,
    },
    // ── Fase 9E: RuVector + Consensus ───────────────────────────────────
    ToolDef {
        name: "nexus_ruvector_insert",
        description: "Embed e indicizza un testo nel database HNSW globale (RuVector).",
        schema: r#"{"type":"object","required":["project_id","text"],"properties":{"project_id":{"type":"string"},"text":{"type":"string"},"id":{"type":"string"},"namespace":{"type":"string"},"tags":{"type":"object"},"ttl_seconds":{"type":"integer","minimum":0}}}"#,
    },
    ToolDef {
        name: "nexus_ruvector_search",
        description: "k-NN search sul database HNSW globale a partire da un testo di query.",
        schema: r#"{"type":"object","required":["project_id","query"],"properties":{"project_id":{"type":"string"},"query":{"type":"string"},"k":{"type":"integer","minimum":1,"maximum":100},"namespace":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_ruvector_stats",
        description: "Stats correnti del database HNSW (nodi totali, fan-out medio, entry point).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_consensus_vote",
        description: "Valuta voti multi-agente via ConsensusEngine (simple/super/unanimous/weighted).",
        schema: r#"{"type":"object","required":["project_id","votes"],"properties":{"project_id":{"type":"string"},"strategy":{"type":"string","enum":["simple_majority","super_majority","unanimous","weighted_majority"]},"votes":{"type":"array","minItems":1,"items":{"type":"object","required":["agent","approve"],"properties":{"agent":{"type":"string"},"approve":{"type":"boolean"},"confidence":{"type":"number"},"reason":{"type":"string"}}}}}}"#,
    },
    // ── Fase 9F: Utility batch (10) ─────────────────────────────────────
    ToolDef {
        name: "nexus_fs_read",
        description: "Legge un file del project con line range opzionale.",
        schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"start_line":{"type":"integer"},"end_line":{"type":"integer"},"max_bytes":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_fs_list",
        description: "Lista file di una directory del project con filtro regex sul nome.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"dir":{"type":"string"},"pattern":{"type":"string"},"max_results":{"type":"integer"},"recursive":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_fs_grep",
        description: "Regex search ricorsivo nei file sorgente del project.",
        schema: r#"{"type":"object","required":["project_id","pattern"],"properties":{"project_id":{"type":"string"},"pattern":{"type":"string"},"dir":{"type":"string"},"file_glob":{"type":"string"},"max_matches":{"type":"integer"},"case_insensitive":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_fs_tree",
        description: "Albero file del project in JSON con max_depth configurabile.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"dir":{"type":"string"},"max_depth":{"type":"integer"},"max_entries":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_json_parse",
        description: "Valida + pretty-print di una stringa o file JSON.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"content":{"type":"string"},"path":{"type":"string"},"pretty":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_json_get",
        description: "Estrae un valore da JSON via dot-path (es 'users[0].name').",
        schema: r#"{"type":"object","required":["project_id","query"],"properties":{"project_id":{"type":"string"},"json_content":{"type":"string"},"path":{"type":"string"},"query":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_base64_encode",
        description: "Base64 encode di una stringa UTF-8 (standard o url-safe).",
        schema: r#"{"type":"object","required":["project_id","input"],"properties":{"project_id":{"type":"string"},"input":{"type":"string"},"url_safe":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_base64_decode",
        description: "Base64 decode a stringa UTF-8.",
        schema: r#"{"type":"object","required":["project_id","input"],"properties":{"project_id":{"type":"string"},"input":{"type":"string"},"url_safe":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_hash_content",
        description: "SHA-256 / SHA-512 hash di una stringa o file.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"content":{"type":"string"},"path":{"type":"string"},"algo":{"type":"string","enum":["sha256","sha512"]}}}"#,
    },
    ToolDef {
        name: "nexus_uuid_generate",
        description: "Genera UUID v4 in batch (hyphenated o compact).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"count":{"type":"integer"},"hyphenated":{"type":"boolean"}}}"#,
    },
    // ── Fase 9F: VCS batch (4) ──────────────────────────────────────────
    ToolDef {
        name: "nexus_git_branch_list",
        description: "Lista branch git (locali + remoti opzionali) con upstream.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"include_remote":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_git_remote_list",
        description: "Lista remote git con fetch e push URL.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_show",
        description: "`git show <ref>` con parsing commit + numstat file changes.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"ref":{"type":"string"},"stats_only":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_git_tag_list",
        description: "Lista tag git sortati per creator date.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer"}}}"#,
    },
    // ── Fase 9F: GitHub batch (3) ───────────────────────────────────────
    ToolDef {
        name: "nexus_gh_workflow_list",
        description: "`gh workflow list --json` (workflow Actions del repo).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_gh_run_list",
        description: "`gh run list --json` (workflow runs) con count success/failed.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer"},"workflow":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_gh_release_list",
        description: "`gh release list --json`.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer"}}}"#,
    },
    // ── Fase 9F: CodeAnalysis / Quality batch (3) ───────────────────────
    ToolDef {
        name: "nexus_count_loc",
        description: "Conta LOC per linguaggio (walker self-contained).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"dir":{"type":"string"},"max_files":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_find_todos",
        description: "Cerca TODO/FIXME/HACK/XXX nei sorgenti (markers custom opzionali).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"markers":{"type":"array","items":{"type":"string"}},"max_results":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_fmt_check",
        description: "`cargo fmt --all -- --check` con conteggio file da riformattare.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    // ── Fase 9G: Utility batch (8) ──────────────────────────────────────
    ToolDef {
        name: "nexus_fs_write",
        description: "Scrive testo su un file del project (overwrite o append, crea dir intermedie).",
        schema: r#"{"type":"object","required":["project_id","path","content"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"},"content":{"type":"string"},"append":{"type":"boolean"},"create_dirs":{"type":"boolean"},"max_bytes":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_fs_stat",
        description: "Metadata file/dir: size, mtime, type, readonly.",
        schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_fs_glob",
        description: "Glob match (*, ?) ricorsivo nei file del project.",
        schema: r#"{"type":"object","required":["project_id","pattern"],"properties":{"project_id":{"type":"string"},"pattern":{"type":"string"},"dir":{"type":"string"},"max_results":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_env_get",
        description: "Legge variabili d'ambiente (con masking automatico per nomi sensibili).",
        schema: r#"{"type":"object","required":["project_id","names"],"properties":{"project_id":{"type":"string"},"names":{"type":"array","items":{"type":"string"}},"allow_secrets":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_time_now",
        description: "Timestamp UTC corrente (unix, iso8601, rfc3339).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_regex_replace",
        description: "Replace regex su stringa o file (read-only, in-memory).",
        schema: r#"{"type":"object","required":["project_id","pattern","replacement"],"properties":{"project_id":{"type":"string"},"pattern":{"type":"string"},"replacement":{"type":"string"},"content":{"type":"string"},"path":{"type":"string"},"max":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_text_diff",
        description: "Diff line-based LCS tra due testi o file.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"a":{"type":"string"},"b":{"type":"string"},"path_a":{"type":"string"},"path_b":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_uuid_parse",
        description: "Valida e descrive un UUID (versione, variant, hyphenated).",
        schema: r#"{"type":"object","required":["project_id","input"],"properties":{"project_id":{"type":"string"},"input":{"type":"string"}}}"#,
    },
    // ── Fase 9G: VCS batch (4) ──────────────────────────────────────────
    ToolDef {
        name: "nexus_git_stash_list",
        description: "Lista git stash (index, ref, branch, message).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_grep",
        description: "`git grep -n -E` regex search nei file tracked.",
        schema: r#"{"type":"object","required":["project_id","pattern"],"properties":{"project_id":{"type":"string"},"pattern":{"type":"string"},"case_insensitive":{"type":"boolean"},"max_matches":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_git_describe",
        description: "`git describe --tags --long --dirty` parsato in tag/commits/sha.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_git_shortlog",
        description: "`git shortlog -sne` aggregato per autore.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"limit":{"type":"integer"}}}"#,
    },
    // ── Fase 9G: GitHub batch (3) ───────────────────────────────────────
    ToolDef {
        name: "nexus_gh_pr_list",
        description: "`gh pr list --json` (filtri state/base).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"state":{"type":"string","enum":["open","closed","merged","all"]},"limit":{"type":"integer"},"base":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_gh_pr_view",
        description: "`gh pr view <number> --json` dettaglio PR completo.",
        schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_gh_repo_view",
        description: "`gh repo view --json` metadata repository.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    // ── Fase 9G: Cargo / Build batch (3) ────────────────────────────────
    ToolDef {
        name: "nexus_cargo_doc",
        description: "`cargo doc --no-deps` con conteggio HTML generati.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"no_deps":{"type":"boolean"},"private_items":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_locate_project",
        description: "`cargo locate-project` (root + workspace manifest).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "nexus_cargo_pkgid",
        description: "`cargo pkgid` con parsing name+version.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"package":{"type":"string"}}}"#,
    },
    // ── Fase 9G: CodeAnalysis batch (2) ─────────────────────────────────
    ToolDef {
        name: "nexus_find_unsafe",
        description: "Trova `unsafe` blocks/fn/impl nei sorgenti Rust.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"max_results":{"type":"integer"}}}"#,
    },
    ToolDef {
        name: "nexus_find_pubapi",
        description: "Conta `pub` items per file Rust (top-files).",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"top":{"type":"integer"}}}"#,
    },
    // ── Fase 9H: Cargo extras (20) ──────────────────────────────────────
    ToolDef { name: "nexus_cargo_run", description: "`cargo run [--release] [--bin name]` esecuzione.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"release":{"type":"boolean"},"bin":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_install_list", description: "Parse `cargo install --list` in name/version.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_search", description: "`cargo search <query>` (network egress).", schema: r#"{"type":"object","required":["project_id","query"],"properties":{"project_id":{"type":"string"},"query":{"type":"string"},"limit":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_cargo_publish_dry", description: "`cargo publish --dry-run --allow-dirty` rehearsal.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_check_release", description: "`cargo check --release` con conteggi warning/error.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_check_all_features", description: "`cargo check --all-features` con conteggi warning/error.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_test_doc", description: "`cargo test --doc` con passed/failed.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_test_lib", description: "`cargo test --lib` con passed/failed.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_features_list", description: "Parse `[features]` da Cargo.toml root.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_targets_list", description: "Lista target (bin/lib/example/test/bench) via metadata.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_workspace_members", description: "Lista workspace members via `cargo metadata`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_dep_versions", description: "Rileva pacchetti duplicati (stesso name, versioni multiple).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_lockfile_check", description: "Verifica Cargo.lock (presenza, versione, package count).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_msrv_detect", description: "Trova `rust-version` (MSRV) nei manifest del workspace.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_edition_detect", description: "Raggruppa crate del workspace per `edition`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_env_overrides", description: "Legge env var rilevanti per cargo (RUSTFLAGS, ecc.).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_build_artifact_check", description: "Lista binari in target/<profile>/ con dimensioni.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"profile":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_clean_dry", description: "Calcola dimensione di target/ senza rimuoverla.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_size_estimate", description: "Somma size dei binari in target/release.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_cargo_doc_check", description: "`cargo doc --no-deps --quiet` con doc warning/error counts.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    // ── Fase 9I: Git extras (20) ────────────────────────────────────────
    ToolDef { name: "nexus_git_rev_parse", description: "`git rev-parse <ref>` ref → SHA.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"ref":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_count_objects", description: "`git count-objects -v` repo size info.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_reflog", description: "`git reflog -n N` reference log.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"n":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_git_clean_dry", description: "`git clean -nd` dry-run (lista file rimuovibili).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_check_ignore", description: "`git check-ignore -v` per i path forniti.", schema: r#"{"type":"object","required":["project_id","paths"],"properties":{"project_id":{"type":"string"},"paths":{"type":"array","items":{"type":"string"}}}}"# },
    ToolDef { name: "nexus_git_ls_files", description: "`git ls-files` lista file tracciati.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"max":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_git_ls_tree", description: "`git ls-tree -r <ref>` lista file in commit.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"ref":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_cat_file", description: "`git cat-file -p <ref>` object content (preview).", schema: r#"{"type":"object","required":["project_id","ref"],"properties":{"project_id":{"type":"string"},"ref":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_for_each_ref", description: "`git for-each-ref` enumera tutte le ref.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_merge_base", description: "`git merge-base a b` common ancestor.", schema: r#"{"type":"object","required":["project_id","a","b"],"properties":{"project_id":{"type":"string"},"a":{"type":"string"},"b":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_diff_stat", description: "`git diff --shortstat <range>` summary.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"range":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_log_graph", description: "`git log --oneline --graph -n N`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"n":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_git_show_branch", description: "`git show-branch --all` overview.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_archive_dry", description: "Stima dimensione `git archive` senza scriverlo.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"ref":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_bundle_verify", description: "`git bundle verify <path>`.", schema: r#"{"type":"object","required":["project_id","path"],"properties":{"project_id":{"type":"string"},"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_fsck", description: "`git fsck --no-progress` repo integrity.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_gc_dry", description: "Verifica se `git gc` è necessario (loose threshold).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_config_list", description: "`git config --list --local` (valori sensibili mascherati).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_worktree_list", description: "`git worktree list --porcelain`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_git_submodule_list", description: "`git submodule status` lista submodule.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    // ── Fase 9J: GitHub extras (20) ─────────────────────────────────────
    ToolDef { name: "nexus_gh_issue_view", description: "`gh issue view <num> --json` dettaglio issue.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_issue_create", description: "`gh issue create --title --body`.", schema: r#"{"type":"object","required":["project_id","title"],"properties":{"project_id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_issue_close", description: "`gh issue close <num>`.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_issue_comment", description: "`gh issue comment <num> --body`.", schema: r#"{"type":"object","required":["project_id","number","body"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"},"body":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_pr_close", description: "`gh pr close <num>`.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_pr_merge", description: "`gh pr merge <num> --squash|--merge|--rebase`.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"},"strategy":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_pr_review", description: "`gh pr review <num>` approve/request-changes/comment.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"},"action":{"type":"string"},"body":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_pr_diff", description: "`gh pr diff <num>` con conteggio +/-.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_pr_checks", description: "`gh pr checks <num>` pass/fail/pending.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_pr_files", description: "`gh pr view <num> --json files` lista file changed.", schema: r#"{"type":"object","required":["project_id","number"],"properties":{"project_id":{"type":"string"},"number":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_workflow_view", description: "`gh workflow view <name>`.", schema: r#"{"type":"object","required":["project_id","name"],"properties":{"project_id":{"type":"string"},"name":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_workflow_run", description: "`gh workflow run <name> --ref`.", schema: r#"{"type":"object","required":["project_id","name"],"properties":{"project_id":{"type":"string"},"name":{"type":"string"},"ref":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_run_view", description: "`gh run view <id> --json`.", schema: r#"{"type":"object","required":["project_id","id"],"properties":{"project_id":{"type":"string"},"id":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_run_logs", description: "`gh run view <id> --log`.", schema: r#"{"type":"object","required":["project_id","id"],"properties":{"project_id":{"type":"string"},"id":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_run_cancel", description: "`gh run cancel <id>`.", schema: r#"{"type":"object","required":["project_id","id"],"properties":{"project_id":{"type":"string"},"id":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_gh_release_view", description: "`gh release view <tag> --json`.", schema: r#"{"type":"object","required":["project_id","tag"],"properties":{"project_id":{"type":"string"},"tag":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_release_create", description: "`gh release create <tag> --title --notes`.", schema: r#"{"type":"object","required":["project_id","tag"],"properties":{"project_id":{"type":"string"},"tag":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"},"draft":{"type":"boolean"}}}"# },
    ToolDef { name: "nexus_gh_repo_clone_url", description: "`gh repo view --json url,sshUrl`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_repo_fork_list", description: "`gh repo view --json forkCount,parent`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_gh_label_list", description: "`gh label list --json`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },

    // ── Fase 9K: Database extras (20) ───────────────────────────────────
    ToolDef { name: "nexus_db_ping", description: "SELECT 1 connectivity test against DATABASE_URL.", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_table_list", description: "List tables in a schema (default public).", schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_table_count", description: "SELECT COUNT(*) for a specific table.", schema: r#"{"type":"object","required":["table"],"properties":{"schema":{"type":"string"},"table":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_index_list", description: "List indexes in a schema from pg_indexes.", schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_view_list", description: "List views in a schema from pg_views.", schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_role_list", description: "List roles from pg_roles.", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_extension_list", description: "List installed extensions from pg_extension.", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_size", description: "Total size of the current database (pg_database_size).", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_connection_info", description: "Current connection info (user, db, host, version).", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_migration_list", description: "List .sql migration files under db/migrations or migrations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_seq_list", description: "List sequences in a schema.", schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_foreign_keys", description: "List foreign keys in a schema with referenced table/column.", schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_unused_indexes", description: "Indexes never scanned (idx_scan = 0) from pg_stat_user_indexes.", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_dead_tuples", description: "Top tables by dead tuples from pg_stat_user_tables.", schema: r#"{"type":"object","properties":{"limit":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_db_bloat_check", description: "Quick bloat estimate via dead/live ratio.", schema: r#"{"type":"object","properties":{"limit":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_db_table_size", description: "Total + heap size for a specific table.", schema: r#"{"type":"object","required":["table"],"properties":{"schema":{"type":"string"},"table":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_constraint_list", description: "List constraints in a schema with type.", schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"# },
    ToolDef { name: "nexus_db_lock_list", description: "Active locks from pg_locks joined with pg_stat_activity.", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_active_queries", description: "Non-idle queries from pg_stat_activity.", schema: r#"{"type":"object","properties":{}}"# },
    ToolDef { name: "nexus_db_replication_status", description: "Replication status from pg_stat_replication.", schema: r#"{"type":"object","properties":{}}"# },

    // ── Fase 9L: Documentation extras (20) ──────────────────────────────
    ToolDef { name: "nexus_doc_readme_check", description: "Check README.md presence and minimal sections.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_changelog_check", description: "Check CHANGELOG.md presence and release count.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_license_detect", description: "Detect LICENSE file and license type.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_codeowners_check", description: "Check CODEOWNERS file presence.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_contributing_check", description: "Check CONTRIBUTING.md presence.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_security_md_check", description: "Check SECURITY.md presence with contact/disclosure.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_toc_extract", description: "Extract markdown headings (table of contents).", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_links_extract", description: "Extract markdown links from a file.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_word_count", description: "Count words/lines/chars in a markdown file.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_link_check_local", description: "Check that local links in a .md exist on disk.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_image_list", description: "List images referenced from a .md.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_frontmatter_parse", description: "Parse YAML frontmatter from a .md.", schema: r#"{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_md_lint", description: "Basic markdown lint (long lines, trailing spaces, tabs).", schema: r#"{"type":"object","properties":{"path":{"type":"string"},"max_line":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_doc_orphan_md", description: "Markdown files not referenced from README.md.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_size_report", description: "Total .md file count and bytes in project.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_heading_depth", description: "Max heading depth and per-level distribution.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_codeblocks_extract", description: "Extract fenced code blocks with language.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_codeblocks_count", description: "Count fenced code blocks per language.", schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_api_list", description: "List .md files under docs/api.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_doc_examples_list", description: "List entries under examples/.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },

    // ── Fase 9M: Performance extras (20) ────────────────────────────────
    ToolDef { name: "nexus_perf_cargo_build_time", description: "Run `cargo build --timings`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_binary_size", description: "Sizes of binaries in target/release.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_cargo_bloat", description: "`cargo bloat --release --crates -n 20`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_target_dir_size", description: "Total size of target/.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_largest_files", description: "Top N .rs files by size.", schema: r#"{"type":"object","properties":{"limit":{"type":"integer"}}}"# },
    ToolDef { name: "nexus_perf_loc_per_crate", description: "LOC per workspace crate.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_unused_deps", description: "Heuristic unused deps in Cargo.toml.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_test_count", description: "Count #[test] / #[tokio::test].", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_async_funcs", description: "Count `async fn` and `.await`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_unsafe_blocks", description: "Count `unsafe {`, `unsafe fn`, `unsafe impl`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_panic_count", description: "Count `panic!`/`unwrap`/`expect`/`todo!`/`unimplemented!`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_clone_count", description: "Count `.clone()` and `.to_owned()`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_string_alloc", description: "Count String allocation patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_box_count", description: "Count `Box<dyn` and `Box::new`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_arc_mutex", description: "Count Arc<Mutex/RwLock> patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_dep_count", description: "Count deps in Cargo.toml sections.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_compile_units", description: "Workspace package count via cargo metadata.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_optimization_check", description: "Inspect [profile.release] keys.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_lto_check", description: "Check LTO in [profile.release].", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_perf_codegen_units", description: "Check codegen-units in [profile.release].", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },

    // ── Fase 9N: Testing extras (20) ────────────────────────────────────
    ToolDef { name: "nexus_test_run_unit", description: "Run `cargo test --lib --quiet`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_run_integration", description: "Run `cargo test --tests --quiet`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_run_quiet", description: "Run `cargo test --quiet` with optional filter.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"},"filter":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_run_workspace", description: "Run `cargo test --workspace --quiet`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_count_files", description: "Count *_test.rs and tests/*.rs files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_ignored_count", description: "Count `#[ignore]` attributes.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_should_panic_count", description: "Count `#[should_panic` attributes.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_module_count", description: "Count test modules / `#[cfg(test)]`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_assert_count", description: "Count assert!/assert_eq!/debug_assert.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_proptest_count", description: "Count proptest! / prop_assert.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_quickcheck_count", description: "Count #[quickcheck] / quickcheck!.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_mock_count", description: "Count mockall/wiremock usages.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_bench_count", description: "Count #[bench]/criterion macros.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_doc_count", description: "Count doctest fences in /// comments.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_fixtures_list", description: "List entries under tests/fixtures/.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_snapshots_list", description: "Walk for `.snap` files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_stale_snapshots", description: "Walk for `.snap.new` files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_coverage_summary", description: "Check for cobertura/lcov reports.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_failed_log", description: "Run `cargo test --no-run` to validate test compilation.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_test_workflow_files", description: "List .github/workflows/*.yml with test mentions.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },

    // ── Fase 9O: Security extras (20) ───────────────────────────────────
    ToolDef { name: "nexus_sec_secret_patterns", description: "Heuristic scan for hardcoded secrets.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_unwrap_count", description: "Count `.unwrap()` and `.expect(`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_panic_count", description: "Count panic-inducing macros.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_env_var_check", description: "Count std::env::var and fallbacks.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_http_url_count", description: "Count plaintext http:// vs https://.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_localhost_count", description: "Count localhost / loopback references.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_eval_check", description: "Heuristic scan for eval-like patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_sql_injection_check", description: "Find string interpolation in SQL queries.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_cmd_injection_check", description: "Find Command::new + shell -c patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_dependency_count", description: "Count deps across all Cargo.toml.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_git_secrets_check", description: "Scan .git/config for credentials.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_env_files_check", description: "Find .env* files / .gitignore coverage.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_dockerfile_user_check", description: "Check Dockerfile USER directive.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_workflow_perms_check", description: "Check workflows for permissions: blocks.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_cors_check", description: "Find permissive CORS patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_jwt_secret_check", description: "Find hardcoded JWT secrets / weak algos.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_md5_sha1_check", description: "Find weak hash algorithms.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_random_check", description: "Find non-secure RNG usage.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_tls_check", description: "Find TLS verify=false patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_sec_audit_summary", description: "High-level security audit overview.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },

    // ── Fase 9P: Code Analysis extras (20) ──────────────────────────────
    ToolDef { name: "nexus_ca_struct_count", description: "Count struct declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_enum_count", description: "Count enum declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_trait_count", description: "Count trait declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_impl_count", description: "Count impl blocks.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_fn_count", description: "Count fn declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_pub_fn_count", description: "Count public function declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_macro_count", description: "Count macro definitions.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_use_count", description: "Count `use` statements.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_mod_count", description: "Count module declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_lifetime_count", description: "Count lifetime annotations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_generic_count", description: "Count generic param usage.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_derive_count", description: "Count derive macros.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_attr_count", description: "Count common attribute macros.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_doc_comment_count", description: "Count doc comments.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_inline_comment_count", description: "Count inline comments.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_todo_fixme_count", description: "Count TODO/FIXME markers.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_match_count", description: "Count match expressions.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_if_let_count", description: "Count if let patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_while_let_count", description: "Count loop constructs.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_ca_complexity_estimate", description: "Heuristic cyclomatic complexity.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },

    // ── Fase 9Q: Build / Deploy (21) ────────────────────────────────────
    ToolDef { name: "nexus_build_target_list", description: "List subdirectories under target/.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_artifact_age", description: "Newest mtime under target/release.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_release_size", description: "Sum binary sizes in target/release.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_debug_size", description: "Sum binary sizes in target/debug.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_incremental_dir", description: "Check incremental directory.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_lockfile_age", description: "Mtime/size of Cargo.lock.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_log_tail", description: "Tail .rustc_info.json / fingerprint logs.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_rerun_checks", description: "Count cargo:rerun-if- directives.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_script_count", description: "Count build.rs files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_workspace_check", description: "`cargo check --workspace --quiet`.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_build_profile_list", description: "List [profile.*] in root Cargo.toml.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_dockerfile_count", description: "Count Dockerfile* files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_compose_check", description: "Find docker-compose files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_k8s_check", description: "Find kubernetes manifests.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_helm_check", description: "Find Chart.yaml/values.yaml.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_terraform_check", description: "Find *.tf and tfstate files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_ansible_check", description: "Find ansible playbooks/configs.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_systemd_check", description: "Find systemd unit files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_nginx_check", description: "Find nginx*.conf files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_env_files_count", description: "Count .env / .envrc files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_deploy_release_artifacts", description: "List release artifact dirs.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    // Fase 9R — API / Memory / Other (20)
    ToolDef { name: "nexus_api_openapi_files", description: "Find openapi/swagger spec files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_route_count", description: "Count axum/actix/warp/rocket route declarations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_handler_count", description: "Count async fn handlers (heuristic).", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_endpoint_list", description: "Extract endpoint paths from .route() literals.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_graphql_check", description: "Detect GraphQL schemas/usages.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_grpc_check", description: "Detect gRPC/.proto/tonic usages.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_postman_check", description: "Find postman collection files.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_api_middleware_count", description: "Count tower/axum middleware layer registrations.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_memory_namespace_count", description: "Count distinct memory namespaces in DB.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_memory_size_estimate", description: "Estimate aggregate memory_namespace size.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_memory_pattern_list", description: "List distinct memory keys/patterns.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_memory_recent_writes", description: "Recent memory_namespace writes.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_memory_topkeys", description: "Top namespaces by row count.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_memory_evict_stats", description: "Evictable rows older than TTL.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_util_disk_free", description: "Best-effort disk info at project_root.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_util_uptime", description: "Process uptime in seconds since first call.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_util_hostname", description: "Hostname/user from environment.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_util_cpu_count", description: "Logical CPU count via available_parallelism.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_util_now_iso", description: "Current time as RFC3339 + epoch seconds.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_util_pid", description: "Process id of running mcp-core.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    // Fase 9S — Final meta tools (5)
    ToolDef { name: "nexus_meta_catalog_count", description: "Total + implemented tool counts in catalog.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_meta_categories_list", description: "List all NexusToolCategory variants with counts.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_meta_version_info", description: "Crate name/version + profile + os/arch.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_meta_health_summary", description: "Basic health: project_root, db, catalog.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    ToolDef { name: "nexus_meta_self_test", description: "Smoke-test a small set of read-only handlers.", schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}"# },
    // ── System: Shell exec ──────────────────────────────────────────────────
    // ── service_control ────────────────────────────────────────────────────
    ToolDef {
        name: "nexus_service_status",
        description: "Elenca tutti i servizi systemd associati al progetto e il loro stato (active/inactive/failed/…). Il progetto può avere N servizi, non è limitato a tre.",
        schema: r#"{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"}}}"#,
    },
    ToolDef {
        name: "nexus_service_control",
        description: "Avvia, ferma o riavvia un servizio systemd del progetto. Il parametro 'service' è il nome corto del servizio (es. 'api', 'worker-email', 'frontend-admin') senza prefisso progetto né suffisso .service. Usa nexus_service_status prima per conoscere i nomi disponibili.",
        schema: r#"{"type":"object","required":["project_id","service","action"],"properties":{"project_id":{"type":"string","description":"UUID del progetto"},"service":{"type":"string","description":"Nome corto del servizio (es. 'api', 'worker', 'frontend-admin')"},"action":{"type":"string","enum":["start","stop","restart"],"description":"Azione da eseguire"}}}"#,
    },
    ToolDef {
        name: "nexus_shell_exec",
        description: "Usa questo tool per installare runtime e pacchetti (dotnet, apt, npm, pip, curl), eseguire script bash, comandi di sistema arbitrari. Ideale per setup ambienti, installazione .NET SDK, Node.js, strumenti CLI. Timeout configurabile fino a 600s per installazioni lente.",
        schema: r#"{"type":"object","required":["command"],"properties":{"command":{"type":"string"},"project_id":{"type":"string"},"timeout_secs":{"type":"integer"}}}"#,
    },
    // -- database (provisioning + esecuzione, collegati al pannello Database) --
    ToolDef {
        name: "nexus_db_provision",
        description: "Crea/registra un database per il progetto. mode=internal (default) provisiona un Postgres dedicato gestito da Nexus SENZA chiedere host/porta/credenziali. mode=external registra un DB esistente data una connection_string. Usa questo tool quando l utente chiede di creare/configurare un database: NON chiedere credenziali per mode=internal.",
        schema: r#"{"type":"object","properties":{"mode":{"type":"string","enum":["internal","external"],"description":"internal (default) o external"},"name":{"type":"string","description":"Nome logico della connessione (default primary)"},"db_name":{"type":"string","description":"Nome del database fisico (solo internal)"},"connection_string":{"type":"string","description":"Connection string (richiesta solo per mode=external)"}}}"#,
    },
    ToolDef {
        name: "nexus_db_execute_sql",
        description: "Esegue SQL (DDL o DML) sul database applicativo del progetto. Le DDL vengono archiviate automaticamente come nota KB + file migration versionato. Usa per creare tabelle, indici, inserire dati. La connessione e risolta dal pannello Database del progetto.",
        schema: r#"{"type":"object","required":["sql"],"properties":{"sql":{"type":"string","description":"Statement SQL (CREATE TABLE, ALTER, INSERT, ...)"},"connection":{"type":"string","description":"Nome connessione DB (default primary)"}}}"#,
    },
    ToolDef {
        name: "nexus_db_apply_schema_file",
        description: "Importa lo schema da un file SQL del progetto (es. backend/db_schema.sql, schema.sql, migrations/*.sql) ed esegue il contenuto sul DB del progetto. Se file_path manca cerca candidati comuni; se ambiguo ritorna la lista per farti scegliere. Preferisci questo tool quando esiste gia un file schema nel repo.",
        schema: r#"{"type":"object","properties":{"file_path":{"type":"string","description":"Percorso del file SQL relativo alla root (opzionale)"},"connection":{"type":"string","description":"Nome connessione DB (default primary)"}}}"#,
    },
    ToolDef {
        name: "nexus_db_status",
        description: "Ritorna le connessioni database configurate per il progetto e, per la connessione primaria, la lista delle tabelle. Usa per capire lo stato del DB prima di provisionare o creare tabelle.",
        schema: r#"{"type":"object","properties":{}}"#,
    },
    // -- alias per nomi tool comunemente allucinati dall agente (regola H: instradano ai canonici) --
    ToolDef {
        name: "nexus_db_query",
        description: "Esegue una query/statement SQL (SELECT, INSERT, UPDATE, DELETE, DDL) sul database del progetto. Alias di nexus_db_execute_sql.",
        schema: r#"{"type":"object","required":["sql"],"properties":{"sql":{"type":"string","description":"Statement SQL (SELECT, INSERT, UPDATE, DELETE, DDL)"},"connection":{"type":"string","description":"Nome connessione DB (default primary)"}}}"#,
    },
    ToolDef {
        name: "nexus_db_tables",
        description: "Elenca le tabelle del database del progetto. Alias di nexus_db_table_list.",
        schema: r#"{"type":"object","properties":{"schema":{"type":"string"}}}"#,
    },
    // ── ADR 0020: build graph derivato dai config ────────────────────────
    ToolDef {
        name: "nexus_build_graph_info",
        description: "Ritorna la mappa autoritativa del build graph del progetto, derivata dai file di configurazione (tsconfig.json, Cargo.toml, pyproject.toml, go.mod). Output: language, include_globs[], exclude_globs[], entry_points[], monorepo_members[], generated_dirs[], sources[]. USA QUESTO STRUMENTO prima di modificare un file di codice (.ts, .tsx, .rs, .py, .go) quando hai dubbi su quale path sia 'quello vero' del progetto (es. file con stesso nome in src/ e in figma_export/).",
        schema: r#"{"type":"object","properties":{"project_id":{"type":"string","description":"UUID del progetto (opzionale: se assente usa il progetto del contesto)"}}}"#,
    },
];
