//! audit-settings — censimento configurazioni `settings`: DB live + migrazioni
//! vs lettori nel codice vs UI. Porting 1:1 di scripts/audit_settings.py.
//!
//! Punto unico (regola L) per l'audit "ogni setting esposta in admin e' davvero
//! letta dal codice". Quattro collettori:
//!
//!   A1. DB live          — SELECT key, category FROM settings (docker exec psql)
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
use std::process::Command;

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
fn collect_db_live() -> Option<Vec<(String, String)>> {
    let out = Command::new("docker")
        .args([
            "exec",
            "ideai-postgres-nexus-1",
            "psql",
            "-U",
            "nexus",
            "-d",
            "nexus",
            "-t",
            "-A",
            "-F",
            "|",
            "-c",
            "SELECT key, category FROM settings ORDER BY key",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Python: rows[key] = cat su un dict -> ultima categoria vince ma la
    // posizione e' quella della prima occorrenza della chiave. Con ORDER BY key
    // le chiavi sono uniche, ma replichiamo comunque la semantica del dict.
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for line in stdout.lines() {
        if let Some(idx) = line.find('|') {
            let key = line[..idx].trim().to_string();
            let cat = line[idx + 1..].trim().to_string();
            if let Some(&pos) = seen.get(&key) {
                rows[pos].1 = cat;
            } else {
                seen.insert(key.clone(), rows.len());
                rows.push((key, cat));
            }
        }
    }
    if rows.is_empty() { None } else { Some(rows) }
}

// ---------------------------------------------------------------------------
// A2 — Migrazioni: INSERT INTO settings / DELETE FROM settings
// ---------------------------------------------------------------------------
/// Spezza il body di un VALUES in tuple, rispettando apici e parentesi.
/// Porting fedele di _split_sql_tuples (Python).
fn split_sql_tuples(body: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = body.chars().collect();
    let mut tuples: Vec<Vec<String>> = Vec::new();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut cur = String::new();
    let mut fields: Vec<String> = Vec::new();
    let mut i = 0usize;
    let n = chars.len();
    while i < n {
        let ch = chars[i];
        if in_str {
            if ch == '\'' {
                if i + 1 < n && chars[i + 1] == '\'' {
                    // apice escapato ''
                    cur.push('\'');
                    i += 2;
                    continue;
                }
                in_str = false;
            } else {
                cur.push(ch);
            }
        } else if ch == '\'' {
            in_str = true;
        } else if ch == '(' {
            depth += 1;
            if depth == 1 {
                fields = Vec::new();
                cur = String::new();
                i += 1;
                continue;
            }
            cur.push(ch);
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                fields.push(cur.trim().to_string());
                tuples.push(std::mem::take(&mut fields));
                cur = String::new();
                i += 1;
                continue;
            }
            cur.push(ch);
        } else if ch == ',' && depth == 1 {
            fields.push(cur.trim().to_string());
            cur = String::new();
        } else if depth >= 1 {
            cur.push(ch);
        }
        i += 1;
    }
    tuples
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

    // Python: sorted(mig_dir.glob("*.sql")) -> ordine lessicografico per nome.
    let mut sql_files: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&mig_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("sql") {
                sql_files.push(p);
            }
        }
    }
    sql_files.sort();

    for sql_file in &sql_files {
        let text = read_text(sql_file);
        let fname = sql_file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        for m in ins_re.captures_iter(&text) {
            let cols_raw = m.get(1).map(|g| g.as_str()).unwrap_or_default();
            let cols: Vec<String> = cols_raw
                .split(',')
                .map(|c| c.trim().to_lowercase())
                .collect();
            if !cols.iter().any(|c| c == "key") {
                continue;
            }
            let key_idx = cols.iter().position(|c| c == "key").unwrap();
            let cat_idx = cols.iter().position(|c| c == "category");
            let values_body = m.get(2).map(|g| g.as_str()).unwrap_or_default();
            for tup in split_sql_tuples(values_body) {
                if key_idx < tup.len() {
                    let key = &tup[key_idx];
                    if key.is_empty()
                        || key.to_uppercase().contains("SELECT")
                        || key.contains("||")
                    {
                        // INSERT..SELECT o chiave costruita: fuori scope
                        continue;
                    }
                    let cat = match cat_idx {
                        Some(ci) if ci < tup.len() => tup[ci].clone(),
                        _ => String::new(),
                    };
                    inserted.insert(key.clone(), (cat, fname.clone()));
                    deleted.remove(key);
                }
            }
        }
        for m in del_eq_re.captures_iter(&text) {
            let k = m.get(1).unwrap().as_str().to_string();
            deleted.insert(k.clone());
            inserted.remove(&k);
        }
        for m in del_in_re.captures_iter(&text) {
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

/// Ritorna (chiave -> [siti file:riga]), call site non riconciliati, e il set
/// di TUTTE le stringhe quotate nei sorgenti.
fn collect_code_readers() -> Result<CodeReaders> {
    let root = repo_root();

    // ATTENZIONE alle firme: in Rust la chiave e' il 2o argomento (dopo &db),
    // in Python il 1o. Applicare la regex sbagliata cattura i valori di
    // DEFAULT come chiavi (falsi fantasma).
    let rust_reader_re = Regex::new(
        r#"(?s)\b(get_setting_checked|get_setting_nonempty|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(\s*[^,()]*,\s*"([^"]+)""#,
    )?;
    let py_reader_re = Regex::new(
        r#"\b(get_setting_checked|get_bool_setting_checked|get_int_setting_checked|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(\s*(?:key\s*=\s*)?["']([^"']+)["']"#,
    )?;
    // `FROM settings ... WHERE key = '...'`. La classe [\s"'+\\]* tollera le
    // query SQL spezzate su literal adiacenti (Python concat implicita, JS/TS `+`).
    let sql_key_eq_re =
        Regex::new(r#"(?i)FROM\s+settings\b[\s"'+\\]*WHERE\s+key\s*=\s*'([^']+)'"#)?;
    // Call site dei lettori che NON hanno chiave literal (per riconciliazione).
    let callsite_re = Regex::new(
        r"\b(get_setting_checked|get_setting_nonempty|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(",
    )?;
    let quoted_re = Regex::new(r#""([^"\\\n]{2,120})"|'([^'\\\n]{2,120})'"#)?;
    // Chiavi d'oggetto JS/TS non quotate (es. DB_KEY_MAP del gateway).
    let ts_barekey_re = Regex::new(r"(?m)^\s*([a-z][a-z0-9_]{3,60}):")?;

    let mut readers: HashMap<String, Vec<String>> = HashMap::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut quoted: HashSet<String> = HashSet::new();

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
    let exts = [".rs", ".py", ".ts", ".tsx", ".sh", ".yaml", ".yml"];

    let nexus_auth_lib = "crates/nexus-auth/src/lib.rs";

    for scan_root in &scan_roots {
        if !scan_root.exists() {
            continue;
        }
        let mut files: Vec<PathBuf> = Vec::new();
        walk_files(scan_root, &exts, &mut files);
        for path in &files {
            let fname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            // Lo script stesso e il punto unico non sono "lettori di business".
            if fname == "audit_settings.py"
                || fname == "settings_db.py"
                || rel.ends_with(nexus_auth_lib)
            {
                continue;
            }
            let text = read_text(path);
            let suffix = path.extension().and_then(|e| e.to_str()).unwrap_or("");

            for m in quoted_re.captures_iter(&text) {
                let g = m.get(1).or_else(|| m.get(2));
                if let Some(g) = g {
                    quoted.insert(g.as_str().to_string());
                }
            }
            if suffix == "ts" || suffix == "tsx" {
                for m in ts_barekey_re.captures_iter(&text) {
                    quoted.insert(m.get(1).unwrap().as_str().to_string());
                }
            }

            let mut matched_spans: HashSet<usize> = HashSet::new();
            let regs: &[&Regex] = if suffix == "rs" {
                &[&rust_reader_re]
            } else if suffix == "py" {
                &[&py_reader_re]
            } else {
                &[]
            };
            for reg in regs {
                for m in reg.captures_iter(&text) {
                    let whole = m.get(0).unwrap();
                    let line = line_at(&text, whole.start());
                    let key = m.get(2).unwrap().as_str().to_string();
                    readers.entry(key).or_default().push(format!("{rel}:{line}"));
                    matched_spans.insert(whole.start());
                }
            }
            for m in sql_key_eq_re.captures_iter(&text) {
                let whole = m.get(0).unwrap();
                let line = line_at(&text, whole.start());
                let key = m.get(1).unwrap().as_str().to_string();
                readers.entry(key).or_default().push(format!("{rel}:{line}"));
            }
            // Riconciliazione: call site lettori senza literal riconosciuto.
            if suffix == "rs" || suffix == "py" {
                for m in callsite_re.captures_iter(&text) {
                    let whole = m.get(0).unwrap();
                    if !matched_spans.contains(&whole.start()) {
                        let line = line_at(&text, whole.start());
                        let end = (whole.start() + 80).min(text.len());
                        // slice byte-safe sul confine char
                        let slice = safe_slice(&text, whole.start(), end);
                        let snippet = slice.split('\n').next().unwrap_or("");
                        unresolved.push(format!("{rel}:{line}  {snippet}"));
                    }
                }
            }
        }
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
    let bulk: HashSet<&str> = CATEGORY_BULK_READERS.iter().map(|(c, _)| *c).collect();

    // Replica re.Pattern.match: ancorato all'inizio della stringa.
    let dyn_match = |key: &str| dynamic.iter().any(|r| match_at_start(r, key));
    let runtime_match = |key: &str| runtime.iter().any(|r| match_at_start(r, key));

    // Equivalente di read_via(key, category) -> Option<&str>.
    let read_via = |key: &str, category: &str| -> Option<&'static str> {
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
        if dyn_match(key) {
            return Some("dynamic");
        }
        if bulk.contains(category) {
            return Some("category");
        }
        None
    };

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
        let via = read_via(key, cat);
        let is_runtime = !migrations.contains_key(key) && runtime_match(key);
        match via {
            None => {
                if !migrations.contains_key(key) {
                    // Non in migrazioni e non whitelistata: probabile scrittura
                    // runtime non censita -> da revisionare, NON cancellare.
                    result.runtime_only.insert(key.clone(), cat.clone());
                } else {
                    result.morta.insert(key.clone(), cat.clone());
                }
            }
            Some("test-only") => {
                result.test_only.insert(key.clone(), cat.clone());
            }
            Some(_) => {
                if ui_cats.is_some() && !ui_cats.unwrap().contains(cat) {
                    result.invisibile.insert(key.clone(), cat.clone());
                } else {
                    result.viva.insert(key.clone(), cat.clone());
                }
                if is_runtime {
                    // dict.setdefault: non sovrascrive se gia' presente.
                    result.runtime_only.set_default(key.clone(), cat.clone());
                }
            }
        }
    }

    // Filtro forma-chiave: esclude default/valori catturati per errore
    // ("foo", "5", URL) — una chiave vera ha namespace con . o _ .
    let keylike = Regex::new(r"^[a-z][a-z0-9_.:-]*$")?;
    if db_live.is_some() {
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
            result.fantasma.insert(key.clone(), top3);
        }
    }

    Ok(result)
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

    // counts = {k: len(v) for k, v in res.items()}
    let counts: Vec<(&str, usize)> = vec![
        ("viva", res.viva.len()),
        ("morta", res.morta.len()),
        ("fantasma", res.fantasma.len()),
        ("invisibile", res.invisibile.len()),
        ("runtime_only", res.runtime_only.len()),
        ("test_only", res.test_only.len()),
    ];

    let db_live_keys = db_live.as_ref().map(|m| m.len()).unwrap_or(0);
    let migration_keys = migrations.len();
    let reader_keys = code.readers.len();
    let unresolved_n = code.unresolved.len();

    // summary (ordine d'inserimento come Python)
    let mut summary = Map::new();
    summary.insert("db_live_keys".into(), json!(db_live_keys));
    summary.insert("migration_keys".into(), json!(migration_keys));
    summary.insert("reader_keys".into(), json!(reader_keys));
    summary.insert("unresolved_call_sites".into(), json!(unresolved_n));
    let ui_cats_summary: Value = match &ui_cats {
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

fn print_report(
    summary: &Map<String, Value>,
    res: &Classes,
    ui_cats: Option<&BTreeSet<String>>,
    unresolved: &[String],
) {
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

    // for cls in ("morta", "fantasma", "invisibile", "runtime_only", "test_only")
    for cls in ["morta", "fantasma", "invisibile", "runtime_only", "test_only"] {
        match cls {
            "fantasma" => {
                if !res.fantasma.is_empty() {
                    println!("\n--- {} ({}) ---", cls.to_uppercase(), res.fantasma.len());
                    for key in res.fantasma.keys() {
                        // valore = lista di siti -> Python stampa la repr lista
                        let sites = &res.fantasma[key];
                        println!("  {key}  [{}]", py_list_repr(sites));
                    }
                }
            }
            other => {
                let m = match other {
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
    }

    if !unresolved.is_empty() {
        println!("\n--- CALL SITE NON RICONCILIATI ({}) ---", unresolved.len());
        for u in unresolved.iter().take(60) {
            println!("  {u}");
        }
        if unresolved.len() > 60 {
            println!("  ... e altri {}", unresolved.len() - 60);
        }
    }
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
    format!("[{}]", inner.join(", "))
}

fn run_gate(baseline_path: &str, res: &Classes) -> Result<i32> {
    let base_path = Path::new(baseline_path);
    let cur_morta = res.morta.len();
    let cur_fantasma = res.fantasma.len();
    let cur_invisibile = res.invisibile.len();

    if !base_path.exists() {
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
        return Ok(0);
    }

    let base_text = std::fs::read_to_string(base_path)
        .with_context(|| format!("lettura baseline {baseline_path}"))?;
    let base: Value = serde_json::from_str(&base_text)
        .with_context(|| format!("parse baseline {baseline_path}"))?;
    let base_get = |k: &str| base.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let cur_pairs = [
        ("morta", cur_morta),
        ("fantasma", cur_fantasma),
        ("invisibile", cur_invisibile),
    ];

    // regress = {k: (cur[k], base.get(k, 0)) for k in cur if cur[k] > base.get(k, 0)}
    let mut regress: Vec<(&str, usize, usize)> = Vec::new();
    for (k, cur_v) in cur_pairs {
        let base_v = base_get(k);
        if cur_v > base_v {
            regress.push((k, cur_v, base_v));
        }
    }

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
        return Ok(1);
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
    Ok(0)
}
