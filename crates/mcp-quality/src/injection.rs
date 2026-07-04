//! Detector unico di SQL injection nel codice applicativo (Rust/Python/TS/JS).
//! Sostituisce mcp-db::check_injection_patterns (rimosso) e la logica del
//! tool nexus_sec_sql_injection_check. ADR 0021.
//!
//! Principio: la SQL injection e' un difetto del CODICE che costruisce la query,
//! non del file `.sql` statico. Il detector gira solo su file di codice e cerca il
//! pattern: keyword SQL + interpolazione/concatenazione di valore non-costante,
//! escludendo le query parametrizzate (placeholder $1/?/:name/%s + bind).

use std::sync::LazyLock;

use regex::Regex;

pub struct InjectionFinding {
    pub line: usize,
    pub severity: String, // "high" | "medium"
    pub snippet: String,
    pub detail: String,
}

/// Linguaggio dedotto dall'estensione del file. `Unsupported` => nessuna analisi.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    TsJs,
    Unsupported,
}

fn detect_lang(file_path: &str) -> Lang {
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Lang::Rust,
        "py" => Lang::Python,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Lang::TsJs,
        _ => Lang::Unsupported,
    }
}

// Keyword SQL: serve almeno una di queste perche' la stringa sia una query.
// La keyword NON deve essere preceduta da `.` o `:` (evita i falsi positivi dei
// METODI/funzioni omonime: `.insert(`/`.into()`/`.update(`/`.delete(`/`.from(`,
// `Type::from(`, ecc. — di mappe, iterator, conversioni e query-builder, che non
// sono SQL). Una vera query ha la keyword dentro un letterale stringa (preceduta
// da apice/spazio) o a inizio riga, entrambi coperti da `(?:^|[^.:\w])`.
static SQL_KEYWORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^.:\w])(SELECT|INSERT|UPDATE|DELETE|FROM|WHERE|JOIN|VALUES|INTO)\b")
        .unwrap()
});

// Nomi di variabile che suggeriscono input esterno => severity high.
static EXTERNAL_INPUT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(user|input|param|req|request|body|query|arg|name|email|search|filter|form|payload|data)",
    )
    .unwrap()
});

// --- Rust ---
// format!("...") con almeno un placeholder {} o {ident}.
static RS_FORMAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"format!\s*\(\s*".*\{[^}]*\}.*""#).unwrap());
// Concatenazione: chiusura stringa (eventuale .to_string()) seguita da + &var.
static RS_CONCAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""\s*(?:\.to_string\(\))?\s*\+\s*&?\s*[a-zA-Z_]"#).unwrap());
// push_str con variabile (non literal).
static RS_PUSH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\.push_str\s*\(\s*&?\s*[a-zA-Z_]\w*"#).unwrap());

// --- Python ---
// f-string con interpolazione {ident}.
static PY_FSTRING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"f["'].*\{[^}]*[a-zA-Z_][^}]*\}.*["']"#).unwrap());
// "..." % var  (interpolazione percent).
static PY_PERCENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']\s*%\s*[a-zA-Z_(]"#).unwrap());
// "..." + var
static PY_CONCAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']\s*\+\s*[a-zA-Z_]"#).unwrap());
// "...".format(...)
static PY_FORMAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']\s*\.format\s*\("#).unwrap());

// --- TS/JS ---
// Template literal con ${ident}.
static TS_TEMPLATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"`[^`]*\$\{[^}]*[a-zA-Z_][^}]*\}[^`]*`"#).unwrap());
// Concatenazione "..." + var.
static TS_CONCAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']\s*\+\s*[a-zA-Z_]"#).unwrap());
// Estrae i candidati identificatore (variabili) da una riga per valutarne il
// nome ai fini della severity. Cattura ogni token alfanumerico; le keyword note
// di linguaggio e SQL vengono filtrate in `severity_for`.
static IDENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[a-zA-Z_]\w*"#).unwrap());

// Keyword da ignorare nella valutazione della severity (non sono nomi di
// variabile-input ma costrutti del linguaggio o SQL che contengono per caso
// substring sospette, es. `format` contiene "form").
const IGNORED_IDENTS: &[&str] = &[
    "format", "println", "print", "string", "to_string", "push_str", "execute",
    "select", "insert", "update", "delete", "from", "where", "join", "values",
    "into", "order", "group", "having", "limit", "offset", "and", "or", "not",
    "set", "table", "by",
];

/// True se la riga e' una query parametrizzata sicura (whitelist universale +
/// per linguaggio). In tal caso NON va segnalata.
fn is_parameterized_safe(lang: Lang, line: &str) -> bool {
    // Whitelist Rust: macro sqlx compile-checked e .bind(...).
    if lang == Lang::Rust
        && (line.contains("sqlx::query!")
            || line.contains("sqlx::query_as!")
            || line.contains("sqlx::query_scalar!")
            || line.contains("query_as!")
            || line.contains("query_scalar!")
            || line.contains(".bind("))
    {
        return true;
    }
    // Whitelist Python: .execute(...) con secondo argomento (params tuple/dict).
    if lang == Lang::Python && python_execute_with_params(line) {
        return true;
    }
    false
}

/// Riconosce `cursor.execute("...", params)` / `.execute("...", (...))` con un
/// secondo argomento => query parametrizzata sicura.
fn python_execute_with_params(line: &str) -> bool {
    if let Some(pos) = line.find(".execute(") {
        let after = &line[pos + ".execute(".len()..];
        // C'e' una virgola di primo livello dopo la stringa SQL => secondo arg.
        return has_top_level_comma(after);
    }
    false
}

/// Cerca una virgola a livello 0 di parentesi dentro `s`, saltando i contenuti
/// stringa. Usata per capire se `.execute("...", params)` ha un secondo arg.
fn has_top_level_comma(s: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_str: Option<char> = None;
    let mut prev = '\0';
    for ch in s.chars() {
        match in_str {
            Some(q) => {
                if ch == q && prev != '\\' {
                    in_str = None;
                }
            }
            None => match ch {
                '"' | '\'' => in_str = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    if depth == 0 {
                        return false; // chiusura della execute( senza virgola
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => return true,
                _ => {}
            },
        }
        prev = ch;
    }
    false
}

/// True se la riga contiene un costrutto di interpolazione/concatenazione del
/// linguaggio applicato a una stringa (potenziale costruzione query dinamica).
fn has_interpolation(lang: Lang, line: &str) -> bool {
    match lang {
        Lang::Rust => {
            RS_FORMAT_RE.is_match(line)
                || RS_CONCAT_RE.is_match(line)
                || RS_PUSH_RE.is_match(line)
        }
        Lang::Python => {
            PY_FSTRING_RE.is_match(line)
                || PY_PERCENT_RE.is_match(line)
                || PY_CONCAT_RE.is_match(line)
                || PY_FORMAT_RE.is_match(line)
        }
        Lang::TsJs => TS_TEMPLATE_RE.is_match(line) || TS_CONCAT_RE.is_match(line),
        Lang::Unsupported => false,
    }
}

/// Determina la severity in base al nome della variabile interpolata.
/// high se suggerisce input esterno, medium altrimenti.
fn severity_for(line: &str) -> &'static str {
    // Valuta ogni identificatore della riga (potenziale nome variabile), saltando
    // le keyword di linguaggio/SQL che potrebbero contenere per caso una substring
    // sospetta (es. `format` -> "form"). high solo se un nome-variabile reale
    // suggerisce input esterno.
    for m in IDENT_RE.find_iter(line) {
        let ident = m.as_str();
        let lower = ident.to_ascii_lowercase();
        if IGNORED_IDENTS.contains(&lower.as_str()) {
            continue;
        }
        if EXTERNAL_INPUT_RE.is_match(ident) {
            return "high";
        }
    }
    "medium"
}

/// Rileva costruzione non sicura di query SQL nel codice applicativo.
/// Line-based, O(n). Ritorna vec vuoto per linguaggi non supportati (inclusi i
/// file `.sql`, che non vengono mai analizzati: ADR 0021).
pub fn detect_sql_injection(file_path: &str, source: &str) -> Vec<InjectionFinding> {
    let lang = detect_lang(file_path);
    if lang == Lang::Unsupported {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        let line = raw.trim();
        // Salta righe di commento evidenti per linguaggio.
        if line.starts_with("//") || line.starts_with('#') || line.starts_with('*') {
            continue;
        }
        // Condizione 1: keyword SQL presente.
        if !SQL_KEYWORD_RE.is_match(line) {
            continue;
        }
        // Whitelist: query parametrizzata sicura => non segnalare.
        if is_parameterized_safe(lang, line) {
            continue;
        }
        // Condizione 2+3: interpolazione/concatenazione di valore non-costante.
        if !has_interpolation(lang, line) {
            continue;
        }

        let severity = severity_for(line).to_string();
        let mut snippet = line.to_string();
        if snippet.chars().count() > 160 {
            snippet = snippet.chars().take(160).collect();
        }
        findings.push(InjectionFinding {
            line: idx + 1,
            severity,
            snippet,
            detail: "Costruzione di query SQL via interpolazione/concatenazione di stringa rilevata. \
                     Usa query parametrizzate (placeholder $1/?/:name + bind) invece di inserire \
                     valori direttamente nella stringa SQL."
                .into(),
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(file: &str, src: &str) -> usize {
        detect_sql_injection(file, src).len()
    }

    // --- VERI POSITIVI ---

    #[test]
    fn rust_format_select_user_input_high() {
        let src = r#"let q = format!("SELECT * FROM users WHERE name = '{}'", user_input);"#;
        let f = detect_sql_injection("x.rs", src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, "high");
    }

    #[test]
    fn rust_concat_delete() {
        let src = r#"let q = "DELETE FROM logs WHERE id = ".to_string() + &id;"#;
        assert_eq!(count("x.rs", src), 1);
    }

    #[test]
    fn rust_push_str_query() {
        // push_str con variabile su una riga che contiene anche keyword SQL.
        let src = r#"let _ = "SELECT id FROM t".to_string(); q.push_str(&user_filter);"#;
        assert_eq!(count("x.rs", src), 1);
    }

    #[test]
    fn python_fstring_high() {
        let src = r#"sql = f"SELECT * FROM t WHERE x = {user_param}""#;
        let f = detect_sql_injection("x.py", src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, "high");
    }

    #[test]
    fn python_percent_interpolation() {
        // `%s` e' placeholder ma `% val` e' interpolazione Python => injection.
        let src = r#"sql = "UPDATE t SET a = %s" % val"#;
        assert_eq!(count("x.py", src), 1);
    }

    #[test]
    fn ts_template_literal_high() {
        let src = "const q = `SELECT * FROM users WHERE id = ${userId}`;";
        let f = detect_sql_injection("x.ts", src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, "high");
    }

    #[test]
    fn rust_dynamic_table_name_medium() {
        // Nessun nome che suggerisce input esterno => medium.
        let src = r#"let q = format!("SELECT * FROM {} ORDER BY id", table);"#;
        let f = detect_sql_injection("x.rs", src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, "medium");
    }

    // --- VERI NEGATIVI ---

    #[test]
    fn rust_sqlx_query_macro_safe() {
        let src = r#"sqlx::query!("SELECT * FROM users WHERE id = $1", id);"#;
        assert_eq!(count("x.rs", src), 0);
    }

    #[test]
    fn rust_bind_safe() {
        let src = r#"sqlx::query("SELECT * FROM t WHERE id = $1").bind(id);"#;
        assert_eq!(count("x.rs", src), 0);
    }

    #[test]
    fn python_execute_with_params_safe() {
        let src = r#"cursor.execute("SELECT * FROM t WHERE id = %s", (id,))"#;
        assert_eq!(count("x.py", src), 0);
    }

    #[test]
    fn ts_parameterized_query_safe() {
        let src = r#"db.query("SELECT * FROM t WHERE id = $1", [id]);"#;
        assert_eq!(count("x.ts", src), 0);
    }

    #[test]
    fn rust_format_without_sql_keyword_safe() {
        let src = r#"let msg = format!("hello {}", name);"#;
        assert_eq!(count("x.rs", src), 0);
    }

    #[test]
    fn rust_map_insert_method_not_flagged() {
        // `.insert(` e' un metodo di mappa/set, NON la keyword SQL INSERT: la
        // presenza di un `format!` sulla stessa riga non deve produrre finding.
        let src = r#"obj.insert("error".into(), format!("boom {code}"));"#;
        assert_eq!(count("x.rs", src), 0);
    }

    #[test]
    fn rust_into_and_from_methods_not_flagged() {
        // `.into()` / `.from(` sono conversioni/builder, non keyword SQL.
        let src = r#"let v = Foo::from(bar).into(); let s = format!("v={v}");"#;
        assert_eq!(count("x.rs", src), 0);
    }

    #[test]
    fn rust_sql_keyword_in_string_still_flagged_after_method_exclusion() {
        // Regressione inversa: la keyword dentro un letterale (preceduta da
        // apice) resta un vero positivo anche dopo l'esclusione dei metodi.
        let src = r#"let q = format!("INSERT INTO t VALUES ('{}')", v);"#;
        assert_eq!(count("x.rs", src), 1);
    }

    #[test]
    fn rust_bcrypt_hash_literal_safe() {
        // Il caso incident: hash bcrypt in una stringa, senza keyword SQL.
        let src = r#"let hash = "$2a$10$dummyhashfortestingonly";"#;
        assert_eq!(count("x.rs", src), 0);
    }

    #[test]
    fn sql_file_never_analyzed() {
        // Anche con contenuto che sembrerebbe sospetto, i `.sql` non sono analizzati.
        let src = "INSERT INTO users VALUES ('a', '$2a$10$hash');";
        assert_eq!(count("schema.sql", src), 0);
    }
}
