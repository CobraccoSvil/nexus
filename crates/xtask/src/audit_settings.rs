//! audit-settings — censimento configurazioni `settings`: DB live + migrazioni
//! vs lettori nel codice vs UI. Porting 1:1 di scripts/audit_settings.py.
//!
//! Punto unico (regola L) per l'audit "ogni setting esposta in admin e' davvero
//! letta dal codice". Quattro collettori:
//!
//!   A1. DB live          — SELECT key, category FROM settings (sqlx via DATABASE_URL)
//!   A2. Migrazioni       — parser di INSERT/DELETE su db/migrations/*.sql
//!   B.  Lettori codice   — regex sulle API punto-unico (nexus-auth / settings_db)
//!                          + SQL diretto + pattern dinamici whitelistati
//!   C.  UI admin         — categorie navigabili dalla sidebar (admin-sidebar.tsx)
//!
//! Classificazione per chiave del DB live:
//!   VIVA        in DB + letta dal codice (literal, prefisso, wildcard o categoria)
//!   MORTA       in DB + nessun lettore trovato
//!   FANTASMA    letta literal nel codice ma assente dal DB live
//!   INVISIBILE  in DB + letta, ma categoria non raggiungibile dalla UI admin
//!   RUNTIME     in DB, non nelle migrazioni, scritta da codice noto (whitelist)
//!   TEST-ONLY   letta solo da file di test
//!
//! Uso (wrapper: scripts/audit-settings.sh):
//!   cargo xtask audit-settings --report          # tabella riassuntiva
//!   cargo xtask audit-settings --json out.json   # dump completo
//!   cargo xtask audit-settings --no-db           # senza DB (solo A2 vs B)
//!   cargo xtask audit-settings --gate            # exit!=0 su regressioni vs baseline

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Pattern dinamici noti (whitelist con motivazione). Una chiave del DB che
// matcha uno di questi pattern conta come LETTA anche senza literal nel codice.
// Tenere allineata ai call site citati nel commento.
// ---------------------------------------------------------------------------
const DYNAMIC_READ_PATTERNS: &[(&str, &str)] = &[
    // environment.rs:738,1203 + model_catalog_sync.rs (format!("{}_api_key", provider))
    // + brain/providers/api_key_loader.py
    (r".*_api_key$", "chiavi API provider lette per pattern <provider>_api_key"),
    // playwright_install.rs:395 — chiave per-progetto creata/letta a runtime
    (
        r"^project:[0-9a-f-]+:playwright_enabled$",
        "flag playwright per-progetto",
    ),
    // brain/agents/nodes/helpers.py:86,147,2722 — prefissi LIKE
    (r"^agent\.iteration_budget\..*", "LIKE agent.iteration_budget.% (helpers.py)"),
    (r"^agent\.complexity\..*", "LIKE agent.complexity.% (helpers.py)"),
    (r"^agent\.tier_floor\..*", "LIKE agent.tier_floor.% (helpers.py)"),
    (r"^agent\.context\..*", "LIKE agent.context.% (helpers.py)"),
];

// Categorie lette PER INTERO dal codice (SELECT ... WHERE category = '<cat>').
// Call site: brain/agents/meta_steps.py:63, clarify_or_expand_node.py:117,
// brain/agents/orchestrator_config.py, environment.rs:738 (providers).
const CATEGORY_BULK_READERS: &[(&str, &str)] = &[
    (
        "orchestrator",
        "meta_steps.py / clarify_or_expand_node.py / orchestrator_config.py",
    ),
    ("providers", "environment.rs (api keys per categoria)"),
];

// Chiavi scritte a runtime da codice (non da migrazione): non sono morte.
const RUNTIME_WRITTEN_KEYS: &[(&str, &str)] = &[
    (r"^model_catalog_last_sync$", "models.rs:341 (timestamp sync)"),
    (r"^project:.*", "playwright_install.rs (chiavi per-progetto)"),
    (r"^jwt_secret$", "nexus-auth get_or_create_jwt_secret"),
];

// Eccezioni deliberate: nessun lettore nel codice ma MANTENUTE per contratto
// (verifica adversariale audit 2026-06-11, vedi ADR 0031). Non contano come
// morte nel gate.
const KEEP_DESPITE_NO_READER: &[&str] = &[
    "agent.visual_compare.similarity_threshold",
    "gitlab_personal_access_token",
];

const EXCLUDE_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".next",
    "__pycache__",
    ".git",
    ".turbo",
    "generated",
    "dist",
    "build",
    ".dup-report",
    "recovery",
    ".venv",
];

/// Radice del repo: la cwd e' garantita dal wrapper audit-settings.sh (cd ROOT).
fn repo_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// ---------------------------------------------------------------------------
// A1 — DB live
// ---------------------------------------------------------------------------
/// Ritorna le righe (key, category) NELL'ORDINE restituito da `ORDER BY key`
/// di Postgres (collation del DB, non byte-order). L'ordine va preservato:
/// lo script Python itera `db_live.items()` di un dict che conserva l'ordine
/// d'inserimento, e popola le classi in quell'ordine -> il JSON delle classi
/// riflette la collation Postgres, non l'ordinamento lessicografico Rust.
/// Esegue la SELECT su `settings` via sqlx contro DATABASE_URL e ritorna le
/// righe `(key, category-nullable)` nell'ordine `ORDER BY key`, o None se il DB
/// non e' raggiungibile (-> il chiamante degrada a --no-db).
///
/// Connection string canonica dal .env (regola G: unica fonte di verita', niente
/// docker exec hardcoded). Su Windows nativo il meta-DB e' il servizio Postgres
/// locale, la stessa DATABASE_URL che usa mcp-core. Se .env manca o DATABASE_URL
/// non e' definita -> None (rete di sicurezza preservata).
fn query_settings_live() -> Option<Vec<(String, Option<String>)>> {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").ok()?;

    // sqlx e' async: runtime current-thread effimero (questo collettore gira una
    // sola volta per invocazione, niente pool da tenere vivo). Qualunque errore
    // di costruzione/connessione/query collassa a None -> degrado a --no-db.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    // SELECT identica al vecchio `psql -c`: key + category, ORDER BY key (la
    // collation del DB definisce l'ordine, vedi doc sopra). `category` puo' essere
    // NULL -> Option, mappata a "" dal chiamante come psql in modalita' -t -A.
    rt.block_on(async {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(&database_url)
            .await
            .ok()?;
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT key, category FROM settings ORDER BY key",
        )
        .fetch_all(&pool)
        .await
        .ok();
        pool.close().await;
        rows
    })
}

/// Le chiavi che il CATALOGO DEI SERVIZI dichiara come `port_setting_key`.
///
/// Sono lette a runtime da `nexus_service_catalog::resolve_port`, che fa
/// `get_setting_checked(db, key)` con la chiave presa dal catalogo (regola G):
/// nel codice non compare alcun literal, quindi il censimento testuale le
/// dichiarerebbe MORTE pur essendo vive — e cancellarle romperebbe la
/// risoluzione della porta del servizio (`qdrant_port` e' il caso che ha fatto
/// arrossire il gate).
///
/// Si LEGGONO dal catalogo invece di elencarle in `DYNAMIC_READ_PATTERNS`: una
/// lista scritta a mano divergerebbe al primo servizio nuovo, che e' esattamente
/// il difetto che i manifest derivati dal catalogo hanno eliminato. Cosi' ogni
/// voce futura con `port_setting_key` e' coperta senza toccare questo file.
///
/// Degrada a vuoto se il DB non e' raggiungibile (modalita' `--no-db`): la
/// classificazione resta quella testuale, come per tutto il resto.
fn port_setting_keys_dal_catalogo() -> HashSet<String> {
    let mut out = HashSet::new();
    dotenvy::dotenv().ok();
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return out;
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return out;
    };
    let raw: Option<String> = rt.block_on(async {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(&database_url)
            .await
            .ok()?;
        let v = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'system.services_catalog'",
        )
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();
        pool.close().await;
        v
    });
    let Some(raw) = raw else { return out };
    if let Ok(Value::Array(voci)) = serde_json::from_str::<Value>(&raw) {
        for v in voci {
            if let Some(k) = v.get("port_setting_key").and_then(Value::as_str) {
                out.insert(k.to_string());
            }
        }
    }
    out
}

fn collect_db_live() -> Option<Vec<(String, String)>> {
    let raw = query_settings_live()?;
    // Python: rows[key] = cat su un dict -> ultima categoria vince ma la
    // posizione e' quella della prima occorrenza della chiave. Con ORDER BY key
    // le chiavi sono uniche, ma replichiamo comunque la semantica del dict.
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (key, cat) in raw {
        let cat = cat.unwrap_or_default();
        if let Some(&pos) = seen.get(&key) {
            rows[pos].1 = cat;
        } else {
            seen.insert(key.clone(), rows.len());
            rows.push((key, cat));
        }
    }
    if rows.is_empty() { None } else { Some(rows) }
}

// ---------------------------------------------------------------------------
// A2 — Migrazioni: INSERT INTO settings / DELETE FROM settings
// ---------------------------------------------------------------------------
/// Stato dell'automa che spezza un body di VALUES in tuple, rispettando apici
/// e parentesi annidate. `step` consuma un carattere e ritorna quanti caratteri
/// avanzare (1 di norma, 2 sull'apice escapato ''), come i `i += ...`/`continue`
/// del porting Python originale.
#[derive(Default)]
struct TupleSplitter {
    tuples: Vec<Vec<String>>,
    depth: i32,
    in_str: bool,
    cur: String,
    fields: Vec<String>,
}

impl TupleSplitter {
    /// Consuma `chars[i]` (con `chars[i+1]` per la look-ahead) e ritorna il passo
    /// di avanzamento dell'indice.
    fn step(&mut self, chars: &[char], i: usize) -> usize {
        let ch = chars[i];
        if self.in_str {
            if ch == '\'' {
                if i + 1 < chars.len() && chars[i + 1] == '\'' {
                    // apice escapato ''
                    self.cur.push('\'');
                    return 2;
                }
                self.in_str = false;
            } else {
                self.cur.push(ch);
            }
        } else if ch == '\'' {
            self.in_str = true;
        } else if ch == '(' {
            self.depth += 1;
            if self.depth == 1 {
                self.fields = Vec::new();
                self.cur = String::new();
                return 1;
            }
            self.cur.push(ch);
        } else if ch == ')' {
            self.depth -= 1;
            if self.depth == 0 {
                self.fields.push(self.cur.trim().to_string());
                self.tuples.push(std::mem::take(&mut self.fields));
                self.cur = String::new();
                return 1;
            }
            self.cur.push(ch);
        } else if ch == ',' && self.depth == 1 {
            self.fields.push(self.cur.trim().to_string());
            self.cur = String::new();
        } else if self.depth >= 1 {
            self.cur.push(ch);
        }
        1
    }
}

/// Spezza il body di un VALUES in tuple, rispettando apici e parentesi.
/// Porting fedele di _split_sql_tuples (Python).
fn split_sql_tuples(body: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = body.chars().collect();
    let mut sp = TupleSplitter::default();
    let mut i = 0usize;
    while i < chars.len() {
        i += sp.step(&chars, i);
    }
    sp.tuples
}

/// Elenca i file `*.sql` in `mig_dir` ordinati per nome (come il Python
/// `sorted(mig_dir.glob("*.sql"))`, ordine lessicografico).
fn list_migration_files(mig_dir: &Path) -> Vec<PathBuf> {
    let mut sql_files: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(mig_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("sql") {
                sql_files.push(p);
            }
        }
    }
    sql_files.sort();
    sql_files
}

/// Processa gli `INSERT INTO settings (...) VALUES (...)` di un file di
/// migrazione, aggiornando `inserted` e rimuovendo le chiavi da `deleted`.
fn apply_insert_statements(
    ins_re: &Regex,
    text: &str,
    fname: &str,
    inserted: &mut BTreeMap<String, (String, String)>,
    deleted: &mut BTreeSet<String>,
) {
    for m in ins_re.captures_iter(text) {
        let cols_raw = m.get(1).map(|g| g.as_str()).unwrap_or_default();
        let cols: Vec<String> = cols_raw
            .split(',')
            .map(|c| c.trim().to_lowercase())
            .collect();
        let Some(key_idx) = cols.iter().position(|c| c == "key") else {
            continue;
        };
        let cat_idx = cols.iter().position(|c| c == "category");
        let values_body = m.get(2).map(|g| g.as_str()).unwrap_or_default();
        for tup in split_sql_tuples(values_body) {
            if key_idx >= tup.len() {
                continue;
            }
            let key = &tup[key_idx];
            if key.is_empty() || key.to_uppercase().contains("SELECT") || key.contains("||") {
                // INSERT..SELECT o chiave costruita: fuori scope
                continue;
            }
            let cat = match cat_idx {
                Some(ci) if ci < tup.len() => tup[ci].clone(),
                _ => String::new(),
            };
            inserted.insert(key.clone(), (cat, fname.to_string()));
            deleted.remove(key);
        }
    }
}

/// Processa i `DELETE FROM settings WHERE key = '...'` e `... key IN (...)` di un
/// file di migrazione, aggiornando `deleted` e rimuovendo le chiavi da `inserted`.
fn apply_delete_statements(
    del_eq_re: &Regex,
    del_in_re: &Regex,
    text: &str,
    inserted: &mut BTreeMap<String, (String, String)>,
    deleted: &mut BTreeSet<String>,
) {
    for m in del_eq_re.captures_iter(text) {
        let k = m.get(1).unwrap().as_str().to_string();
        deleted.insert(k.clone());
        inserted.remove(&k);
    }
    for m in del_in_re.captures_iter(text) {
        let body = m.get(1).unwrap().as_str();
        for raw in body.split(',') {
            let k = raw.trim().trim_matches('\'').to_string();
            if !k.is_empty() {
                deleted.insert(k.clone());
                inserted.remove(&k);
            }
        }
    }
}

/// Ritorna (chiavi inserite -> (categoria, file)), chiavi cancellate.
fn collect_migrations() -> Result<(BTreeMap<String, (String, String)>, BTreeSet<String>)> {
    let mut inserted: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut deleted: BTreeSet<String> = BTreeSet::new();
    let mig_dir = repo_root().join("db").join("migrations");

    let ins_re = Regex::new(
        r"(?is)INSERT\s+INTO\s+settings\s*\(([^)]*)\)\s*VALUES\s*(.*?);",
    )?;
    let del_eq_re =
        Regex::new(r"(?i)DELETE\s+FROM\s+settings\s+WHERE\s+key\s*=\s*'([^']+)'")?;
    let del_in_re =
        Regex::new(r"(?i)DELETE\s+FROM\s+settings\s+WHERE\s+key\s+IN\s*\(([^)]*)\)")?;

    for sql_file in &list_migration_files(&mig_dir) {
        let text = read_text(sql_file);
        let fname = sql_file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        apply_insert_statements(&ins_re, &text, &fname, &mut inserted, &mut deleted);
        apply_delete_statements(&del_eq_re, &del_in_re, &text, &mut inserted, &mut deleted);
    }
    Ok((inserted, deleted))
}

// ---------------------------------------------------------------------------
// B — Lettori nel codice
// ---------------------------------------------------------------------------
fn is_test_path(p: &Path) -> bool {
    let s = p.to_string_lossy().replace('\\', "/");
    s.contains("/tests/")
        || s.contains("/test_")
        || s.ends_with("_test.rs")
        || s.contains("/__tests__/")
        || s.contains(".test.")
        || s.contains(".spec.")
}

fn read_text(p: &Path) -> String {
    match std::fs::read(p) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => String::new(),
    }
}

/// Conta i `\n` nei byte da 0 a `byte_off` (escluso) e somma 1, come
/// `text.count("\n", 0, m.start()) + 1` in Python (m.start() e' un offset byte).
fn line_at(text: &str, byte_off: usize) -> usize {
    text.as_bytes()[..byte_off].iter().filter(|&&b| b == b'\n').count() + 1
}

/// Walk top-down identico a os.walk(): per ogni dir, prima i file (nell'ordine
/// di read_dir), poi ricorsione nelle sottodir (stesso ordine), escludendo
/// EXCLUDE_DIRS. read_dir su Linux usa readdir come os.scandir -> stesso ordine.
fn walk_files(root: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !EXCLUDE_DIRS.contains(&name.as_str()) {
                dirs.push(p);
            }
        } else {
            let name = entry.file_name().to_string_lossy().into_owned();
            if exts.iter().any(|e| name.ends_with(e)) {
                files.push(p);
            }
        }
    }
    for f in files {
        out.push(f);
    }
    for d in dirs {
        walk_files(&d, exts, out);
    }
}

struct CodeReaders {
    readers: HashMap<String, Vec<String>>,
    unresolved: Vec<String>,
    quoted: HashSet<String>,
}

/// Regex precompilate condivise dalla scansione dei lettori nel codice.
struct ReaderRegexes {
    rust_reader: Regex,
    py_reader: Regex,
    sql_key_eq: Regex,
    callsite: Regex,
    quoted: Regex,
    ts_barekey: Regex,
}

impl ReaderRegexes {
    fn compile() -> Result<Self> {
        Ok(ReaderRegexes {
            // ATTENZIONE alle firme: in Rust la chiave e' il 2o argomento (dopo
            // &db), in Python il 1o. Applicare la regex sbagliata cattura i
            // valori di DEFAULT come chiavi (falsi fantasma).
            rust_reader: Regex::new(
                r#"(?s)\b(get_setting_checked|get_setting_nonempty|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(\s*[^,()]*,\s*"([^"]+)""#,
            )?,
            py_reader: Regex::new(
                r#"\b(get_setting_checked|get_bool_setting_checked|get_int_setting_checked|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(\s*(?:key\s*=\s*)?["']([^"']+)["']"#,
            )?,
            // `FROM settings ... WHERE key = '...'`. La classe [\s"'+\\]* tollera
            // le query SQL spezzate su literal adiacenti (Python concat
            // implicita, JS/TS `+`).
            sql_key_eq: Regex::new(
                r#"(?i)FROM\s+settings\b[\s"'+\\]*WHERE\s+key\s*=\s*'([^']+)'"#,
            )?,
            // Call site dei lettori che NON hanno chiave literal (riconciliazione).
            callsite: Regex::new(
                r"\b(get_setting_checked|get_setting_nonempty|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(",
            )?,
            quoted: Regex::new(r#""([^"\\\n]{2,120})"|'([^'\\\n]{2,120})'"#)?,
            // Chiavi d'oggetto JS/TS non quotate (es. DB_KEY_MAP del gateway).
            ts_barekey: Regex::new(r"(?m)^\s*([a-z][a-z0-9_]{3,60}):")?,
        })
    }
}

/// Estrae le stringhe quotate (e le bare-key JS/TS) di un file, aggiungendole a
/// `quoted`.
fn collect_quoted_strings(
    re: &ReaderRegexes,
    text: &str,
    suffix: &str,
    quoted: &mut HashSet<String>,
) {
    for m in re.quoted.captures_iter(text) {
        if let Some(g) = m.get(1).or_else(|| m.get(2)) {
            quoted.insert(g.as_str().to_string());
        }
    }
    if suffix == "ts" || suffix == "tsx" {
        for m in re.ts_barekey.captures_iter(text) {
            quoted.insert(m.get(1).unwrap().as_str().to_string());
        }
    }
}

/// Scansiona un singolo file per lettori di settings, popolando `readers`,
/// `unresolved` e `quoted`. `rel` e' il path relativo alla radice del repo.
fn scan_file_for_readers(
    re: &ReaderRegexes,
    text: &str,
    suffix: &str,
    rel: &str,
    readers: &mut HashMap<String, Vec<String>>,
    unresolved: &mut Vec<String>,
    quoted: &mut HashSet<String>,
) {
    collect_quoted_strings(re, text, suffix, quoted);

    let mut matched_spans: HashSet<usize> = HashSet::new();
    let reader_re: Option<&Regex> = match suffix {
        "rs" => Some(&re.rust_reader),
        "py" => Some(&re.py_reader),
        _ => None,
    };
    if let Some(reg) = reader_re {
        for m in reg.captures_iter(text) {
            let whole = m.get(0).unwrap();
            let line = line_at(text, whole.start());
            let key = m.get(2).unwrap().as_str().to_string();
            readers.entry(key).or_default().push(format!("{rel}:{line}"));
            matched_spans.insert(whole.start());
        }
    }
    for m in re.sql_key_eq.captures_iter(text) {
        let whole = m.get(0).unwrap();
        let line = line_at(text, whole.start());
        let key = m.get(1).unwrap().as_str().to_string();
        readers.entry(key).or_default().push(format!("{rel}:{line}"));
    }
    // Riconciliazione: call site lettori senza literal riconosciuto.
    if suffix == "rs" || suffix == "py" {
        for m in re.callsite.captures_iter(text) {
            let whole = m.get(0).unwrap();
            if !matched_spans.contains(&whole.start()) {
                let line = line_at(text, whole.start());
                let end = (whole.start() + 80).min(text.len());
                // slice byte-safe sul confine char
                let slice = safe_slice(text, whole.start(), end);
                let snippet = slice.split('\n').next().unwrap_or("");
                unresolved.push(format!("{rel}:{line}  {snippet}"));
            }
        }
    }
}

/// Ritorna (chiave -> [siti file:riga]), call site non riconciliati, e il set
/// di TUTTE le stringhe quotate nei sorgenti.
/// Raccoglie tutti i file con estensione ammessa sotto le radici di scansione
/// (ordine top-down come os.walk), saltando le radici inesistenti.
fn gather_scan_files(root: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let scan_roots = [
        root.join("crates"),
        root.join("brain"),
        root.join("apps"),
        root.join("packages"),
        root.join("scripts"),
        root.join("evals"),
        root.join("deploy"),
        root.join("config"),
    ];
    let mut files: Vec<PathBuf> = Vec::new();
    for scan_root in &scan_roots {
        if scan_root.exists() {
            walk_files(scan_root, exts, &mut files);
        }
    }
    files
}

/// True se il file (script stesso o punto unico) non e' un "lettore di
/// business" e va escluso dalla scansione.
fn is_excluded_reader_file(fname: &str, rel: &str) -> bool {
    fname == "audit_settings.py"
        || fname == "settings_db.py"
        || rel.ends_with("crates/nexus-auth/src/lib.rs")
}

fn collect_code_readers() -> Result<CodeReaders> {
    let root = repo_root();
    let re = ReaderRegexes::compile()?;

    let mut readers: HashMap<String, Vec<String>> = HashMap::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut quoted: HashSet<String> = HashSet::new();

    let exts = [".rs", ".py", ".ts", ".tsx", ".sh", ".yaml", ".yml"];

    for path in &gather_scan_files(&root, &exts) {
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_excluded_reader_file(fname, &rel) {
            continue;
        }
        let text = read_text(path);
        let suffix = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        scan_file_for_readers(
            &re,
            &text,
            suffix,
            &rel,
            &mut readers,
            &mut unresolved,
            &mut quoted,
        );
    }

    Ok(CodeReaders {
        readers,
        unresolved,
        quoted,
    })
}

/// Slice [start, end) sicuro sui confini di carattere (replica text[a:a+80] di
/// Python, che lavora su code-point; qui approssimiamo sul byte ma evitiamo il
/// panic se end cade dentro un char multibyte arretrando al confine valido).
fn safe_slice(text: &str, start: usize, mut end: usize) -> &str {
    if start >= text.len() {
        return "";
    }
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    if end > text.len() {
        end = text.len();
    }
    let mut s = start;
    while s < end && !text.is_char_boundary(s) {
        s += 1;
    }
    &text[s..end]
}

// ---------------------------------------------------------------------------
// C — UI admin: categorie navigabili
// ---------------------------------------------------------------------------
/// Ritorna le categorie navigabili dalla UI admin, o None = TUTTE.
fn collect_ui_categories() -> Result<Option<BTreeSet<String>>> {
    let root = repo_root();
    let dynamic = root.join("apps/web-ide/lib/settings-categories.ts");
    if dynamic.exists() {
        let text = read_text(&dynamic);
        if text.contains("useSettingsCategories") && text.contains("settings-categories") {
            return Ok(None); // sidebar dinamica: tutte le categorie raggiungibili
        }
    }
    let mut cats: BTreeSet<String> = BTreeSet::new();
    let sidebar = root.join("apps/web-ide/components/admin-sidebar.tsx");
    if sidebar.exists() {
        let text = read_text(&sidebar);
        let re = Regex::new(r"/admin/settings/([a-z0-9_-]+)")?;
        for m in re.captures_iter(&text) {
            cats.insert(m.get(1).unwrap().as_str().to_string());
        }
    }
    let panel = root.join("apps/web-ide/components/settings/settings-panel.tsx");
    if panel.exists() {
        let text = read_text(&panel);
        let order_re = Regex::new(r"(?s)CATEGORY_ORDER\s*=\s*\[([^\]]*)\]")?;
        if let Some(m) = order_re.captures(&text) {
            let body = m.get(1).unwrap().as_str();
            let item_re = Regex::new(r#"["']([a-z0-9_-]+)["']"#)?;
            for im in item_re.captures_iter(body) {
                cats.insert(im.get(1).unwrap().as_str().to_string());
            }
        }
    }
    Ok(Some(cats))
}

// ---------------------------------------------------------------------------
// Classificazione
// ---------------------------------------------------------------------------
/// Mappa che preserva l'ordine d'inserimento (come il dict di Python). Le classi
/// viva/morta/invisibile/runtime_only/test_only sono popolate iterando il DB
/// live nell'ordine `ORDER BY key` di Postgres: il JSON deve riflettere quella
/// collation, non l'ordinamento byte di un BTreeMap. `setdefault` non sovrascrive.
#[derive(Default)]
struct OrderedMap {
    order: Vec<String>,
    map: HashMap<String, String>,
}

impl OrderedMap {
    fn insert(&mut self, k: String, v: String) {
        if !self.map.contains_key(&k) {
            self.order.push(k.clone());
        }
        self.map.insert(k, v);
    }
    /// Come dict.setdefault: inserisce solo se assente, conserva la posizione.
    fn set_default(&mut self, k: String, v: String) {
        if !self.map.contains_key(&k) {
            self.order.push(k.clone());
            self.map.insert(k, v);
        }
    }
    fn len(&self) -> usize {
        self.order.len()
    }
    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
    fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.order.iter().map(move |k| (k, &self.map[k]))
    }
    /// Iterazione ordinata per chiave (per il report, che usa sorted()).
    fn iter_sorted(&self) -> Vec<(&String, &String)> {
        let mut v: Vec<(&String, &String)> = self.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    }
}

struct Classes {
    viva: OrderedMap,
    morta: OrderedMap,
    fantasma: BTreeMap<String, Vec<String>>,
    invisibile: OrderedMap,
    runtime_only: OrderedMap,
    test_only: OrderedMap,
}

fn classify(
    db_live: Option<&[(String, String)]>,
    migrations: &BTreeMap<String, (String, String)>,
    readers: &HashMap<String, Vec<String>>,
    ui_cats: Option<&BTreeSet<String>>,
    quoted: &HashSet<String>,
) -> Result<Classes> {
    let root = repo_root();
    let dynamic: Vec<Regex> = DYNAMIC_READ_PATTERNS
        .iter()
        .map(|(p, _)| Regex::new(p))
        .collect::<Result<_, _>>()?;
    let runtime: Vec<Regex> = RUNTIME_WRITTEN_KEYS
        .iter()
        .map(|(p, _)| Regex::new(p))
        .collect::<Result<_, _>>()?;
    let keep: HashSet<&str> = KEEP_DESPITE_NO_READER.iter().copied().collect();
    // Chiavi dichiarate dal catalogo servizi come `port_setting_key`: lette a
    // runtime dalla chiave, mai come literal nel codice (vedi la funzione).
    let porte_catalogo = port_setting_keys_dal_catalogo();
    let bulk: HashSet<&str> = CATEGORY_BULK_READERS.iter().map(|(c, _)| *c).collect();

    // Replica re.Pattern.match: ancorato all'inizio della stringa.
    let runtime_match = |key: &str| runtime.iter().any(|r| match_at_start(r, key));

    let mut result = Classes {
        viva: OrderedMap::default(),
        morta: OrderedMap::default(),
        fantasma: BTreeMap::new(),
        invisibile: OrderedMap::default(),
        runtime_only: OrderedMap::default(),
        test_only: OrderedMap::default(),
    };

    let empty_db: Vec<(String, String)> = Vec::new();
    let db: &[(String, String)] = db_live.unwrap_or(&empty_db);
    let keyset_db: HashSet<&String> = db.iter().map(|(k, _)| k).collect();

    for (key, cat) in db.iter() {
        let via = read_via(&root, readers, quoted, &keep, &dynamic, &bulk, &porte_catalogo, key, cat);
        let is_runtime = !migrations.contains_key(key) && runtime_match(key);
        classify_db_key(&mut result, migrations, ui_cats, key, cat, via, is_runtime);
    }

    if db_live.is_some() {
        detect_ghost_keys(&root, readers, &keyset_db, &mut result.fantasma)?;
    }

    Ok(result)
}

/// Equivalente di read_via(key, category) -> Option<&str>: come e' letta una
/// chiave del DB live (literal, test-only, quoted, dynamic, category), o None.
#[allow(clippy::too_many_arguments)]
fn read_via(
    root: &Path,
    readers: &HashMap<String, Vec<String>>,
    quoted: &HashSet<String>,
    keep: &HashSet<&str>,
    dynamic: &[Regex],
    bulk: &HashSet<&str>,
    porte_catalogo: &HashSet<String>,
    key: &str,
    category: &str,
) -> Option<&'static str> {
    if keep.contains(key) {
        return Some("keep-exception");
    }
    if let Some(sites) = readers.get(key) {
        let all_test = sites
            .iter()
            .all(|s| is_test_path(&root.join(s.split(':').next().unwrap_or(""))));
        if all_test {
            return Some("test-only");
        }
        return Some("literal");
    }
    if quoted.contains(key) {
        return Some("quoted");
    }
    if dynamic.iter().any(|r| match_at_start(r, key)) {
        return Some("dynamic");
    }
    if porte_catalogo.contains(key) {
        return Some("catalogo-porte");
    }
    if bulk.contains(category) {
        return Some("category");
    }
    None
}

/// Assegna una chiave del DB live alla classe corretta di `result` in base a
/// come e' letta (`via`) e allo stato runtime/migrazioni.
fn classify_db_key(
    result: &mut Classes,
    migrations: &BTreeMap<String, (String, String)>,
    ui_cats: Option<&BTreeSet<String>>,
    key: &str,
    cat: &str,
    via: Option<&'static str>,
    is_runtime: bool,
) {
    match via {
        None => {
            if !migrations.contains_key(key) {
                // Non in migrazioni e non whitelistata: probabile scrittura
                // runtime non censita -> da revisionare, NON cancellare.
                result.runtime_only.insert(key.to_string(), cat.to_string());
            } else {
                result.morta.insert(key.to_string(), cat.to_string());
            }
        }
        Some("test-only") => {
            result.test_only.insert(key.to_string(), cat.to_string());
        }
        Some(_) => {
            if ui_cats.is_some() && !ui_cats.unwrap().contains(cat) {
                result.invisibile.insert(key.to_string(), cat.to_string());
            } else {
                result.viva.insert(key.to_string(), cat.to_string());
            }
            if is_runtime {
                // dict.setdefault: non sovrascrive se gia' presente.
                result.runtime_only.set_default(key.to_string(), cat.to_string());
            }
        }
    }
}

/// Individua i FANTASMA: chiavi lette literal nel codice ma assenti dal DB live,
/// filtrando i falsi positivi per forma-chiave e i lettori solo-test.
fn detect_ghost_keys(
    root: &Path,
    readers: &HashMap<String, Vec<String>>,
    keyset_db: &HashSet<&String>,
    fantasma: &mut BTreeMap<String, Vec<String>>,
) -> Result<()> {
    // Filtro forma-chiave: esclude default/valori catturati per errore
    // ("foo", "5", URL) — una chiave vera ha namespace con . o _ .
    let keylike = Regex::new(r"^[a-z][a-z0-9_.:-]*$")?;
    // sorted(set(readers) - keyset_db)
    let mut candidates: Vec<&String> = readers
        .keys()
        .filter(|k| !keyset_db.contains(*k))
        .collect();
    candidates.sort();
    for key in candidates {
        // not keylike.match(key) or ("." not in key and "_" not in key)
        //   or "://" in key or ":" in key.split(".")[0]
        let first_seg = key.split('.').next().unwrap_or("");
        if !match_at_start(&keylike, key)
            || (!key.contains('.') && !key.contains('_'))
            || key.contains("://")
            || first_seg.contains(':')
        {
            continue;
        }
        let sites = &readers[key];
        let all_test = sites
            .iter()
            .all(|s| is_test_path(&root.join(s.split(':').next().unwrap_or(""))));
        if all_test {
            continue;
        }
        let top3: Vec<String> = sites.iter().take(3).cloned().collect();
        fantasma.insert(key.clone(), top3);
    }
    Ok(())
}

/// Replica re.Pattern.match (ancorato all'inizio): true se la regex matcha a
/// partire dal byte 0. Le regex sorgente non sono ancorate con `^`, ma Python
/// usa .match() che ancora implicitamente all'inizio.
fn match_at_start(re: &Regex, hay: &str) -> bool {
    match re.find(hay) {
        Some(m) => m.start() == 0,
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Entry point del sottocomando
// ---------------------------------------------------------------------------
struct Args {
    report: bool,
    json: Option<String>,
    no_db: bool,
    gate: bool,
    baseline: String,
}

fn parse_args(raw: &[String]) -> Args {
    let mut a = Args {
        report: false,
        json: None,
        no_db: false,
        gate: false,
        baseline: repo_root()
            .join("scripts/audit-settings-baseline.json")
            .to_string_lossy()
            .into_owned(),
    };
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--report" => a.report = true,
            "--no-db" => a.no_db = true,
            "--gate" => a.gate = true,
            "--json" => {
                i += 1;
                if i < raw.len() {
                    a.json = Some(raw[i].clone());
                }
            }
            "--baseline" => {
                i += 1;
                if i < raw.len() {
                    a.baseline = raw[i].clone();
                }
            }
            _ => {}
        }
        i += 1;
    }
    a
}

pub fn run(raw_args: &[String]) -> Result<i32> {
    let args = parse_args(raw_args);

    let db_live = if args.no_db {
        None
    } else {
        collect_db_live()
    };
    if db_live.is_none() && !args.no_db {
        eprintln!("AVVISO: DB live non raggiungibile, procedo in modalita --no-db");
    }
    let (migrations, _deleted) = collect_migrations()?;
    let code = collect_code_readers()?;
    let ui_cats = collect_ui_categories()?;
    let res = classify(
        db_live.as_deref(),
        &migrations,
        &code.readers,
        ui_cats.as_ref(),
        &code.quoted,
    )?;

    let summary = build_summary(db_live.as_deref(), &migrations, &code, &ui_cats, &res);

    if let Some(json_path) = &args.json {
        let payload = build_json_payload(&summary, &res, &code.unresolved);
        let serialized = serde_json::to_string_pretty(&payload)?;
        std::fs::write(json_path, serialized)
            .with_context(|| format!("scrittura JSON in {json_path}"))?;
        println!("JSON scritto in {json_path}");
    }

    if args.report || !(args.json.is_some() || args.gate) {
        print_report(&summary, &res, ui_cats.as_ref(), &code.unresolved);
    }

    if args.gate {
        return run_gate(&args.baseline, &res);
    }

    Ok(0)
}

/// Costruisce la mappa `summary` (ordine d'inserimento come il dict Python):
/// conteggi chiavi DB/migrazioni/lettori + categorie UI + conteggi per classe.
fn build_summary(
    db_live: Option<&[(String, String)]>,
    migrations: &BTreeMap<String, (String, String)>,
    code: &CodeReaders,
    ui_cats: &Option<BTreeSet<String>>,
    res: &Classes,
) -> Map<String, Value> {
    // counts = {k: len(v) for k, v in res.items()}
    let counts: [(&str, usize); 6] = [
        ("viva", res.viva.len()),
        ("morta", res.morta.len()),
        ("fantasma", res.fantasma.len()),
        ("invisibile", res.invisibile.len()),
        ("runtime_only", res.runtime_only.len()),
        ("test_only", res.test_only.len()),
    ];

    let mut summary = Map::new();
    summary.insert("db_live_keys".into(), json!(db_live.map(|m| m.len()).unwrap_or(0)));
    summary.insert("migration_keys".into(), json!(migrations.len()));
    summary.insert("reader_keys".into(), json!(code.readers.len()));
    summary.insert("unresolved_call_sites".into(), json!(code.unresolved.len()));
    let ui_cats_summary: Value = match ui_cats {
        None => json!("dinamiche (tutte navigabili)"),
        Some(set) => {
            let v: Vec<&String> = set.iter().collect();
            json!(v)
        }
    };
    summary.insert("ui_categories".into(), ui_cats_summary);
    for (k, v) in &counts {
        summary.insert((*k).to_string(), json!(v));
    }
    summary
}

/// Costruisce il payload JSON {summary, classi, unresolved} con classi nello
/// stesso ordine d'inserimento di Python (viva, morta, fantasma, invisibile,
/// runtime_only, test_only).
fn build_json_payload(summary: &Map<String, Value>, res: &Classes, unresolved: &[String]) -> Value {
    let mut classi = Map::new();
    classi.insert("viva".into(), map_to_value(&res.viva));
    classi.insert("morta".into(), map_to_value(&res.morta));
    classi.insert("fantasma".into(), vecmap_to_value(&res.fantasma));
    classi.insert("invisibile".into(), map_to_value(&res.invisibile));
    classi.insert("runtime_only".into(), map_to_value(&res.runtime_only));
    classi.insert("test_only".into(), map_to_value(&res.test_only));

    let mut payload = Map::new();
    payload.insert("summary".into(), Value::Object(summary.clone()));
    payload.insert("classi".into(), Value::Object(classi));
    payload.insert(
        "unresolved".into(),
        Value::Array(unresolved.iter().map(|s| json!(s)).collect()),
    );
    Value::Object(payload)
}

fn map_to_value(m: &OrderedMap) -> Value {
    let mut obj = Map::new();
    for (k, v) in m.iter() {
        obj.insert(k.clone(), json!(v));
    }
    Value::Object(obj)
}

fn vecmap_to_value(m: &BTreeMap<String, Vec<String>>) -> Value {
    let mut obj = Map::new();
    for (k, v) in m {
        obj.insert(k.clone(), json!(v));
    }
    Value::Object(obj)
}

/// Stampa l'intestazione e il riepilogo (summary + categorie UI).
fn print_report_header(summary: &Map<String, Value>, ui_cats: Option<&BTreeSet<String>>) {
    println!("=== audit settings: riepilogo ===");
    for (k, v) in summary {
        if k != "ui_categories" {
            // i valori numerici stampati come interi (json! di usize)
            println!("  {k}: {}", render_summary_value(v));
        }
    }
    match ui_cats {
        None => println!("  ui_categories: dinamiche dal DB (tutte navigabili)"),
        Some(set) => {
            let joined = set.iter().cloned().collect::<Vec<_>>().join(", ");
            println!("  ui_categories ({}): {}", set.len(), joined);
        }
    }
}

/// Stampa le classi problematiche (morta, fantasma, invisibile, runtime_only,
/// test_only) nell'ordine e nel formato dello script Python originale.
fn print_report_classes(res: &Classes) {
    for cls in ["morta", "fantasma", "invisibile", "runtime_only", "test_only"] {
        if cls == "fantasma" {
            if !res.fantasma.is_empty() {
                println!("\n--- {} ({}) ---", cls.to_uppercase(), res.fantasma.len());
                for (key, sites) in &res.fantasma {
                    // valore = lista di siti -> Python stampa la repr lista
                    println!("  {key}  [{}]", py_list_repr(sites));
                }
            }
            continue;
        }
        let m = match cls {
            "morta" => &res.morta,
            "invisibile" => &res.invisibile,
            "runtime_only" => &res.runtime_only,
            "test_only" => &res.test_only,
            _ => unreachable!(),
        };
        if !m.is_empty() {
            println!("\n--- {} ({}) ---", cls.to_uppercase(), m.len());
            // Python: for key in sorted(res[cls])
            for (key, cat) in m.iter_sorted() {
                println!("  {key}  [{cat}]");
            }
        }
    }
}

/// Stampa i call site non riconciliati (max 60, poi un troncamento).
fn print_report_unresolved(unresolved: &[String]) {
    if unresolved.is_empty() {
        return;
    }
    println!("\n--- CALL SITE NON RICONCILIATI ({}) ---", unresolved.len());
    for u in unresolved.iter().take(60) {
        println!("  {u}");
    }
    if unresolved.len() > 60 {
        println!("  ... e altri {}", unresolved.len() - 60);
    }
}

fn print_report(
    summary: &Map<String, Value>,
    res: &Classes,
    ui_cats: Option<&BTreeSet<String>>,
    unresolved: &[String],
) {
    print_report_header(summary, ui_cats);
    print_report_classes(res);
    print_report_unresolved(unresolved);
}

/// Rende un valore di summary come lo stamperebbe Python str(): interi senza
/// virgolette. Tutti i valori non-ui_categories sono interi.
fn render_summary_value(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// repr di una lista Python di stringhe: ['a', 'b', 'c'].
fn py_list_repr(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    // FALSO POSITIVO del detector SQL injection: il metodo Rust `.join(", ")` su
    // uno slice viene scambiato per la keyword SQL JOIN (word-boundary) e, unito
    // al placeholder {} del format!, attiva SQL_KEYWORD_RE + RS_FORMAT_RE. Non e'
    // una query: e' la costruzione della repr testuale di una lista. Invariato.
    format!("[{}]", inner.join(", "))
}

/// Crea il file baseline con i conteggi correnti (prima esecuzione del gate).
fn write_gate_baseline(
    base_path: &Path,
    baseline_path: &str,
    cur_morta: usize,
    cur_fantasma: usize,
    cur_invisibile: usize,
) -> Result<()> {
    // base_path.write_text(json.dumps(cur, indent=2) + "\n")
    let cur = json!({
        "morta": cur_morta,
        "fantasma": cur_fantasma,
        "invisibile": cur_invisibile,
    });
    let mut out = serde_json::to_string_pretty(&cur)?;
    out.push('\n');
    std::fs::write(base_path, out)
        .with_context(|| format!("scrittura baseline {baseline_path}"))?;
    println!("Baseline creata: {baseline_path}");
    Ok(())
}

fn run_gate(baseline_path: &str, res: &Classes) -> Result<i32> {
    let base_path = Path::new(baseline_path);
    let cur_morta = res.morta.len();
    let cur_fantasma = res.fantasma.len();
    let cur_invisibile = res.invisibile.len();

    if !base_path.exists() {
        write_gate_baseline(base_path, baseline_path, cur_morta, cur_fantasma, cur_invisibile)?;
        return Ok(0);
    }

    let base_text = std::fs::read_to_string(base_path)
        .with_context(|| format!("lettura baseline {baseline_path}"))?;
    let base: Value = serde_json::from_str(&base_text)
        .with_context(|| format!("parse baseline {baseline_path}"))?;

    Ok(compare_gate_counts(&base, cur_morta, cur_fantasma, cur_invisibile))
}

/// Confronta i conteggi correnti con la baseline: exit 1 (con dettaglio delle
/// regressioni su stderr) se una metrica e' salita, 0 altrimenti.
fn compare_gate_counts(
    base: &Value,
    cur_morta: usize,
    cur_fantasma: usize,
    cur_invisibile: usize,
) -> i32 {
    let base_get = |k: &str| base.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let cur_pairs = [
        ("morta", cur_morta),
        ("fantasma", cur_fantasma),
        ("invisibile", cur_invisibile),
    ];

    // regress = {k: (cur[k], base.get(k, 0)) for k in cur if cur[k] > base.get(k, 0)}
    let regress: Vec<(&str, usize, usize)> = cur_pairs
        .into_iter()
        .filter_map(|(k, cur_v)| {
            let base_v = base_get(k);
            (cur_v > base_v).then_some((k, cur_v, base_v))
        })
        .collect();

    if !regress.is_empty() {
        // Replica repr del dict Python: {'morta': (1, 0), ...}
        let inner: Vec<String> = regress
            .iter()
            .map(|(k, c, b)| format!("'{k}': ({c}, {b})"))
            .collect();
        eprintln!(
            "GATE FALLITO (regressioni vs baseline): {{{}}}",
            inner.join(", ")
        );
        return 1;
    }

    // GATE OK: {cur} <= baseline {base}
    let cur_repr = format!(
        "{{'morta': {cur_morta}, 'fantasma': {cur_fantasma}, 'invisibile': {cur_invisibile}}}"
    );
    let base_repr = format!(
        "{{'morta': {}, 'fantasma': {}, 'invisibile': {}}}",
        base_get("morta"),
        base_get("fantasma"),
        base_get("invisibile")
    );
    println!("GATE OK: {cur_repr} <= baseline {base_repr}");
    0
}
