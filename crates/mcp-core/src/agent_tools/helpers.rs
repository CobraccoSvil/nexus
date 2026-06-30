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

/// Punto unico (regola L) che riconosce un fallimento di build/compilazione dal
/// suo output. tsc/cargo/eslint/webpack falliscono con un elenco di errori
/// file:riga e un totale in fondo ("Found N errors", "could not compile").
/// Deve avere PRIORITA' sui rami file-oriented: messaggi come `error TS2304:
/// Cannot find name 'foo'` o `error[E0425]: cannot find value` contengono
/// "cannot find" ma NON sono "file non trovato" — sono errori di compilazione.
/// Gate `exit_code != 0`: un grep che stampa "found" esce a 0 e non entra qui.
fn is_build_failure(exit_code: i32, combined: &str) -> bool {
    if exit_code == 0 {
        return false;
    }
    combined.contains("error ts")
        || combined.contains("error[e")
        || combined.contains("found ") && (combined.contains(" error") || combined.contains(" problem"))
        || combined.contains("compilation failed")
        || combined.contains("compilation error")
        || combined.contains("build failed")
        || combined.contains("failed to compile")
        || combined.contains("problems (")
        || combined.contains("cannot find module")
        || combined.contains("type error")
        || combined.contains("ts(")
        || combined.contains("could not compile")
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
    // Ramo build/compilazione PRIMA di "no such file": un errore tsc/cargo del
    // tipo "Cannot find name"/"cannot find value" contiene "cannot find" ma NON
    // e' un file mancante — va correggendo i file segnalati, non cercando un
    // percorso. Il messaggio generico induceva invece a RI-ESEGUIRE il build.
    if is_build_failure(exit_code, &combined) {
        return "build fallito con errori di compilazione — NON ripetere lo stesso comando per vedere se cambia: ri-eseguire il build non riduce gli errori, li riduce solo correggere i file. Leggi TUTTI gli errori nell'output (ognuno ha file:riga, in fondo c'e' il totale tipo 'Found N errors'), apri con read_file OGNI file segnalato e correggilo con edit_file (correzione batch nello stesso turno), poi ri-esegui il build UNA sola volta per confermare. Se l'output era troncato, correggi gli errori visibili e segnala che potrebbero mancarne altri";
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

/// Hint platform-aware per i comandi agente che falliscono perche' usano sintassi
/// cmd/PowerShell (Windows-native) mentre `run_command` gira in Git Bash (POSIX).
/// Senza, l'agente ripete lo stesso comando -> repeated_action -> force-close
/// ("non completato") anche dopo aver gia' creato il progetto. Solo su Windows
/// (su Unix la shell e' bash nativa e questi comandi non sono attesi).
#[cfg(windows)]
pub(crate) fn windows_shell_hint(command: &str) -> Option<&'static str> {
    let c = command.trim_start().to_lowercase();
    // Comandi cmd/PowerShell che NON esistono in Git Bash.
    let posix_violation = c.starts_with("dir ")
        || c == "dir"
        || c.starts_with("dir/")
        || c.starts_with("dir\\")
        || c.contains("get-childitem")
        || c.contains("select-object")
        || c.contains("where-object")
        || c.contains("-recurse")
        || c.starts_with("ss ")
        || c == "ss"
        || c.starts_with("ss -")
        || c.contains("findstr");
    // taskkill con singolo slash: Git Bash (MSYS) converte '/F' in un path ->
    // argomento invalido. Serve il doppio slash '//F //PID'.
    let bad_taskkill = c.contains("taskkill")
        && (c.contains(" /f") || c.contains(" /pid") || c.contains(" /im"))
        && !c.contains("//");
    if posix_violation || bad_taskkill {
        return Some(
            "Su Windows run_command esegue in Git Bash (POSIX), NON cmd/PowerShell. \
             Usa comandi POSIX: 'ls'/'find' invece di 'dir'/'Get-ChildItem', 'grep' \
             invece di 'findstr', 'netstat -ano' invece di 'ss'. Per terminare un \
             processo: 'taskkill //F //PID <pid>' (DOPPIO slash: Git Bash interpreta \
             '/F' come percorso) oppure 'kill <pid>'. Riprova con la sintassi POSIX, \
             non ripetere il comando in stile Windows.",
        );
    }
    None
}

#[cfg(not(windows))]
pub(crate) fn windows_shell_hint(_command: &str) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_tsc_build_failure_emette_guida_build() {
        let stdout = "src/app.ts(12,5): error TS2304: Cannot find name 'foo'.\n\
                      src/util.ts(3,1): error TS2552: Cannot find name 'bar'.\n\
                      Found 2 errors in 2 files.\n";
        let hint = classify_command_error(1, "", stdout);
        assert!(
            hint.contains("build fallito con errori di compilazione"),
            "atteso ramo build, ottenuto: {hint}"
        );
        assert!(
            hint.contains("NON ripetere lo stesso comando"),
            "la guida deve scoraggiare la ripetizione del build: {hint}"
        );
        assert!(
            hint.contains("edit_file"),
            "la guida deve ordinare la correzione dei file: {hint}"
        );
    }

    #[test]
    fn classify_cargo_build_failure_emette_guida_build() {
        let stderr = "error[E0425]: cannot find value `x` in this scope\n\
                      error: could not compile `mcp-core` due to previous error\n";
        let hint = classify_command_error(101, stderr, "");
        assert!(
            hint.contains("build fallito con errori di compilazione"),
            "atteso ramo build per cargo, ottenuto: {hint}"
        );
    }

    #[test]
    fn classify_cannot_find_name_non_e_file_mancante() {
        // Regressione: "Cannot find name"/"cannot find value" sono errori di
        // compilazione, NON file mancanti. Il ramo build deve avere priorita'
        // sul ramo "no such file" che altrimenti li intercetterebbe.
        let tsc = classify_command_error(
            1,
            "",
            "src/a.ts(1,1): error TS2304: Cannot find name 'foo'.\nFound 1 error.\n",
        );
        assert!(
            tsc.contains("build fallito"),
            "tsc 'Cannot find name' deve essere build, non file mancante: {tsc}"
        );
        let cargo =
            classify_command_error(101, "error[E0425]: cannot find value `x` in this scope", "");
        assert!(
            cargo.contains("build fallito"),
            "cargo 'cannot find value' deve essere build, non file mancante: {cargo}"
        );
    }

    #[test]
    fn classify_no_such_file_resta_file_mancante() {
        // Un vero file mancante (senza marker di build) resta classificato
        // come percorso errato.
        let hint = classify_command_error(2, "cat: foo.txt: No such file or directory", "");
        assert!(
            hint.contains("file o directory non trovata"),
            "un file realmente mancante deve restare nel ramo file: {hint}"
        );
    }

    #[test]
    fn classify_grep_no_match_non_e_build() {
        // grep che non trova nulla: exit 1 senza output -> ramo grep, non build.
        let hint = classify_command_error(1, "", "");
        assert!(
            !hint.contains("build fallito"),
            "un grep vuoto non deve essere classificato come build: {hint}"
        );
        assert!(hint.contains("nessuna corrispondenza"));
    }

    #[test]
    fn classify_grep_trova_found_a_exit0_non_e_build() {
        // Un comando andato a buon fine (exit 0) il cui output contiene "found"
        // non deve entrare nel ramo build (gate exit_code != 0).
        let hint = classify_command_error(0, "", "Found 3 matching lines\n");
        assert!(
            !hint.contains("build fallito"),
            "exit 0 non deve mai essere build: {hint}"
        );
    }

    #[test]
    fn classify_command_not_found_resta_prioritario() {
        // I rami specifici precedono il ramo build anche con exit != 0.
        let hint = classify_command_error(127, "tsc: command not found", "");
        assert!(
            hint.contains("comando non trovato"),
            "command not found deve avere priorita' sul ramo build: {hint}"
        );
    }
}
