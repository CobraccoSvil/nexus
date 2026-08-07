//! Enforcement porte hardcoded nei tool di scrittura file.
//!
//! Quando l'agente prova a scrivere/modificare codice che contiene una porta
//! TCP hardcoded fuori dal bucket Nexus (20000..40000), il tool restituisce
//! un messaggio di rifiuto che istruisce l'uso di `request_port(label=...)`.
//!
//! Il flag globale e' letto dalla tabella `settings` (chiave
//! `agent.enforce_port_allocation`) tramite `nexus_auth::get_bool_setting`, che
//! e' anche il posto in cui vive la cache (TTL 60s, chiavata sul pool).
//!
//! Vedi ADR 0010 per il contesto della decisione.

use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::PgPool;

const NEXUS_PORT_MIN: u32 = 20000;
const NEXUS_PORT_MAX: u32 = 40000;

/// Patterns di file da escludere dallo scan.
///
/// Solo .env* resta whitelistato come posto canonico dove dichiarare le
/// porte come variabili d ambiente. docker-compose* e Dockerfile* NON
/// sono piu skippati: i pattern dedicati (ports:, EXPOSE, range) e la
/// whitelist gestiscono le forme legittime, le altre vengono rifiutate.
const SKIP_FILE_PREFIXES: &[&str] = &[".env"];

const ENV_PORT_HINTS: &[&str] = &[
    "process.env.PORT",
    "os.environ.get(\"PORT\")",
    "os.environ.get('PORT')",
    "os.environ[\"PORT\"]",
    "os.environ['PORT']",
    "env::var(\"PORT\")",
    "env::var('PORT')",
    "getenv(\"PORT\")",
    "getenv('PORT')",
    "PORT=$",
    "PORT=${",
    "${PORT}",
    "${PORT_",
    "$PORT_",
    "request_port(",
];

static PORT_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)\.listen\s*\(\s*(\d{2,5})\b").unwrap(),
        Regex::new(r#"(?i)\.bind\s*\(\s*['"]?[\d.]*:(\d{2,5})\b"#).unwrap(),
        Regex::new(r"(?i)\blisten\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bPORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bBACKEND_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bFRONTEND_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bDATABASE_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bDB_PORT\s*=\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\b(?:host|listen)_port\s*[=:]\s*(\d{2,5})\b").unwrap(),
        Regex::new(r"(?i)\bport\s*:\s*(\d{2,5})\b").unwrap(),
        Regex::new(r#"(?i)\bports["']?\s*:\s*\[\s*(\d{2,5})\b"#).unwrap(),
        // YAML list item: - 3000:3000 (mapping host:container)
        Regex::new(r"^\s*-\s*(\d{2,5})\s*:\s*\d{2,5}\b").unwrap(),
        // YAML list item plain: - 3000
        Regex::new(r"^\s*-\s+(\d{2,5})\s*$").unwrap(),
        Regex::new(r"(?i)\bEXPOSE\s+(\d{2,5})\b").unwrap(),
        // CLI flag dei dev-server (vite/next/astro/nuxt) negli script del
        // package.json: `vite --port 21954` / `--port=21954`. Solo la forma LUNGA
        // `--port`: la forma breve `-p` e' troppo ambigua (mkdir -p, docker run -p
        // host:cont). Una porta hardcoded qui (anche dentro il bucket) bypassava
        // l'allocazione -> disallineamento col .env e porte che si rimescolano.
        Regex::new(r"(?i)--port[=\s]+(\d{2,5})\b").unwrap(),
    ]
});

/// La porta dentro un URL (`http://localhost:3000`, `ws://127.0.0.1:8080`).
///
/// Sta FUORI da [`PORT_REGEXES`] perche' alimenta un controllo diverso, e la
/// differenza e' quella che rende il riconoscimento utilizzabile: dentro un
/// `.env` un URL con porta e' spesso LEGITTIMO (`DATABASE_URL=...:5432/db` e'
/// il modo previsto di connettersi al Postgres), quindi non puo' entrare nel
/// criterio "deve essere allocata al progetto" senza rompere ogni backend.
/// Quello che non e' mai legittimo e' puntare a un'APPLICAZIONE di Nexus:
/// e' il solo giudizio che questa cattura alimenta.
///
/// MISURATO il 06/08/2026 (agenda-medica): il frontend generato aveva
/// `VITE_API_URL=http://localhost:3000` — la web-ide di Nexus — e nessuna
/// delle regex esistenti poteva vederlo, perche' cercano tutte una porta
/// preceduta da `PORT=`, `.listen(`, `port:` o simili. La politica sui `.env`
/// era gia' corretta (vedi `ports_needing_allocation`, che cita un incidente
/// del 22/07 con la stessa porta): mancava la RILEVAZIONE.
static URL_PORT_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)://[^/\s:@]+:(\d{2,5})\b").unwrap());

static RANGE_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\brange\s+(\d{2,5})\s*-\s*(\d{2,5})\b").unwrap());

/// Porte di APPLICAZIONI Nexus citate nel contenuto, da qualunque forma le si
/// scriva (URL o dichiarazione). Un progetto utente non deve mai puntarci:
/// chiederebbe i propri dati all'interfaccia o all'API di Nexus.
///
/// Vale anche nei `.env`, che sono esenti dagli altri controlli: li' e' il
/// posto giusto per una configurazione, ma non per QUESTO valore.
pub fn collect_nexus_app_ports(content: &str) -> Vec<PortFinding> {
    // Le forme DICHIARATE passano dal punto unico di parsing (regola L), che
    // porta con se' whitelist, hint env e dedup gia' verificati.
    let mut findings = collect_ports(content, |p| {
        u16::try_from(p).is_ok_and(nexus_tool_kit::ports::is_nexus_app_port)
    });
    // Gli URL no: nessuna regex esistente li vede, ed e' la forma da cui il
    // difetto e' passato.
    for (idx, raw_line) in content.lines().enumerate() {
        for caps in URL_PORT_REGEX.captures_iter(raw_line) {
            let Some(port) = caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) else {
                continue;
            };
            if u16::try_from(port).is_ok_and(nexus_tool_kit::ports::is_nexus_app_port) {
                findings.push(PortFinding {
                    line: idx + 1,
                    port,
                    snippet: raw_line.chars().take(200).collect(),
                    origin: PortOrigin::Literal,
                });
            }
        }
    }
    findings.sort_by_key(|f| (f.line, f.port));
    findings.dedup_by_key(|f| (f.line, f.port));
    findings
}

/// Il messaggio di rifiuto: dice QUALE servizio di Nexus e' stato preso di
/// mira, perche' «porta non ammessa» non aiuta a correggere.
fn format_nexus_app_message(path: &str, findings: &[PortFinding]) -> String {
    let mut msg = format!(
        "\u{274C} [Errore: scrittura su '{}' rifiutata. L'indirizzo punta a un servizio di NEXUS, \
         non a un servizio di questo progetto.]\n\n\
         Un'applicazione del progetto non deve mai chiamare le porte dell'infrastruttura: \
         chiederebbe i propri dati all'interfaccia o all'API di Nexus invece che al proprio backend.\n\n\
         Dettaglio:\n",
        path
    );
    for f in findings.iter().take(10) {
        msg.push_str(&format!(
            "  - riga {}: porta {} ({}) | {}\n",
            f.line,
            f.port,
            descrizione_porta_nexus(f.port),
            f.snippet.trim()
        ));
    }
    msg.push_str(
        "\nAzione richiesta:\n\
         1. Chiama `request_port(label=\"backend\")` (o il servizio che ti serve) e usa il numero ritornato.\n\
         2. Nei file di configurazione scrivi quell'indirizzo, non una porta scelta a mano.\n",
    );
    msg
}

/// A quale servizio Nexus appartiene la porta, per il messaggio di rifiuto.
fn descrizione_porta_nexus(port: u32) -> &'static str {
    match port {
        3000 | 4001 => "interfaccia web di Nexus",
        4000 => "API di mcp-core",
        4010 => "admin-service",
        4030 => "doc-service",
        4050 => "plugin-service",
        4060 => "gateway AI",
        3001 => "Grafana",
        9090 => "Prometheus",
        _ => "servizio interno di Nexus",
    }
}

/// Default di variabile in stile shell/Docker Compose: `${VAR:-NNNN}`. Il valore
/// dopo `:-` e' una porta EFFETTIVA (usata quando la variabile non e' impostata,
/// tipico dell'avvio manuale del compose), quindi va trattato come hardcoded
/// anche se la riga usa una variabile env. Senza questo controllo i default fuori
/// bucket nei docker-compose (es. `${PORT_FRONTEND:-20001}`) sfuggivano al
/// guard-rail e facevano partire container su porte non ammissibili (ADR 0010).
static DEFAULT_PORT_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$\{[A-Za-z_][A-Za-z0-9_]*:?[-=](\d{2,5})\}").unwrap());

/// Fallback env con porta numerica letterale: `process.env.PORT || 5000`,
/// `os.environ.get("PORT", 5000)`, `env::var("PORT").unwrap_or("3000")`, ecc.
/// Come `${VAR:-NNNN}`, il default e' una porta EFFETTIVA (usata quando la
/// variabile non e' impostata) e va trattato come hardcoded, anche se la riga
/// contiene un hint env: senza questo controllo lo skip `ENV_PORT_HINTS`
/// faceva passare l'intera riga e i fallback fuori-bucket sfuggivano al
/// guard-rail (incidente Beauty-Book: `const port = process.env.PORT || 5000`).
///
/// Anti-falsi-positivi: il NOME della variabile env deve contenere il segmento
/// `PORT` delimitato da `_` (PORT, APP_PORT, PORT_BACKEND, VITE_PORT) — cosi'
/// `process.env.TIMEOUT || 5000` e `process.env.REPORT_LIMIT || 5000` NON
/// matchano (REPORT contiene PORT ma non come segmento delimitato).
static ENV_FALLBACK_PORT_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    // Segmento nome variabile che e' PORT o *_PORT o PORT_* (case-insensitive
    // sul confine, ma PORT in maiuscolo nei nomi env reali).
    let name = r"(?:[A-Za-z0-9]+_)*PORT(?:_[A-Za-z0-9]+)*";
    vec![
        // JS/TS: process.env.PORT || 5000  /  ?? 5000  (+ import.meta.env per Vite)
        Regex::new(&format!(
            r#"(?i)(?:process|import\.meta)\.env\.{name}\s*(?:\|\||\?\?)\s*['"]?(\d{{2,5}})\b"#
        ))
        .unwrap(),
        // JS/TS bracket: process.env["PORT"] || 5000
        Regex::new(&format!(
            r#"(?i)(?:process|import\.meta)\.env\[\s*['"]{name}['"]\s*\]\s*(?:\|\||\?\?)\s*['"]?(\d{{2,5}})\b"#
        ))
        .unwrap(),
        // Python: os.environ.get("PORT", 5000) / os.getenv('PORT', '5000') / getenv(...)
        Regex::new(&format!(
            r#"(?i)(?:os\.environ\.get|os\.getenv|getenv)\(\s*['"]{name}['"]\s*,\s*['"]?(\d{{2,5}})\b"#
        ))
        .unwrap(),
        // Python: os.environ.get("PORT") or 5000 / int(os.getenv("PORT") or 8080)
        Regex::new(&format!(
            r#"(?i)(?:os\.environ\.get|os\.getenv|getenv)\(\s*['"]{name}['"]\s*\)\s*or\s+['"]?(\d{{2,5}})\b"#
        ))
        .unwrap(),
        // Rust: env::var("PORT").unwrap_or("3000") / .unwrap_or_else(|_| "3000".into())
        // Finestra bounded [^;\n]{0,80} per catene .ok().and_then(...).
        Regex::new(&format!(
            r#"env::var\(\s*"{name}"\s*\)[^;\n]{{0,80}}?unwrap_or(?:_else)?\s*\(\s*(?:\|[^|]{{0,20}}\|\s*)?"?(\d{{2,5}})\b"#
        ))
        .unwrap(),
        // PHP/Kotlin elvis: getenv("PORT") ?: 8080
        Regex::new(&format!(
            r#"(?i)getenv\(\s*['"]{name}['"]\s*\)\s*\?:\s*['"]?(\d{{2,5}})\b"#
        ))
        .unwrap(),
    ]
});

#[derive(Debug)]
pub enum PortScanOutcome {
    Allowed,
    Reject(Vec<PortFinding>),
}

/// Provenienza della porta rilevata, valorizzata QUI (nel punto di push, dove
/// il regex che ha trovato la porta e' noto con certezza) e MAI re-indovinata
/// a valle da un secondo giudizio testuale sullo snippet (regola L/M): un
/// consumatore come `security::resource_linter` che rifacesse la domanda "ha
/// un fallback env?" sul solo testo dello snippet divergerebbe strutturalmente
/// da questo produttore ogni volta che aggiunge un pattern qui e non li'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortOrigin {
    /// La porta e' il valore di FALLBACK di una lettura da env riconosciuta da
    /// [`DEFAULT_PORT_REGEX`] o [`ENV_FALLBACK_PORT_REGEXES`] (`${VAR:-N}`,
    /// `process.env.PORT || N`, `env::var(..).unwrap_or(N)`,
    /// `os.getenv(..) or N`, ...).
    EnvFallback,
    /// Porta letterale semplice, senza lettura da env riconosciuta sulla riga.
    Literal,
}

#[derive(Debug, Clone)]
pub struct PortFinding {
    pub line: usize,
    pub port: u32,
    pub snippet: String,
    pub origin: PortOrigin,
}

/// Il flag `agent.enforce_port_allocation` (regola I). L'enforcement e'
/// fail-closed: ricade su `true` in tutti e TRE i casi in cui non c'e' un
/// booleano da leggere — chiave assente, DB irraggiungibile, valore fuori dal
/// vocabolario di `nexus_auth::parse_setting_bool`. Un controllo di sicurezza
/// non si spegne perche' qualcuno ha scritto una parola che nessuno ha capito.
///
/// La lettura passa dal punto unico `nexus_auth` (regola L), che possiede la
/// query e la cache — TTL 60s, chiavata su `pool_identity`. La cache locale che
/// stava qui era un secondo TTL identico, sopra la stessa tabella, ma GLOBALE:
/// col pool di un progetto avrebbe servito il valore del meta, che e' l'errore
/// documentato da `pool_identity`.
///
/// L'errore DB resta LOGGATO: `get_bool_setting` lo propaga, e qui e' l'unico
/// punto che sa distinguere «flag assente» (silenzio legittimo) da «non ho
/// potuto leggere» (regola M). Prima del ripiego, quindi, il warn.
pub async fn is_enforcement_enabled(db: &PgPool) -> bool {
    nexus_auth::get_bool_setting(db, "agent.enforce_port_allocation")
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(
                error = %err,
                "port_scanner: lettura setting agent.enforce_port_allocation fallita, default=true"
            );
            None
        })
        .unwrap_or(true)
}

fn should_skip_path(path: &str) -> bool {
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    let lower = file_name.to_lowercase();
    for prefix in SKIP_FILE_PREFIXES {
        if lower.starts_with(&prefix.to_lowercase()) {
            return true;
        }
    }
    false
}

/// Porte di servizi-infrastruttura ben note a cui i progetti si CONNETTONO come
/// client (DB, cache, code, search), NON che bindano. Una `port: N` / `DB_PORT=N`
/// verso queste e' una connessione legittima, non un binding hardcoded da
/// governare: lo scanner non distingue `app.listen(N)` (binding) da `port: N`
/// dentro un client di connessione, e il regex generico `port:` (PORT_REGEXES)
/// catturava `port: 5433` di `new Pool({...})` (DB Nexus) -> rifiuto deterministico
/// di edit_file -> l'agente ripete identico -> loop force-close. Include l'infra
/// Nexus (5433 postgres-nexus, 6379 redis, 6333/6334 qdrant, regola E) + i DB/cache
/// di terze parti piu' comuni. Falso-negativo accettabile: un progetto che bindasse
/// davvero una di queste e' raro (Nexus provisiona i DB, i servizi usano il bucket).
const WELL_KNOWN_SERVICE_PORTS: &[u32] = &[
    5432, 5433,  // PostgreSQL (5433 = ideai-postgres-nexus)
    3306,  // MySQL / MariaDB
    27017, // MongoDB
    6379,  // Redis
    6333, 6334,  // Qdrant (REST / gRPC)
    1433,  // SQL Server
    5672,  // RabbitMQ (amqp)
    9200,  // Elasticsearch
    11211, // Memcached
];

fn port_is_violating(port: u32) -> bool {
    if (NEXUS_PORT_MIN..NEXUS_PORT_MAX).contains(&port) {
        return false;
    }
    if port < 1024 {
        return false;
    }
    if WELL_KNOWN_SERVICE_PORTS.contains(&port) {
        return false;
    }
    true
}

/// Colleziona le porte rilevate nel contenuto che soddisfano `keep`. Punto
/// unico di parsing (regola L): sia lo scan delle violazioni fuori-bucket sia
/// quello delle porte host nel bucket (per la verifica di allocazione) usano
/// gli stessi regex/whitelist, cambiando solo il predicato sulla porta.
fn collect_ports(content: &str, keep: impl Fn(u32) -> bool) -> Vec<PortFinding> {
    let mut findings: Vec<PortFinding> = Vec::new();
    let snip = |raw_line: &str| {
        if raw_line.len() > 200 {
            format!("{}...", &raw_line[..200])
        } else {
            raw_line.to_string()
        }
    };
    for (line_idx, raw_line) in content.lines().enumerate() {
        // I default `${VAR:-NNNN}` sono porte EFFETTIVE: vengono usate quando la
        // variabile non e' impostata. Vanno validati SEMPRE, anche se la riga
        // contiene un hint env (es. `${PORT_BACKEND:-20002}` contiene `${PORT_`):
        // altrimenti il default fuori-bucket o nel-bucket-non-allocato sfugge al
        // guard-rail e fa partire container su porte non ammissibili (ADR 0010).
        for caps in DEFAULT_PORT_REGEX.captures_iter(raw_line) {
            if let Some(m) = caps.get(1) {
                if let Ok(port) = m.as_str().parse::<u32>() {
                    if keep(port) {
                        findings.push(PortFinding {
                            line: line_idx + 1,
                            port,
                            snippet: snip(raw_line),
                            origin: PortOrigin::EnvFallback,
                        });
                    }
                }
            }
        }
        // Fallback env con porta letterale (`process.env.PORT || 5000`, ecc.):
        // validati SEMPRE, anche se la riga contiene un hint env. Stesso
        // principio del `${VAR:-NNNN}` sopra. DEVE precedere lo skip
        // `ENV_PORT_HINTS`, altrimenti la riga col fallback verrebbe saltata.
        for regex in ENV_FALLBACK_PORT_REGEXES.iter() {
            for caps in regex.captures_iter(raw_line) {
                if let Some(m) = caps.get(1) {
                    if let Ok(port) = m.as_str().parse::<u32>() {
                        if keep(port) {
                            findings.push(PortFinding {
                                line: line_idx + 1,
                                port,
                                snippet: snip(raw_line),
                                origin: PortOrigin::EnvFallback,
                            });
                        }
                    }
                }
            }
        }
        if ENV_PORT_HINTS.iter().any(|hint| raw_line.contains(hint)) {
            continue;
        }
        for caps in RANGE_REGEX.captures_iter(raw_line) {
            let lo = caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok());
            let hi = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
            for opt in [lo, hi].iter().flatten() {
                if keep(*opt) {
                    findings.push(PortFinding {
                        line: line_idx + 1,
                        port: *opt,
                        snippet: snip(raw_line),
                        origin: PortOrigin::Literal,
                    });
                }
            }
        }
        for regex in PORT_REGEXES.iter() {
            for caps in regex.captures_iter(raw_line) {
                if let Some(port_str) = caps.get(1) {
                    if let Ok(port) = port_str.as_str().parse::<u32>() {
                        if keep(port) {
                            findings.push(PortFinding {
                                line: line_idx + 1,
                                port,
                                snippet: snip(raw_line),
                                origin: PortOrigin::Literal,
                            });
                        }
                    }
                }
            }
        }
    }
    // Dedup difensivo: la stessa (riga, porta) puo' essere catturata da piu'
    // regex (es. un fallback env che matcha anche un PORT_REGEX generico).
    findings.sort_by_key(|f| (f.line, f.port));
    findings.dedup_by_key(|f| (f.line, f.port));
    findings
}

/// Wrapper pub: porte fuori-bucket (violazioni dirette). Punto unico riusato
/// dal linter di governance (`security::port_linter`).
pub fn collect_out_of_bucket_ports(content: &str) -> Vec<PortFinding> {
    collect_ports(content, port_is_violating)
}

/// Wrapper pub: porte host NEL bucket Nexus (lecite solo se allocate). Il
/// chiamante filtra poi su `nexus_port_allocations`.
pub fn collect_bucket_ports(content: &str) -> Vec<PortFinding> {
    collect_ports(content, port_in_bucket)
}

/// Porta dentro il bucket Nexus (20000-39999). Le porte nel bucket sono lecite
/// SOLO se allocate per il progetto (vedi `reject_unallocated_bucket_ports`).
fn port_in_bucket(port: u32) -> bool {
    (NEXUS_PORT_MIN..NEXUS_PORT_MAX).contains(&port)
}

/// PUNTO UNICO del criterio "quali porte di questo file vanno verificate contro
/// le allocazioni del progetto", funzione PURA (testabile senza DB).
///
/// Nei `.env*` vale per QUALUNQUE porta non riservata: il file e' il posto
/// canonico dove DICHIARARE la porta (percio' `scan_content` non lo tratta come
/// hardcode vietato), ma il numero deve comunque venire da `request_port`.
/// Altrove il criterio resta il bucket Nexus.
///
/// Perche' serve la distinzione: un `PORT=3000` nel `.env` non e' nel bucket,
/// quindi il controllo sul range non lo vedeva, e il file era saltato del tutto
/// dallo scan -- passava da entrambe le verifiche (incidente 2026-07-22, con in
/// piu' la sfortuna che 3000 e' la porta della UI di Nexus).
fn ports_needing_allocation(path: &str, content: &str) -> Vec<PortFinding> {
    if should_skip_path(path) {
        collect_ports(content, |p| p >= 1024)
    } else {
        collect_ports(content, port_in_bucket)
    }
}

pub fn scan_content(path: &str, content: &str) -> PortScanOutcome {
    if should_skip_path(path) {
        return PortScanOutcome::Allowed;
    }
    let findings = collect_ports(content, port_is_violating);
    if findings.is_empty() {
        PortScanOutcome::Allowed
    } else {
        PortScanOutcome::Reject(findings)
    }
}

/// Enforcement "allocation-aware" (ADR 0010 + richiesta utente): una porta host
/// NEL bucket Nexus e' lecita solo se REALMENTE allocata per il progetto in
/// `nexus_port_allocations` (cioe' ottenuta via `request_port`). Senza questo
/// controllo l'agente poteva scrivere una porta a caso nel range (es. 20001)
/// nei docker-compose senza passare dall'allocatore: numericamente valida ma
/// non tracciata, con rischio di collisione tra progetti. Ritorna `Some(msg)`
/// se ci sono porte nel bucket non allocate (la write va rifiutata), altrimenti
/// `None`. NON tocca le porte fuori-bucket (gestite da `scan_content`).
fn format_unallocated_message(path: &str, findings: &[PortFinding]) -> String {
    let mut msg = format!(
        "\u{274C} [Errore: scrittura su '{}' rifiutata. Sono state rilevate {} porta/e NON allocate a questo progetto.]\n\n\
         La porta non si sceglie a mano: chiedila a `request_port(label=\"...\")`, che ne \
         alloca una libera, la registra ed evita le collisioni con gli altri progetti e \
         con i servizi di Nexus. Poi scrivi ESATTAMENTE il numero che ti restituisce.\n\n\
         Dettaglio:\n",
        path,
        findings.len()
    );
    for f in findings.iter().take(10) {
        msg.push_str(&format!(
            "  - riga {}: porta {} | {}\n",
            f.line,
            f.port,
            f.snippet.trim()
        ));
    }
    if findings.len() > 10 {
        msg.push_str(&format!(
            "  ... e altri {} riscontri.\n",
            findings.len() - 10
        ));
    }
    msg.push_str(
        "\nUna porta nel range Nexus NON va scelta a mano: anche se il numero e' nel bucket, \
         deve essere ALLOCATA dall'allocatore per evitare collisioni tra progetti.\n\n\
         Azione richiesta:\n\
         1. Chiama `request_port(label=\"<nome_servizio>\")` per ciascun servizio (es. 'backend', 'frontend').\n\
         2. Usa la porta HOST ritornata nel mapping docker (es. ports: <porta_allocata>:<porta_container>) \
            o in process.env.PORT; la porta CONTAINER resta quella dell'app.\n\
         3. Riprova la scrittura.\n\
         \nVedi <port_allocation> nel system prompt e ADR 0010.",
    );
    msg
}

/// Punto unico di enforcement porte in scrittura (write_file/edit_file): gate
/// setting -> scan fuori-bucket -> check allocazione nel bucket. Su violazione
/// registra l'audit in `nexus_resource_audit` (action `port_hardcode_rejected`
/// o `port_unallocated_rejected`, outcome `blocked`) e ritorna `Some(messaggio)`
/// di rifiuto. `None` = scrittura ammessa. Consolida i due blocchi prima
/// duplicati in files.rs (regola L) e aggiunge la traccia di sicurezza che
/// prima mancava (il rifiuto era solo un tool_result, invisibile all'audit).
pub async fn enforce_write_ports(
    ctx: &nexus_agent_tools::ToolContextCore,
    tool_name: &str,
    path: &str,
    content: &str,
) -> Option<String> {
    if !is_enforcement_enabled(ctx.db.as_ref()).await {
        return None;
    }
    // PRIMO controllo, e vale anche nei `.env` (esenti dagli altri): puntare a
    // un'applicazione di Nexus non e' mai una configurazione legittima, mentre
    // una porta di datastore nello stesso file lo e'. Precede gli altri perche'
    // il messaggio e' piu' specifico: dice QUALE servizio si sta chiamando.
    let app_nexus = collect_nexus_app_ports(content);
    let rejection: Option<String> = if !app_nexus.is_empty() {
        audit_port_rejection(ctx, "port_nexus_app_rejected", tool_name, path, &app_nexus);
        Some(format_nexus_app_message(path, &app_nexus))
    } else if let PortScanOutcome::Reject(findings) = scan_content(path, content) {
            audit_port_rejection(ctx, "port_hardcode_rejected", tool_name, path, &findings);
            Some(format_reject_message(path, &findings))
        } else if let Some(unalloc) =
            unallocated_bucket_findings(ctx.db.as_ref(), ctx.project_id, path, content).await
        {
            audit_port_rejection(ctx, "port_unallocated_rejected", tool_name, path, &unalloc);
            Some(format_unallocated_message(path, &unalloc))
        } else {
            None
        };
    // Arricchimento anti-loop: l'agente spesso ha GIA' chiamato request_port ma
    // poi hardcoda una porta diversa (es. allocata 21950, scrive 21951) ed entra
    // in loop sullo stesso rifiuto. Mostrargli le porte gia' allocate al progetto
    // rompe il loop indicando il numero ESATTO da usare (fix definitivo, regola H).
    match rejection {
        Some(mut msg) => {
            msg.push_str(&allocated_ports_hint(ctx.db.as_ref(), ctx.project_id).await);
            Some(msg)
        }
        None => None,
    }
}

/// Blocco testuale con le porte GIA' allocate al progetto (label incluso), da
/// appendere ai messaggi di rifiuto. Stringa vuota se nessuna allocazione o su
/// errore DB (best-effort: non deve impedire il rifiuto).
async fn allocated_ports_hint(db: &PgPool, project_id: uuid::Uuid) -> String {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT port::int, COALESCE(label, '?') \
         FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    // Il blocco dice all'agente "usa ESATTAMENTE una di queste": una porta del
    // bucket altrui finita nel registro sarebbe un consiglio dannoso, non un
    // dettaglio di elenco. Stesso criterio del punto unico (regola L).
    let ports: Vec<(u32, String)> = rows
        .into_iter()
        .filter(|(p, _)| {
            u16::try_from(*p).is_ok_and(|p| {
                crate::project_workspace::services::port_in_project_bucket(&project_id, p)
            })
        })
        .map(|(p, l)| (p as u32, l))
        .collect();
    render_allocated_hint(&ports)
}

/// Rendering puro del blocco "porte gia' allocate" (testabile senza DB).
fn render_allocated_hint(ports: &[(u32, String)]) -> String {
    if ports.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\nPORTE GIA' ALLOCATE a questo progetto (usa ESATTAMENTE una di queste, \
         non un numero a caso):\n",
    );
    for (port, label) in ports.iter().take(20) {
        s.push_str(&format!("  - {port} (label: {label})\n"));
    }
    s.push_str(
        "Se la porta che ti serve e' gia' in questa lista, riscrivi l'edit con quel \
         numero ESATTO (o, meglio, leggi solo da env senza fallback hardcoded). Chiama \
         request_port SOLO per un servizio NUOVO non ancora elencato qui.\n",
    );
    s
}

/// Variante di `reject_unallocated_bucket_ports` che ritorna i findings (per
/// auditarli) invece del solo messaggio. Punto unico riusato.
async fn unallocated_bucket_findings(
    db: &PgPool,
    project_id: uuid::Uuid,
    path: &str,
    content: &str,
) -> Option<Vec<PortFinding>> {
    // I `.env*` restano il posto CANONICO dove dichiarare la porta -- per questo
    // `scan_content` non li tratta come hardcode vietato. Ma "posto canonico" non
    // vuol dire "numero libero": la porta scritta li' dev'essere quella ALLOCATA
    // via `request_port`. Senza questo controllo il file sfuggiva a ENTRAMBE le
    // verifiche e il modello ci scriveva un numero scelto a mano (incidente
    // 2026-07-22: `PORT=3000` -- fuori bucket, quindi invisibile al controllo sul
    // range, e per giunta la porta della UI di Nexus).
    //
    // Nei `.env` contano quindi TUTTE le porte non riservate, non solo quelle del
    // bucket; altrove resta il criterio del bucket.
    let bucket_ports = ports_needing_allocation(path, content);
    if bucket_ports.is_empty() {
        return None;
    }
    // Punto unico (regola L): "allocata a questo progetto" vuol dire registrata
    // E nel bucket del progetto. Con la sola query sul registro, una porta del
    // bucket altrui gia' registrata (es. dal rilevamento porta-da-output)
    // autorizzava la scrittura che l'aveva prodotta.
    let allocated =
        crate::security::resource_linter::legitimate_ports_for_project(db, project_id).await;
    let unallocated: Vec<PortFinding> = bucket_ports
        .into_iter()
        .filter(|f| !allocated.contains(&f.port))
        .collect();
    if unallocated.is_empty() {
        None
    } else {
        Some(unallocated)
    }
}

/// Registra una violazione porte respinta in `nexus_resource_audit` (resource
/// kind `port`, outcome `blocked`). Best-effort: il writer e' batch async e
/// degrada in silenzio se non inizializzato.
fn audit_port_rejection(
    ctx: &nexus_agent_tools::ToolContextCore,
    action: &str,
    tool_name: &str,
    path: &str,
    findings: &[PortFinding],
) {
    let ports: Vec<u32> = {
        let mut v: Vec<u32> = findings.iter().map(|f| f.port).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let resource_id = ports
        .iter()
        .take(10)
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let detail_findings: Vec<serde_json::Value> = findings
        .iter()
        .take(5)
        .map(|f| {
            let snippet: String = f.snippet.chars().take(120).collect();
            serde_json::json!({ "line": f.line, "port": f.port, "snippet": snippet })
        })
        .collect();
    let mut entry =
        crate::security::AuditEntry::blocked(ctx.project_id, action.to_string(), "port")
            .with_resource(resource_id)
            .with_details(serde_json::json!({
                "tool": tool_name,
                "path": path,
                "ports": ports,
                "findings": detail_findings,
            }))
            .with_actor_user(ctx.user_id);
    if let Some(s) = ctx.session_id {
        entry = entry.with_actor_session(s);
    }
    crate::security::record_audit(entry);
}

pub fn format_reject_message(path: &str, findings: &[PortFinding]) -> String {
    let mut msg = String::new();
    msg.push_str(&format!(
        "\u{274C} [Errore: scrittura su '{}' rifiutata. Sono state rilevate {} porta/e TCP hardcoded fuori dal bucket Nexus (20000-39999).]\n",
        path,
        findings.len()
    ));
    msg.push_str("\nDettaglio:\n");
    for f in findings.iter().take(10) {
        msg.push_str(&format!(
            "  - riga {}: porta {} | {}\n",
            f.line,
            f.port,
            f.snippet.trim()
        ));
    }
    if findings.len() > 10 {
        msg.push_str(&format!(
            "  ... e altri {} riscontri.\n",
            findings.len() - 10
        ));
    }
    msg.push_str(
        "\nAzione richiesta:\n\
         1. Chiama il tool `request_port(label=\"<nome_servizio>\")` per ottenere una porta libera dal range 20000-39999 (verifica le porte gia' assegnate con `nexus_list_ports`).\n\
         2. Sostituisci la porta hardcoded con il valore ritornato. Puoi leggerla da variabile env, ma SENZA un default numerico: un fallback come `process.env.PORT || 5000` e' a tutti gli effetti una porta hardcoded e verra' rifiutato. Se vuoi un default, usa la porta ALLOCATA da request_port:\n\
            - JS/TS: process.env.PORT (oppure `process.env.PORT || <porta_allocata>`)\n\
            - Python: os.environ.get(\"PORT\") (oppure con default la porta allocata)\n\
            - Rust: env::var(\"PORT\")\n\
            - Docker/shell: ${PORT} oppure ${PORT_BACKEND:-<porta_allocata>}\n\
         3. Riprova la scrittura.\n\
         \nVedi il blocco <port_allocation> nel system prompt e ADR 0010 per i dettagli.",
    );
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_env_files() {
        let res = scan_content(".env", "PORT=3000\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
        let res = scan_content("config/.env.local", "PORT=3000\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allocated_hint_elenca_le_porte() {
        // Fix anti-loop porte: il messaggio di rifiuto elenca le porte gia'
        // allocate con il numero ESATTO da usare.
        let ports = vec![
            (21950u32, "frontend".to_string()),
            (21951u32, "backend".to_string()),
        ];
        let hint = render_allocated_hint(&ports);
        assert!(hint.contains("21950 (label: frontend)"), "{hint}");
        assert!(hint.contains("21951 (label: backend)"), "{hint}");
        assert!(hint.contains("ESATTAMENTE"), "{hint}");
    }

    #[test]
    fn allocated_hint_vuoto_senza_allocazioni() {
        assert!(render_allocated_hint(&[]).is_empty());
    }

    #[test]
    fn docker_compose_no_longer_skipped() {
        let res = scan_content(
            "docker-compose.yml",
            "services:\n  web:\n    ports:\n      - 3000:3000\n",
        );
        assert!(matches!(res, PortScanOutcome::Reject(_)));
    }

    #[test]
    fn detect_hardcoded_listen() {
        let res = scan_content("src/server.js", "app.listen(3000)\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert_eq!(f.len(), 1);
                assert_eq!(f[0].port, 3000);
                assert_eq!(f[0].line, 1);
            }
            _ => panic!("dovrebbe essere Reject"),
        }
    }

    #[test]
    fn allow_in_bucket() {
        let res = scan_content("src/server.js", "app.listen(25432)\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn env_file_la_porta_va_comunque_allocata() {
        // Il .env resta il posto CANONICO dove dichiarare la porta: non e'
        // hardcode vietato, quindi lo scan lo lascia passare...
        assert!(matches!(
            scan_content(".env", "PORT=3000\nDB_PATH=./data/app.db\n"),
            PortScanOutcome::Allowed
        ));
        // ...ma il numero entra nella verifica di allocazione. Prima il file era
        // saltato del tutto e una porta fuori bucket (3000, per giunta quella
        // della UI di Nexus) sfuggiva a ENTRAMBI i controlli.
        let da_verificare = ports_needing_allocation(".env", "PORT=3000\n");
        assert_eq!(da_verificare.len(), 1, "la porta del .env va verificata");
        assert_eq!(da_verificare[0].port, 3000);

        // Fuori dai .env il criterio resta il bucket: 3000 non ci rientra e viene
        // gia' gestita come hardcode da `scan_content`.
        assert!(ports_needing_allocation("src/server.js", "PORT=3000\n").is_empty());

        // Le forme che leggono la porta dall'ambiente non sono candidate: e'
        // esattamente il modo corretto di scrivere il file.
        assert!(ports_needing_allocation(".env", "PORT=${BACKEND_PORT}\n").is_empty());

        // Le riservate (<1024) non passano dall'allocatore di progetto.
        assert!(ports_needing_allocation(".env", "PORT=443\n").is_empty());
    }

    #[test]
    fn allow_db_connection_port() {
        // Regressione: `port: 5433` in una config di CONNESSIONE al DB (new Pool)
        // NON e' un binding hardcoded -> non deve essere rifiutato (prima mandava
        // l'agente in loop su edit_file di server.js). Vedi WELL_KNOWN_SERVICE_PORTS.
        let res = scan_content(
            "backend/server.js",
            "const pool = new Pool({\n  host: 'localhost',\n  port: 5433,\n  database: 'app',\n});\n",
        );
        assert!(
            matches!(res, PortScanOutcome::Allowed),
            "porta DB di connessione (5433) non deve essere rifiutata"
        );
        // Un vero binding fuori bucket resta rifiutato.
        let res2 = scan_content("backend/server.js", "app.listen(5000)\n");
        assert!(
            matches!(res2, PortScanOutcome::Reject(_)),
            "binding fuori bucket (5000) deve restare rifiutato"
        );
    }

    #[test]
    fn detect_cli_port_flag_in_script() {
        // Fix A1: porta hardcoded via `--port` negli script package.json
        // (Vite/Next/Astro). Prima sfuggiva: nessun regex copriva la sintassi CLI
        // separata da spazio (incidente Beauty-Book: "vite --port 21954").
        let res = scan_content(
            "package.json",
            "{\n  \"scripts\": {\n    \"dev\": \"vite --port 3000\"\n  }\n}\n",
        );
        match res {
            PortScanOutcome::Reject(f) => assert!(f.iter().any(|p| p.port == 3000), "{f:?}"),
            _ => panic!("--port 3000 dovrebbe essere Reject"),
        }
        // Anche la forma `--port=NNNN`.
        assert!(matches!(
            scan_content("package.json", "\"dev\": \"next --port=4000\"\n"),
            PortScanOutcome::Reject(_)
        ));
        // Una porta NEL bucket via --port viene catturata da collect_bucket_ports
        // (il chiamante poi verifica l'allocazione nel DB).
        let bucket = collect_bucket_ports("\"dev\": \"vite --port 21954\"\n");
        assert!(bucket.iter().any(|p| p.port == 21954), "{bucket:?}");
        // La forma da ENV (nessun numero hardcoded) NON deve essere segnalata.
        assert!(matches!(
            scan_content("package.json", "\"dev\": \"vite --port $VITE_PORT\"\n"),
            PortScanOutcome::Allowed
        ));
    }

    #[test]
    fn reject_env_fallback_port_line() {
        // Incidente Beauty-Book: `const port = process.env.PORT || 5000;` ELUDEVA
        // lo scanner perche' la riga conteneva l'hint `process.env.PORT` e lo
        // skip ENV_PORT_HINTS saltava l'intera riga. Il fallback numerico e' a
        // tutti gli effetti una porta hardcoded: deve essere rifiutato.
        let res = scan_content("src/server.js", "app.listen(process.env.PORT || 3000)\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000), "fallback 3000 rilevato");
            }
            _ => panic!("il fallback env hardcoded deve essere Reject"),
        }
        // Caso reale Beauty-Book: porta 5000.
        let res = scan_content("server.js", "const port = process.env.PORT || 5000;\n");
        assert!(matches!(res, PortScanOutcome::Reject(_)));
    }

    #[test]
    fn allow_env_port_read_without_default() {
        // Lettura pura da env, senza fallback numerico: ammessa.
        let res = scan_content("src/server.js", "app.listen(process.env.PORT)\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
        let res = scan_content(
            "src/server.js",
            "app.listen(parseInt(process.env.PORT, 10))\n",
        );
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn reject_env_fallback_variants() {
        // JS nullish
        assert!(matches!(
            scan_content("a.ts", "const p = import.meta.env.VITE_PORT ?? 5173\n"),
            PortScanOutcome::Reject(_)
        ));
        // JS bracket
        assert!(matches!(
            scan_content("a.js", "const p = process.env[\"PORT\"] || 8080\n"),
            PortScanOutcome::Reject(_)
        ));
        // Python get con default
        assert!(matches!(
            scan_content("a.py", "port = os.environ.get(\"PORT\", 5000)\n"),
            PortScanOutcome::Reject(_)
        ));
        // Python or
        assert!(matches!(
            scan_content("a.py", "port = int(os.getenv(\"PORT\") or 8080)\n"),
            PortScanOutcome::Reject(_)
        ));
        // Rust unwrap_or
        assert!(matches!(
            scan_content(
                "a.rs",
                "let p = env::var(\"PORT\").unwrap_or(\"3000\".to_string());\n"
            ),
            PortScanOutcome::Reject(_)
        ));
        // Rust unwrap_or_else con closure
        assert!(matches!(
            scan_content(
                "a.rs",
                "let p = env::var(\"PORT\").unwrap_or_else(|_| \"3000\".into());\n"
            ),
            PortScanOutcome::Reject(_)
        ));
    }

    #[test]
    fn allow_non_port_env_fallback() {
        // Anti-falsi-positivi: variabili che NON sono porte, anche se contengono
        // sottostringhe simili. Il numero di fallback non e' una porta.
        assert!(matches!(
            scan_content("a.js", "const t = process.env.TIMEOUT || 5000\n"),
            PortScanOutcome::Allowed
        ));
        assert!(matches!(
            scan_content("a.js", "const r = process.env.REPORT_LIMIT || 5000\n"),
            PortScanOutcome::Allowed
        ));
    }

    #[test]
    fn env_fallback_in_bucket_collected_for_allocation_check() {
        // Un fallback NEL bucket non e' una violazione fuori-bucket (scan_content
        // Allowed), ma viene raccolto da collect_bucket_ports per il check di
        // allocazione (simmetrico a `${PORT_BACKEND:-20002}`).
        assert!(matches!(
            scan_content("server.js", "app.listen(process.env.PORT || 25000)\n"),
            PortScanOutcome::Allowed
        ));
        let bucket = collect_bucket_ports("app.listen(process.env.PORT || 25000)\n");
        assert!(bucket.iter().any(|p| p.port == 25000));
    }

    #[test]
    fn reject_shell_colon_equal_default() {
        // `${APP_PORT:=3000}` (assegnazione default) come `:-`.
        assert!(matches!(
            scan_content("entrypoint.sh", ": \"${APP_PORT:=3000}\"\n"),
            PortScanOutcome::Reject(_)
        ));
    }

    #[test]
    fn detect_bind_with_host() {
        let res = scan_content("main.py", "s.bind(\"0.0.0.0:8080\")\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert_eq!(f[0].port, 8080);
            }
            _ => panic!("dovrebbe essere Reject"),
        }
    }

    #[test]
    fn detect_port_assignment() {
        let res = scan_content("config.py", "PORT = 5173\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert_eq!(f[0].port, 5173);
            }
            _ => panic!("dovrebbe essere Reject"),
        }
    }

    #[test]
    fn allow_reserved_low_ports() {
        let res = scan_content("docs.md", "Default HTTP port = 80, HTTPS = 443.\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_yaml_port_key() {
        let res = scan_content("config.yaml", "server:\n  port: 3000\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("YAML port deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_dockerfile_expose() {
        let res = scan_content("Dockerfile", "FROM node:20\nEXPOSE 3000\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("EXPOSE deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_backend_port_env_assign() {
        let res = scan_content("config.sh", "BACKEND_PORT=3000\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("BACKEND_PORT=3000 deve essere rifiutato"),
        }
    }

    #[test]
    fn allow_backend_port_in_bucket() {
        let res = scan_content("config.sh", "BACKEND_PORT=32100\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn bucket_ports_host_only_per_verifica_allocazione() {
        // Enforcement allocation-aware: dai docker-compose si raccoglie SOLO la
        // porta HOST nel bucket (per verificarne l'allocazione via DB), mai la
        // porta CONTAINER. Cosi' "20001:3000" -> raccoglie 20001, non 3000.
        let ports = collect_ports(
            "services:\n  web:\n    ports:\n      - 20001:3000\n",
            port_in_bucket,
        );
        assert!(
            ports.iter().any(|p| p.port == 20001),
            "host 20001 (bucket) raccolta"
        );
        assert!(
            !ports.iter().any(|p| p.port == 3000),
            "container 3000 NON raccolta"
        );
        // scan_content (violazioni fuori-bucket) NON deve segnalare 20001.
        assert!(!collect_ports("x PORT=20001", port_is_violating)
            .iter()
            .any(|p| p.port == 20001));
    }

    #[test]
    fn allow_backend_port_template_var() {
        let res = scan_content("config.sh", "BACKEND_PORT=${PORT}\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allow_backend_port_named_var() {
        let res = scan_content("config.sh", "BACKEND_PORT=$PORT_BACKEND\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_yaml_range_out_of_bucket() {
        let res = scan_content("config.yaml", "scan:\n  range 3001-3100\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3001));
                assert!(f.iter().any(|x| x.port == 3100));
            }
            _ => panic!("range 3001-3100 deve essere rifiutato"),
        }
    }

    #[test]
    fn allow_range_inside_bucket() {
        let res = scan_content("config.yaml", "scan:\n  range 25000-26000\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn allow_request_port_call() {
        let res = scan_content(
            "src/setup.rs",
            "let port = request_port(label=\"backend\");\n",
        );
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    #[test]
    fn detect_json_ports_array() {
        let res = scan_content("service.json", "{\"ports\": [3000, 3001]}\n");
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(f.iter().any(|x| x.port == 3000));
            }
            _ => panic!("JSON ports array fuori bucket deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_db_port_var() {
        // Una porta DB ben nota (5432 Postgres) e' una CONNESSIONE legittima, non un
        // binding -> consentita (WELL_KNOWN_SERVICE_PORTS). Prima era un falso
        // positivo che faceva rifiutare la config DB e loopare l'agente.
        let res = scan_content("dev.sh", "DB_PORT=5432\n");
        assert!(
            matches!(res, PortScanOutcome::Allowed),
            "DB_PORT verso una porta DB ben nota e' una connessione, va consentita"
        );
        // Una porta NON ben nota fuori bucket resta rilevata dal regex DB_PORT.
        let res2 = scan_content("dev.sh", "DB_PORT=5000\n");
        match res2 {
            PortScanOutcome::Reject(f) => assert!(f.iter().any(|x| x.port == 5000)),
            _ => panic!("DB_PORT=5000 (porta non nota) deve essere rifiutato"),
        }
    }

    #[test]
    fn detect_compose_default_out_of_bucket() {
        // `${PORT:-3000}` default FUORI bucket: la riga ha l'hint `${PORT` ma il
        // default 3000 va comunque rifiutato (prima veniva saltato).
        let res = scan_content(
            "docker-compose.yml",
            "    ports:\n      - \"${PORT:-3000}:3000\"\n",
        );
        match res {
            PortScanOutcome::Reject(f) => {
                assert!(
                    f.iter().any(|x| x.port == 3000),
                    "default 3000 deve essere rilevato"
                );
            }
            _ => panic!("default ${{PORT:-3000}} fuori bucket deve essere rifiutato"),
        }
    }

    #[test]
    fn compose_default_in_bucket_collected_for_allocation_check() {
        // `${PORT_BACKEND:-20002}` default NEL bucket: raccolto per la verifica di
        // allocazione (reject_unallocated_bucket_ports), nonostante l'hint env
        // `${PORT_` sulla stessa riga. Questo e' il caso Beauty-Book.
        let host = collect_ports(
            "      - \"${PORT_BACKEND:-20002}:${PORT_BACKEND:-20002}\"\n",
            port_in_bucket,
        );
        assert!(
            host.iter().any(|p| p.port == 20002),
            "il default 20002 nel bucket deve essere raccolto per il check di allocazione"
        );
        // scan_content (solo fuori-bucket) NON deve segnalare 20002 (e' nel bucket).
        assert!(matches!(
            scan_content(
                "docker-compose.yml",
                "      - \"${PORT_BACKEND:-20002}:3000\"\n"
            ),
            PortScanOutcome::Allowed
        ));
    }

    #[test]
    fn allow_default_in_bucket_via_scan() {
        // Un default nel bucket non e' una violazione "fuori-bucket": scan_content
        // lo lascia passare (l'allocazione e' verificata altrove).
        let res = scan_content("docker-compose.yml", "      - \"${PORT:-25000}:3000\"\n");
        assert!(matches!(res, PortScanOutcome::Allowed));
    }

    /// IL DIFETTO MISURATO il 06/08/2026 (agenda-medica): il frontend generato
    /// aveva `VITE_API_URL=http://localhost:3000` — la web-ide di Nexus —
    /// nel proprio `.env`, e avrebbe chiesto i propri dati all'interfaccia di
    /// Nexus invece che al proprio backend (che era sulla 31926).
    ///
    /// Nessun controllo lo vedeva, per DUE ragioni sovrapposte: i `.env` sono
    /// esenti dallo scan degli hardcode (giustamente: e' il posto dove le
    /// configurazioni vanno dichiarate), e nessuna regex riconosce una porta
    /// dentro un URL — le esistenti cercano tutte `PORT=`, `.listen(`, `port:`.
    ///
    /// Mutazione: togliere URL_PORT_REGEX da collect_nexus_app_ports -> il
    /// primo caso torna vuoto e il test rosseggia.
    #[test]
    fn la_porta_di_un_servizio_nexus_dentro_un_url_viene_vista() {
        let trovate = collect_nexus_app_ports("VITE_API_URL=http://localhost:3000\n");
        assert_eq!(trovate.len(), 1, "la porta nell'URL deve essere rilevata");
        assert_eq!(trovate[0].port, 3000);
        // Il messaggio nomina il servizio: «porta non ammessa» non aiuta.
        let msg = format_nexus_app_message("frontend/.env", &trovate);
        assert!(msg.contains("interfaccia web di Nexus"), "{msg}");
    }

    /// IL CONFINE, ed e' la ragione per cui il criterio guarda le sole
    /// applicazioni: un `.env` di progetto contiene LEGITTIMAMENTE la porta di
    /// un datastore condiviso, e vietarla romperebbe ogni backend. Misurato su
    /// biblioteca-scolastica, che ha esattamente questa riga.
    ///
    /// Mutazione: usare NEXUS_RESERVED_PORTS (che include 5432) al posto delle
    /// sole app -> questo test rosseggia.
    #[test]
    fn la_porta_di_un_datastore_condiviso_resta_legittima() {
        let env = "DATABASE_URL=postgresql://postgres:pwd@localhost:5432/agenda?schema=public\n";
        assert!(
            collect_nexus_app_ports(env).is_empty(),
            "connettersi al Postgres e' il modo previsto, non un difetto"
        );
        // E nemmeno un servizio esterno qualunque (MySQL) e' un'app di Nexus.
        assert!(collect_nexus_app_ports("DB_URL=mysql://root@localhost:3306/x\n").is_empty());
    }

    /// La forma DICHIARATA resta coperta dal punto unico di parsing: il
    /// criterio non vale solo per gli URL.
    #[test]
    fn anche_una_porta_nexus_dichiarata_viene_vista() {
        let trovate = collect_nexus_app_ports("PORT=4000\n");
        assert_eq!(trovate.len(), 1);
        assert_eq!(trovate[0].port, 4000);
    }
}
