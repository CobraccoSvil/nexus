//! Tool comandi shell: run_command (con auto-routing long-running) e run_tests.

use super::*;
// Punto unico (regola L) di derivazione nome DB progetto e settings cluster app.
use crate::project_db_routes::load_app_db_setting;
use nexus_types::tool_outcome::RispostaTool;

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

/// Instrada il comando a `run_service` SOLO se chi lancia lo ha dichiarato
/// (`background: true`). Ritorna `Some(messaggio)` se instradato, `None` se il
/// comando va eseguito in-line.
///
/// Non prende piu' il testo del comando, e l'assenza di quel parametro E' il
/// contratto: qui non si guarda cosa c'e' scritto, si guarda cosa e' stato
/// dichiarato. Finche' lo prendeva, qualcuno poteva rimetterci un
/// riconoscimento sul nome senza cambiare la firma.
/// Traduce l'input di `run_command` in quello di `run_service`.
///
/// REGRESSIONE CHIUSA. Il ramo inoltrava l'input GREZZO, e quell'input contiene
/// `background` per costruzione — e' la condizione stessa che attiva
/// l'instradamento. Finche' `tool_run_service` leggeva i campi a mano il
/// sovrappiu' veniva ignorato; da quando legge il proprio contratto tipizzato,
/// `deny_unknown_fields` lo rifiuta e la lettura fallisce PRIMA di allocare la
/// porta e fare lo spawn. Effetto: `run_command` con `background: true` — l'unico
/// percorso di instradamento a servizio rimasto — non avviava piu' niente, e la
/// premessa anteposta continuava ad annunciare un avvio mai avvenuto.
///
/// Il rimedio non e' allargare `RunServiceInput`: `background` non e' un suo
/// parametro, e' la domanda che ha portato qui. Chi instrada verso un altro tool
/// compone l'input SECONDO IL CONTRATTO DI QUEL TOOL, e i tre campi si nominano
/// una volta sola — qui.
///
/// I due contratti dichiarano `working_dir` con lo stesso nome e `run_command`
/// non ha `label`, quindi la traduzione e' una proiezione: si copiano i campi
/// che esistono da entrambe le parti e si lascia fuori il resto.
fn input_per_run_service(input: &Value) -> Value {
    let mut fuori = serde_json::Map::new();
    for campo in ["command", "working_dir", "label"] {
        if let Some(v) = input.get(campo) {
            fuori.insert(campo.to_string(), v.clone());
        }
    }
    Value::Object(fuori)
}

async fn maybe_route_to_service(ctx: &AgentToolContext, input: &Value) -> Option<RispostaTool> {
    let explicit_bg = input
        .get("background")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // ── Livello 1: parametro background esplicito dall'AI ──
    if explicit_bg {
        let routed = service::tool_run_service(ctx, &input_per_run_service(input), "service").await;
        // `run_service` e' migrato: l'esito sta nel campo, quindi la premessa si
        // concatena al solo testo e non puo' piu' coprire nulla. Serviva
        // `prepend_preserving_failure` finche' il fallimento viveva in testa
        // alla stringa; ora non c'e' piu' niente da preservare, ed e' il
        // secondo call site di quel punto unico a sparire.
        return Some(RispostaTool {
            testo: format!(
                "[Background] Comando avviato come servizio server-side (background=true).\n{}",
                routed.testo
            ),
            ..routed
        });
    }

    // NESSUN livello 2. La natura di un comando non si indovina dal suo testo:
    // la DICHIARA chi lancia, scegliendo il tool (`run_service`) o il parametro
    // (`background`). Qui c'era un riconoscimento su vocabolario
    // (`is_long_oneshot` per escludere, `looks_like_long_running_command` +
    // `looks_like_web_service` per instradare) che ha promosso a servizio un
    // `curl`, un `npm run lint`, un `npx eslint` e sette `create-next-app` —
    // zero server su 12 promozioni misurate il 06/08/2026, mentre i server veri
    // passavano gia' da `run_service` esplicito 26 volte.
    //
    // Un comando che non termina non viene piu' promosso: fallisce dichiarando
    // COSA stava facendo (`timeout_con_diagnosi`, che lo osserva vivo). Se era
    // davvero un servizio, l'agente riceve la porta e il nome del tool giusto.
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
fn resolve_work_dir(ctx: &AgentToolContext, input: &Value) -> Result<PathBuf, RispostaTool> {
    if let Some(sub) = working_dir_param(input) {
        match resolve_relative_path(&ctx.root_path, sub) {
            Ok(p) => Ok(p),
            // Il percorso e' un parametro che l'agente controlla, e il
            // resolver dice gia' fin dove esiste.
            Err(e) => Err(RispostaTool::fallito_rimediabile(format!(
                "[Errore percorso working_dir: {}]",
                e.1["error"].as_str().unwrap_or("path error")
            ))),
        }
    } else {
        Ok(ctx.root_path.clone())
    }
}

/// Ricostruisce l'esito di un ramo che INOLTRA il risultato di un tool ancora
/// legacy (routing a run_service, comandi privilegiati, guardie di sicurezza):
/// il loro unico canale d'esito e' il marker nel testo, quindi qui il ponte e'
/// l'unica lettura onesta di cio' che hanno dichiarato. Sparisce quando quei
/// tool porteranno il proprio esito in un campo.
fn inoltro_legacy(testo: String) -> RispostaTool {
    RispostaTool::da_testo_legacy(testo)
}

/// Compone il testo del ramo di rifiuto anteponendo gli hint DB-driven, se ce
/// ne sono. Il testo e' testo (regola Q): l'esito lo dichiara il campo di
/// [`RispostaTool`], quindi comporre qui non puo' nascondere nulla — che e'
/// esattamente cio' che accadeva quando il fallimento viveva in testa alla
/// stringa.
fn con_hint(hints_prefix: &str, testo: String) -> String {
    let hint = hints_prefix.trim_end();
    if hint.is_empty() {
        testo
    } else {
        format!("{hint}\n{testo}")
    }
}

pub(super) async fn tool_run_command(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let command = match input.get("command").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return RispostaTool::fallito("[Errore: parametro 'command' mancante]"),
    };
    if command.trim().is_empty() {
        return RispostaTool::fallito("[Errore: comando vuoto]");
    }

    if let Some(msg) = command_security_gate(ctx, &command).await {
        return inoltro_legacy(msg);
    }

    // Hints DB-driven + guardie di routing pre-esecuzione (privilegiato,
    // migration-only, background/long-running). Err = ritorno anticipato;
    // Ok = prefisso hint da prependare al risultato finale.
    let hints_prefix = match command_hints_and_routing(ctx, input, &command).await {
        Ok(prefix) => prefix,
        Err(risposta) => return risposta,
    };

    // Blocca la duplicazione working_dir + path nel comando PRIMA dell'esecuzione
    // (causa radice di ambiente incoerente, vedi
    // helpers::detect_workdir_path_duplication): working_dir e' gia' la CWD, se il
    // comando ripete quel segmento ('cd frontend', 'frontend/...') i path si
    // sommano e rm/install/build operano sulla dir sbagliata. Rifiutare qui evita
    // il danno silenzioso; il messaggio dice all'agente come correggere.
    // Il rifiuto E' un fallimento del tool e lo dichiara nel CAMPO (regola Q):
    // il comando NON e' stato eseguito, quindi non esiste alcun exit code, e un
    // consumatore che vedesse solo l'assenza di exit code non potrebbe
    // distinguere "invocazione rifiutata" da "eseguito senza stato d'uscita".
    // Il final_gate fa esattamente quella domanda per decidere se un criterio e'
    // fallito o non misurabile: senza dichiarazione, un criterio la cui
    // invocazione e' stata rifiutata verrebbe ASSOLTO invece che rieseguito
    // corretto.
    if let Some(wd) = working_dir_param(input) {
        if let Some(msg) = super::helpers::detect_workdir_path_duplication(wd, &command) {
            return RispostaTool::fallito(con_hint(&hints_prefix, msg));
        }
    }

    // ── Livello 3: probe timeout — esegui, se non finisce in 10s ri-lancia nel terminale ──
    let work_dir = match resolve_work_dir(ctx, input) {
        Ok(p) => p,
        // Percorso invalido: il comando non parte, quindi nessun exit code.
        // `resolve_work_dir` dichiara gia' esito e natura (rimediabile: e' un
        // parametro della chiamata), quindi si inoltra senza ricomporre nulla.
        Err(risposta) => return risposta,
    };

    // Sezione critica dei package manager: due install concorrenti sulla stessa
    // directory di dipendenze si corrompono a vicenda. Il guard vive fino al
    // termine della funzione, quindi copre spawn + attesa.
    let _pkg_guard = match acquire_pkg_manager_slot(ctx.project_id, &command).await {
        Ok(g) => g,
        // Attesa scaduta: il comando NON e' stato eseguito. E' lo stesso caso
        // dell'invocazione rifiutata — fallimento dichiarato, nessun exit code
        // — e prima non dichiarava nulla, cioe' passava per un comando la cui
        // esecuzione non era misurabile.
        Err(msg) => return RispostaTool::fallito(con_hint(&hints_prefix, msg)),
    };

    let child = match spawn_command_child(ctx, &command, &work_dir).await {
        Ok(c) => c,
        // Il processo non e' nemmeno partito: nessuno stato d'uscita esiste.
        Err(msg) => return RispostaTool::fallito(msg),
    };

    run_command_probe(ctx, &command, child, &hints_prefix).await
}

/// Calcola il prefisso hint DB-driven e applica le guardie di routing
/// pre-esecuzione (comandi privilegiati → Sudo Manager, policy migration-only,
/// background/long-running → run_service). Ritorna `Err(risposta)` se il
/// comando va instradato/bloccato (ritorno anticipato del chiamante), `Ok(prefix)`
/// col prefisso hint se l'esecuzione in-line puo' proseguire.
///
/// L'`Err` porta gia' la [`RispostaTool`] e non il solo testo: ogni ramo sa se
/// sta INOLTRANDO l'esito di un tool legacy (che va letto dal suo marker) o se
/// sta RIFIUTANDO l'esecuzione in proprio (fallimento senza exit code). Con una
/// stringa sola le due cose erano indistinguibili a valle.
async fn command_hints_and_routing(
    ctx: &AgentToolContext,
    input: &Value,
    command: &str,
) -> Result<String, RispostaTool> {
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
        // Nessun ponte: l'esecutore della suite e' migrato e la risposta arriva
        // gia' coi campi valorizzati.
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
        // L'esito si legge PRIMA di comporre: `privileged` e' ancora legacy e
        // dichiara il proprio fallimento col marker in testa, che una premessa
        // anteposta annullerebbe. Composta la premessa nel campo `testo`,
        // l'esito e' un dato e non si puo' piu' perdere.
        let r = inoltro_legacy(routed);
        return Err(RispostaTool {
            testo: con_hint(&hints_prefix, r.testo),
            ..r
        });
    }

    // Policy migration-only: l'esecuzione e' NEGATA qui, non altrove. E' un
    // rifiuto del tool — fallimento dichiarato, nessun exit code — e non un
    // comando andato bene senza stato d'uscita, che e' come il gate lo leggeva.
    if let Some(msg) = migration_only_block(ctx, command).await {
        return Err(RispostaTool::fallito(msg));
    }

    // ── Livelli 1-2: routing a run_service (background esplicito o comando noto
    // long-running/web-service). Se instradato ritorna il messaggio; None = prosegue. ──
    // Nessun ponte legacy: `run_service` e' migrato e la sua risposta arriva
    // gia' coi campi valorizzati. Ricostruirne l'esito dal testo sarebbe ora
    // una perdita — la natura del fallimento non e' deducibile dal marker.
    if let Some(risposta) = maybe_route_to_service(ctx, input).await {
        return Err(risposta);
    }

    Ok(hints_prefix)
}

/// Esegue il child del comando one-shot con la logica di probe timeout: attende
/// fino a `probe_secs` (lungo per gli one-shot install/build, 10s altrimenti);
/// se termina compone l'output finale, se scade re-instrada a run_service o
/// segnala il timeout. Drena stdout/stderr in parallelo a `wait()` per evitare
/// il deadlock della pipe (~64KB). Estratto da `tool_run_command`
/// (behavior-preserving).
/// La natura del figlio, o l'ignoto dichiarato se il pid non c'e' piu'.
///
/// Estratta perche' i due punti che la interrogano (dopo l'attesa breve e alla
/// scadenza del tetto lungo) devono porre la STESSA domanda: due formulazioni
/// diverse dello stesso quesito sono il modo in cui un instradamento diventa
/// incoerente con se stesso.
async fn natura_comando_del_figlio(pid: Option<u32>) -> natura_comando::NaturaOsservata {
    match pid {
        Some(pid) => natura_comando::natura_osservata(pid).await,
        None => natura_comando::NaturaOsservata::NonOsservabile {
            motivo: "pid del processo non disponibile".to_string(),
        },
    }
}

async fn run_command_probe(
    ctx: &AgentToolContext,
    command: &str,
    mut child: tokio::process::Child,
    hints_prefix: &str,
) -> RispostaTool {
    // Drain stdout/stderr IN PARALLELO con child.wait() (evita deadlock pipe ~64KB).
    let (stdout_task, stderr_task) = spawn_output_drainers(&mut child);

    // Il pid serve per CHIEDERE al sistema operativo cosa sta facendo il
    // processo. Assente solo se il child e' gia' stato atteso, il che qui non
    // e' ancora accaduto.
    let pid_figlio = child.id();

    // CONTRATTO RIGIDO: `run_command` esegue un comando che TERMINA. Non
    // promuove nulla a servizio — quella decisione la dichiara chi lancia,
    // scegliendo `run_service`, e non si indovina qui.
    //
    // MISURATO il 06/08/2026 su tre progetti: l'auto-promozione e' intervenuta
    // 12 volte e in NESSUN caso su un server. Aveva promosso a servizio un
    // `curl`, un `npm run lint`, un `npx eslint`, uno script di generazione
    // Prisma, e sette volte `npx create-next-app` — queste ultime tutte
    // fallite, perche' promuovere significa UCCIDERE e RILANCIARE: lo
    // scaffolder ripartiva su una directory che la prima esecuzione aveva gia'
    // riempito a meta', e non poteva che rifiutarsi. Nello stesso periodo i
    // server veri sono stati lanciati con `run_service` esplicito 26 volte.
    // Precisione dell'euristica: zero. Danno: un run morto in 600 secondi.
    let atteso = tokio::time::timeout(
        std::time::Duration::from_secs(LONG_ONESHOT_PROBE_SECS),
        child.wait(),
    )
    .await;

    match atteso {
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
            // L'attesa del processo e' fallita: nessuno stato d'uscita e'
            // osservabile, e il tool lo dichiara invece di tacere.
            RispostaTool::fallito(format!("[Errore attesa comando '{}': {}]", command, e))
        }
        Err(_) => {
            // Si GUARDA prima di uccidere: dopo, la tabella dei listener non
            // dice piu' nulla di questo processo, e la diagnosi che l'agente
            // riceve sarebbe un «timeout» muto.
            let natura = natura_comando_del_figlio(pid_figlio).await;
            let _ = child.kill().await;
            // I due drainer vanno ABORTITI, non lasciati cadere: in tokio
            // droppare un `JoinHandle` STACCA il task, non lo annulla. Restavano
            // in `read_to_end` senza tetto su una pipe il cui write end e'
            // ancora aperto dai NIPOTI del processo ucciso (su Windows non c'e'
            // un job object che li porti via con il padre), quindi non
            // terminavano mai e trattenevano il buffer gia' letto piu' la catena
            // di processi orfani. Un `.abort()` chiude il read end e libera
            // entrambi.
            stdout_task.abort();
            stderr_task.abort();
            timeout_con_diagnosi(command, pid_figlio, natura).await
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

/// Compone l'esito finale di `run_command` per un comando terminato entro il
/// probe. L'unico I/O e' il cap di troncamento (DB-driven, regola G): la
/// composizione vera e' pura e vive in [`componi_esito_comando`], cosi' e'
/// interrogabile senza montare un DB.
async fn format_command_completed(
    ctx: &AgentToolContext,
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    hints_prefix: &str,
) -> RispostaTool {
    let max_chars = load_run_command_max_chars(&ctx.db).await;
    componi_esito_comando(command, exit_code, stdout, stderr, hints_prefix, max_chars)
}

/// L'esito di un comando ESEGUITO: lo stato d'uscita nel CAMPO, il resto nel
/// testo.
///
/// Il testo RIPETE `EXIT CODE: N` perche' e' informazione utile al modello che
/// legge il tool_result, non perche' qualcuno debba ri-estrarla: chi decide
/// legge `RispostaTool::exit_code`. La differenza non e' estetica — il prefisso
/// hint viene da `nexus_command_hints`, testo scritto dall'admin, e un hint che
/// citi "EXIT CODE: 0" precede il valore vero nella stringa: qualunque lettura
/// posizionale del testo (il `find` del ponte legacy) prenderebbe il primo, cioe'
/// quello dell'hint.
///
/// L'esito resta `Riuscito` anche con `exit_code != 0`: il tool ha fatto il suo
/// lavoro, il comando no, e sono due assi distinti (vedi `RispostaTool::comando`).
fn componi_esito_comando(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    hints_prefix: &str,
    max_chars: usize,
) -> RispostaTool {
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
    // mai il collo di bottiglia che decapita la coda prima del brain. Tagliare
    // il testo non puo' toccare l'esito, che vive in un campo a parte.
    let testo = if combined.chars().count() > max_chars {
        smart_truncate_test_output(&combined, max_chars)
    } else {
        combined
    };
    RispostaTool::comando(testo, exit_code)
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

/// Il timeout di `run_command`, con la DIAGNOSI di cio' che il processo stava
/// facendo.
///
/// L'esito e' sempre un FALLIMENTO: `run_command` esegue comandi che
/// terminano, e uno che non termina non ha fatto il suo lavoro. Prima questo
/// ramo promuoveva a servizio cio' che sembrava long-running, e la misura del
/// 06/08/2026 dice che in 12 promozioni non ne ha azzeccata una (vedi il
/// commento nel probe).
///
/// Cio' che cambia e' il MOTIVO, e viene da un fatto osservato mentre il
/// processo era ancora vivo, non dal suo nome:
///
/// - ascolta su una porta -> e' un servizio lanciato col tool sbagliato, e la
///   risposta lo dice con la porta in mano: l'agente ha un rimedio preciso
///   invece di un timeout da interpretare;
/// - non ascolta -> stava lavorando e non e' arrivato in fondo: il rimedio e'
///   spezzarlo, non cambiare tool;
/// - non osservabile -> si dichiara di non sapere, e si danno entrambe le
///   strade. «Non ho potuto guardare» non diventa una diagnosi inventata.
async fn timeout_con_diagnosi(
    command: &str,
    pid: Option<u32>,
    natura: natura_comando::NaturaOsservata,
) -> RispostaTool {
    let breve: String = command.chars().take(120).collect();
    let rimedio = match &natura {
        natura_comando::NaturaOsservata::Serve { porte } => format!(
            "Il processo era in ascolto su {}: e' un SERVIZIO, non un comando che termina.              Rilancialo con run_service (riceve una porta allocata e resta vivo).",
            porte
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        natura_comando::NaturaOsservata::NonServe => {
            "Il processo non era in ascolto su alcuna porta: stava lavorando e non e' arrivato              in fondo. Eseguilo per passi piu' piccoli."
                .to_string()
        }
        natura_comando::NaturaOsservata::NonOsservabile { motivo } => format!(
            "Non e' stato possibile osservare se fosse un servizio ({motivo}). Se e' un server              usa run_service; se e' un build/install lungo, eseguilo per passi."
        ),
    };
    tracing::debug!(
        %breve, pid = ?pid, esito = %natura.descrizione(),
        "run_command: tempo scaduto, nessuna promozione a servizio"
    );
    RispostaTool::fallito(format!(
        "[Timeout] Il comando '{breve}' non e' terminato in {LONG_ONESHOT_PROBE_SECS}s ed e'          stato interrotto. {rimedio}"
    ))
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
pub(crate) async fn tool_run_tests(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    // 1. Determina comando test
    let command = resolve_test_command(ctx, input);

    if command.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "[Errore: impossibile rilevare il comando test per questo progetto. \
             Specifica il parametro 'command' (es. 'npm test', 'cargo test', 'pytest').]",
        );
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
        Err(risposta) => return risposta,
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
async fn run_tests_execution(command: &str, work_dir: &Path, timeout: u64) -> RispostaTool {
    let child = crate::sandbox::isolated_command(&crate::sandbox::agent_shell())
        .arg("-c")
        .arg(super::helpers::shell_line(command))
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        // L'avvio dipende dalla shell isolata e dall'ambiente, non dalla riga
        // che l'agente ha scritto.
        Err(e) => {
            return RispostaTool::fallito_di_sistema(format!(
                "[Errore avvio test '{command}': {e}]"
            ))
        }
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
            // L'exit code nel CAMPO: `format_run_tests_output` lo scrive anche
            // nel testo come "(exit code: N)", che il ponte legacy — il quale
            // cerca "EXIT CODE: " maiuscolo — non ha mai riconosciuto. Il tool
            // resta RIUSCITO: ha eseguito e ha riportato, e i test rossi sono
            // l'esito del comando.
            RispostaTool::comando(
                format_run_tests_output(command, exit_code, &stdout, &stderr),
                exit_code,
            )
        }
        Ok(Err(e)) => {
            RispostaTool::fallito_di_sistema(format!("[Errore attesa test '{command}': {e}]"))
        }
        Err(_) => {
            let _ = child.kill().await;
            // Usciva NUDO: una suite uccisa dal timeout arrivava all'agente come
            // un'esecuzione riuscita di cui mancava solo l'esito. TRANSITORIO, e
            // il testo nomina gia' il parametro che offre l'altra strada.
            RispostaTool::fallito_transitorio(format!(
                "=== RUN TEST ===\nComando: {command}\n\
                 [TIMEOUT] I test non sono terminati entro {timeout}s.\n\
                 Suggerimento: usa il parametro 'filter' per eseguire un sottoinsieme di test specifici.\n\
                 === FINE RUN TEST ==="
            ))
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
            out.testo.contains(MARCA_ESECUTORE),
            "la suite deve passare dall'esecutore unico, output: {}",
            out.testo
        );
        assert!(
            !out.testo.contains("EXIT CODE:"),
            "run_command non deve aver eseguito la riga in proprio: {}",
            out.testo
        );
        assert_eq!(
            out.exit_code, None,
            "nessun comando eseguito qui -> nessuno stato d'uscita da riportare"
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
            out.testo.contains(MARCA_ESECUTORE),
            "la suite deve passare dall'esecutore unico, output: {out:?}"
        );
        assert!(
            !out.testo.contains("=== RUN TEST ==="),
            "run_tests non deve aver eseguito la riga in proprio: {out:?}"
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
        assert!(
            out.esito.e_fallito(),
            "deve essere un errore dichiarato nel campo: {}",
            out.testo
        );
        assert!(
            out.testo.contains("run_playwright_tests"),
            "il rifiuto deve indirizzare all'esecutore unico: {}",
            out.testo
        );
        assert!(
            !out.testo.contains("EXIT CODE:") && !out.testo.contains(MARCA_ESECUTORE),
            "ne' eseguita qui ne' delegata a meta': {}",
            out.testo
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

    /// Il probe che scade su un one-shot DICHIARA il fallimento e NON inventa
    /// uno stato d'uscita.
    ///
    /// E' il caso che assolveva un build mai terminato: il ramo non scriveva
    /// ne' `EXIT CODE:` ne' alcuna dichiarazione, quindi a valle
    /// `exit_code = None` e `is_error = false`, e il final_gate calcola
    /// `esito_misurato = exit_code.is_some() || is_error` — falso, cioe'
    /// "criterio NON misurato", che nel gate non boccia nulla.
    ///
    /// Si passa dal PRODUTTORE del ramo (`timeout_con_diagnosi`, quello che
    /// `run_command_probe` chiama allo scadere) e dall'adapter REALE che
    /// alimenta il gate (`map_result_to_outcome`): un timeout vero costerebbe
    /// `LONG_ONESHOT_PROBE_SECS` secondi di attesa, e ricostruire a mano la
    /// risposta fisserebbe proprio l'assunto da verificare.
    ///
    /// MUTAZIONE: si riporta il ramo a `RispostaTool::riuscito(...)` (cioe' a non
    /// dichiarare nulla, com'era) -> il test rosseggia su `esito_misurato` falso.
    #[tokio::test]
    async fn timeout_del_probe_e_un_fallimento_misurato_senza_exit_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test(dir.path().to_path_buf());
        let _ = &ctx;
        let risposta = timeout_con_diagnosi(
            "npm install",
            Some(4242),
            natura_comando::NaturaOsservata::NonServe,
        )
        .await;

        assert_eq!(
            risposta.exit_code, None,
            "il processo e' stato ucciso: uno stato d'uscita non e' mai esistito"
        );
        let outcome =
            crate::agent_graph_adapter::tool_executor::map_result_to_outcome("c", risposta);
        // La coppia esatta che `criteria_runner::check_run_command` legge.
        let esito_misurato = outcome.exit_code.is_some() || outcome.is_error;
        assert!(
            esito_misurato,
            "un build interrotto dal probe deve risultare MISURATO (e fallito), \
             non 'non misurato': content={:?}",
            outcome.content
        );
        assert!(
            outcome
                .content
                .as_str()
                .is_some_and(|t| t.contains("[Timeout]")),
            "il testo resta la guida per l'agente: {:?}",
            outcome.content
        );
    }

    /// Un SERVIZIO lanciato con `run_command` non viene piu' promosso di
    /// nascosto: fallisce, e la risposta dice cosa fare NOMINANDO la porta su
    /// cui il processo era in ascolto.
    ///
    /// E' la differenza fra il vecchio comportamento e questo. Prima il tool
    /// promuoveva a servizio tutto cio' che sembrava long-running e nel farlo
    /// UCCIDEVA e RILANCIAVA il processo: misurate 12 promozioni, zero server
    /// (un `curl`, un `npm run lint`, sette `create-next-app` distrutti a
    /// meta' scaffolding). Ora la decisione resta a chi lancia, e cio' che il
    /// tool aggiunge e' un FATTO osservato, non un'ipotesi sul nome.
    ///
    /// MUTAZIONE: far tornare `timeout_con_diagnosi` allo stesso testo per
    /// tutte le nature -> il test rosseggia, perche' la porta sparisce dal
    /// rimedio e l'agente resta senza il dato che gli serve.
    #[tokio::test]
    async fn un_servizio_lanciato_come_comando_riceve_la_porta_nel_rimedio() {
        let serve = timeout_con_diagnosi(
            "next dev",
            Some(1234),
            natura_comando::NaturaOsservata::Serve { porte: vec![31904] },
        )
        .await;
        let testo = serve.testo.clone();
        assert_eq!(
            serve.esito,
            nexus_types::tool_outcome::EsitoTool::Fallito,
            "un comando che non termina e' un fallimento"
        );
        assert!(testo.contains("31904"), "la porta osservata va nominata: {testo}");
        assert!(testo.contains("run_service"), "il rimedio va nominato: {testo}");

        // Il caso opposto NON deve suggerire run_service: stava lavorando.
        let lavora =
            timeout_con_diagnosi("npm install", Some(1234), natura_comando::NaturaOsservata::NonServe)
                .await;
        assert!(
            !lavora.testo.contains("run_service"),
            "un build lento non e' un servizio: {}",
            lavora.testo
        );

        // E cio' che non si e' potuto osservare non diventa una diagnosi.
        let ignoto = timeout_con_diagnosi(
            "qualcosa",
            None,
            natura_comando::NaturaOsservata::NonOsservabile { motivo: "test".into() },
        )
        .await;
        assert!(ignoto.testo.contains("Non e' stato possibile osservare"));
    }

    /// Lo stato d'uscita sta nel CAMPO, e il testo non puo' contraddirlo.
    ///
    /// Il prefisso hint viene da `nexus_command_hints` (testo scritto
    /// dall'admin) e PRECEDE l'output del comando: una lettura posizionale del
    /// testo — `find("EXIT CODE: ")`, cioe' il ponte legacy — prende quella
    /// dell'hint. Il test lo dimostra chiedendo la stessa cosa nei due modi e
    /// mostrando che danno risposte diverse.
    ///
    /// Si passa dal compositore REALE (`componi_esito_comando`, quello che
    /// `format_command_completed` chiama dopo aver letto il cap dal DB).
    ///
    /// MUTAZIONE: si costruisce la risposta con `RispostaTool::da_testo_legacy`
    /// invece che con `comando(testo, exit_code)` -> il test rosseggia con
    /// `Some(0)` al posto di `Some(1)`, cioe' col valore del difetto reale.
    #[test]
    fn l_exit_code_viene_dal_campo_non_dal_testo_dell_hint() {
        let hint = "[HINT — ERROR] (pattern: `tsc`)\nSe vedi EXIT CODE: 0 ma il build \
                    fallisce, controlla il bundler.\n\n---\n";
        let risposta = componi_esito_comando("pnpm build", 1, "", "error TS2322", hint, 16000);

        assert_eq!(
            risposta.exit_code,
            Some(1),
            "l'exit code e' quello del processo, non quello citato dall'hint"
        );
        assert_eq!(
            nexus_types::tool_outcome::RispostaTool::da_testo_legacy(risposta.testo.clone())
                .exit_code,
            Some(0),
            "premessa del test: leggendo il TESTO si ottiene il numero dell'hint, \
             ed e' la ragione per cui l'esito non puo' viaggiare li'"
        );
        // Il comando e' fallito, il tool no: due assi distinti.
        assert!(!risposta.esito.e_fallito());
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

    /// LA REGRESSIONE, e il test attraversa i DUE contratti veri invece di
    /// fabbricare un input a mano (regola O): quello di `run_command`, che
    /// dichiara `background`, e quello di `run_service`, che non lo dichiara.
    ///
    /// `maybe_route_to_service` inoltrava l'input grezzo, e quell'input contiene
    /// `background` per costruzione — e' la condizione che attiva
    /// l'instradamento. Da quando `tool_run_service` legge il proprio contratto
    /// tipizzato, `deny_unknown_fields` lo rifiuta: `run_command` con
    /// `background: true` non avviava piu' alcun servizio, e la premessa
    /// anteposta annunciava comunque l'avvio.
    ///
    /// COSA COPRE, E COSA NO. Copre la proprieta' su cui il difetto si regge —
    /// i due contratti sono incompatibili, e la traduzione li riconcilia — ma
    /// NON copre la giunzione: chiama `input_per_run_service` direttamente,
    /// quindi resta verde anche rimettendo l'inoltro grezzo nel chiamante.
    /// MISURATO, non supposto: eseguita quella mutazione, questo test passa.
    ///
    /// Il buco e' quello che la regola O descrive: la giunzione vive dentro
    /// `maybe_route_to_service`, e arrivarci significa attraversare
    /// `tool_run_service`, che alloca porte e interroga il DB del progetto. Un
    /// test che lo faccia davvero e' un test di integrazione con un contesto
    /// vero, non un caso in piu' qui.
    ///
    /// Resta perche' e' il documento del difetto: chi rimettesse l'inoltro
    /// grezzo trova scritto, accanto al codice, perche' non funziona.
    #[test]
    fn l_instradamento_a_servizio_compone_l_input_del_contratto_di_destinazione() {
        use nexus_agent_tools::input_contract::InputTool;
        use nexus_agent_tools::tool_inputs::{RunCommandInput, RunServiceInput};

        // L'input come lo produce il modello seguendo il catalogo di run_command,
        // dove `background` E' dichiarato.
        let grezzo = serde_json::json!({
            "command": "npm run dev",
            "working_dir": "frontend",
            "background": true,
        });
        RunCommandInput::leggi(&grezzo).expect("e' un input valido per run_command");

        // Inoltrato tale e quale, il contratto di run_service lo rifiuta: e'
        // esattamente cio' che accadeva in esercizio.
        RunServiceInput::leggi(&grezzo)
            .expect_err("run_service non dichiara 'background': inoltrarlo grezzo fallisce");

        // Tradotto, passa — e conserva i campi che i due contratti condividono.
        let tradotto = input_per_run_service(&grezzo);
        let params = RunServiceInput::leggi(&tradotto)
            .expect("l'input tradotto rispetta il contratto di run_service");
        assert_eq!(params.command, "npm run dev");
        assert_eq!(params.working_dir.as_deref(), Some("frontend"));
        assert!(
            tradotto.get("background").is_none(),
            "background non e' un parametro di run_service: e' la domanda che ha portato qui"
        );
    }
}
