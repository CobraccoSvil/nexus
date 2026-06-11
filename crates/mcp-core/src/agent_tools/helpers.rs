//! Helper condivisi tra i sottomoduli agent_tools: costanti di lettura file,
//! pattern protetti, estrazione struttura, classificazione errori comando.
//!
//! Estratto da mod.rs (refactor god-file). Visibilita pub(super) perche i
//! sottomoduli che fanno use super::* continuano a vederli via re-export in mod.rs.

/// Soglia oltre la quale `read_file` antepone una mappa strutturale del file
/// per orientare l'agente. NON tronca mai: il file viene comunque restituito
/// INTEGRALE (politica "mai troncare-e-buttare").
pub(crate) const READ_FILE_STRUCTURE_HINT_LINES: usize = 300;
/// Numero massimo di righe leggibili con read_file_lines in una singola chiamata.
/// read_file_lines e' un tool a RANGE esplicito (start/end), quindi non perde
/// dati: il chiamante itera i range. Valore molto alto per non spezzare
/// inutilmente letture ampie volute.
pub(crate) const READ_FILE_LINES_MAX: usize = 100_000;

/// File e pattern che l'agente non può mai modificare, indipendentemente dai permessi.
/// Proteggono secrets, configurazioni ambiente e il binario in produzione.
pub(crate) const PROTECTED_PATTERNS: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.staging",
    ".env.development",
    "nexus.env", // env specifico di Nexus
    "secrets",   // qualsiasi file con "secrets" nel nome
    "credentials",
    "id_rsa",
    "id_ed25519",
    ".pem",
    ".key",
    "Cargo.lock", // non modificare il lockfile manualmente
    "pnpm-lock.yaml",
];

/// Controlla se il comando corrisponde a uno dei pattern long-running caricati dal DB.
/// Ogni pattern è una sequenza di token (es. "npm run dev") che viene cercata
/// come sottosequenza contigua nei token del comando.
pub(crate) fn looks_like_long_running_command(command: &str, patterns: &[String]) -> bool {
    let lower = command.to_lowercase();
    let normalized = lower
        .replace("&&", " ")
        .replace("||", " ")
        .replace([';', '|', '(', ')'], " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();

    for pattern in patterns {
        let pat_tokens: Vec<&str> = pattern.split_whitespace().collect();
        if pat_tokens.is_empty() {
            continue;
        }
        // Pattern singolo token: match anche come primo token (es. "vite", "nodemon", "uvicorn")
        if pat_tokens.len() == 1 {
            if tokens.contains(&pat_tokens[0].to_lowercase().as_str())
                || tokens.first().copied() == Some(pat_tokens[0])
            {
                return true;
            }
            // Match case-insensitive su tutti i token
            let pat_lower = pat_tokens[0].to_lowercase();
            if tokens.contains(&pat_lower.as_str()) {
                return true;
            }
        } else {
            // Multi-token: match come sottosequenza contigua
            let pat_lower: Vec<String> = pat_tokens.iter().map(|t| t.to_lowercase()).collect();
            let pat_refs: Vec<&str> = pat_lower.iter().map(|s| s.as_str()).collect();
            if tokens.len() >= pat_refs.len()
                && tokens
                    .windows(pat_refs.len())
                    .any(|w| w == pat_refs.as_slice())
            {
                return true;
            }
        }
    }
    false
}

/// Estrae una mappa strutturale del file: funzioni, classi, componenti con numero di riga.
/// Supporta Rust, TypeScript/JavaScript, Python, C#, Go.
/// Usa corrispondenza su prefisso di parola chiave — nessuna regex, O(n) per riga.
pub(crate) fn extract_file_structure(content: &str) -> Vec<(usize, String)> {
    let mut entries: Vec<(usize, String)> = Vec::new();

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        let line = raw_line.trim();

        // Salta righe vuote e commenti
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with("/*")
            || line.starts_with('#')
        {
            continue;
        }

        // Helper: estrai nome identificatore dopo una keyword
        let ident_after = |s: &str, kw: &str| -> Option<String> {
            let rest = s.strip_prefix(kw)?.trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        };

        // Normalizza spazi multipli per matching keyword composte
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");

        // TypeScript/JavaScript — export function, async function, function
        if let Some(name) = [
            "export async function ",
            "export function ",
            "async function ",
            "function ",
        ]
        .iter()
        .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("fn {name}")));
            continue;
        }

        // TypeScript/JavaScript — export const X = (...) => / = async (
        if normalized.starts_with("export const ") || normalized.starts_with("const ") {
            // Solo se è assegnazione a funzione/arrow
            if normalized.contains("= (")
                || normalized.contains("= async (")
                || normalized.contains(": React.")
                || normalized.contains("FC =")
            {
                if let Some(name) = ident_after(&normalized, "export const ")
                    .or_else(|| ident_after(&normalized, "const "))
                {
                    entries.push((line_num, format!("const {name}")));
                    continue;
                }
            }
        }

        // class (TS/JS/Python/C#)
        if let Some(name) = ["export default class ", "export class ", "class "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("class {name}")));
            continue;
        }

        // Rust — pub async fn, pub fn, async fn, fn
        if let Some(name) = ["pub async fn ", "pub fn ", "async fn ", "fn "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("fn {name}")));
            continue;
        }

        // Rust — impl, struct, enum
        if let Some(name) = ident_after(&normalized, "impl ") {
            entries.push((line_num, format!("impl {name}")));
            continue;
        }
        if let Some(name) = ["pub struct ", "struct "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("struct {name}")));
            continue;
        }
        if let Some(name) = ["pub enum ", "enum "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("enum {name}")));
            continue;
        }

        // Python — def, async def
        if let Some(name) = ["async def ", "def "]
            .iter()
            .find_map(|kw| ident_after(&normalized, kw))
        {
            entries.push((line_num, format!("def {name}")));
            continue;
        }

        // C# — public/private/protected method or class
        if normalized.starts_with("public ")
            || normalized.starts_with("private ")
            || normalized.starts_with("protected ")
        {
            if normalized.contains(" class ")
                || normalized.contains(" interface ")
                || normalized.contains(" enum ")
            {
                let short: String = normalized.chars().take(60).collect();
                entries.push((line_num, format!("class {short}")));
                continue;
            }
            // method: ha parentesi aperta e non è una property semplice
            if normalized.contains('(') && !normalized.ends_with(';') {
                let short: String = normalized.chars().take(60).collect();
                entries.push((line_num, format!("method {short}")));
                continue;
            }
        }
    }

    entries
}

/// Ritorna true se il path è protetto e non deve essere modificato dall'agente.
pub(crate) fn is_protected_path(path_str: &str) -> Option<&'static str> {
    let lower = path_str.to_lowercase();
    // Controlla nome file esatto o pattern nel path
    for pattern in PROTECTED_PATTERNS {
        let pat_lower = pattern.to_lowercase();
        // Match esatto del nome file o estensione
        if lower.ends_with(&pat_lower)
            || lower.contains(&format!("/{}", pat_lower))
            || lower.contains(&format!("\\{}", pat_lower))
            || lower == pat_lower
        {
            return Some(pattern);
        }
    }
    None
}

pub(crate) fn format_process_output(info: &crate::agent_processes::ProcessOutput) -> String {
    let mut msg = format!(
        "Processo: {} (pid: {}, status: {}",
        info.command,
        info.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into()),
        info.status,
    );
    if let Some(code) = info.exit_code {
        msg.push_str(&format!(", exit_code: {}", code));
    }
    msg.push_str(")\n");
    if !info.stdout.is_empty() {
        msg.push_str(&format!("\nSTDOUT:\n{}", info.stdout));
    }
    if !info.stderr.is_empty() {
        msg.push_str(&format!("\nSTDERR:\n{}", info.stderr));
    }
    if info.stdout.is_empty() && info.stderr.is_empty() {
        msg.push_str("\n(Nessun output disponibile)");
    }
    msg
}

/// Classifica l'errore di un comando shell e restituisce un suggerimento diagnostico.
pub(crate) fn classify_command_error(exit_code: i32, stderr: &str, stdout: &str) -> &'static str {
    let err = stderr.to_lowercase();
    let out = stdout.to_lowercase();
    let combined = format!("{err} {out}");
    if exit_code == 127 || combined.contains("command not found") || combined.contains("not found")
    {
        return "comando non trovato — verifica il nome esatto o installa il pacchetto mancante con run_command(\"sudo apt-get install -y <pacchetto>\")";
    }
    if combined.contains("permission denied") || combined.contains("operation not permitted") {
        return "permesso negato — prova ad aggiungere `sudo` oppure verifica i permessi del file con run_command(\"ls -la <percorso>\")";
    }
    if combined.contains("no such file")
        || combined.contains("cannot find")
        || combined.contains("no existe")
    {
        return "file o directory non trovata — verifica il percorso con list_files o run_command(\"ls <directory>\")";
    }
    if combined.contains("already installed") || combined.contains("is already") {
        return "già installato o già presente — il problema è probabilmente altrove, non ripetere l'installazione";
    }
    if combined.contains("syntax error") || combined.contains("unexpected token") {
        return "errore di sintassi nel comando — correggi la sintassi prima di riprovare";
    }
    if combined.contains("connection refused") || combined.contains("network unreachable") {
        return "connessione rifiutata o rete non raggiungibile — verifica che il servizio target sia attivo";
    }
    if exit_code == 1 && stderr.trim().is_empty() && stdout.trim().is_empty() {
        return "exit code 1 senza output — per grep/find significa 'nessuna corrispondenza': prova un pattern diverso";
    }
    "errore generico — leggi stderr per la causa specifica, poi usa un approccio alternativo o un comando diverso"
}
