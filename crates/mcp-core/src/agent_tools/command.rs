//! Tool comandi shell: run_command (con auto-routing long-running) e run_tests.

use super::*;
// Punto unico (regola L) di derivazione nome DB progetto e settings cluster app.
use crate::project_db_routes::load_app_db_setting;

/// Durata del "probe" per rilevare comandi long-running non noti.
/// Se il processo non termina entro questo tempo, viene killato e ri-lanciato nel terminale.
const RUN_COMMAND_PROBE_SECS: u64 = 10;
/// Comandi one-shot LUNGHI (install/build/compile/test/migrate) NON sono server:
/// vanno attesi in sincrono a lungo, NON instradati a run_service (semantica
/// errata + su Windows il wizard setsid/nohup e' rotto -> il processo "service"
/// muore subito). Timeout sincrono generoso.
const LONG_ONESHOT_PROBE_SECS: u64 = 300;

const RUN_TESTS_DEFAULT_TIMEOUT: u64 = 120;
const RUN_TESTS_MAX_TIMEOUT: u64 = 300;

/// Nome con cui questo tool e' esposto al modello: identita' per il gate di
/// redazione, l'audit di sicurezza e la guardia della suite Playwright.
const NOME_TOOL: &str = "run_command";

/// Attesa massima per entrare nella sezione critica dei package manager.
/// Deliberatamente MAGGIORE di `LONG_ONESHOT_PROBE_SECS`: chi tiene il lock e' un
/// install, e il probe lo uccide comunque entro 300s. Aspettare piu' a lungo del
/// massimo che il detentore puo' vivere garantisce che non si rinunci mentre un
/// install legittimo e' ancora in corso.
const PKG_LOCK_WAIT_SECS: u64 = 420;

/// Serializza per PROGETTO i comandi che mutano l'albero delle dipendenze
/// (`is_package_manager_mutation`). Stesso idioma di `PROVISION_LOCKS`
/// (project_db_routes/provision.rs) e stessa ragione: npm/pnpm/yarn/pip non sono
/// concurrency-safe sulla stessa directory di dipendenze, e i sub-agenti sono
/// task tokio dello STESSO processo, quindi un lock in-process li copre tutti.
///
/// La chiave e' il PROGETTO, non la directory: un comando puo' cambiare directory
/// da solo (`cd backend && npm install` con working_dir=root), quindi una chiave
/// per-directory sarebbe elusa proprio dal caso misurato che ha corrotto
/// l'ambiente. Serializzare per progetto costa un po' di parallelismo sugli
/// install e in cambio rende impossibile la corruzione.
static PKG_MANAGER_LOCKS: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Handle del lock package-manager di un progetto (vedi [`PKG_MANAGER_LOCKS`]).
fn pkg_manager_lock(project_id: Uuid) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let mut map = PKG_MANAGER_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(project_id)
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Entra nella sezione critica dei package manager se il comando muta l'albero
/// delle dipendenze; altrimenti non prende alcun lock e resta parallelo.
///
/// `Ok(Some(guard))` = sezione critica acquisita (il chiamante la tiene viva per
/// tutta l'esecuzione), `Ok(None)` = comando che non tocca le dipendenze,
/// `Err(messaggio)` = attesa scaduta, da riportare all'agente.
async fn acquire_pkg_manager_slot(
    project_id: Uuid,
    command: &str,
) -> Result<Option<tokio::sync::OwnedMutexGuard<()>>, String> {
    if !super::helpers::is_package_manager_mutation(command) {
        return Ok(None);
    }
    let lock = pkg_manager_lock(project_id);
    match tokio::time::timeout(
        std::time::Duration::from_secs(PKG_LOCK_WAIT_SECS),
        lock.lock_owned(),
    )
    .await
    {
        Ok(g) => Ok(Some(g)),
        // Oltre il tempo massimo di vita del detentore: NON si procede in
        // parallelo (sarebbe proprio la corruzione da evitare), si riporta il
        // fatto cosi' l'agente puo' riprovare invece di rompere l'ambiente.
        Err(_) => Err(format!(
            "[Dipendenze occupate] Un altro comando di installazione e' in corso su questo \
             progetto e non si e' liberato entro {PKG_LOCK_WAIT_SECS}s. I package manager non \
             sono sicuri in parallelo sulla stessa directory: eseguire adesso corromperebbe \
             node_modules. Riprova questo stesso comando fra poco, oppure prosegui con un \
             lavoro che non tocchi le dipendenze."
        )),
    }
}

/// Drena stdout/stderr di un processo figlio IN PARALLELO a `child.wait()` per
/// evitare il deadlock del buffer pipe Linux (~64 KB): comandi che producono
/// >64KB (playwright test, npm install verbose) bloccherebbero la pipe e
/// `child.wait()` non ritornerebbe mai. Ritorna i due task tokio che accumulano
/// i byte; vanno awaited DOPO `child.wait()`. Punto unico (regola L): usato da
/// `tool_run_command` e `tool_run_tests`.
fn spawn_output_drainers(
    child: &mut tokio::process::Child,
) -> (
    tokio::task::JoinHandle<Vec<u8>>,
    tokio::task::JoinHandle<Vec<u8>>,
) {
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout_handle {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr_handle {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
        }
        buf
    });
    (stdout_task, stderr_task)
}

// `record_playwright_job` viveva qui: registrava un job `kind='playwright_test'`
// A POSTERIORI per qualunque comando che CONTENESSE "playwright", eseguito in
// proprio da questi tool generici. Rimossa con la guardia `playwright_cli`
// (regola L, esecutore unico): la suite non passa piu' di qui, quindi qui non
// c'e' piu' alcun esito di suite da registrare — lo scrive il runner, che e'
// anche l'unico a sapere quali test sono partiti. Cio' che resta e' cio' che
// non era mai stato un test: `playwright install`, `show-report`, `codegen`,
// perfino `cat playwright.config.ts`, che quel `contains` registrava come
// esecuzioni della suite nel pannello.

/// Progetto con DB registrato e `allow_ddl_override = false` (default): schema change solo via migration.
async fn strict_migration_only_project(ctx: &AgentToolContext) -> bool {
    matches!(
        sqlx::query_scalar::<_, bool>(
            "SELECT allow_ddl_override FROM project_database_config WHERE project_id = $1 LIMIT 1",
        )
        .bind(ctx.project_id)
        .fetch_optional(&*ctx.db)
        .await,
        Ok(Some(false))
    )
}

fn shell_command_bypasses_migration_policy(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    c.contains("flyway")
        || c.contains("liquibase")
        || c.contains("alembic upgrade")
        || c.contains("alembic downgrade")
        || c.contains("prisma migrate")
        || c.contains("dotnet ef database update")
        || c.contains("sqlx migrate")
        || c.contains("knex migrate")
        || c.contains("manage.py migrate")
        || c.contains("rails db:migrate")
        || c.contains("rake db:migrate")
        || (c.contains("-f ")
            && (c.contains("migrat") || c.contains("/migrations/") || c.contains("\\migrations\\")))
}

fn shell_looks_like_sql_cli_with_ddl(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    let sql_cli = lower.contains("psql")
        || lower.contains("sqlcmd")
        || lower.contains("sqlite3")
        || lower.starts_with("mysql")
        || lower.contains(" mysql ")
        || lower.contains("/mysql ")
        || lower.contains("mysql -");
    if !sql_cli {
        return false;
    }
    crate::nexus_tools::db_helper::contains_ddl_statement(cmd)
}

/// Applica le guardie di sicurezza pre-esecuzione a un comando shell: rifiuto
/// dei placeholder di redazione copiati come valori (incidente Beaty-Book) e
/// blacklist server-side dei comandi infrastruttura-distruttivi (Livello 0
/// GUARDRAIL). Ritorna `Some(messaggio)` se il comando va bloccato, `None` se
/// puo' proseguire. Estratto da `tool_run_command` (behavior-preserving).
async fn command_security_gate(ctx: &AgentToolContext, command: &str) -> Option<String> {
    // Placeholder di redazione copiati come valori (incidente Beaty-Book):
    // eseguire `DATABASE_URL=[REDACTED:...] node server.js` produce solo
    // errori a runtime. Punto unico: security::redaction_guard (regola L).
    if let Some(msg) = crate::security::redaction_guard::enforce_no_redacted_placeholder(
        ctx,
        NOME_TOOL,
        "command",
        command,
    )
    .await
    {
        return Some(msg);
    }

    // ── Livello 0 GUARDRAIL: blocca comandi infrastruttura-distruttivi ──
    // Difesa in profondita': blacklist server-side che non puo' essere
    // bypassata dal prompt utente / jailbreak. Vedi safety.rs per la lista
    // pattern (psql -d nexus, prisma migrate reset, docker exec ideai-*,
    // DROP/TRUNCATE/DELETE su tabelle Nexus, rm -rf su /home/administrator/ideai).
    if let Some(reason) = super::safety::check_command(command) {
        tracing::warn!(
            "SECURITY_GUARDRAIL: comando BLOCCATO category={} project_id={} cmd_excerpt={:?}",
            reason.category,
            ctx.project_id,
            command.chars().take(160).collect::<String>(),
        );
        let _ = persist_security_audit(ctx, command, &reason).await;
        // PR hardening: audit trail centralizzato (oltre al log security_audit esistente)
        crate::security::record_audit(
            crate::security::AuditEntry::blocked(ctx.project_id, "command_blocked", "command")
                .with_resource(reason.category.to_string())
                .with_details(serde_json::json!({
                    "command_excerpt": command.chars().take(200).collect::<String>(),
                    "reason": reason.message,
                })),
        );
        return Some(super::safety::format_blocked_result(command, &reason));
    }

    None
}

/// Blocca l'esecuzione se il progetto e' migration-only e il comando e' un DDL
/// via CLI SQL ad-hoc (psql/mysql/sqlcmd) non veicolato da un tool di migration.
/// Ritorna `Some(messaggio)` in caso di blocco. Estratto da `tool_run_command`
/// (behavior-preserving).
async fn migration_only_block(ctx: &AgentToolContext, command: &str) -> Option<String> {
    if strict_migration_only_project(ctx).await
        && !shell_command_bypasses_migration_policy(command)
        && shell_looks_like_sql_cli_with_ddl(command)
    {
        return Some(format!(
            "[BLOCCATO — policy database progetto]\n\
             Questo progetto richiede modifiche di schema solo tramite migration versionate (file nel repo + registro Nexus). Non eseguire DDL con psql/mysql/sqlcmd ad-hoc.\n\
             Usa i tool `project_db_create_migration` e `project_db_apply_migration`, oppure il tool di migration dello stack (Flyway, Alembic, Prisma, dotnet ef, ecc.).\n\
             Per eccezioni controllate, un admin può impostare `allow_ddl_override` sulla connessione DB del progetto.\n\
             ---\nComando: {}",
            command.chars().take(400).collect::<String>()
        ));
    }
    None
}

/// Instrada il comando a run_service se richiesto esplicitamente (`background`)
/// oppure se riconosciuto come long-running/web-service (Livelli 1-2). Ritorna
/// `Some(messaggio)` se instradato, `None` se il comando va eseguito in-line.
/// Estratto da `tool_run_command` (behavior-preserving).
async fn maybe_route_to_service(
    ctx: &AgentToolContext,
    input: &Value,
    command: &str,
) -> Option<String> {
    let explicit_bg = input
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // ── Livello 1: parametro background esplicito dall'AI ──
    if explicit_bg {
        let routed = service::tool_run_service(ctx, input, "service").await;
        return Some(format!(
            "[Background] Comando avviato come servizio server-side (background=true).\n{}",
            routed
        ));
    }

    // One-shot lunghi (build/install/test): esecuzione sincrona, non run_service.
    if is_long_oneshot(command) {
        return None;
    }

    // ── Livello 2: lista hardcoded di comandi noti ──
    if looks_like_long_running_command(command, &ctx.long_running_patterns)
        || service::looks_like_web_service(command)
    {
        let routed = service::tool_run_service(ctx, input, "service").await;
        return Some(format!(
            "[Auto-routing] Comando long-running/web-service rilevato: avviato come servizio \
             server-side (PORT allocata nel bucket del progetto).\n{}",
            routed
        ));
    }
    None
}

/// Legge il parametro `working_dir` grezzo dall'input JSON di un tool. Punto
/// unico del nome del campo: usato ovunque questi tool leggono `working_dir`
/// PRIMA di risolverlo in path (la risoluzione resta a [`resolve_work_dir`]).
fn working_dir_param(input: &Value) -> Option<&str> {
    input.get("working_dir").and_then(Value::as_str)
}

/// Risolve la working directory dal parametro `working_dir` (relativo alla root
/// di progetto) o ricade sulla root. Ritorna `Err(messaggio)` se il path e'
/// invalido. Punto unico (regola L): usato da `tool_run_command` e
/// `tool_run_tests`.
fn resolve_work_dir(ctx: &AgentToolContext, input: &Value) -> Result<PathBuf, String> {
    if let Some(sub) = working_dir_param(input) {
        match resolve_relative_path(&ctx.root_path, sub) {
            Ok(p) => Ok(p),
            Err(e) => Err(format!(
                "\u{274C} [Errore percorso working_dir: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            )),
        }
    } else {
        Ok(ctx.root_path.clone())
    }
}

pub(super) async fn tool_run_command(ctx: &AgentToolContext, input: &Value) -> String {
    let command = match input.get("command").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return "\u{274C} [Errore: parametro 'command' mancante]".to_string(),
    };
    if command.trim().is_empty() {
        return "\u{274C} [Errore: comando vuoto]".to_string();
    }

    if let Some(msg) = command_security_gate(ctx, &command).await {
        return msg;
    }

    // Hints DB-driven + guardie di routing pre-esecuzione (privilegiato,
    // migration-only, background/long-running). Err = ritorno anticipato;
    // Ok = prefisso hint da prependare al risultato finale.
    let hints_prefix = match command_hints_and_routing(ctx, input, &command).await {
        Ok(prefix) => prefix,
        Err(msg) => return msg,
    };

    // Blocca la duplicazione working_dir + path nel comando PRIMA dell'esecuzione
    // (causa radice di ambiente incoerente, vedi
    // helpers::detect_workdir_path_duplication): working_dir e' gia' la CWD, se il
    // comando ripete quel segmento ('cd frontend', 'frontend/...') i path si
    // sommano e rm/install/build operano sulla dir sbagliata. Rifiutare qui evita
    // il danno silenzioso; il messaggio dice all'agente come correggere.
    if let Some(wd) = working_dir_param(input) {
        if let Some(msg) = super::helpers::detect_workdir_path_duplication(wd, &command) {
            return format!("{hints_prefix}{msg}");
        }
    }

    // ── Livello 3: probe timeout — esegui, se non finisce in 10s ri-lancia nel terminale ──
    let work_dir = match resolve_work_dir(ctx, input) {
        Ok(p) => p,
        Err(msg) => return msg,
    };

    // Sezione critica dei package manager: due install concorrenti sulla stessa
    // directory di dipendenze si corrompono a vicenda. Il guard vive fino al
    // termine della funzione, quindi copre spawn + attesa.
    let _pkg_guard = match acquire_pkg_manager_slot(ctx.project_id, &command).await {
        Ok(g) => g,
        Err(msg) => return format!("{hints_prefix}{msg}"),
    };

    let child = match spawn_command_child(ctx, &command, &work_dir).await {
        Ok(c) => c,
        Err(msg) => return msg,
    };

    run_command_probe(ctx, input, &command, child, &hints_prefix).await
}

/// Calcola il prefisso hint DB-driven e applica le guardie di routing
/// pre-esecuzione (comandi privilegiati → Sudo Manager, policy migration-only,
/// background/long-running → run_service). Ritorna `Err(messaggio)` se il
/// comando va instradato/bloccato (ritorno anticipato del chiamante), `Ok(prefix)`
/// col prefisso hint se l'esecuzione in-line puo' proseguire. Estratto da
/// `tool_run_command` (behavior-preserving).
async fn command_hints_and_routing(
    ctx: &AgentToolContext,
    input: &Value,
    command: &str,
) -> Result<String, String> {
    // ── Suite Playwright: ha un solo esecutore (punto unico regola L,
    // playwright_cli) ──────────────────────────────────────────────────────
    // Prima di ogni altra guardia di QUESTA funzione, che decidono COME
    // instradare: questa decide SE la riga si esegue qui. Vive in questa
    // funzione — gia' un `Result` di routing pre-esecuzione — cosi'
    // `tool_run_command` non guadagna un ramo proprio per lei: la stessa
    // domanda ("questa riga va instradata altrove?") ha gia' un posto solo.
    if let Some(out) =
        super::playwright_cli::intercetta_suite(ctx, NOME_TOOL, command, working_dir_param(input))
            .await
    {
        return Err(out);
    }

    // ── Command hints (migration 0230) ──────────────────────────────────────
    // Lookup pattern noti in nexus_command_hints (cache 60s). Se match, l'hint
    // viene prependato al risultato finale del comando — guida il modello
    // verso correzioni note (es. shadcn-ui rebrand, create-react-app deprecato)
    // PRIMA che entri in loop di errori. DB-driven, nuovi pattern senza deploy.
    let command_hints = super::command_hints::match_hints(&ctx.db, command).await;
    let hints_prefix = super::command_hints::format_hints_prefix(&command_hints);

    // ── Instradamento comandi privilegiati al Sudo Manager (ADR 0017) ──
    // L'agente puo' installare dipendenze di sistema scrivendo naturalmente
    // `sudo apt-get install -y <pkg>` / `apt install <pkg>` / `apt-get update`
    // o `playwright install --with-deps`: invece di farlo fallire nella shell
    // isolata (NOPASSWD e' concesso SOLO a nexus-sudo-runner, mai a sudo
    // arbitrario), lo instradiamo al gestore privilegiato controllato. Il sudo
    // arbitrario riceve un messaggio guida. Punto unico: privileged.rs (regola L).
    if let Some(routed) = super::privileged::try_route_privileged_command(ctx, command).await {
        return Err(format!("{}{}", hints_prefix, routed));
    }

    if let Some(msg) = migration_only_block(ctx, command).await {
        return Err(msg);
    }

    // ── Livelli 1-2: routing a run_service (background esplicito o comando noto
    // long-running/web-service). Se instradato ritorna il messaggio; None = prosegue. ──
    if let Some(msg) = maybe_route_to_service(ctx, input, command).await {
        return Err(msg);
    }

    Ok(hints_prefix)
}

/// Esegue il child del comando one-shot con la logica di probe timeout: attende
/// fino a `probe_secs` (lungo per gli one-shot install/build, 10s altrimenti);
/// se termina compone l'output finale, se scade re-instrada a run_service o
/// segnala il timeout. Drena stdout/stderr in parallelo a `wait()` per evitare
/// il deadlock della pipe (~64KB). Estratto da `tool_run_command`
/// (behavior-preserving).
async fn run_command_probe(
    ctx: &AgentToolContext,
    input: &Value,
    command: &str,
    mut child: tokio::process::Child,
    hints_prefix: &str,
) -> String {
    // Drain stdout/stderr IN PARALLELO con child.wait() (evita deadlock pipe ~64KB).
    let (stdout_task, stderr_task) = spawn_output_drainers(&mut child);

    // Probe: per gli one-shot LUNGHI (install/build) attesa sincrona lunga; per gli
    // altri 10s, poi re-route a run_service (server long-running tipo dev/serve).
    let is_oneshot = is_long_oneshot(command);
    let probe_secs = if is_oneshot {
        LONG_ONESHOT_PROBE_SECS
    } else {
        RUN_COMMAND_PROBE_SECS
    };
    let probe =
        tokio::time::timeout(std::time::Duration::from_secs(probe_secs), child.wait()).await;

    match probe {
        Ok(Ok(exit_status)) => {
            // Il processo è terminato entro il probe — leggi l'output drainato dai task paralleli
            let exit_code = exit_status.code().unwrap_or(-1);
            let stdout_bytes = stdout_task.await.unwrap_or_default();
            let stderr_bytes = stderr_task.await.unwrap_or_default();
            let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
            format_command_completed(ctx, command, exit_code, &stdout, &stderr, hints_prefix).await
        }
        Ok(Err(e)) => {
            format!("\u{274C} [Errore attesa comando '{}': {}]", command, e)
        }
        Err(_) => {
            // Probe timeout.
            let _ = child.kill().await;
            command_probe_timeout_result(ctx, input, command, is_oneshot, probe_secs, hints_prefix)
                .await
        }
    }
}

/// Avvia il comando one-shot nella shell isolata cross-platform con injection del
/// DB applicativo del progetto. Auto-provisiona il DB (M72) e inietta
/// NEXUS_PROJECT_DB_URL + DATABASE_URL sopra l'env gia' pulito (env_clear + host
/// filtrato): il comando NON vede i segreti Nexus e NON puo' usare il DB
/// 'nexus'. Ritorna `Err(messaggio)` se lo spawn fallisce. Estratto da
/// `tool_run_command` (behavior-preserving).
async fn spawn_command_child(
    ctx: &AgentToolContext,
    command: &str,
    work_dir: &Path,
) -> Result<tokio::process::Child, String> {
    // M72: auto-provisioning del DB applicativo dedicato del progetto e
    // injection di NEXUS_PROJECT_DB_URL + DATABASE_URL nell'env del processo.
    // L'agente NON deve mai usare il DB 'nexus' (infrastruttura). Il DB
    // applicativo si chiama <slug>_app (con `-` → `_` per validita' Postgres).
    // Idempotente: CREATE DATABASE solo se non esiste.
    let (project_db_url, project_db_name) = ensure_project_db_url(&ctx.db, ctx.project_id).await;

    // Shell cross-platform (punto unico crate::sandbox::agent_shell): bash su Unix,
    // Git Bash su Windows. Gli agenti generano comandi in sintassi bash (brace
    // expansion, &&, pipe, pnpm/npm); su Windows /bin/bash non esiste -> os error 3.
    let shell_path = crate::sandbox::agent_shell();

    crate::sandbox::isolated_command(&shell_path)
        .arg("-c")
        .arg(super::helpers::shell_line(command))
        .current_dir(work_dir)
        .env("NEXUS_PROJECT_DB_URL", &project_db_url)
        .env("NEXUS_PROJECT_DB_NAME", &project_db_name)
        .env("DATABASE_URL", &project_db_url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("\u{274C} [Errore avvio comando '{}': {}]", command, e))
}

/// Compone l'output finale di `run_command` per un comando terminato entro il
/// probe: hint semantico + registrazione Playwright + troncamento testa+coda
/// DB-driven. Estratto da `tool_run_command` (behavior-preserving).
async fn format_command_completed(
    ctx: &AgentToolContext,
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    hints_prefix: &str,
) -> String {
    // Con `pipefail` (vedi helpers::shell_line) una pipeline riporta il primo
    // stadio fallito. Se il consumatore a valle chiude presto (`... | head -N`)
    // il produttore muore di SIGPIPE e la pipeline riporterebbe 141: non e' un
    // fallimento del comando, e' il consumatore che ha smesso di leggere. Senza
    // questa normalizzazione, guadagnare l'esito vero degli install (lo scopo di
    // pipefail) costerebbe falsi fallimenti su ogni `| head`.
    let exit_code = if exit_code == super::helpers::EXIT_SIGPIPE {
        0
    } else {
        exit_code
    };
    let hint = command_result_hint(exit_code, stdout, stderr, command);

    let combined = format!(
        "{}EXIT CODE: {}\nSTDOUT:\n{}\nSTDERR:\n{}{}",
        hints_prefix, exit_code, stdout, stderr, hint
    );
    // Troncamento testa+coda NON distruttivo (stesso punto unico di run_tests,
    // regola L): i build tsc/cargo/npm elencano gli errori in ordine col totale
    // "Found N errors" IN FONDO. Tagliare solo la testa (vecchio .take()) faceva
    // perdere la coda con gli ultimi errori + il totale, inducendo l'agente a
    // ri-eseguire il build per "vedere gli altri errori" (loop razionale).
    // Cap DB-driven (regola G), default 16000 >= cap brain cosi' mcp-core non e'
    // mai il collo di bottiglia che decapita la coda prima del brain.
    let max_chars = load_run_command_max_chars(&ctx.db).await;
    if combined.chars().count() > max_chars {
        smart_truncate_test_output(&combined, max_chars)
    } else {
        combined
    }
}

/// Costruisce l'hint semantico da appendere all'output di `run_command` in base
/// all'exit code e alla presenza di output. Estratto da `tool_run_command`
/// (behavior-preserving): stessa logica, nessun effetto osservabile diverso.
fn command_result_hint(exit_code: i32, stdout: &str, stderr: &str, command: &str) -> String {
    if exit_code != 0 {
        let diag = classify_command_error(exit_code, stderr, stdout);
        // Su Windows aggiunge la guida POSIX se il comando usava sintassi
        // cmd/PowerShell (evita il loop repeated_action -> force-close).
        let win = super::helpers::windows_shell_hint(command)
            .map(|h| format!(" {h}"))
            .unwrap_or_default();
        format!("\n\n❌ Comando fallito (exit {exit_code}). {diag}.{win}")
    } else if exit_code == 0 && stdout.trim().is_empty() && stderr.trim().is_empty() {
        "\n[NESSUN RISULTATO: il comando è completato con successo ma non ha prodotto output. \
         Per grep/sed questo significa che il pattern non è stato trovato o il file è vuoto. \
         Non riprovare lo stesso comando — prova un pattern diverso o usa read_file.]"
            .to_string()
    } else if exit_code == 1 && stdout.trim().is_empty() {
        "\n[EXIT CODE 1 + output vuoto: per grep significa nessuna corrispondenza trovata.]"
            .to_string()
    } else {
        String::new()
    }
}

/// Gestisce il timeout del probe di `run_command`: gli one-shot (install/build)
/// segnalano il timeout perche' NON sono server long-running; gli altri vengono
/// re-instradati a run_service. Estratto da `tool_run_command`
/// (behavior-preserving).
async fn command_probe_timeout_result(
    ctx: &AgentToolContext,
    input: &Value,
    command: &str,
    is_oneshot: bool,
    probe_secs: u64,
    hints_prefix: &str,
) -> String {
    if is_oneshot {
        // One-shot (install/build) che non finisce nemmeno in probe_secs:
        // NON è un server long-running -> NON instradare a run_service
        // (semantica errata + su Windows il wizard setsid/nohup è rotto).
        // Segnala il timeout così l'agente può spezzare il comando.
        format!(
            "{}[Timeout] Il comando '{}' non è terminato in {}s ed è stato interrotto. \
             Se è un build/install legittimo molto lungo, eseguilo per passi.",
            hints_prefix,
            command.chars().take(120).collect::<String>(),
            probe_secs
        )
    } else {
        // Long-running non-one-shot (dev server, watcher) → run_service.
        let routed = service::tool_run_service(ctx, input, "service").await;
        format!(
            "[Auto-probe] Il comando non è terminato in {}s — rilevato come long-running.\n\
             Processo server-side terminato e ri-lanciato come servizio.\n{}",
            probe_secs, routed
        )
    }
}

// ---------------------------------------------------------------------------
// run_tests — tool dedicato per cicli test-fix-test iterativi
// ---------------------------------------------------------------------------

/// Esegue i test del progetto in modo sincrono con timeout esteso.
///
/// Dispatchato da `execute_agent_tool` (braccio "run_tests"). Il vecchio
/// chiamante diretto (agent_loop.rs di mcp-core) e' stato smantellato col
/// passaggio del loop al brain Python: il contenimento delle esecuzioni
/// ripetute e' governato dall'anti-loop del brain, non da un contatore qui.
pub(crate) async fn tool_run_tests(ctx: &AgentToolContext, input: &Value) -> String {
    // 1. Determina comando test
    let command = resolve_test_command(ctx, input);

    if command.is_empty() {
        return "\u{274C} [Errore: impossibile rilevare il comando test per questo progetto. \
                Specifica il parametro 'command' (es. 'npm test', 'cargo test', 'pytest').]"
            .to_string();
    }

    // Stessa guardia di `run_command` e per la stessa ragione: questo tool
    // riceve un comando arbitrario dall'agente, e `npx playwright test` scritto
    // qui lancerebbe la suite fuori dal suo unico esecutore.
    if let Some(out) =
        super::playwright_cli::intercetta_suite(ctx, "run_tests", &command, working_dir_param(input))
            .await
    {
        return out;
    }

    // 2. Working directory (punto unico resolve_work_dir, regola L)
    let work_dir = match resolve_work_dir(ctx, input) {
        Ok(p) => p,
        Err(msg) => return msg,
    };

    // 3. Timeout (default 120s, max 300s)
    let timeout = input
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(RUN_TESTS_DEFAULT_TIMEOUT)
        .min(RUN_TESTS_MAX_TIMEOUT);

    // 4. Esecuzione sincrona — NESSUN auto-routing a background
    run_tests_execution(&command, &work_dir, timeout).await
}

/// Esecuzione sincrona dei test (nessun auto-routing a background): spawn nella
/// shell isolata, drain parallelo stdout/stderr, attesa con timeout e
/// formattazione dell'esito (o messaggio di timeout con kill). Estratto da
/// `tool_run_tests` (behavior-preserving).
async fn run_tests_execution(command: &str, work_dir: &Path, timeout: u64) -> String {
    let child = crate::sandbox::isolated_command(&crate::sandbox::agent_shell())
        .arg("-c")
        .arg(super::helpers::shell_line(command))
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => return format!("\u{274C} [Errore avvio test '{}': {}]", command, e),
    };

    // Drain stdout/stderr in parallelo con child.wait() per evitare deadlock pipe (~64KB).
    let (stdout_task, stderr_task) = spawn_output_drainers(&mut child);

    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), child.wait()).await;

    match result {
        Ok(Ok(exit_status)) => {
            let exit_code = exit_status.code().unwrap_or(-1);
            let stdout_bytes = stdout_task.await.unwrap_or_default();
            let stderr_bytes = stderr_task.await.unwrap_or_default();
            let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
            format_run_tests_output(command, exit_code, &stdout, &stderr)
        }
        Ok(Err(e)) => format!("\u{274C} [Errore attesa test '{}': {}]", command, e),
        Err(_) => {
            let _ = child.kill().await;
            format!(
                "=== RUN TEST ===\nComando: {}\n\
                 [TIMEOUT] I test non sono terminati entro {}s.\n\
                 Suggerimento: usa il parametro 'filter' per eseguire un sottoinsieme di test specifici.\n\
                 === FINE RUN TEST ===",
                command, timeout
            )
        }
    }
}

/// Determina il comando test: usa quello esplicito (con eventuale filter
/// appeso) o auto-rileva dai file di config del progetto. Estratto da
/// `tool_run_tests` (behavior-preserving).
fn resolve_test_command(ctx: &AgentToolContext, input: &Value) -> String {
    let explicit_cmd = input.get("command").and_then(Value::as_str);
    let filter = input.get("filter").and_then(Value::as_str);
    if let Some(cmd) = explicit_cmd {
        if let Some(f) = filter {
            format!("{} {}", cmd, f)
        } else {
            cmd.to_string()
        }
    } else {
        detect_test_command(&ctx.root_path, filter)
    }
}

/// Compone il blocco `=== RUN TEST ===` per un'esecuzione test terminata:
/// troncamento intelligente stdout/stderr + label di stato. Estratto da
/// `tool_run_tests` (behavior-preserving).
fn format_run_tests_output(command: &str, exit_code: i32, stdout: &str, stderr: &str) -> String {
    // Troncamento intelligente: preserva errori (in fondo)
    let truncated_stdout = smart_truncate_test_output(stdout, 6000);
    let truncated_stderr = smart_truncate_test_output(stderr, 2000);

    let status_label = if exit_code == 0 {
        "TUTTI I TEST PASSATI"
    } else {
        "TEST FALLITI"
    };
    format!(
        "=== RUN TEST ===\nComando: {}\nStato: {} (exit code: {})\n\n\
         --- STDOUT ---\n{}\n\n--- STDERR ---\n{}\n=== FINE RUN TEST ===",
        command, status_label, exit_code, truncated_stdout, truncated_stderr
    )
}

// Rilevatori del comando test per ecosistema. Ognuno ritorna `Some(comando)` se
// il progetto corrisponde al proprio marker (file di config), `None` altrimenti.
// Estratti da `detect_test_command` per tenerla sotto soglia di lunghezza e
// complessita' (behavior-preserving): l'ordine di valutazione nel chiamante
// preserva la precedenza originale (npm > cargo > pytest > dotnet > go > make).

/// package.json con script `test` → `npm test`.
fn detect_npm_test(root: &Path, filter_str: &str) -> Option<String> {
    let pkg_json = root.join("package.json");
    if !pkg_json.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&pkg_json).ok()?;
    let v = serde_json::from_str::<Value>(&content).ok()?;
    // Presenza dello script `test`: assente => nessun match (operatore `?`).
    v.get("scripts").and_then(|s| s.get("test"))?;
    Some(if filter_str.is_empty() {
        "npm test".to_string()
    } else {
        format!("npm test -- {}", filter_str)
    })
}

/// Cargo.toml → `cargo test`.
fn detect_cargo_test(root: &Path, filter_str: &str) -> Option<String> {
    if !root.join("Cargo.toml").exists() {
        return None;
    }
    Some(if filter_str.is_empty() {
        "cargo test".to_string()
    } else {
        format!("cargo test {}", filter_str)
    })
}

/// pyproject.toml / pytest.ini / setup.cfg / setup.py → `python -m pytest`.
fn detect_pytest(root: &Path, filter_str: &str) -> Option<String> {
    let has_pytest = root.join("pyproject.toml").exists()
        || root.join("pytest.ini").exists()
        || root.join("setup.cfg").exists()
        || root.join("setup.py").exists();
    if !has_pytest {
        return None;
    }
    Some(if filter_str.is_empty() {
        "python -m pytest -v".to_string()
    } else {
        format!("python -m pytest -v -k '{}'", filter_str)
    })
}

/// *.sln / *.csproj nella root → `dotnet test`.
fn detect_dotnet_test(root: &Path, filter_str: &str) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sln") || name.ends_with(".csproj") {
            return Some(if filter_str.is_empty() {
                "dotnet test".to_string()
            } else {
                format!("dotnet test --filter {}", filter_str)
            });
        }
    }
    None
}

/// go.mod → `go test ./...`.
fn detect_go_test(root: &Path, filter_str: &str) -> Option<String> {
    if !root.join("go.mod").exists() {
        return None;
    }
    Some(if filter_str.is_empty() {
        "go test ./...".to_string()
    } else {
        format!("go test -run {} ./...", filter_str)
    })
}

/// Makefile con target `test:` → `make test`.
fn detect_make_test(root: &Path) -> Option<String> {
    let makefile = root.join("Makefile");
    if !makefile.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&makefile).ok()?;
    if content.contains("\ntest:") || content.starts_with("test:") {
        return Some("make test".to_string());
    }
    None
}

/// Auto-rileva il comando test dal progetto analizzando i file di configurazione.
/// Precedenza: npm > cargo > pytest > dotnet > go > make (invariata).
fn detect_test_command(root: &Path, filter: Option<&str>) -> String {
    let filter_str = filter.unwrap_or("");

    if let Some(cmd) = detect_npm_test(root, filter_str) {
        return cmd;
    }
    if let Some(cmd) = detect_cargo_test(root, filter_str) {
        return cmd;
    }
    if let Some(cmd) = detect_pytest(root, filter_str) {
        return cmd;
    }
    if let Some(cmd) = detect_dotnet_test(root, filter_str) {
        return cmd;
    }
    if let Some(cmd) = detect_go_test(root, filter_str) {
        return cmd;
    }
    if let Some(cmd) = detect_make_test(root) {
        return cmd;
    }

    String::new()
}

/// Troncamento intelligente per output test: 20% testa + 80% coda.
/// I sommari di errore sono tipicamente alla fine dell'output.
fn smart_truncate_test_output(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }
    let head_size = max_chars / 5;
    let tail_size = max_chars * 4 / 5;
    let head: String = output.chars().take(head_size).collect();
    let tail: String = {
        let chars: Vec<char> = output.chars().collect();
        if chars.len() > tail_size {
            chars[chars.len() - tail_size..].iter().collect()
        } else {
            output.to_string()
        }
    };
    let omitted = output.len().saturating_sub(head_size + tail_size);
    format!(
        "{}\n\n[... {} caratteri omessi — errori e sommario preservati in fondo ...]\n\n{}",
        head, omitted, tail
    )
}

// `PlaywrightSummary` / `parse_playwright_summary` / `extract_test_count`
// vivevano qui: leggevano i contatori di Playwright dal TESTO dell'output per
// riempire il job registrato a posteriori. Rimossi con quel job: i contatori
// veri li produce il runner leggendo il proprio stdout riga per riga
// (`parse_playwright_output_stats` in testing.rs), che e' anche l'unico punto
// che sa quale suite ha lanciato.

/// Persiste l'evento di blocco su `nexus_security_audit` (mig 0154).
/// Best-effort: se la tabella non esiste o il DB e' down, log warn e prosegue.
async fn persist_security_audit(
    ctx: &AgentToolContext,
    command: &str,
    reason: &super::safety::BlockReason,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO nexus_security_audit
           (project_id, user_id, session_id, tool_name, command_excerpt, category, message, blocked)
           VALUES ($1, $2, $3, $4, $5, $6, $7, true)"#,
    )
    .bind(ctx.project_id)
    .bind(ctx.user_id)
    .bind(ctx.session_id)
    .bind(NOME_TOOL)
    .bind(command.chars().take(2000).collect::<String>())
    .bind(reason.category)
    .bind(reason.message)
    .execute(&*ctx.db)
    .await
    .map(|_| ())
}

/// M72+M74+M75 — garantisce che esista un DB applicativo dedicato per il progetto
/// e ritorna `(connection_url, db_name)`. Idempotente.
///
/// **Architettura di isolamento (Livello 6 + Livello 2):**
/// - Il DB applicativo vive in un container Postgres SEPARATO (`postgres-app`)
///   dal container infrastruttura (`postgres-nexus`). Cluster distinti: non c'e'
///   modo che l'agente raggiunga il DB Nexus anche con escalation di privilegi.
/// - L'URL ritornato usa il role `nexus_app` (NOSUPERUSER, NOCREATEROLE,
///   NOREPLICATION, NOBYPASSRLS, CREATEDB) — vedi infra/sql/init-postgres-app.sh.
///
/// Settings DB-driven (cache nei caller via sqlx pool, refresh 60s lato app):
///   - nexus_app_db_host / nexus_app_db_port (default: localhost:5434)
///   - nexus_app_db_user / nexus_app_db_password (default: nexus_app/<dev>)
///   - nexus_app_admin_user / nexus_app_admin_password (per CREATE DATABASE)
///
/// Strategia:
/// 1. Legge `projects.slug`, sanifica → nome DB `<slug>_app`
/// 2. Connessione admin (al container postgres-app, NON al nexus) per
///    CREATE DATABASE idempotente con OWNER = nexus_app
/// 3. Ritorna URL `postgresql://nexus_app:<pwd>@<host>:<port>/<db>`
///
/// Se il container postgres-app non risponde, ritorna comunque un URL valido
/// così l'env injection avviene — l'agente vedra' un errore di connessione
/// che NON contaminera' il DB Nexus.
///
/// PUNTO UNICO (regola L) dell'injection DB progetto: chiamata sia da
/// `run_command` (one-shot) sia da `agent_processes::spawn_agent_process`
/// (servizi long-running) — per questo prende (pool meta, project_id) e non
/// l'intero AgentToolContext.
pub(crate) async fn ensure_project_db_url(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
) -> (String, String) {
    let slug: Option<String> =
        sqlx::query_scalar("SELECT slug FROM projects WHERE id = $1 LIMIT 1")
            .bind(project_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

    // Nome DB e settings dal punto unico (regola L): la derivazione viveva anche
    // qui in copia, e il troncamento divergente (52 vs 56) faceva creare due
    // database fisici per lo stesso progetto su slug lunghi.
    let db_name = crate::project_db_routes::derive_project_db_name(
        slug.as_deref(),
        project_id,
        crate::project_db_routes::DbRole::App,
    );

    // Lettura settings DB-driven (single batch, default conservativi).
    let host = load_app_db_setting(db, "nexus_app_db_host", "localhost").await;
    let port = load_app_db_setting(db, "nexus_app_db_port", "5434").await;
    let user = load_app_db_setting(db, "nexus_app_db_user", "nexus_app").await;
    let pwd = load_app_db_setting(db, "nexus_app_db_password", "nexus_app_dev_secret").await;
    let admin_user = load_app_db_setting(db, "nexus_app_admin_user", "nexus_admin").await;
    let admin_pwd = load_app_db_setting(db, "nexus_app_admin_password", "nexus_admin_secret").await;

    provision_app_database(
        &host,
        &port,
        &user,
        &admin_user,
        &admin_pwd,
        &db_name,
        project_id,
    )
    .await;

    let url = format!("postgresql://{user}:{pwd}@{host}:{port}/{db_name}");

    register_project_db_config(db, project_id, &db_name, &url).await;

    (url, db_name)
}

/// CREATE DATABASE idempotente sul container postgres-app via admin role.
/// Best-effort: se l'admin pool o il CREATE falliscono, logga WARN e prosegue —
/// l'URL viene comunque iniettato (l'agente vedra' un connection error che NON
/// contamina il DB Nexus). Estratto da `ensure_project_db_url`
/// (behavior-preserving).
async fn provision_app_database(
    host: &str,
    port: &str,
    user: &str,
    admin_user: &str,
    admin_pwd: &str,
    db_name: &str,
    project_id: uuid::Uuid,
) {
    let admin_url = format!("postgresql://{admin_user}:{admin_pwd}@{host}:{port}/postgres");
    match sqlx::PgPool::connect(&admin_url).await {
        Ok(admin_pool) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
                    .bind(db_name)
                    .fetch_one(&admin_pool)
                    .await
                    .unwrap_or(false);
            if !exists {
                // OWNER = nexus_app cosi' il role applicativo ha pieni poteri
                // sul SUO DB (e solo quello).
                let create_sql = format!(
                    "CREATE DATABASE \"{}\" OWNER \"{}\" TEMPLATE template0",
                    db_name, user
                );
                if let Err(e) = sqlx::query(&create_sql).execute(&admin_pool).await {
                    tracing::warn!(
                        "ensure_project_db_url: CREATE DATABASE \"{}\" fallita: {} (procedo, URL comunque iniettato)",
                        db_name, e
                    );
                } else {
                    tracing::info!(
                        "ensure_project_db_url: provisioned db=\"{}\" owner=\"{}\" project_id={}",
                        db_name,
                        user,
                        project_id
                    );
                }
            }
            admin_pool.close().await;
        }
        Err(e) => {
            tracing::warn!(
                "ensure_project_db_url: admin pool fallito su {}: {}. URL iniettato comunque (agente vedra' connection error, NON contaminera' nexus).",
                admin_url.replacen(admin_pwd, "***", 1), e
            );
        }
    }
}

/// Registra/aggiorna il DB applicativo in `project_database_config` (idempotente
/// via ON CONFLICT su `(project_id, LOWER(name))`) e notifica il pannello DB
/// frontend via dispatcher SSE. Best-effort. Estratto da `ensure_project_db_url`
/// (behavior-preserving).
///
/// Senza questa registrazione, il DB veniva creato sul container postgres-app e
/// usato dall'agente (via env var NEXUS_PROJECT_DB_URL/DATABASE_URL), ma il
/// pannello DB Nexus restava vuoto perche' legge solo da project_database_config.
///
/// Nota: l'UNIQUE INDEX della mig 0083 (`uq_project_database_config_project_name`)
/// e' su un'espressione (LOWER(name)), quindi NON puo' essere promosso a
/// CONSTRAINT nominato e va referenziato con `ON CONFLICT (cols)` — non
/// `ON CONFLICT ON CONSTRAINT <nome>`, che richiede un constraint vero e
/// provocava "constraint does not exist" (148 errori/log spam, regola H).
/// connection_secret e' bytea contenente la URL raw (decifrato a runtime con
/// ENCODE escape — vedi project_db_set_connection per il pattern).
async fn register_project_db_config(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    db_name: &str,
    url: &str,
) {
    let upsert_res = sqlx::query(
        r#"INSERT INTO project_database_config
            (id, project_id, name, engine, hosting_mode, connection_secret,
             migration_tool, migration_path, is_primary, allow_ddl_override,
             detection_metadata, created_at, updated_at)
           VALUES (gen_random_uuid(), $1, 'primary', 'postgres', 'internal', $2::bytea,
                   NULL, NULL, true, false, '{"source":"auto_provisioning"}'::jsonb,
                   NOW(), NOW())
           ON CONFLICT (project_id, LOWER(name))
           DO UPDATE SET
             connection_secret = EXCLUDED.connection_secret,
             engine = EXCLUDED.engine,
             hosting_mode = EXCLUDED.hosting_mode,
             updated_at = NOW()"#,
    )
    .bind(project_id)
    .bind(url.as_bytes())
    .execute(db)
    .await;

    log_project_db_config_result(upsert_res, project_id, db_name);
}

/// Logga l'esito dell'upsert di `register_project_db_config` ed emette l'evento
/// SSE `DbConfigUpdated` quando l'upsert ha effettivamente scritto. Estratto
/// (behavior-preserving) per tenere il chiamante sotto soglia di lunghezza.
fn log_project_db_config_result(
    upsert_res: Result<sqlx::postgres::PgQueryResult, sqlx::Error>,
    project_id: uuid::Uuid,
    db_name: &str,
) {
    match upsert_res {
        Ok(r) => {
            if r.rows_affected() > 0 {
                let action = if r.rows_affected() == 1 {
                    "created_or_updated"
                } else {
                    "updated"
                };
                tracing::info!(
                    "ensure_project_db_url: project_database_config registered \
                     project_id={} db_name={} action={}",
                    project_id,
                    db_name,
                    action
                );
                // Notifica il pannello DB frontend via dispatcher SSE.
                nexus_events::dispatcher::emit_global(
                    project_id,
                    nexus_events::event::ProjectEvent::DbConfigUpdated {
                        name: "primary".to_string(),
                        engine: Some("postgres".to_string()),
                        action: action.to_string(),
                    },
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "ensure_project_db_url: UPSERT project_database_config fallita ({}). \
                 URL iniettato comunque, ma pannello DB UI non vedra' la connessione.",
                e
            );
        }
    }
}


/// Cap massimo (in caratteri) dell'output combinato di `run_command`, DB-driven
/// (regola G). Default 16000: deliberatamente alto e >= del cap del brain, cosi'
/// mcp-core NON e' mai il primo collo di bottiglia che decapita la coda con gli
/// ultimi errori + "Found N errors" prima che l'output arrivi al brain.
/// La key e' veicolata da migrazione (settings.agent.command.run_command_max_chars).
const RUN_COMMAND_MAX_CHARS_DEFAULT: usize = 16000;

async fn load_run_command_max_chars(db: &sqlx::PgPool) -> usize {
    let raw = load_app_db_setting(
        db,
        "agent.command.run_command_max_chars",
        &RUN_COMMAND_MAX_CHARS_DEFAULT.to_string(),
    )
    .await;
    match raw.trim().parse::<usize>() {
        Ok(n) if n > 0 => n,
        _ => RUN_COMMAND_MAX_CHARS_DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prova che la riga NON e' stata eseguita da questi tool ma consegnata
    /// all'esecutore unico: su una root vuota il runner si ferma da solo al suo
    /// primo controllo, e quel messaggio lo puo' produrre solo lui.
    ///
    /// Regola O: si asserisce la CONSEGUENZA (chi ha eseguito), non la stringa
    /// del riconoscimento; e l'input e' la riga come la scrive l'agente, non un
    /// `InvocazioneSuite` costruito a mano. Mutazione verificata: rimossa la
    /// guardia da `tool_run_command`, l'output torna a essere "EXIT CODE:" del
    /// comando eseguito in proprio e il test rosseggia.
    const MARCA_ESECUTORE: &str = "[run_playwright_tests] Playwright non trovato nel progetto";

    #[tokio::test]
    async fn run_command_non_esegue_la_suite_playwright_in_proprio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test(dir.path().to_path_buf());
        let out = tool_run_command(&ctx, &serde_json::json!({"command": "npx playwright test"})).await;
        assert!(
            out.contains(MARCA_ESECUTORE),
            "la suite deve passare dall'esecutore unico, output: {out}"
        );
        assert!(
            !out.contains("EXIT CODE:"),
            "run_command non deve aver eseguito la riga in proprio: {out}"
        );
    }

    /// Gemello per `run_tests`: e' l'altro tool che riceve un comando arbitrario
    /// dall'agente, ed era l'altro call site della registrazione a posteriori.
    #[tokio::test]
    async fn run_tests_non_esegue_la_suite_playwright_in_proprio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test(dir.path().to_path_buf());
        let out = tool_run_tests(
            &ctx,
            &serde_json::json!({"command": "npx playwright test --grep smoke"}),
        )
        .await;
        assert!(
            out.contains(MARCA_ESECUTORE),
            "la suite deve passare dall'esecutore unico, output: {out}"
        );
        assert!(
            !out.contains("=== RUN TEST ==="),
            "run_tests non deve aver eseguito la riga in proprio: {out}"
        );
    }

    /// La riga che chiede la suite INSIEME ad altri comandi non si delega (il
    /// runner eseguirebbe la sola suite e `npm ci` sparirebbe in silenzio) e non
    /// si esegue qui: si rifiuta dicendo come spezzarla.
    #[tokio::test]
    async fn riga_composita_con_suite_playwright_e_rifiutata_con_guida() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test(dir.path().to_path_buf());
        let out = tool_run_command(
            &ctx,
            &serde_json::json!({"command": "npm ci && npx playwright test"}),
        )
        .await;
        assert!(out.starts_with('\u{274C}'), "deve essere un errore: {out}");
        assert!(
            out.contains("run_playwright_tests"),
            "il rifiuto deve indirizzare all'esecutore unico: {out}"
        );
        assert!(
            !out.contains("EXIT CODE:") && !out.contains(MARCA_ESECUTORE),
            "ne' eseguita qui ne' delegata a meta': {out}"
        );
    }

    /// Contro-prova: un comando che NOMINA playwright senza lanciare la suite
    /// resta di competenza di questi tool. E' il caso che il vecchio
    /// `contains("playwright")` registrava come esecuzione di test.
    ///
    /// Si ferma alla guardia invece di attraversare tutto `tool_run_command`
    /// perche' il seguito della pipeline legge il DB (hint, policy migration,
    /// routing a servizio) e su un pool lazy ogni lettura costa il proprio
    /// `acquire_timeout`: il costo del test sarebbe minuti, non millisecondi.
    /// Che `run_command` interroghi la guardia lo provano i test qui sopra.
    #[tokio::test]
    async fn altri_usi_del_cli_playwright_restano_a_run_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test(dir.path().to_path_buf());
        for riga in [
            "npx playwright show-report",
            "npx playwright install --with-deps chromium",
            "cat playwright.config.ts",
        ] {
            let deviato =
                super::super::playwright_cli::intercetta_suite(&ctx, "run_command", riga, None)
                    .await;
            assert!(
                deviato.is_none(),
                "non e' una suite e non va deviato al runner: {riga}"
            );
        }
    }

    /// Genera un output di build sintetico con N errori in ordine e il totale
    /// "Found N errors" in FONDO (come fa tsc/npm build).
    fn fake_build_output(n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!(
                "src/file{i}.ts({i},5): error TS2304: Cannot find name 'sym{i}'.\n"
            ));
        }
        s.push_str(&format!("Found {n} errors in {n} files.\n"));
        s
    }

    #[test]
    fn troncamento_preserva_coda_con_found_n_errors() {
        // Output lungo oltre il cap: la testa va persa, ma la coda con
        // "Found N errors" (cio' che il vecchio .take() buttava) deve restare.
        let output = fake_build_output(400);
        assert!(
            output.chars().count() > 16000,
            "il fixture deve superare il cap per esercitare il troncamento"
        );
        let truncated = smart_truncate_test_output(&output, 16000);
        assert!(
            truncated.len() < output.len(),
            "l'output deve essere effettivamente troncato"
        );
        assert!(
            truncated.contains("Found 400 errors"),
            "la coda con il totale degli errori deve sopravvivere al troncamento"
        );
        assert!(
            truncated.contains("caratteri omessi"),
            "il marker testa+coda deve segnalare l'omissione centrale"
        );
    }

    #[test]
    fn troncamento_no_op_sotto_cap() {
        let output = fake_build_output(3);
        let out = smart_truncate_test_output(&output, 16000);
        assert_eq!(out, output, "sotto il cap l'output resta integro");
    }

    /// Il lock dei package manager ESCLUDE davvero due esecuzioni sullo stesso
    /// progetto, e NON penalizza progetti diversi.
    ///
    /// Passa dalla stessa funzione della produzione (`pkg_manager_lock`), non da
    /// una sua imitazione: e' quella che `tool_run_command` usa per entrare in
    /// sezione critica. Il difetto che cattura e' quello misurato su verifica-wd
    /// (due `npm install` da sub-agenti diversi nello stesso secondo sulla stessa
    /// directory di dipendenze).
    #[tokio::test]
    async fn pkg_lock_serializza_lo_stesso_progetto_e_non_progetti_diversi() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let progetto = Uuid::new_v4();

        // Stesso progetto: due sezioni critiche non si sovrappongono MAI.
        let dentro = Arc::new(AtomicUsize::new(0));
        let max_contemporanei = Arc::new(AtomicUsize::new(0));
        let mut task = Vec::new();
        for _ in 0..8 {
            let d = dentro.clone();
            let m = max_contemporanei.clone();
            task.push(tokio::spawn(async move {
                let lock = pkg_manager_lock(progetto);
                let _g = lock.lock_owned().await;
                let ora = d.fetch_add(1, Ordering::SeqCst) + 1;
                m.fetch_max(ora, Ordering::SeqCst);
                // Cede il control al runtime: se il lock non escludesse, un altro
                // task entrerebbe qui e il massimo salirebbe sopra 1.
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                d.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for t in task {
            t.await.expect("task lock");
        }
        assert_eq!(
            max_contemporanei.load(Ordering::SeqCst),
            1,
            "due install sullo stesso progetto non devono mai sovrapporsi"
        );

        // Progetti diversi: lock distinti, nessuna attesa reciproca.
        let altro = Uuid::new_v4();
        let l1 = pkg_manager_lock(progetto);
        let l2 = pkg_manager_lock(altro);
        let _g1 = l1.lock_owned().await;
        assert!(
            l2.try_lock().is_ok(),
            "il lock di un progetto non deve bloccare un progetto diverso"
        );

        // Stesso progetto = stesso lock (non uno nuovo a ogni chiamata, che non
        // escluderebbe nulla).
        assert!(
            std::sync::Arc::ptr_eq(&pkg_manager_lock(progetto), &pkg_manager_lock(progetto)),
            "lo stesso progetto deve condividere UNA sola sezione critica"
        );
    }
}
