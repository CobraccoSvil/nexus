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

/// True se il comando e' un one-shot LUNGO che TERMINA (install/build/compile/
/// test/migrate) — da attendere in sincrono, non da instradare a run_service.
/// PUNTO UNICO (regola L): usato dal probe di `run_command` e dal declassamento
/// kind service->task in `tool_run_service`.
pub(crate) fn is_long_oneshot(command: &str) -> bool {
    let c = command.to_lowercase();
    c.contains("install")
        || c.contains("npm ci")
        || c.contains(" build")
        || c.contains("tsc")
        || c.contains("cargo build")
        || c.contains("cargo check")
        || c.contains("cargo test")
        || c.contains("compile")
        || c.contains("migrate")
        || c.contains("prisma generate")
        || c.contains("playwright test")
        || c.contains("npm add")
        || c.contains("pnpm add")
        || c.contains("yarn add")
}

/// Exit code di un processo ucciso da SIGPIPE (128 + 13).
///
/// Con `pipefail` una pipeline riporta il PRIMO stadio fallito: se il consumatore
/// a valle chiude presto (il caso tipico e' `... | head -N`), il produttore riceve
/// SIGPIPE e la pipeline riporterebbe 141. Non e' un fallimento del comando — e'
/// il consumatore che ha smesso di leggere — quindi va trattato come successo.
pub(crate) const EXIT_SIGPIPE: i32 = 141;

/// Avvolge il comando dell'agente nella riga di shell effettivamente eseguita.
///
/// PUNTO UNICO (regola L): entrambi i punti che lanciano la shell (`run_command` e
/// `run_tests`) passano da qui, cosi' la semantica di esecuzione e' una sola.
///
/// Aggiunge `set -o pipefail`. Senza, l'exit code di una pipeline e' quello
/// dell'ULTIMO stadio: `npm install 2>&1 | tail -5` riportava l'esito di `tail`
/// (sempre 0) e un install FALLITO risultava riuscito. E' il segnale strutturato
/// su cui l'anti-loop e la diagnostica decidono, mascherato alla fonte (regola M).
/// Misurato: su verifica-wd 38 comandi su 183 usano una pipe, e tutti e 6 gli
/// `npm install ... | tail` risultavano exit 0 mentre l'ambiente restava rotto.
///
/// Il costo noto e' [`EXIT_SIGPIPE`], gestito dal chiamante.
pub(crate) fn shell_line(command: &str) -> String {
    format!("set -o pipefail; {command}")
}

/// True se il comando MUTA l'albero delle dipendenze di un package manager
/// (install/add/remove/update di npm, pnpm, yarn, bun, pip, poetry, composer,
/// gem, bundle, go mod).
///
/// Serve a SERIALIZZARE quei comandi per progetto: npm & co. non sono
/// concurrency-safe sulla stessa directory di dipendenze. Due install simultanei
/// si sovrascrivono a vicenda — uno rimuove cio' che l'altro sta scrivendo — e
/// lasciano lo stato interno del package manager (`node_modules/.package-lock.json`)
/// incoerente col disco: da quel momento `npm install <pkg>` risponde "up to date"
/// senza installare nulla, il binario atteso (tsc, vite, ...) non c'e', il build
/// fallisce e il lavoro non converge.
///
/// Misurato sul progetto verifica-wd (2026-07-23): 11 coppie di `npm install` da
/// run_id DIVERSI entro 60s sulla stessa area, di cui una a distanza ZERO secondi
/// (un sub-agente con working_dir=backend e un altro con `cd backend && npm
/// install`), 13 run distinti coinvolti e fino a 12 sub-agenti sovrapposti.
///
/// `cargo` e' ESCLUSO di proposito: ha gia' un file-lock interno sul registry e su
/// target/, quindi serializzarlo qui aggiungerebbe attesa senza togliere rischio.
///
/// Inclusivo per scelta: un falso positivo costa solo un po' di serializzazione,
/// un falso negativo costa un ambiente corrotto.
pub(crate) fn is_package_manager_mutation(command: &str) -> bool {
    /// Package manager che mutano una directory di dipendenze condivisa.
    const PM: &[&str] = &[
        "npm", "pnpm", "yarn", "bun", "pip", "pip3", "poetry", "composer", "gem", "bundle", "go",
    ];
    /// Sotto-comandi che SCRIVONO nell'albero delle dipendenze.
    const MUT: &[&str] = &[
        "install",
        "ci",
        "add",
        "remove",
        "uninstall",
        "update",
        "upgrade",
        "prune",
        "dedupe",
        "link",
        "require",
        "rebuild",
        "get",
        "mod",
    ];
    // Segmenta su &&, ||, ;, | e newline: in `cd frontend && npm install` il
    // package manager sta nel SECONDO segmento, e senza segmentazione il `cd`
    // iniziale maschererebbe la posizione del comando vero.
    command
        .to_lowercase()
        .split(['&', '|', ';', '\n'])
        .any(|seg| {
            let toks: Vec<&str> = seg.split_whitespace().collect();
            match toks.iter().position(|t| PM.contains(t)) {
                // Il sotto-comando mutante deve venire DOPO il package manager:
                // `npm install` si', `install npm` (frase qualsiasi) no.
                Some(i) => toks[i + 1..].iter().any(|t| MUT.contains(t)),
                None => false,
            }
        })
}

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
        || combined.contains("found ")
            && (combined.contains(" error") || combined.contains(" problem"))
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

/// Rileva quando il comando DUPLICA il `working_dir` gia' applicato come CWD.
///
/// Causa radice di ambiente incoerente (misurata sul test pulito diag-deps,
/// 2026-07-23): `run_command` esegue con CWD = <root>/<working_dir>. Se il comando
/// RIPETE quel segmento (`cd frontend`, path `frontend/...`) mentre working_dir e'
/// gia' `frontend`, i path si SOMMANO (`frontend/frontend`): il `rm -rf
/// frontend/node_modules` non tocca il vero node_modules, l'`npm install` scrive
/// altrove, e l'ambiente resta incoerente (typescript/@types assenti -> build
/// impossibile -> todo bloccato -> run che non converge). E' generale, non
/// npm-specifico: spiega anche node_modules creato nella root e i path assoluti
/// errati (`/app/backend`).
///
/// Ritorna `Some(spiegazione)` se il comando duplica il working_dir, `None`
/// altrimenti. Conservativo per evitare falsi positivi: blocca SOLO `cd <wd>` e i
/// path con prefisso `<wd>/` (segno inequivocabile di path); un `<wd>` nudo (es.
/// `echo frontend`) NON e' bloccato. Root (`""`, `"."`, `"./"`) non ha segmento da
/// duplicare. `wd_rel` e' il valore grezzo del parametro `working_dir`.
pub(crate) fn detect_workdir_path_duplication(wd_rel: &str, command: &str) -> Option<String> {
    let wd = wd_rel
        .trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_lowercase();
    if wd.is_empty() || wd == "." {
        return None;
    }
    let prefix = format!("{wd}/");
    let toks: Vec<String> = command
        .split(|c: char| c.is_whitespace() || matches!(c, '&' | ';' | '|' | '(' | ')'))
        .filter(|t| !t.is_empty())
        .map(|t| t.trim_start_matches("./").to_lowercase())
        .collect();
    for (i, tok) in toks.iter().enumerate() {
        // Duplica se: path con prefisso `<wd>/` (percorso inequivocabile) oppure
        // `cd <wd>` esatto (naviga nella CWD gia' impostata). Il caso `cd <wd>/sub`
        // ricade gia' nel prefisso, quindi non serve una condizione a parte.
        let cd_into_wd = i > 0 && toks[i - 1] == "cd" && *tok == wd;
        if tok.starts_with(&prefix) || cd_into_wd {
            return Some(format!(
                "[working_dir gia' applicato] Il comando gira GIA' dentro '{wd_rel}' \
                 (working_dir E' la directory di lavoro corrente). Il token '{tok}' ripete \
                 quel percorso: la directory diventerebbe '<...>/{wd}/{wd}', che non esiste, \
                 e rm/install/build opererebbero sulla dir sbagliata lasciando l'ambiente \
                 incoerente. Correggi il comando: togli 'cd {wd}' e i prefissi '{wd}/' dai \
                 path (usa ad es. 'node_modules', non '{wd}/node_modules'). In alternativa \
                 ometti 'working_dir' e usa i percorsi completi dalla root del progetto."
            ));
        }
    }
    None
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
    fn is_long_oneshot_riconosce_install_test_add() {
        // Regressione pannello Servizi: questi comandi venivano registrati
        // come kind='service' e restavano per sempre nella lista servizi.
        assert!(is_long_oneshot("pnpm install"));
        assert!(is_long_oneshot("npx playwright test --project=chromium"));
        assert!(is_long_oneshot("pnpm add -D @playwright/test"));
        assert!(is_long_oneshot("npm run build"));
        assert!(is_long_oneshot("npx vite build"));
        // I server long-running NON sono one-shot.
        assert!(!is_long_oneshot("node server.js"));
        assert!(!is_long_oneshot("npm run dev"));
        assert!(!is_long_oneshot("npm start"));
    }

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

    /// La riga di shell riporta l'esito del comando che CONTA, non dell'ultimo
    /// stadio della pipe.
    ///
    /// Esegue davvero la shell dell'agente (`sandbox::agent_shell`, la stessa
    /// della produzione) invece di simulare: il difetto misurato e' proprio che
    /// `npm install ... | tail` tornava 0 mentre l'install falliva, e un test che
    /// non attraversa una shell vera non lo vedrebbe.
    #[test]
    fn shell_line_non_lascia_che_la_pipe_mascheri_il_fallimento() {
        let esegui = |cmd: String| -> i32 {
            std::process::Command::new(crate::sandbox::agent_shell())
                .arg("-c")
                .arg(cmd)
                .output()
                .expect("shell agente")
                .status
                .code()
                .unwrap_or(-1)
        };

        // Il caso reale: un comando che FALLISCE dietro una pipe.
        assert_eq!(
            esegui("exit 7 | tail -5".to_string()),
            0,
            "premessa del difetto: senza pipefail la pipe riporta l'esito di tail"
        );
        assert_ne!(
            esegui(shell_line("exit 7 | tail -5")),
            0,
            "con pipefail il fallimento dietro la pipe deve emergere"
        );

        // Un comando che riesce resta riuscito (nessun falso allarme).
        assert_eq!(esegui(shell_line("echo ok | tail -1")), 0);
    }

    #[test]
    fn pkg_mutation_riconosce_i_comandi_misurati_e_non_gli_innocui() {
        // I DUE comandi realmente concorrenti misurati su verifica-wd (07:33:29,
        // run_id diversi, distanza ZERO secondi): entrambi devono entrare in
        // sezione critica, altrimenti si sovrascrivono node_modules a vicenda.
        assert!(is_package_manager_mutation("npm install"));
        assert!(is_package_manager_mutation(
            "cd backend && npm install typescript --no-save"
        ));
        // Altre forme viste nei run reali.
        assert!(is_package_manager_mutation("npm install --legacy-peer-deps"));
        assert!(is_package_manager_mutation("npm install -D typescript"));
        assert!(is_package_manager_mutation(
            "cd frontend && rm -rf node_modules package-lock.json && npm install"
        ));
        assert!(is_package_manager_mutation("npm ci"));
        // Altri ecosistemi: la corruzione da concorrenza non e' specifica di npm.
        assert!(is_package_manager_mutation("pnpm add -D vite"));
        assert!(is_package_manager_mutation("yarn install --frozen-lockfile"));
        assert!(is_package_manager_mutation("pip install -r requirements.txt"));
        assert!(is_package_manager_mutation("poetry add fastapi"));
        assert!(is_package_manager_mutation("go mod download"));

        // NON deve serializzare cio' che non tocca le dipendenze: sarebbe
        // parallelismo perso per nulla.
        assert!(!is_package_manager_mutation("npm run build"));
        assert!(!is_package_manager_mutation("npm run dev"));
        assert!(!is_package_manager_mutation("npx tsc --noEmit"));
        assert!(!is_package_manager_mutation("ls node_modules/.bin"));
        assert!(!is_package_manager_mutation("cat package.json"));
        // `cargo` e' escluso di proposito (ha un file-lock interno).
        assert!(!is_package_manager_mutation("cargo build"));
        // Il sotto-comando mutante deve venire DOPO il package manager.
        assert!(!is_package_manager_mutation("echo install npm"));
    }

    #[test]
    fn detect_workdir_dup_blocca_solo_la_vera_duplicazione() {
        // Caso reale misurato (diag-deps 2026-07-23): working_dir=frontend +
        // 'rm -rf frontend/node_modules...' -> frontend/frontend -> il rm non tocca
        // il vero node_modules, l'ambiente resta incoerente. DEVE bloccare.
        let d = detect_workdir_path_duplication(
            "frontend",
            "rm -rf frontend/node_modules/.package-lock.json frontend/package-lock.json",
        );
        assert!(d.is_some(), "path 'frontend/...' con working_dir=frontend deve bloccare");
        assert!(d.unwrap().contains("working_dir"), "il messaggio spiega il working_dir");
        // 'cd <wd>' = navigazione nella CWD gia' impostata -> blocca.
        assert!(detect_workdir_path_duplication("frontend", "cd frontend && npm install").is_some());
        // 'cd <wd>/sub' idem.
        assert!(detect_workdir_path_duplication("backend", "cd backend/src && ls").is_some());
        // './frontend' normalizzato = frontend.
        assert!(detect_workdir_path_duplication("./frontend", "cat frontend/tsconfig.json").is_some());

        // NESSUN falso positivo:
        // - comando senza duplicazione del working_dir.
        assert!(detect_workdir_path_duplication("frontend", "npm install").is_none());
        // - '<wd>' NUDO (non un path) non e' bloccato (es. echo/grep della parola).
        assert!(detect_workdir_path_duplication("frontend", "echo frontend build ok").is_none());
        // - riferimento a un'ALTRA dir (non il working_dir) non e' duplicazione.
        assert!(detect_workdir_path_duplication("backend", "cat frontend/package.json").is_none());
        // - working_dir root/vuoto: nessun segmento da duplicare.
        assert!(detect_workdir_path_duplication("", "cd frontend && npm install").is_none());
        assert!(detect_workdir_path_duplication(".", "cd frontend && npm install").is_none());
    }
}
