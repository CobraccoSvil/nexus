//! Tool servizio: avvio processi long-running, lettura output, stop, build immagine progetto.

use super::*;
use nexus_types::tool_outcome::RispostaTool;
use std::collections::HashMap;

/// Il comando avvia un server che ha bisogno di una porta TCP?
///
/// Facciata: la risposta la da' il punto unico [`super::avvio_server`], che
/// scompone la riga come farebbe la shell e chiede «l'ESEGUIBILE di uno di
/// questi comandi avvia un server?».
///
/// Il criterio viveva qui ed era `command.to_lowercase().contains(token)` su un
/// vocabolario di sottostringhe. Ha sbagliato nei due versi, e le due volte era
/// lo stesso difetto di FORMA:
/// - token CONTIGUI ciechi al percorso: `"node server"` non compariva in
///   `node src/backend/server.js`, quindi il servizio non riceveva `PORT` e
///   ripiegava sulla porta scritta nel codice (catalogo-libri, 03/08/2026);
/// - token NUDI ciechi al contesto: `"vite"` compariva in `VITE_API_URL`,
///   quindi `grep -r "VITE_API_URL" frontend/` risultava un servizio web,
///   riceveva dalla working directory la label `frontend`, e il gate pre-avvio
///   fermava il frontend vero — vivo da 3h45m — per «deduplicarlo».
///
/// Il primo fu corretto per i soli runtime (`node`, `tsx`, ...); il secondo
/// mostra che la mezza correzione non bastava. Il criterio e' ora uno solo, e
/// vale per l'intera riga.
///
/// In caso di dubbio (es. `make foo`) NON inietta `PORT`: l'agente puo' chiamare
/// `request_port` esplicitamente e includere la porta nel comando.
pub(crate) fn looks_like_web_service(command: &str) -> bool {
    super::avvio_server::riga_avvia_server(command)
}

/// Porta candidata trovata nell'output di un servizio, col verdetto sul suo
/// rapporto con QUESTO progetto. Il verdetto viaggia insieme al numero (regola
/// M): chi decide che farne non lo ri-deriva, e non puo' derivarlo diverso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PortaRilevata {
    port: u16,
    esito: nexus_tool_kit::ports::PortRegistrability,
}

/// Cerca nella combinazione stdout+stderr un pattern di porta TCP in ascolto.
/// Riconosce output di Next.js, Vite, Express, Flask, Django, ecc.
///
/// Ritorna la prima porta che sia plausibilmente il LISTENER del servizio, con
/// la sua classificazione rispetto al progetto (punto unico
/// `nexus_tool_kit::ports::classify_project_port`, regola L).
///
/// Cosa si scarta e cosa no, e perche' la differenza conta:
/// - riservate Nexus e fuori dal range progetti: nei log sono destinazioni di
///   CONNESSIONE (es. Postgres :5434), non l'indirizzo su cui il servizio
///   ascolta. Non dicono niente su questo servizio: si scartano qui;
/// - nel range progetti ma nel bucket di un ALTRO progetto: quella E' la porta
///   del servizio, ed e' una violazione di isolamento. Scartarla in silenzio
///   nasconderebbe il fatto invece di riportarlo, quindi risale al chiamante
///   col proprio verdetto.
fn detect_port_from_output(
    project_id: &Uuid,
    stdout: &str,
    stderr: &str,
) -> Option<PortaRilevata> {
    let combined = format!("{}\n{}", stdout, stderr);
    // Pattern frequenti: "localhost:3002", "0.0.0.0:3000", "port 5173",
    // "Local: http://localhost:3002", "listening on :8080"
    let re = regex::Regex::new(
        r"(?i)(?:localhost|127\.0\.0\.1|0\.0\.0\.0|::)[:\s]+(\d{4,5})|(?:port|porta)\s+(\d{4,5})|Local:\s+https?://[^:]+:(\d{4,5})"
    ).ok()?;
    for cap in re.captures_iter(&combined) {
        let Some(port_str) = cap.get(1).or(cap.get(2)).or(cap.get(3)) else {
            continue;
        };
        if let Ok(p) = port_str.as_str().parse::<u16>() {
            use nexus_tool_kit::ports::PortRegistrability as Esito;
            let esito = nexus_tool_kit::ports::classify_project_port(project_id, p);
            match esito {
                // Rumore: non e' il listener del servizio.
                Esito::Reserved | Esito::OutOfProjectRange => continue,
                // E' il listener: registrabile o in violazione, lo si riporta.
                Esito::Registrable | Esito::OutOfProjectBucket { .. } => {
                    return Some(PortaRilevata { port: p, esito })
                }
            }
        }
    }
    None
}

/// Decisione su cosa fare quando, all'avvio, esiste gia' una risorsa dello
/// stesso scopo (frontend/backend) nel progetto. Punto unico testabile (regola
/// L): la scelta dipende SOLO dal fatto che la risorsa sia in ascolto o meno.
#[derive(Debug, PartialEq, Eq)]
enum ExistingServiceAction {
    /// Servizio gia' ATTIVO (porta in LISTEN): rifiuta, l'agente deve riusarlo.
    RefuseActive,
    /// Allocazione SPENTA (porta allocata in DB ma nessuno in ascolto): NON e'
    /// un duplicato da rifiutare ma una allocazione stantia da ADOTTARE. Si
    /// prosegue l'avvio riusando porta+label (no deadlock quando il servizio non
    /// e' una systemd unit e `service_restart` non puo' riavviarlo).
    AdoptStale,
}

fn existing_service_action(listening: bool) -> ExistingServiceAction {
    if listening {
        ExistingServiceAction::RefuseActive
    } else {
        ExistingServiceAction::AdoptStale
    }
}

/// Nome del tool con cui l'agente ha invocato [`tool_run_service`]: i due
/// bracci del dispatcher condividono l'implementazione ma non il nome, e un
/// messaggio che nomina l'altro manda a cercare la riga sbagliata.
fn nome_del_tool(kind: &str) -> &'static str {
    if kind == "task" {
        "run_in_terminal"
    } else {
        "run_service"
    }
}

/// Suite Playwright arrivata a questo tool: la consegna all'esecutore unico
/// (punto unico `playwright_cli`, regola L). Terzo percorso della guardia, e
/// non un caso di scuola: `helpers::is_long_oneshot` nomina gia'
/// `playwright test` proprio perche' di qui passava.
///
/// La posizione nel chiamante e' fra due vincoli. DOPO la validazione del
/// comando, perche' il gate sui placeholder di redazione vale per qualunque
/// riga a prescindere da chi la esegue, e delegare prima lo scavalcherebbe.
/// PRIMA di tutto cio' che segue, perche' ha effetti: `dedup_and_cleanup_ports`
/// ferma processi e libera porte, e farlo per una suite che da qui non doveva
/// partire costerebbe un servizio vivo.
///
/// Nessuna conversione: `intercetta_suite` e' migrato insieme al suo esecutore
/// (`testing.rs`), quindi la risposta arriva gia' coi campi valorizzati e il
/// ponte legacy che stava qui e' sparito.
async fn suite_playwright_altrove(
    ctx: &AgentToolContext,
    tool: &str,
    command: &str,
    input: &Value,
) -> Option<RispostaTool> {
    super::playwright_cli::intercetta_suite(
        ctx,
        tool,
        command,
        input.get("working_dir").and_then(Value::as_str),
    )
    .await
}

/// Avvia un servizio/processo long-running direttamente sul server.
/// L'output viene catturato nel DB e mostrato nel pannello Output dell'IDE.
pub(super) async fn tool_run_service(
    ctx: &AgentToolContext,
    input: &Value,
    kind: &str,
) -> RispostaTool {
    // Nome del tool come l'ha invocato l'agente, preso PRIMA che `kind` venga
    // ridefinito dal declassamento one-shot: un messaggio che nomina un tool
    // diverso da quello chiamato manda a cercare la riga sbagliata.
    let tool_invocante = nome_del_tool(kind);

    // Fase 1: validazione + label/work_dir (senza refuse: la dedup deve girare prima).
    let launch = match resolve_service_launch(ctx, input, tool_invocante, kind).await {
        Ok(l) => l,
        Err(risposta) => return risposta,
    };
    let ServiceLaunch {
        command,
        kind,
        label,
        work_dir,
    } = launch;

    if let Some(out) = suite_playwright_altrove(ctx, tool_invocante, &command, input).await {
        return out;
    }

    if let Some(rifiuto) = gate_pre_avvio(ctx, &kind, &label, &command, &work_dir).await {
        return rifiuto;
    }

    // ── Strato 1 hardening: auto-inject PORT per servizi web ────────────────
    // Se il comando avvia un server (next dev, vite, gunicorn, ecc.) Nexus
    // alloca automaticamente una porta nel bucket del progetto e la inietta
    // come PORT env. Cosi' il servizio non bindera' sulla porta hardcoded
    // (es. next dev → 3000 → conflitto con web-ide Nexus).
    let env_overrides = match allocate_web_port_env(ctx, &command, &label).await {
        Ok(env) => env,
        Err(risposta) => return risposta,
    };
    // Porta su cui il servizio DEVE mettersi in ascolto. E' il segnale che
    // distingue "il processo esiste" da "il servizio serve"; la sua ASSENZA non
    // e' un difetto (un worker non ascolta), ed e' per questo che
    // `EsitoPorta::NessunaAttesa` e' una variante e non un `None`.
    let porta_attesa = env_overrides
        .as_ref()
        .and_then(|e| e.get("PORT"))
        .and_then(|p| p.trim().parse::<u16>().ok());

    let process_id =
        match spawn_service_process(ctx, &label, &command, &work_dir, env_overrides, &kind).await {
            Ok(process_id) => process_id,
            Err(risposta) => return risposta,
        };

    // Si attende la VITA, non la nascita: il processo esiste sempre (su Windows
    // la shell nasce anche quando il comando dentro muore un istante dopo), e
    // fino al 17/08/2026 quello era l'unico fatto che il tool guardava. Le due
    // domande — «la porta risponde?» e «il capostipite e' uscito?» — hanno
    // ciascuna un punto unico, e `attendi_avvio` le pone entrambe delegando
    // (regola L). L'esito e' tipizzato perche' i tre casi hanno rimedi diversi.
    let esito = super::avvio_servizio::attendi_avvio(
        &ctx.db,
        ctx.project_id,
        process_id,
        porta_attesa,
        super::avvio_servizio::finestra_readiness(&ctx.db).await,
        super::avvio_servizio::finestra_morte_precoce(&ctx.db).await,
    )
    .await;
    report_started_service(ctx, &label, &kind, process_id, esito).await
}

/// Gate pre-avvio di `tool_run_service`: dedup+cleanup porte, poi refuse per
/// scopo gia' attivo, poi quota container. `Some(messaggio)` = rifiuto,
/// ritorno anticipato del chiamante; `None` = via libera all'avvio.
///
/// L'ordine e' il fix strutturale (regola L): dedup PRIMA del refuse, cosi' un
/// riavvio non resta bloccato da un PID/child ancora in ascolto mentre il
/// check refuse girava prima della dedup (incidente Vite 31754).
async fn gate_pre_avvio(
    ctx: &AgentToolContext,
    kind: &str,
    label: &str,
    command: &str,
    work_dir: &Path,
) -> Option<RispostaTool> {
    // La dedup FERMA i servizi vivi con label simile, e lo fa PRIMA che questo
    // processo esista. E' il potere piu' distruttivo del lancio, e spetta solo a
    // chi un servizio lo e': sostituire un servizio con la sua nuova istanza ha
    // senso, un comando qualunque che ne stronca uno vivo no.
    //
    // Era concesso a chiunque passasse di qui, `kind` ignorato. MISURATO sul
    // parco progetti il 03/08/2026: un `grep`, un `dir`, un `ls` e un
    // `vite --version` hanno fermato servizi vivi da ore — fino a un vite da
    // 3h45m. Il riconoscimento lessicale (`avvio_server`) toglie a quei comandi
    // l'ETICHETTA di servizio; questo gate toglie loro il POTERE, che e' la
    // difesa che regge anche quando il riconoscimento sbaglia.
    if kind == "service" {
        let processi = EsitoProcessi::letti(&ctx.db, ctx.project_id).await;
        let vita =
            crate::project_workspace::prenotazione_porta::RunsDelDbDiProgetto::new((*ctx.db).clone());
        dedup_and_cleanup_ports(ctx, label, command, work_dir, processi, &vita).await;
    }

    if let Some(kind_hint) = kind_hint_for(ctx, label, command, work_dir) {
        if let Some(refuse_msg) = refuse_if_same_scope_active(ctx, kind_hint).await {
            return Some(refuse_msg);
        }
    }

    // Solo per kind="service": i tool agente short-lived non contano contro la
    // quota container del progetto.
    if kind == "service" {
        if let Some(quota_msg) = check_container_quotas(ctx, label, command).await {
            return Some(quota_msg);
        }
    }
    None
}

// L'attesa dell'ascolto viveva QUI, come secondo criterio della vita di un
// servizio: `attende_ascolto` con la sua finestra hardcoded e il suo
// `port_listening`, accanto al contratto della remediation che gia' rispondeva
// alla stessa domanda con `probe_port` + `stable_enough` + il ciclo dei due
// orologi. Due criteri per un fatto solo, e il piu' povero dei due montato nel
// posto piu' esposto. Ora la domanda si pone al punto unico
// (`agent_tools::avvio_servizio`, che delega a `service_recovery`), e la
// finestra viene dal DB invece che da una costante (regola G).

/// Dove il servizio stia ascoltando DAVVERO, quando la porta attesa resta muta.
///
/// L'attesa scaduta dice solo «non li'»: e' la classe di fallimento numero uno
/// dei run agentici, e senza il «dove» l'agente non ha nulla da correggere —
/// misurato il caso `frontend` con porta allocata 24806 mentre l'applicazione
/// rispondeva sulla 24804 pinnata nel `.env`, dieci run contro una verifica che
/// nessuna correzione poteva superare e 74 `run_command` di netstat improvvisato.
///
/// Il verdetto DICHIARA, non adotta: nessuna porta trovata qui viene registrata,
/// riallocata o liberata. Nel repo esiste gia' l'incidente opposto (una porta
/// sbagliata ADOTTATA nel registro, che li' faceva da prova di legittimita' a se
/// stessa); qui si produce informazione e la decisione resta al chiamante.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AscoltoAltrove {
    /// Prova diretta: il PID del processo avviato risulta in ascolto su
    /// un'ALTRA porta. L'unica attribuzione che si puo' dimostrare senza
    /// indovinare (regola M): il pid e' un dato, il nome del programma no.
    ProcessoAvviato {
        port: u16,
        esito: nexus_tool_kit::ports::PortRegistrability,
    },
    /// Nessun listener e' riconducibile a questo processo, ma nel bucket del
    /// progetto qualcuno ascolta. Fatti ELENCATI, mai attribuiti al servizio: un
    /// dev server figlio (`npm` -> `node`) gira con un pid diverso da quello
    /// registrato, quindi la sua porta finisce qui e non fra le prove dirette.
    NelBucket(Vec<(u16, u32, String)>),
    /// Interrogato: nessuno ascolta. Il servizio non ha mai fatto bind.
    Nessuno,
    /// Non interrogato. Non e' un «nessuno»: e' l'assenza della domanda, e va
    /// distinta perche' un'assenza spacciata per fatto e' la stessa bugia che
    /// questo enum esiste per togliere (regola O).
    NonAccertato,
}

/// Parte PURA del verdetto: dati i listener della macchina, dove ascolta il
/// servizio che doveva prendere `porta_attesa`?
///
/// Non re-implementa nulla (regola L): i fatti arrivano da
/// `port_recovery::listening_ports` (punto unico di «chi e' in ascolto») e la
/// classificazione della porta trovata da `nexus_tool_kit::ports`.
fn classifica_ascolto_altrove(
    project_id: &Uuid,
    porta_attesa: u16,
    pid_processo: Option<i32>,
    in_ascolto: &[(u16, u32, String)],
) -> AscoltoAltrove {
    let pid_atteso = pid_processo.and_then(|p| u32::try_from(p).ok());
    let altre = in_ascolto.iter().filter(|(p, _, _)| *p != porta_attesa);

    if let Some(pid) = pid_atteso {
        if let Some((port, _, _)) = altre.clone().find(|(_, listener, _)| *listener == pid) {
            return AscoltoAltrove::ProcessoAvviato {
                port: *port,
                esito: nexus_tool_kit::ports::classify_project_port(project_id, *port),
            };
        }
    }

    let nel_bucket: Vec<(u16, u32, String)> = altre
        .filter(|(p, _, _)| nexus_tool_kit::ports::port_in_project_bucket(project_id, *p))
        .cloned()
        .collect();
    if nel_bucket.is_empty() {
        AscoltoAltrove::Nessuno
    } else {
        AscoltoAltrove::NelBucket(nel_bucket)
    }
}

/// Interroga la macchina e classifica. Unico confine col sistema operativo di
/// questa diagnosi: cosi' la decisione resta testabile sui fatti nella forma
/// esatta che `listening_ports` produce.
async fn accerta_ascolto_altrove(
    project_id: Uuid,
    porta_attesa: u16,
    pid_processo: Option<i32>,
) -> AscoltoAltrove {
    let in_ascolto = crate::project_workspace::port_recovery::listening_ports().await;
    classifica_ascolto_altrove(&project_id, porta_attesa, pid_processo, &in_ascolto)
}

/// Traduzione del verdetto in testo per l'agente. Punto unico della resa: il
/// segnale resta l'enum, la prosa non decide nulla.
fn nota_ascolto_altrove(altrove: &AscoltoAltrove) -> String {
    use nexus_tool_kit::ports::PortRegistrability;
    match altrove {
        AscoltoAltrove::ProcessoAvviato { port, esito } => {
            let dove = match esito {
                PortRegistrability::Registrable => {
                    format!("porta {port}, nel bucket di questo progetto")
                }
                PortRegistrability::Reserved => {
                    format!("porta {port}, RISERVATA all'infrastruttura Nexus")
                }
                PortRegistrability::OutOfProjectRange => {
                    format!("porta {port}, fuori dal range dei progetti")
                }
                PortRegistrability::OutOfProjectBucket {
                    bucket_start,
                    bucket_end,
                } => format!(
                    "porta {port}, FUORI dal bucket di questo progetto ({bucket_start}-{bucket_end})"
                ),
            };
            format!(
                "\nDOVE ASCOLTA DAVVERO: il processo appena avviato risulta in ascolto su un'altra \
                 porta ({dove}). Il servizio sta quindi ignorando la porta assegnata: cerca la \
                 porta scritta a mano nella configurazione (.env, vite.config, docker-compose) e \
                 fai leggere al servizio la variabile PORT. Nexus non registra ne' adotta la porta \
                 trovata: la decisione e' tua.\n"
            )
        }
        AscoltoAltrove::NelBucket(porte) => {
            let elenco = porte
                .iter()
                .map(|(p, pid, program)| format!("{p} (pid {pid}, {program})"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "\nDOVE ASCOLTA DAVVERO: nessun listener e' riconducibile al pid del processo \
                 avviato (un dev server figlio gira con un pid diverso). Nel bucket di questo \
                 progetto risultano pero' in ascolto: {elenco}. NON e' provato che appartengano a \
                 questo servizio: verificalo prima di usarne una, e non darla per la porta del \
                 servizio solo perche' risponde.\n"
            )
        }
        AscoltoAltrove::Nessuno => "\nDOVE ASCOLTA DAVVERO: nessuna porta del bucket di questo \
             progetto risulta in ascolto. Il servizio non ha mai fatto bind: la causa e' \
             nell'output qui sotto, non nella porta.\n"
            .to_string(),
        AscoltoAltrove::NonAccertato => String::new(),
    }
}

/// Parametri di avvio risolti da `resolve_service_launch`.
struct ServiceLaunch {
    command: String,
    kind: String,
    label: String,
    work_dir: std::path::PathBuf,
}

/// Legge e valida il parametro `command`: presente, non vuoto e privo di
/// placeholder di redazione copiati come valori (incidente Beaty-Book:
/// `DATABASE_URL=[REDACTED:db_connection_string] node server.js` avviava il
/// backend con host 'base' -> getaddrinfo ENOTFOUND). Punto unico della
/// validazione: security::redaction_guard (regola L). `Err(msg)` = messaggio
/// gia' pronto da restituire.
/// La presenza e il tipo di `command` NON si controllano piu' qui: li pretende
/// il contratto d'ingresso, che e' la stessa scrittura da cui nasce lo schema
/// consegnato al modello. Resta cio' che il contratto non puo' sapere — il
/// comando VUOTO (una stringa e' una stringa anche se non contiene nulla) e i
/// placeholder di redazione copiati come valori.
async fn validate_service_command(
    ctx: &AgentToolContext,
    tool: &str,
    command: String,
) -> Result<String, RispostaTool> {
    if command.trim().is_empty() {
        return Err(RispostaTool::fallito_rimediabile(
            "[Errore: comando vuoto. Passa in 'command' la riga da eseguire.]",
        ));
    }
    if let Some(msg) = crate::security::redaction_guard::enforce_no_redacted_placeholder(
        ctx, tool, "command", &command,
    )
    .await
    {
        // Il guard e' un punto unico non migrato e il suo messaggio resta suo:
        // riscriverlo qui significherebbe interpretarlo. Se ne dichiara la
        // natura, che questo modulo conosce — l'agente ha copiato un segnaposto
        // di redazione al posto di un valore, e sostituirlo e' cio' che deve
        // fare.
        return Err(RispostaTool::fallito_rimediabile(msg));
    }
    Ok(command)
}

/// Fase 1 di `tool_run_service`: valida il comando, declassa one-shot, deriva
/// label e working directory. Il refuse per scopo duplicato avviene DOPO
/// `dedup_and_cleanup_ports` in `tool_run_service` (ordine vincolante).
async fn resolve_service_launch(
    ctx: &AgentToolContext,
    input: &Value,
    tool: &str,
    kind: &str,
) -> Result<ServiceLaunch, RispostaTool> {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::RunServiceInput};

    // Il nome INVOCATO, non quello dichiarato nella macro: questo handler serve
    // anche l'alias storico `run_in_terminal`, e un errore di parametri deve
    // mandare l'agente allo schema del tool che ha davvero chiamato.
    let params = RunServiceInput::leggi_come(tool, input)?;
    let command = validate_service_command(ctx, tool, params.command).await?;

    // Declassamento: comandi che NON avviano un server arrivano qui via
    // auto-routing di run_command (background=true, pattern long-running,
    // auto-probe) con kind="service", ma servizi del progetto non sono.
    // Registrarli con kind='service' li fa comparire nel pannello Servizi e,
    // molto peggio, concede loro i poteri di un servizio. Il processo resta
    // gestito identico (stessa tabella, stop/read_output per id): cambia solo
    // la classificazione.
    //
    // Il criterio e' POSITIVO — «avvia un server?» — e non piu' l'elenco dei
    // one-shot noti. Quell'elenco conteneva `prisma generate` ma non
    // `prisma init`, e le voci mancanti non sono un difetto da tappare una per
    // una: sono la conseguenza di una domanda che non puo' avere risposta
    // completa.
    let kind = declassa_se_non_e_un_servizio(kind, &command);

    // Risoluzione working directory (punto unico riusato anche dal restart).
    let work_dir = resolve_service_work_dir(&ctx.root_path, params.working_dir.as_deref())?;

    // Derivazione identita' (refuse per scopo: dopo dedup in tool_run_service).
    // La conseguenza sulla classificazione la dichiara l'identita' stessa
    // (`classifica`), non il chiamante: un `kind='service'` senza identita' non
    // e' un servizio di progetto e viene declassato come il one-shot qui sopra.
    // Slug del progetto dai due punti unici (nome in `projects` ->
    // `project_service_slug`), lo STESSO da cui nasce il nome unit: serve a
    // riconoscere una label che ripete il progetto. Se il nome non e' leggibile
    // resta `None` e il riconoscimento si regge sul solo suffisso `.service`.
    let slug = sqlx::query_scalar::<_, String>("SELECT name FROM projects WHERE id = $1")
        .bind(ctx.project_id)
        .fetch_optional(ctx.db.as_ref())
        .await
        .ok()
        .flatten()
        .map(|nome| crate::project_workspace::services::project_service_slug(&nome));

    let (kind, label) = resolve_service_label(
        input,
        &command,
        kind,
        &ctx.root_path,
        &work_dir,
        ctx.project_id,
        slug,
    )
    .classifica(kind);

    Ok(ServiceLaunch {
        command,
        kind,
        label,
        work_dir,
    })
}

/// Un `kind='service'` chiesto per un comando che non avvia un server diventa
/// `task`.
///
/// Il processo gira identico (stessa tabella, stop/read_output per id): perde
/// la unit, l'allocazione, la riga nel pannello Servizi e — il punto che conta —
/// il potere di fermare i servizi vivi con label simile.
///
/// MISURATO il 04/08/2026 su prenotazioni-sala, quando il criterio della PORTA
/// era gia' corretto ma questo ancora no: `cd backend && npm init -y` e
/// `cd backend && cargo clippy --all-targets` risultavano `kind='service'` con
/// label `backend`, indistinguibili dal backend vero e capaci di stroncarlo.
/// La meta' mancante di un fix e' la meta' che fa danno.
fn declassa_se_non_e_un_servizio<'a>(kind: &'a str, command: &str) -> &'a str {
    if kind == "service" && !looks_like_web_service(command) {
        "task"
    } else {
        kind
    }
}

/// Label priva di scopo, deliberata: la porta chi NON e' un servizio di progetto
/// (task one-shot, comando qualunque instradato qui con `background=true`).
/// Generica per costruzione, quindi `visible_windows_services` la nasconde dal
/// pannello appena il processo muore e `stop_similar_running_services` non la fa
/// mai combaciare con un servizio vero.
const LABEL_NON_SERVIZIO: &str = "Service";

/// Identita' di un servizio: cio' che lo distingue DENTRO il progetto, e la
/// conseguenza che ne discende sulla classificazione.
///
/// Esiste come tipo perche' le tre risposte non sono la stessa cosa detta in
/// modi diversi: due danno un nome al servizio, la terza dice che un servizio
/// non c'e'. Prima erano tutte una `String`, e "non lo so" era indistinguibile
/// da un'identita' vera.
enum ServiceIdentity {
    /// Un segnale dice il RUOLO: label esplicita, comando, cartella di lavoro.
    Ruolo(String),
    /// Nessun segnale dice il ruolo, ma il comando avvia un server: gli servira'
    /// una porta, quindi gli serve un nome stabile fra i riavvii per riceverla e
    /// conservarla. Ancorato al progetto per COSTRUZIONE (l'uuid), mai per NOME.
    SoloAncoraggio(String),
    /// Non e' un servizio di progetto: nessuna identita' da dare.
    NonServizio,
}

impl ServiceIdentity {
    /// `(kind, label)` conseguenti. Punto unico della traduzione: il tool e i
    /// test attraversano questa, cosi' nessuno dei due ricopia l'altro (regola O).
    ///
    /// Un `kind='service'` senza identita' diventa `task`: il processo gira
    /// identico (stessa tabella, stop/read_output per id), ma non ha unit, non
    /// riceve allocazione e non compare nel pannello Servizi. E' il trattamento
    /// gia' riservato al one-shot declassato, per la stessa ragione.
    fn classifica(self, kind: &str) -> (String, String) {
        match self {
            Self::Ruolo(label) | Self::SoloAncoraggio(label) => (kind.to_string(), label),
            Self::NonServizio => ("task".to_string(), LABEL_NON_SERVIZIO.to_string()),
        }
    }
}

/// IDENTITA' del servizio, mai generica. Il refuse per duplicato attivo e' in
/// `tool_run_service`, dopo `dedup_and_cleanup_ports`.
///
/// La label non e' un'etichetta di comodo: e' l'identita' con cui il servizio
/// esiste nel resto del sistema. Da lei nascono il nome unit
/// (`service_unit_name` -> `{slug}-{label}.service`), la riga
/// `nexus_port_allocations` legata a quell'unit e il match porta<->servizio del
/// pannello. Una label GENERICA ("Service", "server") non ha nessuna parola che
/// identifichi uno scopo: `similar_service_labels("Service", "backend")` e'
/// falso per costruzione, quindi quel servizio non incontra mai la propria
/// allocazione e il pannello non ha un indirizzo da mostrargli.
///
/// Per questo il ripiego storico `unwrap_or("Service")` era il difetto: produceva
/// esattamente cio' che il filtro sulla riga sopra rifiuta all'agente. Se una
/// label senza scopo non e' accettabile quando la propone l'agente, non puo'
/// esserlo quando la sceglie il sistema.
///
/// Ordine di derivazione, dal segnale piu' specifico al piu' stabile:
/// 1. label esplicita dell'agente, se dice qualcosa;
/// 2. scopo (frontend/backend) dedotto da comando + working directory;
/// 3. nome della cartella da cui il servizio gira, SOTTO la radice del progetto
///    (la prima che porti una parola propria);
/// 4. nessuno: e allora si dichiara che non c'e' (vedi [`ServiceIdentity`]).
///
/// Il nome della cartella di PROGETTO non e' un candidato, e non lo e' per
/// costruzione: l'unit e' gia' `{slug}-{label}.service`, quindi lo slug e' la
/// parte che il progetto mette e la label e' la parte che distingue un servizio
/// dall'altro DENTRO quel progetto. Ripetere li' il nome del progetto produce
/// `{slug}-{slug}.service`, che non aggiunge nessuna parola distintiva: dice due
/// volte cio' che si sapeva gia', e non dice il ruolo. E' la stessa forma del
/// difetto gia' tolto a `resource_resolver::orphan_placeholder_label` — dare un
/// nome inventato quando manca il segnale, e lasciarlo entrare nella stessa
/// lista dei nomi veri, dove nessuno potra' piu' distinguerli.
///
/// Misurato il 30/07/2026 su bacheca-attivita: l'UNICA identita' mai prodotta da
/// quel ripiego, sull'intero parco progetti, era `npx -w backend eslint . --ext
/// .ts` lanciato dalla radice — un lint, non un servizio, che il pannello
/// mostrava come `bacheca-attivita-bacheca-attivita.service`.
fn resolve_service_label(
    input: &Value,
    command: &str,
    kind: &str,
    root: &std::path::Path,
    work_dir: &std::path::Path,
    project_id: uuid::Uuid,
    // `slug`: quando il chiamante ce l'ha, serve a riconoscere una label che
    // RIPETE il nome del progetto (l'altra traccia del valore derivato). `None`
    // non indebolisce il criterio principale — il suffisso `.service` si
    // riconosce da solo — ma lascia passare la forma `{slug}-{ruolo}` senza
    // suffisso.
    slug: Option<String>,
) -> ServiceIdentity {
    let explicit_label = input
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| !crate::agent_processes::is_generic_service_label(s))
        // Una label che e' gia' un nome unit non e' un'identita' nuova: e' il
        // valore DERIVATO da un'identita' precedente, riproposto come se fosse
        // primitivo. Accettarlo chiude il ciclo che si autoalimenta (il
        // riconoscimento e la sua misura stanno accanto al produttore,
        // `services::e_nome_unit_derivato`). Si SCARTA, non si ripulisce: la
        // vera identita' la dicono i segnali sotto — comando, scopo, percorso —
        // che sono fatti, mentre una label ripulita sarebbe un'ipotesi.
        .filter(|s| {
            !crate::project_workspace::services::e_nome_unit_derivato(s, slug.as_deref())
        });
    if let Some(label) = explicit_label {
        return ServiceIdentity::Ruolo(label.to_string());
    }

    // Un task one-shot (`npm install`, `playwright test`) NON e' un servizio:
    // non entra nel pannello Servizi (`kind='service'`), non ha unit ne'
    // allocazione. Dargli l'identita' dedotta dallo scopo lo farebbe collidere
    // con il servizio omonimo: `stop_similar_running_services`, che gira per
    // ogni kind, fermerebbe il backend vero perche' l'install gira in backend/.
    if kind != "service" {
        return ServiceIdentity::NonServizio;
    }

    if let Some(hint) = derive_kind_hint(command, "", scope_dir(root, work_dir, command).as_deref())
    {
        return ServiceIdentity::Ruolo(hint.to_string());
    }
    if let Some(dal_percorso) = identita_dal_percorso(root, work_dir, command) {
        return ServiceIdentity::Ruolo(dal_percorso);
    }
    // Nessun segnale di ruolo. Il comando dice se un servizio c'e' comunque:
    // solo chi avvia un server ha bisogno di una porta, e solo per lui vale la
    // pena di un nome di ripiego. Per tutto il resto (un lint, un `git diff`,
    // un comando qualunque arrivato qui con `background=true`) l'assenza di
    // identita' e' la risposta giusta, non un problema da aggirare.
    if looks_like_web_service(command) {
        return ServiceIdentity::SoloAncoraggio(label_di_solo_ancoraggio(project_id));
    }
    ServiceIdentity::NonServizio
}

/// PUNTO UNICO (regola L) della label di SOLO ANCORAGGIO: il nome che nasce
/// quando nessun segnale dice il ruolo ma il comando avvia un server.
///
/// Estratta perche' ha un secondo interrogante oltre al produttore: il censimento
/// del registro porte (ADR 0042, P0(b)) deve saper riconoscere le righe nate da
/// questa via, e riconoscerle CONFRONTANDOLE con cio' che il produttore
/// emetterebbe per quel progetto — non con una regex che ne indovina la forma.
/// Due formule separate divergerebbero al primo cambio di taglio dell'uuid, e il
/// censimento direbbe "nessuna identita' di ripiego" su un parco pieno.
pub(crate) fn label_di_solo_ancoraggio(project_id: uuid::Uuid) -> String {
    format!("service-{}", &project_id.simple().to_string()[..8])
}

/// Identita' derivata dal PERCORSO: nome della cartella da cui il servizio gira,
/// risalendo verso la radice del progetto (ESCLUSA) fino alla prima che porti una
/// parola propria. `services/pagamenti` -> "pagamenti"; working dir sulla radice
/// -> `None`, perche' sopra non c'e' piu' niente che parli del ruolo.
///
/// La radice e' esclusa dallo stesso confine che `scope_dir` traccia gia' per lo
/// scopo: solo la parte SOTTO la radice descrive il servizio, il resto descrive
/// il progetto. Le due domande ("che ruolo ha" e "come si chiama") guardano
/// percio' lo stesso pezzo di percorso, e non possono piu' rispondere da fonti
/// diverse.
///
/// Il vaglio "porta una parola propria" e' `is_generic_service_label` (punto
/// unico del vocabolario label, regola L): una cartella `app/` o `server/` non
/// identifica niente piu' di quanto facesse "Service".
fn identita_dal_percorso(
    root: &std::path::Path,
    work_dir: &std::path::Path,
    command: &str,
) -> Option<String> {
    let sotto_radice = scope_dir(root, work_dir, command);
    let mut candidati: Vec<String> = sotto_radice
        .iter()
        .flat_map(|rel| rel.components())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    // Dalla cartella piu' specifica verso la radice: il ruolo sta piu' vicino al
    // servizio che al progetto.
    candidati.reverse();
    candidati
        .iter()
        .map(|nome| normalizza_label(nome))
        .find(|l| !l.is_empty() && !crate::agent_processes::is_generic_service_label(l))
}

/// Riduce un nome di cartella a label usabile come identita': minuscolo, solo
/// `[a-z0-9-]`, il resto separatore. La label finisce dentro il nome unit
/// `{slug}-{label}.service` e nel path delle API servizi, che rifiutano `/` e
/// `..` (`control_project_service_windows`).
fn normalizza_label(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Deduplicazione servizi prima del refuse: ferma processi simili, raccoglie le
/// allocazioni che nulla tiene in vita, libera LISTEN residuo sullo stesso scopo
/// (zombie/orfani non tracciati in agent_processes). Delega stop a
/// `stop_similar_running_services` e il giudizio sulle porte a
/// [`cleanup_dead_process_ports`] (punti unici, regola L).
///
/// I processi e la vita dei run arrivano come PARAMETRI, non li legge questa
/// funzione. I processi sono la sola osservazione che questo raccoglitore
/// possiede e il GC no, e vanno consegnati come fatto: prima la lettura viveva
/// dentro un `if let Ok(...)` e il suo fallimento saltava in silenzio l'intero
/// cleanup — un ignoto travestito da «niente da fare» (regola Q). `vita` segue
/// la stessa disciplina di `port_gc_loop`, che la costruisce una volta e la
/// passa: e' anche l'unica strada per provare questo percorso su un DB che non
/// ha la directory di routing, dove la porta di produzione risponderebbe
/// `NonInterrogabile` — cioe' un verde per fail-closed invece che per il
/// criterio (regola O).
async fn dedup_and_cleanup_ports(
    ctx: &AgentToolContext,
    label: &str,
    command: &str,
    work_dir: &std::path::Path,
    processi: EsitoProcessi,
    vita: &dyn crate::project_workspace::prenotazione_porta::VitaDelRun,
) {
    let _ =
        crate::agent_processes::stop_similar_running_services(&ctx.db, ctx.project_id, label).await;
    cleanup_dead_process_ports(&ctx.db, ctx.project_id, processi, label, vita).await;
    if let Some(kind_hint) = kind_hint_for(ctx, label, command, work_dir) {
        free_listening_scope_port(ctx, kind_hint).await;
    }
}

/// `derive_kind_hint` sullo scope del comando corrente: punto unico della
/// composizione (regola L), condivisa da [`dedup_and_cleanup_ports`] e
/// [`gate_pre_avvio`]. Le due chiamate erano testualmente identiche gia' prima
/// di questo helper — la duplicazione non era nuova, era solo invisibile al
/// detector finche' una viveva inline in `tool_run_service` con variabili
/// possedute (`&command`, `&label`); estraendola in una funzione a parametri
/// presi in prestito il testo e' tornato IDENTICO a questo, e il detector lo
/// vede correttamente per quello che e'.
fn kind_hint_for(
    ctx: &AgentToolContext,
    label: &str,
    command: &str,
    work_dir: &std::path::Path,
) -> Option<&'static str> {
    derive_kind_hint(
        command,
        label,
        scope_dir(&ctx.root_path, work_dir, command).as_deref(),
    )
}

/// Dopo stop_similar: se lo stesso scopo e' ancora in LISTEN (child orfano,
/// processo non tracciato, stop non verificato), tenta try_free_port sul punto
/// unico port_recovery (regola L). Best-effort: il refuse successivo intercetta
/// solo i casi in cui la porta resta occupata.
async fn free_listening_scope_port(ctx: &AgentToolContext, kind_hint: &str) {
    let Some(res) = crate::project_workspace::resource_resolver::resolve_for_label(
        &ctx.port_registry,
        ctx.project_id,
        kind_hint,
    )
    .await
    else {
        return;
    };
    if !res.listening {
        return;
    }
    let Some(port) = res.port else {
        return;
    };
    tracing::info!(
        kind = %kind_hint,
        port = port,
        matched_label = %res.label,
        pid = ?res.pid,
        "run_service: LISTEN residuo sullo stesso scopo dopo dedup, libero la porta"
    );
    if crate::project_workspace::port_recovery::try_free_port(port).await {
        tracing::info!(port = port, "run_service: porta liberata, proseguo avvio");
    } else {
        tracing::warn!(
            port = port,
            "run_service: porta ancora occupata dopo try_free_port"
        );
    }
}

/// Avvia il processo del servizio tramite il punto unico `spawn_agent_process`.
/// L'iniezione server-side di DATABASE_URL/NEXUS_PROJECT_DB_URL vive DENTRO
/// spawn_agent_process (incidente Beaty-Book, regola L: copre anche wizard,
/// pannello e run config), insieme a env_clear + filtro is_blocked_env
/// sull'ambiente ereditato. NON si passa DATABASE_URL via env_overrides: e' in
/// BLOCKED_ENV e verrebbe rifiutata da validate_env_overrides. Ritorna `Err(msg)`
/// col messaggio d'errore gia' pronto da restituire al chiamante.
async fn spawn_service_process(
    ctx: &AgentToolContext,
    label: &str,
    command: &str,
    work_dir: &std::path::Path,
    env_overrides: Option<HashMap<String, String>>,
    kind: &str,
) -> Result<Uuid, RispostaTool> {
    crate::agent_processes::spawn_agent_process(
        &ctx.db,
        ctx.project_id,
        ctx.session_id,
        label,
        command,
        &work_dir.to_string_lossy(),
        Some(ctx.root_path.clone()),
        env_overrides,
        crate::sandbox::sandbox_enabled(),
        kind,
        None,
    )
    .await
    // Lo spawn fallisce per l'ambiente (sandbox, DB del progetto, risorse), non
    // per come l'agente ha scritto la chiamata: quella era gia' passata dal
    // contratto e da tre gate.
    .map_err(|e| RispostaTool::fallito_di_sistema(format!("[Errore avvio servizio '{label}': {e}]")))
}

/// Registra la porta rilevata, emette gli eventi di pannello e compone il
/// messaggio di ritorno a partire dall'esito GIA' accertato dell'avvio.
///
/// Non attende piu' nulla e non giudica piu' nulla: il verdetto arriva da
/// [`super::avvio_servizio::attendi_avvio`], e qui si traducono le sue tre
/// varianti in cio' che il pannello vede e in cio' che l'agente legge.
///
/// Se la RILETTURA della riga e' fallita l'esito resta quello dichiarato
/// dall'avvio: non aver potuto guardare non e' un fatto sul servizio, e
/// trasformarlo in un fallimento manderebbe l'agente a riavviare un processo
/// vivo (l'errore opposto, con lo stesso costo).
async fn report_started_service(
    ctx: &AgentToolContext,
    label: &str,
    kind: &str,
    process_id: Uuid,
    esito: super::avvio_servizio::EsitoAvvio,
) -> RispostaTool {
    use super::avvio_servizio::AvvioServizio;

    let intestazione = format!("Servizio '{label}' (process_id: {process_id})");
    let info = match esito.info {
        Ok(info) => info,
        Err(e) => {
            return esito
                .avvio
                .risposta(&format!("{intestazione}, output non leggibile: {e}"), None)
        }
    };

    let detected_port = registra_o_audita_porta_rilevata(ctx, label, kind, &info).await;
    // La diagnosi "dove ascolta davvero" si interroga SOLO quando la porta
    // promessa e' rimasta muta e il processo e' ancora vivo: e' li' che «non
    // li'» lascia l'agente senza nulla da correggere. Su un processo MORTO non
    // ha senso cercare un suo listener altrove.
    let altrove = match &esito.avvio {
        AvvioServizio::VivoMaSilenzioso {
            porta_attesa: Some(port),
            ..
        } => accerta_ascolto_altrove(ctx.project_id, *port, info.pid).await,
        _ => AscoltoAltrove::NonAccertato,
    };

    // Dispatcher: notifica avvio servizio -> il pannello Servizi accende il LED.
    // Solo su `Vivo`: accenderlo su un servizio morto o muto ripeterebbe nel
    // pannello la stessa bugia appena tolta all'agente.
    if matches!(esito.avvio, AvvioServizio::Vivo { .. }) {
        nexus_events::dispatcher::emit(
            &ctx.project_channels,
            ctx.project_id,
            nexus_events::event::ProjectEvent::ServiceStarted {
                name: label.to_string(),
                port: detected_port,
                pid: info.pid,
            },
        );
    }

    let nota = nota_ascolto_altrove(&altrove);
    let nota = (!nota.is_empty()).then_some(nota);
    esito.avvio.risposta(&intestazione, nota.as_deref())
}

/// Che fare della porta che il servizio ha dichiarato nel proprio output: qui,
/// dove il verdetto e' quello vero e non una sua ricostruzione. La porta del
/// bucket ALTRUI non si registra (vedi [`audita_porta_fuori_bucket`]) ma non
/// sparisce: viene auditata, e restando senza allocazione resta visibile al
/// port_enforcer e al linter. Ritorna la porta REGISTRATA, cioe' l'unica che il
/// pannello puo' legare a questo servizio.
async fn registra_o_audita_porta_rilevata(
    ctx: &AgentToolContext,
    label: &str,
    kind: &str,
    info: &crate::agent_processes::ProcessOutput,
) -> Option<i32> {
    // Chi non e' un servizio di progetto non registra porte, e non e' una cautela:
    // e' che non ne ha nessuna. `declassa_se_non_e_un_servizio` porta a `task`
    // ogni comando che non avvia un server, e la sua identita' e'
    // `LABEL_NON_SERVIZIO` — generica per costruzione. Una porta che compare
    // nell'output di un task e' percio' di QUALCUN ALTRO (un `curl` verso il
    // backend, un test che stampa l'URL su cui punta), e registrarla significa
    // scriverne nel registro l'identita' che il sistema stesso dichiara priva di
    // significato. E' la strada da cui la label `Service` e' entrata in
    // `nexus_port_allocations` su agenda-medica il 2026-08-06.
    if !registra_le_proprie_porte(kind) {
        return None;
    }
    match detect_port_from_output(&ctx.project_id, &info.stdout, &info.stderr) {
        Some(r) if r.esito == nexus_tool_kit::ports::PortRegistrability::Registrable => {
            register_detected_port(ctx, label, r.port as i32, info.pid).await;
            Some(r.port as i32)
        }
        Some(r) => {
            audita_porta_fuori_bucket(ctx, label, r, info.pid);
            None
        }
        None => None,
    }
}

/// Solo un servizio di progetto registra la porta che il proprio output dichiara.
///
/// Il criterio e' il `kind` gia' risolto da `declassa_se_non_e_un_servizio` +
/// `ServiceIdentity::classifica`, non un secondo giudizio sul comando: due
/// discriminanti per la stessa domanda divergerebbero al primo ritocco (regola L).
fn registra_le_proprie_porte(kind: &str) -> bool {
    kind == "service"
}

/// Una porta rilevata FUORI dal bucket del progetto NON viene registrata. La
/// scelta e' esplicita, e l'alternativa (registrarla marcandola come fuori
/// bucket) e' peggiore: una riga in `nexus_port_allocations` non e' una nota a
/// margine, e' il titolo di proprieta' su cui si fondano il port_enforcer - che
/// sulle porte "allocate" non uccide - e il resource_linter, che sulle porte
/// "allocate" tace. Registrandola, la violazione produceva da se' la prova della
/// propria legittimita': piu' il sistema sbagliava, meno lo segnalava. E
/// l'allocazione sottraeva davvero la porta al progetto proprietario del bucket,
/// che e' la collisione che il bucket esiste per impedire (regola E).
///
/// La visibilita' non si perde, cambia canale: l'audit qui, il port_enforcer che
/// termina il processo fuori bucket e apre la diagnosi, il linter che continua a
/// segnalare la porta hardcoded nel sorgente. Cioe' la CAUSA, non il suo effetto.
fn audita_porta_fuori_bucket(
    ctx: &AgentToolContext,
    label: &str,
    rilevata: PortaRilevata,
    pid: Option<i32>,
) {
    use nexus_tool_kit::ports::PortRegistrability;
    let PortRegistrability::OutOfProjectBucket {
        bucket_start,
        bucket_end,
    } = rilevata.esito
    else {
        return;
    };
    tracing::warn!(
        port = rilevata.port,
        service = %label,
        project_id = %ctx.project_id,
        bucket_start,
        bucket_end,
        reason = rilevata.esito.reason(),
        "porta del servizio fuori dal bucket del progetto: non registrata come allocazione"
    );
    registra_audit_del_contesto(
        ctx,
        crate::security::AuditEntry::blocked(
            ctx.project_id,
            "port_out_of_bucket_detected".to_string(),
            "port",
        )
        .with_resource(rilevata.port.to_string())
        .with_details(serde_json::json!({
            "port": rilevata.port,
            "label": label,
            "pid": pid,
            "bucket_start": bucket_start,
            "bucket_end": bucket_end,
            "reason": rilevata.esito.reason(),
        })),
    );
}

/// Chi ha fatto l'azione lo sa il contesto, non il chiamante: qui l'attore si
/// attacca una volta sola. La sessione e' `Option` — un audit nato fuori da una
/// chat non ne ha una, e inventarla direbbe il falso su chi ha agito.
fn registra_audit_del_contesto(ctx: &AgentToolContext, entry: crate::security::AuditEntry) {
    let mut entry = entry.with_actor_user(ctx.user_id);
    if let Some(s) = ctx.session_id {
        entry = entry.with_actor_session(s);
    }
    crate::security::record_audit(entry);
}

/// Registra in `nexus_port_allocations` la porta auto-rilevata dall'output ed
/// emette l'evento `PortAllocated` per il pannello Porte. La decisione su COSA
/// sia registrabile sta nel chiamante, che ha il verdetto di
/// `detect_port_from_output`: qui non si ri-classifica, altrimenti la stessa
/// domanda avrebbe due risposte possibili (regola L).
///
/// A CHI la porta appartenga gia' lo decide il punto unico
/// [`nexus_tool_kit::ports::classify_port_claim`]. L'evento si emette SOLO quando
/// una riga e' davvero stata scritta: un `PortAllocated` su una porta di un altro
/// direbbe al pannello che il servizio ha un indirizzo che non ha.
async fn register_detected_port(ctx: &AgentToolContext, label: &str, port: i32, pid: Option<i32>) {
    let claim = registra_porta_rilevata(&ctx.db, ctx.project_id, label, port).await;
    if matches!(claim, nexus_tool_kit::ports::PortClaim::DiUnAltro { .. }) {
        audita_conflitto_identita(ctx, label, port, pid, &claim);
        return;
    }
    nexus_events::dispatcher::emit(
        &ctx.project_channels,
        ctx.project_id,
        nexus_events::event::ProjectEvent::PortAllocated {
            port,
            label: label.to_string(),
            pid,
        },
    );
}

/// La porta dichiarata era gia' di un altro: si lascia com'e' e lo si scrive.
/// L'audit nomina ENTRAMBE le identita' — senza il derubato, "conflitto" non dice
/// con chi, e la riga di log non basta a ricostruire il caso.
fn audita_conflitto_identita(
    ctx: &AgentToolContext,
    label: &str,
    port: i32,
    pid: Option<i32>,
    claim: &nexus_tool_kit::ports::PortClaim,
) {
    let nexus_tool_kit::ports::PortClaim::DiUnAltro {
        project_id,
        label: proprietaria,
    } = claim
    else {
        return;
    };
    tracing::warn!(
        port,
        richiedente = %label,
        proprietaria = %proprietaria,
        proprietario_project = %project_id,
        "porta rilevata gia' registrata a un'altra identita': non riscrivo l'allocazione"
    );
    registra_audit_del_contesto(
        ctx,
        crate::security::AuditEntry::blocked(
            ctx.project_id,
            "port_identity_conflict".to_string(),
            "port",
        )
        .with_resource(port.to_string())
        .with_details(serde_json::json!({
            "port": port,
            "richiedente": label,
            "registrata_a": proprietaria,
            "registrata_al_progetto": project_id,
            "pid": pid,
            "reason": claim.reason(),
        })),
    );
}

/// La sola parte con il DB della registrazione porta-da-output, separata perche'
/// la si possa provare contro lo schema vero senza costruire un
/// [`AgentToolContext`] (regola O).
///
/// La riga si identifica da `(project_id, label)`, e il conflitto sulla PORTA non
/// e' piu' un permesso di riscrittura: `ON CONFLICT (port) DO NOTHING` chiude
/// anche la corsa fra la lettura e la scrittura — chi arriva secondo non scrive,
/// che e' l'esito giusto in entrambe le direzioni.
async fn registra_porta_rilevata(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    label: &str,
    port: i32,
) -> nexus_tool_kit::ports::PortClaim {
    use nexus_tool_kit::ports::{classify_port_claim, PortClaim};

    let registrata: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT project_id, label FROM nexus_port_allocations WHERE port = $1",
    )
    .bind(port)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let claim = classify_port_claim(
        &project_id,
        label,
        registrata.as_ref().map(|(p, l)| (*p, l.as_str())),
    );

    match claim {
        PortClaim::Registrabile => {
            let _ = sqlx::query(
                "INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode) \
                 VALUES ($1, $2, $3, 'auto') ON CONFLICT (port) DO NOTHING",
            )
            .bind(project_id)
            .bind(port)
            .bind(label)
            .execute(db)
            .await;
        }
        PortClaim::GiaSua => {
            // Conferma: la riga e' gia' quella giusta, si aggiorna solo quando e'
            // stata vista viva l'ultima volta.
            let _ = sqlx::query(
                "UPDATE nexus_port_allocations SET updated_at = NOW() \
                 WHERE port = $1 AND project_id = $2",
            )
            .bind(port)
            .bind(project_id)
            .execute(db)
            .await;
        }
        PortClaim::DiUnAltro { .. } => {}
    }
    claim
}

// Qui vivevano `format_ascolto_mancante` e `format_started_message`: la
// composizione del messaggio a partire da `Option<(u16, bool)>`, cioe' da un
// booleano che diceva soltanto se la porta avesse risposto. In quel tipo la
// morte del processo non era rappresentabile, e infatti non veniva mai
// dichiarata. Ora il messaggio si compone DAI CAMPI dell'esito tipizzato
// (`AvvioServizio::risposta`, regola Q), e le tre varianti portano ciascuna la
// propria conseguenza.

/// Deriva lo scopo del servizio (frontend/backend) dal comando, dalla label o
/// dalla working directory. Punto unico della classificazione (regola L).
///
/// La working directory e' il terzo segnale perche' e' l'unico che resta quando
/// il comando e' un alias di script (`npm start`, `pnpm serve`, `yarn dev`): li'
/// il ruolo non sta nel comando, sta nella cartella da cui gira. Cercare altre
/// stringhe nel comando non chiuderebbe niente: il caso che ha fallito era
/// `npm start`, il prossimo sara' `pnpm serve`.
///
/// `scope_dir` e' la working directory RELATIVA alla radice del progetto (vedi
/// [`scope_dir`]): il nome della cartella di progetto non e' un segnale di ruolo.
/// Quella working directory il comando puo' dichiararla lui stesso (`cd frontend
/// && npm run dev`), ed e' [`scope_dir`] a saperlo: qui il segnale arriva gia'
/// risolto, quindi le due scritture non possono dare due ruoli diversi.
fn derive_kind_hint(
    command: &str,
    label: &str,
    scope_dir: Option<&std::path::Path>,
) -> Option<&'static str> {
    let cmd = command.to_lowercase();
    let lbl = label.to_lowercase();
    let is_frontend = cmd.contains("vite")
        || cmd.contains("svelte")
        || lbl.contains("frontend")
        || lbl.contains("vite")
        || lbl.contains("react")
        || lbl.contains("svelte")
        || lbl.contains("nuxt")
        || lbl.contains("astro");
    if is_frontend {
        return Some("frontend");
    }
    let is_backend = cmd.contains("express")
        || cmd.contains("fastify")
        || cmd.contains("uvicorn")
        || cmd.contains("gunicorn")
        || cmd.contains("rails")
        || cmd.contains("django")
        || cmd.contains("server.js")
        || (cmd.contains(" run ") && cmd.contains("backend"))
        || lbl.contains("backend")
        || lbl.contains("api");
    if is_backend {
        return Some("backend");
    }
    scope_from_work_dir(scope_dir)
}

/// Scopo dedotto dalle cartelle sotto la radice del progetto. Convenzioni di
/// layout, non elenco di comandi: `backend/`, `apps/web`, `services/api` dicono
/// il ruolo di cio' che gira dentro, qualunque sia lo script che lo avvia.
/// Si guarda dalla cartella piu' specifica verso la radice.
fn scope_from_work_dir(scope_dir: Option<&std::path::Path>) -> Option<&'static str> {
    let rel = scope_dir?;
    for comp in rel.components().rev() {
        let nome = comp.as_os_str().to_string_lossy().to_lowercase();
        for parola in nome.split(|c: char| !c.is_ascii_alphanumeric()) {
            match parola {
                "frontend" | "client" | "web" | "webapp" | "ui" | "www" => {
                    return Some("frontend")
                }
                "backend" | "server" | "api" | "srv" => return Some("backend"),
                _ => {}
            }
        }
    }
    None
}

/// Parte della working directory che sta SOTTO la radice del progetto, `None` se
/// il servizio gira dalla radice stessa.
///
/// Solo quella parte parla del RUOLO del servizio: il nome della cartella di
/// progetto descrive il progetto intero, e dedurne lo scopo classificherebbe come
/// "frontend" anche il backend di un progetto chiamato `shop-frontend`.
///
/// La cartella da cui il servizio gira e' dichiarata in DUE posti, e sono lo
/// stesso dato scritto in due modi: il parametro `working_dir` del tool, e il
/// `cd` in testa al comando (`cd frontend && npm run dev`). Il secondo non e'
/// un'euristica sul nome: il comando viene eseguito con `bash -c` a partire da
/// `working_dir` (vedi `spawn_agent_process`), quindi quel `cd` sposta davvero la
/// directory in cui il servizio gira — dice il ruolo con la stessa autorita' del
/// parametro. Finche' nessuno lo leggeva, un progetto avviato dalla radice con
/// `cd frontend &&` non aveva nessun segnale di ruolo e finiva sull'ancoraggio
/// `service-<uuid>`: nel pannello Servizi compariva una terza voce accanto a
/// backend e frontend, per un'app che ne ha due (misurato il 30/07/2026,
/// progetto bacheca-attivita).
///
/// La dichiarazione nel comando vale SE E SOLO SE indica una cartella reale
/// dentro la radice; in ogni altro caso (`cd` verso l'esterno, verso una cartella
/// inesistente, argomento che non e' un percorso letterale) si ricade sulla sola
/// `working_dir`, cioe' sul comportamento precedente. Un `cd` fuori dalla radice
/// non e' uno scope di questo progetto (regola E), e non deve nemmeno cancellare
/// il ruolo che la working dir dichiarava.
///
/// La radice viene canonicalizzata come fa `resolve_relative_path`, che e' il
/// produttore di `work_dir`: senza, su Windows il prefisso verbatim (`\\?\D:\...`)
/// impedirebbe lo strip e la working directory non parlerebbe mai (regola O: la
/// misura deve raggiungere il suo oggetto per la stessa strada della produzione).
fn scope_dir(
    root: &std::path::Path,
    work_dir: &std::path::Path,
    command: &str,
) -> Option<std::path::PathBuf> {
    let effettiva = directory_effettiva(root, work_dir, command);
    relativa_alla_radice(root, &effettiva).filter(|rel| rel.components().next().is_some())
}

/// La cartella da cui il servizio gira DAVVERO: la working dir, spostata dal `cd`
/// in testa al comando quando questo indica una cartella reale dentro la radice.
///
/// Le due condizioni non sono due casi previsti, sono un vaglio unico. Deve
/// esistere, perche' e' la stessa pretesa che `resolve_relative_path` ha gia' sul
/// parametro `working_dir`: le due scritture dello stesso dato entrano alle stesse
/// condizioni. E deve stare dentro la radice, perche' fuori non c'e' niente che
/// appartenga a questo progetto (regola E). Insieme rendono inutile qualunque
/// elenco di forme da rifiutare (`cd $DIR`, `cd /d D:\x`, una redirezione dopo il
/// percorso): un argomento che non nomina una cartella vera del progetto non passa
/// il vaglio, senza che nessuno l'abbia dovuto prevedere.
///
/// Quando la dichiarazione non passa, si ricade sulla sola working dir: cioe' sul
/// comportamento precedente al fix. Un `cd` verso l'esterno non e' uno scope, ma
/// non deve nemmeno cancellare il ruolo che la working dir dichiarava.
fn directory_effettiva(
    root: &std::path::Path,
    work_dir: &std::path::Path,
    command: &str,
) -> std::path::PathBuf {
    let Some(rel) = cd_dichiarato(command) else {
        return work_dir.to_path_buf();
    };
    let dichiarata = normalizza_percorso(&work_dir.join(rel));
    if dichiarata.is_dir() && relativa_alla_radice(root, &dichiarata).is_some() {
        dichiarata
    } else {
        work_dir.to_path_buf()
    }
}

/// Percorso di `dir` relativo alla radice del progetto: vuoto per la radice
/// stessa, `None` se `dir` le sta fuori. Punto unico della misura "dove sta questa
/// cartella rispetto al progetto": il confine di appartenenza e la parte che parla
/// del ruolo sono la stessa domanda, e due implementazioni potrebbero rispondere
/// in modo diverso sullo stesso percorso.
fn relativa_alla_radice(
    root: &std::path::Path,
    dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let root_canonico = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    dir.strip_prefix(&root_canonico)
        .or_else(|_| dir.strip_prefix(root))
        .ok()
        .map(std::path::Path::to_path_buf)
}

/// Risolve `.` e `..` senza toccare il filesystem. Necessario e non sostituibile
/// da `canonicalize`: su Windows un percorso col prefisso verbatim (`\\?\D:\...`,
/// quello che `resolve_relative_path` produce) NON viene normalizzato dal SO, e
/// `..` resterebbe un componente letterale che nessuna cartella ha.
fn normalizza_percorso(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            altro => out.push(altro.as_os_str()),
        }
    }
    out
}

/// Separatori fra due comandi di shell, dal piu' lungo al piu' corto: `&&` non va
/// letto come `&`, ne' `||` come `|`.
const SEPARATORI_COMANDO: &[&str] = &["&&", "||", ";", "|", "&"];

/// Directory che il comando dichiara con uno o piu' `cd` in testa, relativa alla
/// working dir da cui il comando parte (assoluta se il `cd` lo era: `join` la
/// sostituisce, ed e' il caso che [`directory_effettiva`] scartera' perche' fuori
/// dalla radice). `None` se il comando non comincia con un `cd`.
///
/// Si leggono solo i `cd` INIZIALI: al primo comando che non lo e' ci si ferma,
/// perche' da li' in poi si sta descrivendo cio' che il servizio fa, non da dove.
fn cd_dichiarato(command: &str) -> Option<std::path::PathBuf> {
    let mut resto = command;
    let mut dichiarata: Option<std::path::PathBuf> = None;
    loop {
        let (primo, dopo) = spezza_al_separatore(resto);
        let Some(arg) = argomento_cd(primo) else {
            break;
        };
        dichiarata = Some(dichiarata.unwrap_or_default().join(arg));
        match dopo {
            Some(r) => resto = r,
            None => break,
        }
    }
    dichiarata
}

/// Primo comando della riga e cio' che resta dopo il separatore.
fn spezza_al_separatore(riga: &str) -> (&str, Option<&str>) {
    let mut primo_taglio: Option<(usize, usize)> = None;
    for sep in SEPARATORI_COMANDO {
        if let Some(i) = riga.find(sep) {
            if primo_taglio.is_none_or(|(j, _)| i < j) {
                primo_taglio = Some((i, sep.len()));
            }
        }
    }
    match primo_taglio {
        Some((i, len)) => (&riga[..i], Some(&riga[i + len..])),
        None => (riga, None),
    }
}

/// Argomento di un `cd`, se questo comando e' un `cd`. `None` per tutto il resto,
/// incluso `cd` nudo (che porta alla home, non a uno scope del progetto) e i
/// comandi che cominciano per "cd" senza esserlo (`cdk deploy`).
fn argomento_cd(comando: &str) -> Option<&str> {
    let comando = comando.trim();
    if !comando.get(..2)?.eq_ignore_ascii_case("cd") {
        return None;
    }
    let arg = comando[2..]
        .strip_prefix(char::is_whitespace)?
        .trim();
    if arg.is_empty() {
        return None;
    }
    Some(sfila_apici(arg))
}

/// Toglie una coppia di apici o virgolette che racchiuda l'intero argomento
/// (`cd "mia cartella"`), lasciando intatto tutto il resto.
fn sfila_apici(arg: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(interno) = arg.strip_prefix(q).and_then(|a| a.strip_suffix(q)) {
            return interno;
        }
    }
    arg
}

/// Risolve il working directory di un servizio dal parametro `working_dir`,
/// ricadendo sulla radice del progetto. In caso di path invalido ritorna
/// direttamente il messaggio d'errore da restituire al chiamante.
///
/// Prende `root` invece dell'intero contesto perche' e' l'unico campo che usa:
/// cosi' un test puo' partire dallo STESSO input del tool e attraversare questo
/// produttore, invece di fabbricarsi la working dir a mano (regola O).
fn resolve_service_work_dir(
    root: &std::path::Path,
    working_dir: Option<&str>,
) -> Result<std::path::PathBuf, RispostaTool> {
    let Some(sub) = working_dir.filter(|s| !s.is_empty()) else {
        return Ok(root.to_path_buf());
    };
    match resolve_relative_path(root, sub) {
        Ok(p) => Ok(p),
        // Il percorso e' un parametro che l'agente controlla, e il resolver dice
        // gia' perche' non va bene: rimediabile con l'informazione a bordo.
        Err(e) => Err(RispostaTool::fallito_rimediabile(format!(
            "[Errore percorso: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        ))),
    }
}

/// Anti-duplicato convergente sul PUNTO UNICO resource_resolver (regola L).
/// Valutato DOPO `dedup_and_cleanup_ports`: refuse solo se, dopo stop_similar e
/// try_free_port, lo stesso scopo e' ancora in LISTEN (servizio legit da riusare).
async fn refuse_if_same_scope_active(
    ctx: &AgentToolContext,
    kind: &str,
) -> Option<RispostaTool> {
    let res = crate::project_workspace::resource_resolver::resolve_for_label(
        &ctx.port_registry,
        ctx.project_id,
        kind,
    )
    .await?;
    match existing_service_action(res.listening) {
        ExistingServiceAction::RefuseActive => {
            // Servizio dello stesso scopo gia' ATTIVO: REFUSE, riusa la sua porta.
            let port = res
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string());
            // RIMEDIABILE, ed e' il caso in cui la variante e' letteralmente
            // vera: il messaggio non dice solo che e' andata male, dice le due
            // cose da fare — riusare la porta indicata, o riavviare con la label
            // indicata.
            Some(RispostaTool::fallito_rimediabile(format!(
                "[Errore: servizio '{}' di tipo {} gia' ATTIVO sulla porta {}. \
                 Riusalo invece di crearne uno nuovo (puoi accedere a http://localhost:{}). \
                 Se vuoi davvero riavviarlo usa `service_restart` con label='{}'.]",
                res.label, kind, port, port, res.label
            )))
        }
        ExistingServiceAction::AdoptStale => {
            // Allocazione dello stesso scopo SPENTA: NON rifiutare (era la causa
            // radice del deadlock allocazione stantia: il refuse rimandava a
            // `service_restart`, che pero' non puo' riavviare un servizio
            // non-systemd con service_unit NULL). Si ADOTTA e si prosegue
            // l'avvio: piu' sotto `cleanup_dead_process_ports` preserva questa
            // porta e `find_or_allocate` (punto unico, regola L) riusa la stessa
            // allocazione per la label. Per i servizi non-web (es.
            // `node src/server.js`) il processo viene semplicemente ri-spawnato.
            tracing::info!(
                label = %res.label,
                kind = %kind,
                port = ?res.port,
                "run_service: allocazione spenta dello stesso scopo, ADOTTO e riavvio (no refuse)"
            );
            None
        }
    }
}

/// Verifica le quote container (numero e RAM) prima di avviare un servizio.
/// Ritorna `Some(risposta)` col fallimento da restituire se una quota e'
/// raggiunta; `None` se l'avvio puo' proseguire. Registra l'audit dei blocchi.
///
/// DIVERGENZA CHIUSA: i due rami di quota ritornavano un messaggio SENZA marker
/// di fallimento. Il servizio non partiva e l'esito diceva riuscito — un avvio
/// bloccato dalla policy che l'agente riceveva come conferma, e su cui costruiva
/// i passi successivi. E' la stessa forma dell'incidente che ha motivato
/// `attende_ascolto` («il processo esiste» scambiato per «il servizio serve»),
/// qui nella variante in cui il processo non nasce affatto. Col campo non e' piu'
/// una dimenticanza possibile: il tipo pretende che l'esito sia dichiarato.
///
/// La natura e' `DelSistema` per entrambi, e non e' una scelta di comodo: una
/// quota e' una decisione del progetto: ripetere non la cambia, e non c'e' un
/// parametro della chiamata che la aggiri. Stesso precedente del permesso di
/// scrittura negato in `write_file`.
async fn check_container_quotas(
    ctx: &AgentToolContext,
    label: &str,
    command: &str,
) -> Option<RispostaTool> {
    // Separazione DB: agent_processes (conteggi container/RAM) vive nel DB
    // del progetto; le quote restano nel meta. Punto unico project_db_routes.
    // Fail-closed: quota non verificabile -> avvio bloccato con messaggio
    // esplicito (lo spawn fallirebbe comunque sullo stesso DB).
    let run_pool =
        match crate::project_db_routes::project_data_pool_from(&ctx.db, ctx.project_id).await {
            Ok(p) => p,
            Err(e) => {
                return Some(RispostaTool::fallito_di_sistema(format!(
                    "[Errore: DB del progetto non disponibile, quote container non verificabili: {e}]"
                )))
            }
        };
    if let Err(reason) =
        crate::security::quotas::check_can_start_container(&ctx.db, &run_pool, ctx.project_id).await
    {
        crate::security::record_audit(
            crate::security::AuditEntry::blocked(ctx.project_id, "container_create", "container")
                .with_resource(label.to_string())
                .with_details(serde_json::json!({"reason": reason, "command": command})),
        );
        return quota_superata("Quota raggiunta", reason);
    }
    // Quota RAM pre-avvio (governance container/memory_quota): blocca se i
    // servizi gia' attivi del progetto saturano max_memory_mb.
    let ram_gate =
        crate::security::resource_governance::policy(&ctx.db, "container", "memory_quota")
            .await
            .enabled;
    if !ram_gate {
        return None;
    }
    if let Err(reason) =
        crate::security::quotas::check_can_use_memory(&ctx.db, &run_pool, ctx.project_id).await
    {
        crate::security::record_audit(
            crate::security::AuditEntry::blocked(
                ctx.project_id,
                "container_quota_blocked",
                "container",
            )
            .with_resource(label.to_string())
            .with_details(serde_json::json!({"reason": reason})),
        );
        return quota_superata("Quota memoria raggiunta", reason);
    }
    None
}

/// Il rifiuto di una quota, composto in un punto solo.
///
/// I tre rami di [`check_container_quotas`] dicevano la stessa cosa in tre modi,
/// e due di loro la dicevano MALE: tornavano un messaggio senza il marker di
/// fallimento, cioe' un avvio bloccato che l'agente riceveva come conferma.
///
/// Sempre [`NaturaFallimento::DelSistema`]: una quota e' una decisione del
/// progetto, non c'e' un parametro della chiamata che la aggiri e ripeterla non
/// la cambia. Stesso precedente del permesso di scrittura negato in `write_file`.
fn quota_superata(cosa: &str, reason: impl std::fmt::Display) -> Option<RispostaTool> {
    Some(RispostaTool::fallito_di_sistema(format!(
        "[{cosa}: {reason}]"
    )))
}

/// Per i servizi web alloca una porta nel bucket del progetto e prepara gli
/// override d'ambiente (`PORT`/`HOST`). Ritorna `Ok(None)` per i comandi che
/// non sono server web (nessun override), `Err(msg)` se l'allocazione fallisce.
async fn allocate_web_port_env(
    ctx: &AgentToolContext,
    command: &str,
    label: &str,
) -> Result<Option<HashMap<String, String>>, RispostaTool> {
    if !looks_like_web_service(command) {
        return Ok(None);
    }
    // PUNTO UNICO (regola L) dell'alloca+inietta: alloca la porta del bucket, la
    // lega all'unit del servizio (senza `service_unit` il GC la rilascerebbe a
    // servizio fermo — drift 31792->31798, incidente Beaty-Book) e pretende che
    // sia bindabile prima di prometterla al processo in avvio.
    let env = crate::project_workspace::allocate_port::web_service_port_env(
        &ctx.db,
        &ctx.port_registry,
        ctx.project_id,
        label,
    )
    .await
    // Il bucket del progetto puo' essere esaurito, o la porta puo' non essere
    // bindabile: in entrambi i casi non c'e' nulla nella chiamata da correggere,
    // e il servizio non parte senza una porta che gli sia stata promessa.
    .map_err(|e| {
        RispostaTool::fallito_di_sistema(format!("[Errore porta per servizio '{label}': {e}]"))
    })?;
    Ok(Some(env))
}

/// Rilascia le allocazioni di questo progetto che nessuna prova tiene in vita.
///
/// # Perche' NON ha un criterio proprio (regola L)
///
/// Il GC periodico (`port_registry::cleanup_orphaned_ports`) e questo
/// raccoglitore cancellano righe della STESSA tabella rispondendo alla STESSA
/// domanda — «questa allocazione va raccolta?» — e avevano DUE criteri. Qui ce
/// n'era uno solo, il piu' debole: «la porta e' bindabile?». Bastava che una
/// prenotazione appena scritta da `request_port` (mig 0741) portasse una label
/// diversa da quella in avvio perche' venisse cancellata all'istante, nel
/// momento peggiore e senza nemmeno la grace — e la terza prova di vita, che il
/// GC aveva appena imparato, qui non esisteva.
///
/// Ora il verdetto viene dal punto unico
/// [`crate::project_workspace::raccolta_allocazione`] e questa funzione porta
/// soltanto i FATTI che possiede e il GC no:
///  - ETA': la colonna `created_at` confrontata con la stessa grace del GC.
///  - ASCOLTO: un tentativo di bind per riga (il GC usa una fotografia dei
///    listener presa a inizio giro: strumenti diversi, stessa domanda).
///  - IMPIEGO: la label del servizio in avvio ADESSO, piu' quelle dei processi
///    running/starting. E' l'unica prova che il GC non ha, ed e' la ragione per
///    cui non basta chiamare il GC da qui.
async fn cleanup_dead_process_ports(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    processi: EsitoProcessi,
    preserve_label: &str,
    vita: &dyn crate::project_workspace::prenotazione_porta::VitaDelRun,
) {
    let attive = label_attive(&processi);
    let grace = crate::project_workspace::raccolta_allocazione::grace_secs(db).await;
    let adesso = chrono::Utc::now();

    for riga in allocazioni_dinamiche(db, project_id).await {
        let impiego = impiego_della_label(&riga.label, preserve_label, attive.as_ref(), &processi);
        let verdetto = crate::project_workspace::raccolta_allocazione::giudica_riga(
            db,
            crate::project_workspace::raccolta_allocazione::RigaAllocazione {
                project_id,
                porta: riga.port as u16,
                service_unit: riga.service_unit.clone(),
                prenotata_da_run: riga.prenotata_da_run,
            },
            osservazioni_della_riga(&riga, impiego, grace, adesso).await,
            vita,
        )
        .await;

        if verdetto.raccoglie() {
            rilascia_allocazione_raccolta(db, project_id, riga.port, &riga.label, verdetto).await;
        }
    }
}

/// Una riga `dynamic` del registro, coi soli campi su cui il criterio decide.
///
/// Struct e non tupla: cinque colonne dello stesso identico tipo di ritorno
/// scambiabili per posizione, dove `service_unit` e `label` sono entrambe
/// `Option<String>`/`String` — un ordine sbagliato non lo vedrebbe ne' il
/// compilatore ne' un test.
#[derive(sqlx::FromRow)]
struct RigaDinamica {
    port: i32,
    label: String,
    service_unit: Option<String>,
    prenotata_da_run: Option<uuid::Uuid>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Il corpus di questo raccoglitore: le sole allocazioni `dynamic` del progetto.
///
/// Errore di lettura -> nessuna riga, quindi nessuna cancellazione: non si
/// distrugge cio' che non si e' potuto leggere.
async fn allocazioni_dinamiche(db: &sqlx::PgPool, project_id: uuid::Uuid) -> Vec<RigaDinamica> {
    sqlx::query_as::<_, RigaDinamica>(
        "SELECT port, label, service_unit, prenotata_da_run, created_at \
         FROM nexus_port_allocations \
         WHERE project_id = $1 AND allocation_mode = 'dynamic'",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Le label dei processi ancora attivi. `None` non e' un insieme vuoto: e'
/// l'ignoto dichiarato dalla lettura fallita, e i due portano a decisioni
/// opposte (regola Q).
fn label_attive(processi: &EsitoProcessi) -> Option<std::collections::HashSet<String>> {
    match processi {
        EsitoProcessi::Elenco(v) => Some(
            v.iter()
                .filter(|p| p.status == "running" || p.status == "starting")
                .map(|p| p.label.clone())
                .collect(),
        ),
        EsitoProcessi::NonInterrogabili(_) => None,
    }
}

/// Le tre osservazioni che questo raccoglitore consegna al criterio.
///
/// Il bind e' l'unica che costa una syscall, e si paga solo dove puo' cambiare
/// il verdetto: sotto grace o con la label in uso la riga e' gia' salva, e
/// interrogare il sistema non aggiungerebbe nulla.
async fn osservazioni_della_riga(
    riga: &RigaDinamica,
    impiego: crate::project_workspace::raccolta_allocazione::ImpiegoDellaLabel,
    grace: i64,
    adesso: chrono::DateTime<chrono::Utc>,
) -> crate::project_workspace::raccolta_allocazione::OsservazioniDelChiamante {
    use crate::project_workspace::raccolta_allocazione as raccolta;
    let eta = raccolta::EtaAllocazione::da_creazione(riga.created_at, grace, adesso);
    let gia_salva = matches!(eta, raccolta::EtaAllocazione::DentroLaGrace)
        || matches!(impiego, raccolta::ImpiegoDellaLabel::InUso);
    let ascolto = if gia_salva {
        raccolta::Ascolto::Nessuno
    } else {
        raccolta::Ascolto::da_bind(
            &crate::project_workspace::port_recovery::probe_bind(riga.port as u16).await,
        )
    };
    raccolta::OsservazioniDelChiamante {
        eta,
        ascolto,
        impiego,
    }
}

/// Cio' che si e' potuto sapere dei processi del progetto.
///
/// Non e' un `Vec` eventualmente vuoto (regola Q): «nessun processo attivo» e
/// «non ho potuto leggerli» portano a decisioni opposte, e il secondo caso —
/// DB del progetto irraggiungibile — non deve autorizzare a cancellare la porta
/// di un servizio che sta partendo.
pub(crate) enum EsitoProcessi {
    Elenco(Vec<crate::agent_processes::ProcessSummary>),
    NonInterrogabili(String),
}

impl EsitoProcessi {
    /// La lettura come la fa la produzione.
    pub(crate) async fn letti(db: &sqlx::PgPool, project_id: uuid::Uuid) -> Self {
        match crate::agent_processes::list_processes(db, project_id).await {
            Ok(v) => Self::Elenco(v),
            Err(e) => Self::NonInterrogabili(e),
        }
    }
}

/// «Un servizio con questa label la sta usando adesso?», come lo sa questo
/// raccoglitore.
///
/// La label in avvio vince su tutto e non richiede alcuna lettura: e' il
/// servizio che stiamo facendo partire in questo istante, e la sua porta spenta
/// serve all'adozione (deadlock dell'allocazione stantia).
fn impiego_della_label(
    alloc_label: &str,
    preserve_label: &str,
    attive: Option<&std::collections::HashSet<String>>,
    processi: &EsitoProcessi,
) -> crate::project_workspace::raccolta_allocazione::ImpiegoDellaLabel {
    use crate::project_workspace::raccolta_allocazione::ImpiegoDellaLabel;
    if alloc_label == preserve_label {
        return ImpiegoDellaLabel::InUso;
    }
    match (attive, processi) {
        (Some(a), _) if a.contains(alloc_label) => ImpiegoDellaLabel::InUso,
        (Some(_), _) => ImpiegoDellaLabel::NessunProcesso,
        (None, EsitoProcessi::NonInterrogabili(causa)) => {
            ImpiegoDellaLabel::NonInterrogabile(causa.clone())
        }
        (None, EsitoProcessi::Elenco(_)) => ImpiegoDellaLabel::NessunProcesso,
    }
}

/// L'unica cancellazione di questo raccoglitore. Il motivo del verdetto entra
/// nel log: «rilasciata» senza il perche' non distingue un residuo da un errore
/// di giudizio, ed e' proprio la riga che il difetto del 18/08/2026 avrebbe
/// prodotto su una prenotazione valida.
async fn rilascia_allocazione_raccolta(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    port: i32,
    alloc_label: &str,
    verdetto: crate::project_workspace::raccolta_allocazione::VerdettoRaccolta,
) {
    let _ = sqlx::query(
        "DELETE FROM nexus_port_allocations \
         WHERE project_id = $1 AND port = $2 AND allocation_mode <> 'manual'",
    )
    .bind(project_id)
    .bind(port)
    .execute(db)
    .await;
    tracing::info!(
        port = port,
        label = %alloc_label,
        motivo = verdetto.motivo(),
        "cleanup avvio servizio: allocazione rilasciata"
    );
}

/// L'uuid di un processo, dal parametro che il contratto ha gia' letto come
/// stringa. PUNTO UNICO (regola L) dei tre handler che lo ricevono: il
/// messaggio di un id malformato era scritto tre volte, identico, e diceva
/// soltanto «non valido» senza dire dove prenderne uno buono.
///
/// RIMEDIABILE per costruzione: l'agente ha sbagliato a copiare un id, e il
/// testo nomina il tool che glielo restituisce (regola: dire «rimediabile»
/// senza dire come e' una promessa non mantenuta).
fn process_id_valido(grezzo: &str) -> Result<Uuid, RispostaTool> {
    Uuid::parse_str(grezzo).map_err(|_| {
        RispostaTool::fallito_rimediabile(format!(
            "[Errore: process_id '{grezzo}' non e' un identificatore valido. \
             Usa list_active_services per l'elenco dei processi con i loro id.]"
        ))
    })
}

/// Legge l'output di un servizio avviato con run_service. MIGRATO al contratto
/// d'ingresso e a `RispostaTool`.
///
/// DIVERGENZA CHIUSA: l'handler accettava l'assenza di `process_id` e ripiegava
/// sull'ultimo processo del progetto, mentre il catalogo lo dichiara — e da
/// sempre — OBBLIGATORIO. Un ripiego che il modello non poteva conoscere, e che
/// quindi non ha mai potuto invocare deliberatamente. La capacita' non si perde
/// togliendolo: `tail_service_logs` la offre e la DICHIARA nel proprio schema
/// («Se omesso, usa l'ultimo processo del progetto»), che e' il posto in cui un
/// comportamento del genere e' utile perche' e' promesso.
pub(super) async fn tool_read_service_output(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::ReadServiceOutputInput};

    let params = match ReadServiceOutputInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let process_id = match process_id_valido(&params.process_id) {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };
    match crate::agent_processes::read_process_output(&ctx.db, ctx.project_id, process_id, 4000)
        .await
    {
        Ok(info) => RispostaTool::riuscito(format_process_output(&info)),
        // Il processo puo' non esistere, oppure il DB puo' non rispondere: sono
        // due cause diverse che arrivano qui come lo stesso errore opaco, e
        // distinguerle leggendo il messaggio sarebbe la regola M al contrario.
        // Fra le due letture possibili, DelSistema manda a cercare un'altra
        // strada invece di far ripetere una chiamata che rifallira' identica.
        Err(e) => RispostaTool::fallito_di_sistema(format!("[Errore lettura output: {e}]")),
    }
}

/// Ferma un servizio avviato con run_service. MIGRATO al contratto e a
/// `RispostaTool`.
pub(super) async fn tool_stop_service(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::StopServiceInput};

    let params = match StopServiceInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let process_id = match process_id_valido(&params.process_id) {
        Ok(id) => id,
        Err(risposta) => return risposta,
    };
    match crate::agent_processes::stop_process(&ctx.db, ctx.project_id, process_id).await {
        Ok(msg) => {
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::ServiceStopped {
                    name: format!("process:{}", process_id),
                },
            );
            RispostaTool::riuscito(msg)
        }
        // `stop_process` VERIFICA l'esito (pid morto + porta libera) prima di
        // dichiarare fermo un servizio: se fallisce, il processo e' ancora vivo
        // e ritentare la stessa chiamata non lo cambia.
        Err(e) => RispostaTool::fallito_di_sistema(format!("[Errore stop servizio: {e}]")),
    }
}

/// MIGRATO a `RispostaTool`. Nessun contratto d'ingresso da leggere: il tool
/// non ha parametri, e il catalogo lo dichiara con `properties` vuote.
pub(super) async fn tool_build_project_image(ctx: &AgentToolContext) -> RispostaTool {
    use crate::sandbox::build_project_service_image;
    match build_project_service_image(ctx.project_id, &ctx.root_path, &ctx.root_path).await {
        Ok(tag) => RispostaTool::riuscito(format!(
            "Immagine Docker progetto buildata con successo: {tag}. \
             I servizi avviati con run_service useranno questa immagine."
        )),
        // La build dell'immagine dipende dal daemon Docker e dal Dockerfile del
        // progetto: nessuna delle due e' una correzione che l'agente possa fare
        // riformulando QUESTA chiamata, che non ha parametri.
        Err(e) => RispostaTool::fallito_di_sistema(format!("[Errore build immagine: {e}]")),
    }
}

/// Riavvia un servizio: ferma tutti i processi con la stessa label,
/// poi li riesegue con lo stesso comando. Attende output iniziale.
///
/// La premessa finale si compone DAI campi (regola Q): l'esito e la natura di
/// `tool_run_service` restano dove sono, e anteporre prosa non li puo' piu'
/// coprire. Prima serviva `prepend_preserving_failure` proprio perche' il
/// fallimento viveva in testa al testo — un campo travestito da prosa, che un
/// `format!` distratto cancellava. Col campo non c'e' piu' niente da
/// ricordarsi di preservare.
///
/// Una label che non corrisponde a nessun servizio e' RIMEDIABILE, e il testo
/// lo dimostra invece di limitarsi a dirlo: nomina i due tool con cui
/// correggerla o creare il servizio che non c'e'.
pub(super) async fn tool_service_restart(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::ServiceRestartInput};

    let params = match ServiceRestartInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    let label = params.label;

    // Cerca il processo esistente con questa label per recuperare il comando
    let existing = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return RispostaTool::fallito_di_sistema(format!("[Errore lista processi: {e}]")),
    };

    let matching: Vec<_> = existing.iter().filter(|p| p.label == label).collect();
    if matching.is_empty() {
        return RispostaTool::fallito_rimediabile(format!(
            "[Errore: nessun servizio trovato con label '{label}'. \
             Usa list_active_services per le label attive, o run_service per avviarlo.]"
        ));
    }

    // Recupera il comando originale dal processo piu' recente con questa label
    let original_command = matching[0].command.clone();

    // Leggi working_dir dal record completo via DB (punto unico estratto).
    let work_dir = restart_work_dir(ctx, matching[0].id).await;

    // Ferma tutti i processi attivi con questa label. A stop non verificato si
    // abortisce il restart (vedi `stop_matching_processes`).
    if let Err(msg) = stop_matching_processes(ctx, &matching, &label).await {
        return msg;
    }

    // Riavvia con lo stesso comando
    let restart_input = serde_json::json!({
        "command": original_command,
        "label": label,
        "working_dir": work_dir,
    });

    let result = tool_run_service(ctx, &restart_input, "service").await;
    RispostaTool {
        testo: format!("Servizio '{label}' riavviato.\n{}", result.testo),
        ..result
    }
}

/// Legge il `working_dir` di un processo dal DB del progetto, ricadendo su
/// `root_path` se assente o vuoto. agent_processes e' tabella migrata: instrada
/// sul pool del progetto corrente (separazione DB per-progetto).
async fn restart_work_dir(ctx: &AgentToolContext, process_id: Uuid) -> String {
    let fallback = || ctx.root_path.to_string_lossy().to_string();
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&ctx.db, ctx.project_id).await {
            Ok(p) => p,
            Err(e) => {
                // Stessa degradazione della query fallita qui sotto: si ricade
                // su root_path (fallback di business, non un altro DB).
                tracing::warn!(
                    project_id = %ctx.project_id,
                    error = %e,
                    "restart_work_dir: DB progetto non disponibile, uso root_path"
                );
                return fallback();
            }
        };
    let row = sqlx::query("SELECT working_dir FROM agent_processes WHERE id = $1")
        .bind(process_id)
        .fetch_optional(&proj_pool)
        .await;
    match row {
        Ok(Some(row)) => {
            let wd: String = row.try_get("working_dir").unwrap_or_default();
            if wd.is_empty() {
                fallback()
            } else {
                wd
            }
        }
        _ => fallback(),
    }
}

/// Ferma tutti i processi attivi (running/starting) fra i `matching`. stop_process
/// VERIFICA l'esito (PID morto + porta della label libera, con retry) prima di
/// marcare 'stopped': a verifica fallita ritorna `Err(msg)` per abortire il
/// restart invece di rilanciare un processo destinato a EADDRINUSE.
async fn stop_matching_processes(
    ctx: &AgentToolContext,
    matching: &[&crate::agent_processes::ProcessSummary],
    label: &str,
) -> Result<(), RispostaTool> {
    for proc in matching
        .iter()
        .filter(|p| p.status == "running" || p.status == "starting")
    {
        if let Err(e) = crate::agent_processes::stop_process(&ctx.db, ctx.project_id, proc.id).await
        {
            // Il processo vecchio e' ancora vivo e tiene la sua porta: e' la
            // ragione per cui il restart abortisce invece di rilanciare. Non e'
            // transitorio — riprovare subito troverebbe lo stesso processo — e
            // non e' rimediabile riformulando la chiamata, che ha un solo
            // parametro e quello era giusto.
            return Err(RispostaTool::fallito_di_sistema(format!(
                "[Errore restart '{label}': stop del processo esistente non verificato: {e}]"
            )));
        }
    }
    Ok(())
}

/// Legge le ultime N righe di output di un servizio, con opzione di attesa
/// per catturare output aggiuntivo (simula follow per X secondi).
pub(super) async fn tool_tail_service_logs(
    ctx: &AgentToolContext,
    input: &Value,
) -> RispostaTool {
    use nexus_agent_tools::{input_contract::InputTool, tool_inputs::TailServiceLogsInput};

    let params = match TailServiceLogsInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // I default restano quelli dell'handler: il contratto dichiara i campi
    // opzionali, non i valori che assumono quando mancano. `max(0)` prima del
    // cast toglie il caso che il tipo `i64` del catalogo ammette e che l'handler
    // a mano non vedeva: un negativo diventava un `usize` enorme passando da
    // `as`, e qui satura a zero invece di chiedere al DB tutto l'output.
    let max_chars = params.max_chars.unwrap_or(8000).max(0) as usize;
    let follow_secs = params.follow_seconds.unwrap_or(0).clamp(0, 60) as u64;

    // Risolvi process_id: specifico oppure ultimo del progetto (punto unico).
    let process_id = match resolve_process_id_or_last(ctx, params.process_id.as_deref()).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            // L'assenza E' la risposta, non un fallimento: il tool ha fatto cio'
            // che doveva. Stesso criterio con cui `list_files` tratta una
            // directory vuota come successo e una assente come errore.
            return RispostaTool::riuscito("Nessun servizio avviato per questo progetto.");
        }
        Err(risposta) => return risposta,
    };

    if follow_secs == 0 {
        return match crate::agent_processes::read_process_output(
            &ctx.db,
            ctx.project_id,
            process_id,
            max_chars,
        )
        .await
        {
            Ok(info) => RispostaTool::riuscito(format_process_output(&info)),
            Err(e) => RispostaTool::fallito_di_sistema(format!("[Errore lettura output: {e}]")),
        };
    }

    RispostaTool::riuscito(follow_service_logs(ctx, process_id, max_chars, follow_secs).await)
}

/// Risolve il process_id target: assente o vuoto = l'ultimo processo del
/// progetto, altrimenti lo parsa.
///
/// I tre casi sono TRE (regola Q): un id risolto, l'ASSENZA di processi — che
/// non e' un fallimento e per questo e' `Ok(None)` e non un `Err` —, e un
/// errore vero. Prima il secondo viaggiava come `Err` con un testo privo di
/// marker, cioe' un non-fallimento che percorreva il canale dei fallimenti e ne
/// usciva riuscito solo perche' qualcuno si era ricordato di non scrivere il
/// marker. Ora lo dice il tipo.
///
/// Resta un punto unico anche con un solo chiamante: `read_service_output` non
/// vi passa piu' perche' il suo `process_id` e' obbligatorio nel catalogo, ed
/// e' quella la divergenza chiusa — non l'esistenza di questa funzione.
async fn resolve_process_id_or_last(
    ctx: &AgentToolContext,
    process_id_str: Option<&str>,
) -> Result<Option<Uuid>, RispostaTool> {
    if let Some(grezzo) = process_id_str.filter(|s| !s.is_empty()) {
        return process_id_valido(grezzo).map(Some);
    }
    let rows = crate::agent_processes::list_processes(&ctx.db, ctx.project_id)
        .await
        .map_err(|e| RispostaTool::fallito_di_sistema(format!("[Errore: {e}]")))?;
    Ok(rows.first().map(|p| p.id))
}

/// Modalita' follow: polleggia l'output ogni 2 secondi per `follow_secs`,
/// accumulando le nuove porzioni di stdout/stderr fino alla scadenza o alla
/// terminazione del processo.
async fn follow_service_logs(
    ctx: &AgentToolContext,
    process_id: Uuid,
    max_chars: usize,
    follow_secs: u64,
) -> String {
    let mut combined_output = String::new();
    // (offset gia' consumato di stdout, offset gia' consumato di stderr).
    let mut seen = (0usize, 0usize);

    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < follow_secs {
        match crate::agent_processes::read_process_output(
            &ctx.db,
            ctx.project_id,
            process_id,
            max_chars,
        )
        .await
        {
            Ok(info) => {
                if append_new_output(&mut combined_output, &mut seen, &info) {
                    break; // processo terminato: footer gia' aggiunto
                }
            }
            Err(e) => {
                combined_output.push_str(&format!("\n[Errore lettura: {}]", e));
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    if combined_output.is_empty() {
        "(Nessun output durante il periodo di follow)".to_string()
    } else {
        combined_output
    }
}

/// Accoda a `combined` le porzioni nuove di stdout/stderr rispetto agli offset
/// gia' consumati in `seen` (stdout, stderr). Se il processo e' terminato
/// aggiunge il footer e ritorna `true` (il chiamante interrompe il follow).
fn append_new_output(
    combined: &mut String,
    seen: &mut (usize, usize),
    info: &crate::agent_processes::ProcessOutput,
) -> bool {
    if info.stdout.len() > seen.0 {
        combined.push_str(&info.stdout[seen.0..]);
        seen.0 = info.stdout.len();
    }
    if info.stderr.len() > seen.1 {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&format!("[STDERR] {}", &info.stderr[seen.1..]));
        seen.1 = info.stderr.len();
    }
    if info.status != "running" && info.status != "starting" {
        combined.push_str(&format!(
            "\n--- Processo terminato (status: {}, exit_code: {}) ---",
            info.status,
            info.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        ));
        return true;
    }
    false
}

/// Lista i servizi/processi registrati per il progetto corrente.
///
/// Migrato alla regola Q: l'esito sta nel campo di [`RispostaTool`] e il testo
/// e' solo testo — niente marker da anteporre e niente da rileggere a valle. La
/// resa la compone il punto unico [`super::service_listing`] DAI campi; qui si
/// raccolgono i fatti e basta.
pub(super) async fn tool_list_active_services(
    ctx: &AgentToolContext,
    _input: &Value,
) -> RispostaTool {
    let rows = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return RispostaTool::fallito(format!("elenco servizi non leggibile: {e}")),
    };
    // Le porte del progetto dalla cache gia' in contesto: nessuna query nuova, e
    // soprattutto nessuna seconda idea di "quali porte ha questo progetto"
    // (regola L). Cache indisponibile -> l'elenco esce senza porte, che e' un
    // dato in meno, non un dato inventato.
    let porte = ctx.port_registry.ports_for_project(&ctx.project_id).await;
    let elenco = super::service_listing::elenco_da_processi(
        &rows,
        &porte,
        chrono::Utc::now(),
        crate::agent_processes::LIMITE_ELENCO_PROCESSI,
    );
    RispostaTool::riuscito(elenco.testo())
}

#[cfg(test)]
mod tests {
    use super::{
        classifica_ascolto_altrove, declassa_se_non_e_un_servizio, derive_kind_hint,
        detect_port_from_output, existing_service_action, nota_ascolto_altrove,
        looks_like_web_service, registra_le_proprie_porte, resolve_service_label,
        resolve_service_work_dir, scope_dir, tool_run_service, AscoltoAltrove,
        ExistingServiceAction, PortaRilevata, Uuid, LABEL_NON_SERVIZIO,
    };
    use crate::agent_processes::{is_generic_service_label, similar_service_labels};
    use crate::project_workspace::services::{project_service_slug, service_unit_name};
    use nexus_tool_kit::ports::{project_bucket_range, PortRegistrability};
    use serde_json::json;
    use std::path::{Path, PathBuf};

    /// Radice di progetto REALE (le dir vengono create): `resolve_relative_path`
    /// canonicalizza e rifiuta i percorsi inesistenti, quindi un test su path
    /// inventati non attraverserebbe il produttore della working dir.
    fn progetto(nome: &str, sottocartelle: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join(nome);
        std::fs::create_dir_all(&root).expect("root");
        for sub in sottocartelle {
            std::fs::create_dir_all(root.join(sub)).expect("sub");
        }
        (tmp, root)
    }

    fn id(prefisso: &str) -> uuid::Uuid {
        uuid::Uuid::parse_str(&format!("{prefisso}-0000-0000-0000-000000000000")).expect("uuid")
    }

    /// Identita' come la produce il tool: dallo STESSO input JSON, passando per il
    /// produttore vero della working dir e per la STESSA traduzione
    /// identita' -> (kind, label) che usa `resolve_service_launch` (regola O:
    /// ricopiarla qui renderebbe verde un test che misura la propria copia).
    /// Ritorna anche l'unit, che e' la conseguenza a valle
    /// (`nexus_port_allocations.service_unit`).
    fn identita(
        nome_progetto: &str,
        root: &Path,
        input: &serde_json::Value,
        kind: &str,
    ) -> (String, String, String) {
        let command = input["command"].as_str().unwrap_or_default().to_string();
        let work_dir = resolve_service_work_dir(root, input["working_dir"].as_str())
            .expect("working dir risolta");
        // Lo slug lo passa il PRODUTTORE, come in produzione: e' lo stesso da
        // cui nasce l'unit sotto, quindi il test non puo' misurare uno slug
        // diverso da quello con cui il riconoscimento del derivato lavora.
        let slug = project_service_slug(nome_progetto);
        let (kind, label) = resolve_service_label(
            input,
            &command,
            kind,
            root,
            &work_dir,
            id("1a2b3c4d"),
            Some(slug.clone()),
        )
        .classifica(kind);
        let unit = service_unit_name(&slug, &label);
        (label, unit, kind)
    }

    /// IL CASO MISURATO: un server Node avviato col PERCORSO non era riconosciuto.
    ///
    /// Il 03/08/2026 su catalogo-libri il comando era `node src/backend/server.js`
    /// e `WEB_TOKENS` conteneva il token CONTIGUO "node server": fra runtime e
    /// nome del file c'e' il percorso, quindi `contains` era falso. Il servizio
    /// non risultava un web service, non riceveva porta ne' PORT, e il progetto
    /// ha chiuso con ZERO allocazioni e il backend fallito due volte.
    ///
    /// MUTAZIONE: togliere il ramo `RUNTIME_CON_SCRIPT` da
    /// `avvio_server::comando_avvia_server` fa rosseggiare la prima asserzione —
    /// il valore del difetto reale.
    #[test]
    fn un_server_node_col_percorso_e_un_web_service() {
        assert!(
            looks_like_web_service("node src/backend/server.js"),
            "il caso misurato: il percorso fra runtime e nome del file non deve nascondere il server"
        );
        assert!(looks_like_web_service("node dist/main.js"));
        assert!(looks_like_web_service("tsx api/server.ts"));
        assert!(looks_like_web_service("bun src/index.ts"));
        assert!(looks_like_web_service("nodemon backend/app.js"));
        // Il runtime puo' arrivare col proprio percorso.
        assert!(looks_like_web_service("/usr/bin/node dist/server.js"));
        // Il difetto ADIACENTE che questo test dichiarava non coperto — un server
        // sotto `build/` bocciato da `is_long_oneshot`, che cercava " build" come
        // substring — e' chiuso: il criterio non passa piu' da quella funzione,
        // perche' un criterio POSITIVO non ha bisogno di un veto che elenchi cio'
        // che server non e'.
        assert!(
            looks_like_web_service("node build/server.js"),
            "la cartella in cui vive lo script non dice cosa lo script fa"
        );
        assert!(looks_like_web_service("npm run dev"));
        assert!(looks_like_web_service("node server.js"));
    }

    /// I comandi MISURATI il 04/08/2026 su prenotazioni-sala, che il criterio
    /// della sola porta lasciava passare come servizi.
    ///
    /// Non e' una ripetizione dei test di `avvio_server`: quelli verificano la
    /// RISPOSTA, questo verifica la CONSEGUENZA — che un `kind='service'`
    /// chiesto per quei comandi diventi `task`, e con esso perda il potere di
    /// fermare i servizi vivi.
    ///
    /// MUTAZIONE: rimettere `is_long_oneshot(command) &&` nella condizione di
    /// `declassa_se_non_e_un_servizio` fa rosseggiare le prime due asserzioni —
    /// ne' `npm init` ne' `cargo clippy` sono in quell'elenco.
    #[test]
    fn cio_che_non_avvia_un_server_non_resta_un_servizio() {
        assert_eq!(
            declassa_se_non_e_un_servizio("service", "cd backend && npm init -y"),
            "task"
        );
        assert_eq!(
            declassa_se_non_e_un_servizio("service", "cd backend && cargo clippy --all-targets"),
            "task"
        );
        assert_eq!(
            declassa_se_non_e_un_servizio("service", "npx prisma init --datasource-provider postgresql"),
            "task"
        );
        assert_eq!(
            declassa_se_non_e_un_servizio("service", r#"grep -r "VITE_API_URL" frontend/"#),
            "task"
        );
        // I servizi veri restano servizi: declassarli e' il danno peggiore
        // (niente porta, niente unit, invisibili nel pannello).
        assert_eq!(declassa_se_non_e_un_servizio("service", "npm run dev"), "service");
        assert_eq!(
            declassa_se_non_e_un_servizio("service", "cd backend && npm run start"),
            "service"
        );
        assert_eq!(
            declassa_se_non_e_un_servizio("service", "vite --port 24804 --host 0.0.0.0"),
            "service"
        );
        assert_eq!(
            declassa_se_non_e_un_servizio("service", "node src/backend/server.js"),
            "service"
        );
        // Un task resta task: la funzione non promuove mai.
        assert_eq!(declassa_se_non_e_un_servizio("task", "npm run dev"), "task");
    }

    /// Il criterio non allarga a cio' che server non e': uno script con un altro
    /// nome, o un one-shot, restano fuori. Iniettare PORT a un processo che non
    /// ascolta non fa danno diretto, ma gli alloca una porta del bucket che
    /// nessuno usera' — ed e' il genere di riga fantasma che il GC deve poi
    /// rilasciare.
    #[test]
    fn cio_che_non_e_un_server_resta_fuori() {
        assert!(!looks_like_web_service("node scripts/seed.js"));
        assert!(!looks_like_web_service("node tools/migrate.js"));
        assert!(!looks_like_web_service("npx eslint src --max-warnings=0"));
        assert!(!looks_like_web_service("npm run build"));
        // Un runtime senza script non avvia niente.
        assert!(!looks_like_web_service("node"));
        // Estensione che non e' di uno script: nessuno avvia un .txt. Col
        // vocabolario a sottostringhe questa riga era un falso positivo,
        // catturata dal token contiguo "node server".
        assert!(!looks_like_web_service("node server.txt"));
    }

    /// LA CATENA VERA, riprodotta: il valore che il registro consegnava
    /// all'agente rientra come identita' e il ciclo si autoalimenta.
    ///
    /// Misurato su DUE progetti indipendenti (agenda-corsi 02/08 22:35, 76
    /// secondi dopo aver letto `nexus_list_ports`; bacheca-attivita 30/07
    /// 05:36, 47 secondi dopo): il modello copia `service_unit` dentro `label`,
    /// `service_unit_name` antepone di nuovo lo slug, e l'unit successiva se lo
    /// ripete dentro. Sul parco misurato, 10 righe su 26 avevano per label un
    /// nome gia' derivato, fino alla TERZA generazione.
    ///
    /// Il test parte dal PRODUTTORE (`service_unit_name`), non da una stringa
    /// scritta a mano: e' esattamente il valore che l'agente aveva in mano
    /// (regola O). Se un domani il formato dell'unit cambia, questo test lo
    /// segue invece di misurare una forma fossile.
    ///
    /// MUTAZIONE: togliere il filtro `e_nome_unit_derivato` da
    /// `resolve_service_label` fa passare la label derivata e l'unit diventa
    /// `agenda-corsi-agenda-corsi-Backend API (Express).service.service`, che e'
    /// la riga misurata in `nexus_port_allocations`.
    #[test]
    fn il_nome_unit_riproposto_come_label_non_diventa_identita() {
        let (_tmp, root) = progetto("agenda-corsi", &["backend"]);
        let slug = project_service_slug("agenda-corsi");

        // Il giro 1: l'identita' vera, e cio' che il registro ne derivava.
        let unit_del_giro_1 = service_unit_name(&slug, "Backend API (Express)");
        assert_eq!(unit_del_giro_1, "agenda-corsi-Backend API (Express).service");

        // Il giro 2: l'agente ripropone quel valore come label.
        let input = json!({
            "command": "npm run dev",
            "working_dir": "backend",
            "label": unit_del_giro_1,
        });
        let (label, unit, kind) = identita("agenda-corsi", &root, &input, "service");

        assert_ne!(
            label, unit_del_giro_1,
            "un nome unit non e' un'identita' nuova: e' il derivato di una precedente"
        );
        assert_eq!(
            label, "backend",
            "scartata la label derivata, l'identita' la dicono i fatti (comando + cartella)"
        );
        assert_eq!(unit, "agenda-corsi-backend.service", "niente slug ripetuto");
        assert_eq!(kind, "service");
    }

    /// L'altra traccia del derivato: la label che RIPETE lo slug, senza il
    /// suffisso `.service`. E' la forma di seconda generazione misurata su
    /// gestione-spese (`gestione-spese-gestione-spese-backend`).
    #[test]
    fn la_label_che_ripete_il_progetto_non_diventa_identita() {
        let (_tmp, root) = progetto("gestione-spese", &["backend"]);
        let input = json!({
            "command": "npm start",
            "working_dir": "backend",
            "label": "gestione-spese-backend",
        });
        let (label, unit, _) = identita("Gestione Spese", &root, &input, "service");
        assert_eq!(label, "backend");
        assert_eq!(unit, "gestione-spese-backend.service");
    }

    /// Il criterio non e' un divieto sul TESTO: una label legittima che contenga
    /// per caso la parola del progetto in mezzo, o un ruolo dal nome lungo, deve
    /// passare. Si rifiuta solo cio' che il produttore emette DAVVERO: suffisso
    /// `.service`, o prefisso `{slug}-`.
    #[test]
    fn una_label_legittima_non_viene_scartata() {
        let (_tmp, root) = progetto("agenda-corsi", &["backend"]);
        let input = json!({
            "command": "npm run dev",
            "working_dir": "backend",
            "label": "api-agenda-corsi-v2",
        });
        let (label, _, _) = identita("agenda-corsi", &root, &input, "service");
        assert_eq!(
            label, "api-agenda-corsi-v2",
            "il progetto nominato in MEZZO non fa di una label un valore derivato"
        );
    }

    /// REGRESSIONE (caso reale, progetto gestione-spese): il backend girava con
    /// `npm start` da `backend/` e finiva etichettato "Service". Da li' in poi non
    /// poteva piu' avere un indirizzo: l'identita' del servizio e'
    /// `{slug}-{label}.service` e deve combaciare con
    /// `nexus_port_allocations.service_unit`, ma una label generica non incontra
    /// mai la propria allocazione (`similar_service_labels("Service","backend")`
    /// e' falso per costruzione). Il segnale c'era ed era a portata di mano: la
    /// cartella da cui il comando gira.
    #[test]
    fn npm_start_in_backend_prende_l_identita_che_l_allocazione_usa() {
        let (_tmp, root) = progetto("gestione-spese", &["backend"]);
        let input = json!({ "command": "npm start", "working_dir": "backend" });

        let (label, unit, _) = identita("Gestione Spese", &root, &input, "service");

        assert_eq!(label, "backend", "la working dir dice il ruolo che il comando tace");
        // La conseguenza: e' l'unit che il pannello ricostruisce dalla label del
        // processo e a cui `link_allocation_to_service_unit` lega la porta.
        assert_eq!(unit, "gestione-spese-backend.service");
        // E il servizio incontra la propria allocazione, che con "Service" non
        // poteva accadere.
        assert!(similar_service_labels(&label, "backend"));
        assert!(!is_generic_service_label(&label));
    }

    /// Il difetto era il RIPIEGO, non solo la working dir: `unwrap_or("Service")`
    /// produceva esattamente cio' che il filtro sulla riga sopra rifiuta
    /// all'agente. Se non c'e' nessun segnale di ruolo, l'identita' si ancora al
    /// percorso, mai a una parola che il sistema stesso considera vuota.
    #[test]
    fn senza_segnali_di_ruolo_l_identita_viene_dal_percorso_mai_generica() {
        // Comando muto, cartella che non dice un ruolo ma ha un nome proprio.
        let (_tmp, root) = progetto("gestione-spese", &["services/pagamenti"]);
        let input = json!({ "command": "npm start", "working_dir": "services/pagamenti" });
        let (label, _, _) = identita("Gestione Spese", &root, &input, "service");
        assert_eq!(label, "pagamenti");
        assert!(!is_generic_service_label(&label));
    }

    /// REGRESSIONE (misurata il 30/07/2026, progetto bacheca-attivita): il
    /// pannello Servizi elencava `bacheca-attivita-bacheca-attivita.service`, che
    /// non identifica un ruolo — ripete il progetto due volte. Nasceva dal
    /// ripiego sul nome della cartella di progetto, e cio' che aveva registrato
    /// era `npx -w backend eslint . --ext .ts` lanciato dalla radice: un lint,
    /// non un servizio.
    ///
    /// Due asserzioni distinte, perche' sono due fatti distinti: lo slug non e'
    /// un'identita', e cio' che non ha identita' non e' un servizio di progetto.
    #[test]
    fn dalla_radice_senza_ruolo_non_nasce_un_servizio_che_ripete_il_progetto() {
        let (_tmp, root) = progetto("bacheca-attivita", &["backend"]);
        let input = json!({ "command": "npx -w backend eslint . --ext .ts" });

        let (label, unit, kind) = identita("bacheca-attivita", &root, &input, "service");

        assert_eq!(
            kind, "task",
            "un comando senza ruolo e senza bisogno di porta non e' un servizio: {label}"
        );
        assert_ne!(label, "bacheca-attivita", "il nome del progetto non e' un ruolo");
        assert_ne!(
            unit, "bacheca-attivita-bacheca-attivita.service",
            "l'unit fantasma vista nel pannello"
        );
        // Declassato a task, il pannello lo nasconde appena muore: la label e'
        // generica per costruzione (`visible_windows_services`).
        assert!(is_generic_service_label(&label));
    }

    /// Un servizio VERO che gira dalla radice (progetto mono-servizio) ha bisogno
    /// di una porta, quindi di un nome stabile fra i riavvii per riceverla e
    /// conservarla. Lo distingue il comando, non il percorso: `npm start` avvia un
    /// server, `npx eslint` no. Il nome di ripiego si ancora al progetto per
    /// COSTRUZIONE (l'uuid), mai per NOME: brutto da leggere, ma dichiara di
    /// essere un ripiego invece di sembrare una scelta.
    #[test]
    fn il_servizio_web_dalla_radice_riceve_un_ancoraggio_non_il_nome_del_progetto() {
        let (_tmp, root) = progetto("gestione-spese", &[]);
        let input = json!({ "command": "npm start" });

        let (label, unit, kind) = identita("Gestione Spese", &root, &input, "service");

        assert_eq!(kind, "service", "un server resta un servizio");
        assert_eq!(label, "service-1a2b3c4d");
        assert_eq!(unit, "gestione-spese-service-1a2b3c4d.service");
        assert!(
            !is_generic_service_label(&label),
            "un'identita' che il sistema considera vuota non riceve porte: {label}"
        );
    }

    /// Caso limite dichiarato: nemmeno la cartella di lavoro ha una parola propria
    /// (`app/`). Stessa risposta del caso sopra — il vaglio sulle cartelle e
    /// quello sul comando sono due domande separate, e la seconda decide da sola.
    #[test]
    fn cartella_senza_nome_proprio_ancora_l_identita_al_progetto() {
        let (_tmp, root) = progetto("gestione-spese", &["app"]);
        let input = json!({ "command": "npm start", "working_dir": "app" });
        let (label, _, kind) = identita("Gestione Spese", &root, &input, "service");
        assert_eq!(kind, "service");
        assert_eq!(label, "service-1a2b3c4d", "identita' ancorata al progetto");
        assert!(!is_generic_service_label(&label));
    }

    /// La label esplicita dell'agente e' il segnale piu' specifico e vince; ma se
    /// e' generica non e' un'identita' e non deve sopravvivere al filtro.
    #[test]
    fn label_esplicita_vince_se_dice_qualcosa() {
        let (_tmp, root) = progetto("gestione-spese", &["backend"]);

        let input = json!({ "command": "npm start", "working_dir": "backend", "label": "Checkout API" });
        let (label, _, _) = identita("Gestione Spese", &root, &input, "service");
        assert_eq!(label, "Checkout API");

        // "Service" proposta dall'agente: scartata come quando la sceglieva il
        // sistema, si ricade sul segnale della working dir.
        let input = json!({ "command": "npm start", "working_dir": "backend", "label": "Service" });
        let (label, _, _) = identita("Gestione Spese", &root, &input, "service");
        assert_eq!(label, "backend");
    }

    /// Un task one-shot NON e' un servizio: nessun unit, nessuna allocazione. Dargli
    /// l'identita' dedotta dallo scopo lo farebbe collidere col servizio omonimo,
    /// perche' `stop_similar_running_services` gira per ogni kind: un
    /// `npm install` lanciato in backend/ fermerebbe il backend vero.
    #[test]
    fn il_task_one_shot_non_eredita_l_identita_del_servizio() {
        let (_tmp, root) = progetto("gestione-spese", &["backend"]);
        let input = json!({ "command": "npm install", "working_dir": "backend" });
        let (label, _, kind) = identita("Gestione Spese", &root, &input, "task");
        assert_eq!(label, "Service");
        assert_eq!(kind, "task");
        assert!(
            !similar_service_labels(&label, "backend"),
            "un task non deve deduplicare il servizio backend"
        );
    }

    /// Lo scopo dalla working dir e' una convenzione di LAYOUT, non un elenco di
    /// comandi: vale per `npm start` come per `pnpm serve`. Il nome della cartella
    /// di progetto invece non e' un segnale di ruolo, altrimenti in un progetto
    /// `shop-frontend` anche il backend risulterebbe frontend.
    #[test]
    fn lo_scopo_viene_dalle_cartelle_sotto_la_radice() {
        let root = Path::new("/progetti/shop-frontend");
        let rel = |cmd: &str, p: &str| scope_dir(root, &root.join(p), cmd);

        assert_eq!(
            derive_kind_hint("pnpm serve", "", rel("pnpm serve", "apps/web").as_deref()),
            Some("frontend")
        );
        assert_eq!(
            derive_kind_hint("yarn start", "", rel("yarn start", "services/api").as_deref()),
            Some("backend")
        );
        // Working dir sulla radice: nessun ruolo, il nome del progetto non conta.
        assert_eq!(scope_dir(root, root, "npm start"), None);
        assert_eq!(derive_kind_hint("npm start", "", None), None);
        // Il comando resta il segnale piu' specifico quando parla.
        assert_eq!(
            derive_kind_hint("vite --host", "", rel("vite --host", "services/api").as_deref()),
            Some("frontend")
        );
    }

    /// REGRESSIONE (misurata il 30-31/07/2026, progetto bacheca-attivita): il
    /// pannello Servizi mostrava TRE voci — backend, frontend e un
    /// `service-66f4bf72` — per un'app che ne ha due. Nasceva dai due comandi
    /// lanciati DALLA RADICE col `cd` dentro di se': `scope_dir` era vuoto, `npm
    /// run dev`/`npm start` non nominano nessuna tecnologia, quindi nessun segnale
    /// diceva il ruolo e l'identita' ripiegava sull'ancoraggio all'uuid — lo stesso
    /// per entrambi, che e' il motivo per cui le due righe collassavano in una
    /// terza voce sola.
    ///
    /// Il segnale c'era, scritto in un altro posto: `cd frontend &&` non e' un nome
    /// da cui indovinare, e' la dichiarazione della cartella in cui il comando
    /// gira — `spawn_agent_process` lo esegue con `bash -c` a partire dalla working
    /// dir, quindi quel `cd` sposta davvero il servizio.
    ///
    /// Mutazione che rende rosso: far ritornare `None` a `cd_dichiarato` (cioe'
    /// tornare a leggere la sola `working_dir`) -> entrambe le label tornano
    /// `service-1a2b3c4d`, il valore reale del difetto, e l'unit torna a essere la
    /// voce fantasma vista nel pannello.
    #[test]
    fn il_cd_in_testa_al_comando_dichiara_il_ruolo_come_la_working_dir() {
        let (_tmp, root) = progetto("bacheca-attivita", &["frontend", "backend"]);

        let input = json!({ "command": "cd frontend && npm run dev" });
        let (label, unit, kind) = identita("bacheca-attivita", &root, &input, "service");
        assert_eq!(label, "frontend", "il cd dichiara la cartella, la cartella il ruolo");
        assert_eq!(unit, "bacheca-attivita-frontend.service");
        assert_eq!(kind, "service");

        let input = json!({ "command": "cd backend && npm start" });
        let (label, unit, _) = identita("bacheca-attivita", &root, &input, "service");
        assert_eq!(label, "backend");
        assert_eq!(unit, "bacheca-attivita-backend.service");
        // La conseguenza per cui il difetto era visibile: due comandi diversi non
        // condividono piu' un'unica identita' di ripiego.
        assert!(!is_generic_service_label(&label));
    }

    /// Le due scritture della stessa cosa devono dare la stessa identita': un
    /// servizio non puo' chiamarsi in due modi a seconda di dove l'agente ha
    /// scritto la sua cartella. Il `cd` si compone con la working dir, perche' e'
    /// relativo a quella (`bash -c` parte da li').
    #[test]
    fn cd_nel_comando_e_working_dir_dicono_lo_stesso_scope() {
        let (_tmp, root) = progetto("shop", &["apps/web"]);

        let dal_parametro = json!({ "command": "npm run dev", "working_dir": "apps/web" });
        let dal_comando = json!({ "command": "cd apps/web && npm run dev" });
        let composto = json!({ "command": "cd web && npm run dev", "working_dir": "apps" });

        let (atteso, _, _) = identita("shop", &root, &dal_parametro, "service");
        assert_eq!(atteso, "frontend");
        assert_eq!(identita("shop", &root, &dal_comando, "service").0, atteso);
        assert_eq!(identita("shop", &root, &composto, "service").0, atteso);
    }

    /// Un `cd` verso l'esterno non e' uno scope di questo progetto (regola E), ma
    /// non deve nemmeno cancellare cio' che la working dir gia' diceva: si ignora
    /// la dichiarazione e resta il comportamento di prima. La cartella esiste
    /// davvero, cosi' il test misura il CONFINE e non l'inesistenza del percorso.
    ///
    /// Questo e' l'unico dei test sul `cd` che resta verde disattivando la lettura
    /// del comando, ed e' voluto: misura cio' che il fix NON deve cambiare. La
    /// mutazione che lo rende rosso e' l'opposta — togliere da
    /// `directory_effettiva` il vaglio `relativa_alla_radice(...).is_some()` ->
    /// l'identita' diventa `altro-progetto`, cioe' il servizio prende il nome di
    /// una cartella che non appartiene a questo progetto.
    #[test]
    fn un_cd_fuori_dalla_radice_non_cambia_l_identita() {
        let (tmp, root) = progetto("bacheca-attivita", &["backend"]);
        std::fs::create_dir_all(tmp.path().join("altro-progetto")).expect("cartella esterna");

        let input = json!({
            "command": "cd ../../altro-progetto && npm start",
            "working_dir": "backend",
        });
        let (label, _, _) = identita("bacheca-attivita", &root, &input, "service");
        assert_eq!(
            label, "backend",
            "fuori dalla radice non c'e' nessuno scope da leggere, e la working dir parla ancora"
        );
    }

    /// Il `cd` puo' anche portare il servizio SULLA radice, e allora la risposta e'
    /// che il ruolo non c'e': la stessa che si da' a chi parte dalla radice senza
    /// muoversi. Il nome della cartella di progetto non e' un candidato, e non deve
    /// rientrare dalla porta di servizio che questo fix apre.
    #[test]
    fn un_cd_verso_la_radice_non_produce_il_nome_del_progetto() {
        let (_tmp, root) = progetto("bacheca-attivita", &["backend"]);
        let input = json!({ "command": "cd .. && npm start", "working_dir": "backend" });

        let (label, unit, kind) = identita("bacheca-attivita", &root, &input, "service");

        assert_eq!(kind, "service", "resta un server, e gli servira' una porta");
        assert_eq!(label, "service-1a2b3c4d", "nessun ruolo: ancoraggio all'uuid");
        assert_ne!(unit, "bacheca-attivita-bacheca-attivita.service");
    }

    /// Cosa NON e' una dichiarazione di working directory. Nessuno di questi casi
    /// e' un elenco di forme da rifiutare: passano tutti dallo stesso vaglio
    /// (nomina una cartella reale dentro la radice?), e cio' che non lo supera
    /// lascia il comportamento identico a prima del fix.
    #[test]
    fn solo_un_cd_verso_una_cartella_vera_sposta_lo_scope() {
        let (_tmp, root) = progetto("shop", &["backend"]);
        let scope = |cmd: &str| scope_dir(&root, &root, cmd);

        assert_eq!(scope("cd backend && npm start").as_deref(), Some(Path::new("backend")));
        // Cartella inesistente: non c'e' niente da leggere.
        assert_eq!(scope("cd frontend && npm start"), None);
        // Non e' un `cd`, per quanto cominci per "cd".
        assert_eq!(scope("cdk deploy backend"), None);
        // `cd` nudo porta alla home, che non e' uno scope del progetto.
        assert_eq!(scope("cd && npm start"), None);
        // Il `cd` va letto in TESTA: piu' avanti descrive cosa fa il servizio, non
        // da dove parte.
        assert_eq!(scope("npm start && cd backend"), None);
        // Un percorso assoluto esterno resta escluso dal confine sulla radice.
        assert_eq!(scope("cd /usr/lib && npm start"), None);
        // Apici attorno all'intero argomento: il percorso e' quello che resta.
        assert_eq!(scope("cd 'backend' && npm start").as_deref(), Some(Path::new("backend")));
    }

    fn uscita_processo(status: &str, stdout: &str, stderr: &str) -> crate::agent_processes::ProcessOutput {
        crate::agent_processes::ProcessOutput {
            command: "npm run dev".into(),
            pid: Some(17488),
            status: status.into(),
            exit_code: None,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// Il messaggio come lo compone la PRODUZIONE: fatti -> criterio -> testo,
    /// passando da `classifica_avvio` e `AvvioServizio::risposta` invece di
    /// fabbricare la variante desiderata (regola O). Prima questi test
    /// chiamavano `format_started_message` con un `Option<(u16, bool)>`, un
    /// tipo in cui la morte del processo non era nemmeno esprimibile.
    fn messaggio_di_avvio(
        label: &str,
        info: &crate::agent_processes::ProcessOutput,
        porta: super::super::avvio_servizio::EsitoPorta,
        altrove: &AscoltoAltrove,
    ) -> nexus_types::tool_outcome::RispostaTool {
        use super::super::avvio_servizio::{classifica_avvio, FattiAvvio, UscitaCapostipite};
        let uscita = if info.exit_code.is_some() || matches!(info.status.as_str(), "stopped" | "failed") {
            UscitaCapostipite::Uscito {
                exit_code: info.exit_code,
            }
        } else {
            UscitaCapostipite::NonUscito
        };
        let mut output = info.stdout.trim().to_string();
        if !info.stderr.trim().is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(info.stderr.trim());
        }
        let nota = nota_ascolto_altrove(altrove);
        let nota = (!nota.is_empty()).then_some(nota);
        classifica_avvio(&FattiAvvio { uscita, porta }, 20_000, output).risposta(
            &format!("Servizio '{label}' (process_id: {})", uuid::Uuid::nil()),
            nota.as_deref(),
        )
    }

    /// REGRESSIONE (2026-07-26): il tool riportava "Servizio avviato" guardando
    /// solo se il PROCESSO esisteva. `nodemon` sopravvive al crash
    /// dell'applicazione ("app crashed - waiting for file changes"), quindi lo
    /// stato restava `running` con la porta chiusa: l'agente riceveva una
    /// conferma falsa e proseguiva a costruire su un servizio inesistente,
    /// mentre la causa gli stava gia' nello stderr.
    #[test]
    fn senza_ascolto_il_messaggio_dichiara_il_fallimento_e_mostra_la_causa() {
        let info = uscita_processo(
            "running",
            "[nodemon] app crashed - waiting for file changes",
            "Error: Cannot find module 'D:\\progetto\\backend\\src\\index.js'",
        );
        let msg = messaggio_di_avvio(
            "backend",
            &info,
            super::super::avvio_servizio::EsitoPorta::Muta { porta: 32976 },
            &AscoltoAltrove::NonAccertato,
        );
        // Il fallimento va DICHIARATO alla macchina, non solo raccontato:
        // l'anti-loop che legge un esito riuscito chiude il run come stallo
        // invece di instradarlo a diagnosi (regola M). Ora e' un CAMPO, e la
        // differenza non e' cosmetica: prima l'asserzione era sul marker in
        // testa alla stringa, quindi passava anche se qualcuno a valle
        // anteponeva prosa e lo spostava — il test misurava la composizione di
        // QUESTA funzione, non cio' che il chiamante consegna.
        assert_eq!(
            msg.esito,
            nexus_types::tool_outcome::EsitoTool::Fallito,
            "un servizio che non ascolta e' un fallimento dichiarato: {msg:?}"
        );
        // La NATURA dice all'agente cosa fare, ed e' la meta' che prima non
        // esisteva: qui «ritentare dopo aver corretto» invece di un errore
        // indistinto su cui sceglieva a caso.
        assert_eq!(
            msg.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Transitorio),
            "{msg:?}"
        );
        assert!(
            msg.testo.contains("NESSUN ASCOLTO"),
            "un servizio che non ascolta non va annunciato come avviato: {msg:?}"
        );
        assert!(msg.testo.contains("32976"), "il messaggio deve nominare la porta attesa: {msg:?}");
        assert!(
            msg.testo.contains("Cannot find module"),
            "lo stderr contiene la causa e deve raggiungere l'agente: {msg:?}"
        );
        // Il processo VIVO non deve mai diventare la prova che il servizio sia su.
        assert!(
            !msg.testo.contains("Servizio 'backend' avviato"),
            "status 'running' non basta a dichiarare l'avvio: {msg:?}"
        );
    }

    #[test]
    fn con_ascolto_verificato_il_messaggio_lo_dichiara() {
        let info = uscita_processo("running", "server listening", "");
        let msg = messaggio_di_avvio(
            "backend",
            &info,
            super::super::avvio_servizio::EsitoPorta::Risponde { porta: 32976 },
            &AscoltoAltrove::NonAccertato,
        );
        assert!(
            !msg.esito.e_fallito(),
            "un servizio che ascolta non e' un fallimento: {msg:?}"
        );
        assert!(
            msg.testo.contains("VIVO: in ascolto sulla porta 32976"),
            "l'ascolto verificato va dichiarato, cosi' l'agente distingue una \
             conferma provata da una presunta: {msg:?}"
        );
    }

    // ── Contratto d'ingresso: cio' che il catalogo promette, l'handler lo
    //    pretende (e nient'altro) ───────────────────────────────────────────

    /// LA DIVERGENZA CHIUSA. Il catalogo dichiara `process_id` OBBLIGATORIO per
    /// `read_service_output` da sempre; l'handler leggeva il campo a mano con
    /// `unwrap_or("")` e, trovandolo assente, ripiegava sull'ultimo processo del
    /// progetto — un comportamento che il modello non poteva conoscere, quindi
    /// non poteva nemmeno invocare deliberatamente.
    ///
    /// Il test attraversa il CONTRATTO REALE, non una sua imitazione: la stessa
    /// `leggi` che l'handler chiama (regola O). Costruire qui un errore serde a
    /// mano proverebbe soltanto che serde funziona.
    ///
    /// MUTAZIONE ESEGUITA (08/08/2026): spostare `process_id` fra gli
    /// `opzionali` di `ReadServiceOutputInput`. Il test NON rosseggia — non ci
    /// arriva: il campo diventa `Option<String>`, l'handler lo passa a
    /// `process_id_valido` che vuole `&str`, e la compilazione si ferma con
    /// `E0308`. E' l'esito piu' forte del rosso, e va detto per quello che e':
    /// dopo il contratto tipizzato quella divergenza non e' piu'
    /// RAPPRESENTABILE, mentre con `input.get("process_id").unwrap_or("")`
    /// entrambe le strade compilavano ed era questa la ragione per cui nessun
    /// test poteva vederla. Il test resta a coprire cio' che il compilatore non
    /// vede: che il messaggio nomini il campo e dichiari la natura giusta.
    #[test]
    fn read_service_output_pretende_il_process_id_che_il_catalogo_promette() {
        use nexus_agent_tools::input_contract::InputTool;
        use nexus_agent_tools::tool_inputs::ReadServiceOutputInput;

        let mancante = ReadServiceOutputInput::leggi(&json!({}))
            .expect_err("il catalogo lo dichiara obbligatorio: l'handler deve pretenderlo");
        assert_eq!(
            mancante.esito,
            nexus_types::tool_outcome::EsitoTool::Fallito,
            "{mancante:?}"
        );
        assert_eq!(
            mancante.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "un parametro mancante lo corregge l'agente: {mancante:?}"
        );
        assert!(
            mancante.testo.contains("process_id"),
            "il messaggio deve nominare il campo che manca: {mancante:?}"
        );

        // La capacita' NON e' sparita: vive dove e' PROMESSA. Il gemello la
        // dichiara opzionale, e li' l'assenza continua a significare «l'ultimo».
        nexus_agent_tools::tool_inputs::TailServiceLogsInput::leggi(&json!({}))
            .expect("tail_service_logs dichiara process_id opzionale, e lo accetta assente");
    }

    /// Il catalogo non promette alias, e con il contratto non se ne possono piu'
    /// accettare in silenzio: `deny_unknown_fields` e' la meta' che nessuno
    /// doveva ricordarsi di scrivere.
    ///
    /// MUTAZIONE: togliere `deny_unknown_fields` da `tool_object!` e questo
    /// diventa verde su un campo che il modello non ha mai visto nel catalogo.
    #[test]
    fn run_service_non_accetta_campi_che_il_catalogo_non_dichiara() {
        use nexus_agent_tools::input_contract::InputTool;
        use nexus_agent_tools::tool_inputs::RunServiceInput;

        RunServiceInput::leggi(&json!({"command": "npm run dev", "cwd": "frontend"}))
            .expect_err("'cwd' non e' nel catalogo: il campo giusto e' 'working_dir'");
        RunServiceInput::leggi(&json!({"command": "npm run dev", "working_dir": "frontend"}))
            .expect("i campi dichiarati passano");
    }

    /// Il messaggio di parametri invalidi nomina il tool INVOCATO.
    ///
    /// `run_in_terminal` e `run_service` condividono handler e contratto, ma
    /// sono due nomi: col nome dichiarato nella macro, un errore su
    /// `run_in_terminal` rimanderebbe l'agente allo schema di un tool che non ha
    /// chiamato — la stessa ragione per cui `nome_del_tool` esiste.
    ///
    /// MUTAZIONE: far ignorare a `leggi_come` il parametro `tool` e usare il
    /// nome della macro; la seconda assert rosseggia.
    #[test]
    fn l_errore_di_parametri_nomina_il_tool_invocato() {
        use nexus_agent_tools::input_contract::InputTool;
        use nexus_agent_tools::tool_inputs::RunServiceInput;

        let e = RunServiceInput::leggi(&json!({})).expect_err("command e' obbligatorio");
        assert!(e.testo.contains("run_service"), "{e:?}");

        let e = RunServiceInput::leggi_come(super::nome_del_tool("task"), &json!({}))
            .expect_err("command e' obbligatorio");
        assert!(
            e.testo.contains("run_in_terminal"),
            "l'agente ha invocato run_in_terminal e li' deve tornare: {e:?}"
        );
    }

    /// Un percorso che non si risolve e' RIMEDIABILE: e' un parametro che
    /// l'agente controlla, e il resolver dice gia' cosa non va.
    #[test]
    fn una_working_dir_inesistente_e_rimediabile() {
        let (_tmp, root) = progetto("app", &[]);
        let e = resolve_service_work_dir(&root, Some("non-esiste"))
            .expect_err("il resolver rifiuta i percorsi inesistenti");
        assert_eq!(
            e.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "{e:?}"
        );
        // Assente e vuota sono la stessa cosa, e sono la radice: nessun
        // fallimento da dichiarare.
        assert_eq!(
            resolve_service_work_dir(&root, None).expect("assente = radice"),
            resolve_service_work_dir(&root, Some("")).expect("vuota = radice")
        );
    }

    /// Un id malformato manda l'agente dove trovarne uno buono, invece di dirgli
    /// soltanto «non valido» — che e' cio' che i tre handler ripetevano identico.
    #[test]
    fn un_process_id_malformato_dice_dove_prenderne_uno_valido() {
        let e = super::process_id_valido("non-un-uuid").expect_err("id malformato");
        assert_eq!(
            e.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "{e:?}"
        );
        assert!(
            e.testo.contains("list_active_services"),
            "dire 'rimediabile' senza dire come e' una promessa non mantenuta: {e:?}"
        );
        super::process_id_valido(&uuid::Uuid::nil().to_string()).expect("un uuid valido passa");
    }

    /// Servizi senza porta (task, worker): nessun ascolto da attendere, quindi
    /// il messaggio resta quello storico. Il controllo non deve trasformare in
    /// fallimento cio' che non espone una porta.
    #[test]
    fn senza_porta_attesa_il_messaggio_resta_invariato() {
        let info = uscita_processo("running", "job avviato", "");
        let msg = messaggio_di_avvio(
            "worker",
            &info,
            super::super::avvio_servizio::EsitoPorta::NessunaAttesa,
            &AscoltoAltrove::NonAccertato,
        );
        assert!(msg.testo.contains("VIVO"), "{msg:?}");
        assert!(!msg.testo.contains("NESSUN ASCOLTO"), "{msg:?}");
        assert!(!msg.esito.e_fallito(), "{msg:?}");
    }

    /// L'incidente misurato: label `frontend`, porta allocata 24806, e
    /// l'applicazione viva e rispondente sulla 24804 pinnata nel `.env`. Dieci
    /// run distinti hanno sbattuto contro un messaggio che diceva solo «non li'»,
    /// e l'agente ha reagito con 74 `run_command` di netstat improvvisato contro
    /// una sola `request_port`.
    ///
    /// I fatti entrano nella forma esatta che `listening_ports` produce
    /// (`(porta, pid, programma)`), e il verdetto nasce dal classificatore, mai
    /// scritto a mano (regola O).
    #[test]
    fn quando_lascolto_manca_il_messaggio_dice_dove_il_processo_ascolta_davvero() {
        let progetto = progetto_di_prova();
        let (bucket_start, _) = project_bucket_range(&progetto);
        let attesa = bucket_start + 6;
        let reale = bucket_start + 4;
        // Il dev server e' un FIGLIO (npm -> node): pid diverso da quello
        // registrato, quindi nessuna prova diretta e la porta finisce fra i
        // fatti del bucket, dichiarati e non attribuiti.
        let fatti = vec![(reale, 9931, "node".to_string())];
        let altrove = classifica_ascolto_altrove(&progetto, attesa, Some(17488), &fatti);
        assert_eq!(
            altrove,
            AscoltoAltrove::NelBucket(vec![(reale, 9931, "node".to_string())])
        );

        let info = uscita_processo("running", "VITE ready", "");
        let msg = messaggio_di_avvio(
            "frontend",
            &info,
            super::super::avvio_servizio::EsitoPorta::Muta { porta: attesa },
            &altrove,
        );
        assert!(
            msg.testo.contains(&reale.to_string()),
            "il messaggio deve nominare la porta su cui si ascolta davvero: {msg:?}"
        );
        assert!(
            msg.testo.contains("NON e' provato"),
            "una porta trovata non e' una porta del servizio: {msg:?}"
        );
    }

    /// Prova diretta: il pid registrato E' quello del listener. Unica
    /// attribuzione dimostrabile senza indovinare (regola M).
    #[test]
    fn il_pid_del_processo_avviato_prova_la_porta_reale() {
        let progetto = progetto_di_prova();
        let (bucket_start, bucket_end) = project_bucket_range(&progetto);
        let attesa = bucket_start + 6;
        // Fuori bucket: e' il caso in cui il framework ha ripiegato da solo.
        let fuori = bucket_end + 1;
        assert!(!nexus_tool_kit::ports::port_in_project_bucket(
            &progetto, fuori
        ));
        let fatti = vec![(fuori, 17488, "node".to_string())];
        let altrove = classifica_ascolto_altrove(&progetto, attesa, Some(17488), &fatti);
        assert!(
            matches!(altrove, AscoltoAltrove::ProcessoAvviato { port, .. } if port == fuori),
            "{altrove:?}"
        );
        let info = uscita_processo("running", "", "");
        let msg = messaggio_di_avvio(
            "frontend",
            &info,
            super::super::avvio_servizio::EsitoPorta::Muta { porta: attesa },
            &altrove,
        );
        assert!(msg.testo.contains(&fuori.to_string()), "{msg:?}");
        assert!(
            msg.testo.contains("processo appena avviato risulta in ascolto"),
            "{msg:?}"
        );
    }

    /// Nessun listener nel bucket: l'assenza e' un FATTO utile (il servizio non
    /// ha mai fatto bind), e va distinta dal caso in cui non si e' guardato.
    #[test]
    fn senza_listener_nel_bucket_il_verdetto_e_nessuno() {
        let progetto = progetto_di_prova();
        let (bucket_start, bucket_end) = project_bucket_range(&progetto);
        // Porta di un ALTRO bucket: non e' un fatto di questo progetto.
        let estranea = bucket_end + 1;
        assert!(!nexus_tool_kit::ports::port_in_project_bucket(
            &progetto, estranea
        ));
        let fatti = vec![(estranea, 4242, "node".to_string())];
        assert_eq!(
            classifica_ascolto_altrove(&progetto, bucket_start + 6, Some(17488), &fatti),
            AscoltoAltrove::Nessuno
        );
    }

    /// Regressione FIX P5 (Pannello "Porte"): la regex `localhost:(\d{4,5})`
    /// cattura anche la stringa di CONNESSIONE al DB nei log del servizio (es.
    /// Postgres :5434). Quella e' una porta d'infrastruttura riservata
    /// (NEXUS_RESERVED_PORTS), non un listener del servizio: non deve mai essere
    /// ritornata/registrata come porta del progetto.
    #[test]
    fn detect_port_scarta_infrastruttura_riservata_accetta_bucket() {
        let progetto = progetto_di_prova();
        // La porta del cluster app Postgres (:5434) compariva come "backend-api".
        assert_eq!(
            detect_port_from_output(&progetto, "connecting to postgres at localhost:5434", ""),
            None
        );
        // Anche le altre porte d'infrastruttura riservate vanno scartate,
        // qualunque sia l'ordine in cui compaiono nell'output.
        assert_eq!(
            detect_port_from_output(&progetto, "", "db pool -> 127.0.0.1:5433 / qdrant 0.0.0.0:6333"),
            None
        );
        assert_eq!(
            detect_port_from_output(&progetto, "redis at localhost:6379", ""),
            None
        );
        // Una porta del bucket DI QUESTO progetto e' un listener legittimo.
        let (bucket_start, bucket_end) = project_bucket_range(&progetto);
        assert_eq!(
            detect_port_from_output(
                &progetto,
                &format!("Local: http://localhost:{bucket_start}/"),
                ""
            ),
            Some(PortaRilevata {
                port: bucket_start,
                esito: PortRegistrability::Registrable,
            })
        );
        assert_eq!(
            detect_port_from_output(&progetto, "", &format!("listening on 0.0.0.0:{bucket_end}")),
            Some(PortaRilevata {
                port: bucket_end,
                esito: PortRegistrability::Registrable,
            })
        );
    }

    /// Progetto reale dell'incidente: "gestione-spese", bucket 33600-33649.
    fn progetto_di_prova() -> Uuid {
        Uuid::parse_str("39802bb6-9540-4d70-82c1-fcf35c3a9b65").unwrap()
    }

    /// Caso reale, dal produttore alla conseguenza (regola O): l'agente scrive a
    /// mano `process.env.PORT || 20001`, il servizio parte e lo DICE nel proprio
    /// output. Da quell'output nasceva l'allocazione: `detect_port_from_output`
    /// vedeva una porta "del range progetti" e la dava per buona, poi
    /// `register_detected_port` la scriveva in `nexus_port_allocations` come
    /// porta di questo progetto. Ma 20001 sta nel bucket 20000-20049, di un ALTRO
    /// progetto: e' la collisione che il bucket esiste per impedire.
    ///
    /// Il verdetto e' il segnale su cui il chiamante decide (regola M): rilevata
    /// SI' - va vista e auditata - registrabile NO. Le due cose sono distinte, ed
    /// e' la distinzione che il tipo di ritorno rende impossibile confondere.
    ///
    /// Mutazione che rende rosso: togliere il ramo `OutOfProjectBucket` da
    /// `classify_project_port` (cioe' far decidere di nuovo al solo range globale)
    /// -> l'esito torna `Registrable`, la prima asserzione cade e con essa la
    /// condizione che in `report_started_service` porta alla registrazione.
    #[test]
    fn porta_del_bucket_altrui_non_e_registrabile_per_questo_progetto() {
        let progetto = progetto_di_prova();
        // L'output vero di un Express partito sulla porta hardcoded.
        let stdout = "Server in ascolto su http://localhost:20001";
        let rilevata = detect_port_from_output(&progetto, stdout, "")
            .expect("la porta del servizio va vista, non ignorata in silenzio");
        assert_eq!(rilevata.port, 20001);
        assert_eq!(
            rilevata.esito,
            PortRegistrability::OutOfProjectBucket {
                bucket_start: 33600,
                bucket_end: 33649,
            },
            "20001 e' nel range dei progetti ma nel bucket di un altro: senza \
             project_id la domanda 'e' TUA?' non era nemmeno ponibile"
        );
        // La conseguenza: il ramo che registra e' quello, e solo quello, in cui
        // l'esito e' Registrable (vedi report_started_service).
        assert_ne!(rilevata.esito, PortRegistrability::Registrable);

        // La porta che l'allocatore aveva assegnato per la via corretta (ultima
        // del bucket) resta registrabile: il vincolo non spegne il rilevamento.
        assert_eq!(
            detect_port_from_output(&progetto, "Local: http://localhost:33649/", ""),
            Some(PortaRilevata {
                port: 33649,
                esito: PortRegistrability::Registrable,
            })
        );
    }

    /// Il predicato unico (regola L) esclude le riservate Nexus, il fuori-range e
    /// il bucket altrui; ammette il bucket del progetto. Qui si verifica che il
    /// rilevamento usi QUEL criterio: la classificazione in se' e' testata nel
    /// punto unico (`nexus_tool_kit::ports`).
    #[test]
    fn predicato_registrabile_esclude_riservate() {
        use nexus_tool_kit::ports::is_project_registrable_port;
        let progetto = progetto_di_prova();
        for p in [80u16, 443, 3000, 4000, 4060, 5432, 5433, 5434, 6333, 6334, 6379, 8080] {
            assert!(
                !is_project_registrable_port(&progetto, p),
                "porta riservata {p} non deve essere registrabile per un progetto"
            );
        }
        let (bucket_start, bucket_end) = project_bucket_range(&progetto);
        assert!(is_project_registrable_port(&progetto, bucket_start));
        assert!(is_project_registrable_port(&progetto, bucket_end));
        // Fuori dal range progetti.
        assert!(!is_project_registrable_port(&progetto, 3002));
        assert!(!is_project_registrable_port(&progetto, 19999));
        assert!(!is_project_registrable_port(&progetto, 40000));
        // Nel range, ma nel bucket di un altro progetto.
        assert!(!is_project_registrable_port(&progetto, 20001));
        assert!(!is_project_registrable_port(&progetto, bucket_end + 1));
    }

    #[test]
    fn web_service_riconosce_dev_server_diretti_e_indiretti() {
        // Punto unico (regola L): looks_like_web_service governa SIA l'iniezione di
        // PORT del bucket SIA il routing run_command -> run_service. Deve catturare i
        // dev-server diretti e gli script npm/pnpm/yarn, altrimenti il server parte
        // fuori bucket via run_command e il port_enforcer lo uccide (causa C).
        assert!(looks_like_web_service("vite --port 35198 --host 0.0.0.0"));
        assert!(looks_like_web_service("npm run dev"));
        assert!(looks_like_web_service("pnpm dev"));
        assert!(looks_like_web_service("next dev"));
        // `npm start` nudo (alias di `npm run start`): prima del fix NON veniva
        // riconosciuto, il server partiva senza PORT injection e leggeva la
        // porta stantia dal .env (incidente Beaty-Book 2026-07-02).
        assert!(looks_like_web_service("npm start"));
        assert!(looks_like_web_service("cd backend && npm start"));
        assert!(!looks_like_web_service("ls -la"));
        assert!(!looks_like_web_service("cargo build"));
        assert!(!looks_like_web_service("npm install"));
        // Regressione final_gate: `npx vite build` non va a run_service (exit_code null).
        assert!(!looks_like_web_service("npx vite build"));
        assert!(!looks_like_web_service("pnpm vite build"));
        assert!(!looks_like_web_service("npm run build"));
    }

    /// Regressione deadlock allocazione stantia + ordine dedup/refuse.
    #[test]
    fn esistente_attivo_rifiuta_spento_adotta() {
        assert_eq!(
            existing_service_action(true),
            ExistingServiceAction::RefuseActive
        );
        assert_eq!(
            existing_service_action(false),
            ExistingServiceAction::AdoptStale
        );
    }

    /// Il terzo tool che riceve un comando arbitrario non deve avviare la suite
    /// come se fosse un servizio: la consegna all'esecutore unico, che su una
    /// root vuota si ferma da solo con un messaggio che solo lui produce.
    ///
    /// Regola O: si attraversa `tool_run_service` per intero, come fa il
    /// dispatcher. Se la guardia venisse rimossa, il flusso proseguirebbe verso
    /// dedup e allocazione porte — che leggono il DB — e questo test lo
    /// segnalerebbe restando appeso invece di passare in millisecondi.
    #[tokio::test]
    async fn run_service_non_avvia_la_suite_playwright_come_servizio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test(dir.path().to_path_buf());
        let out = tool_run_service(
            &ctx,
            &serde_json::json!({"command": "npx playwright test", "label": "e2e"}),
            "task",
        )
        .await;
        assert!(
            out.testo
                .contains("[run_playwright_tests] Playwright non trovato nel progetto"),
            "la suite deve passare dall'esecutore unico, output: {out:?}"
        );
    }

    /// IL FURTO DI IDENTITA', contro lo schema VERO (regola O: schema dalla
    /// migrazione meta, non da un `CREATE TABLE` ricopiato).
    ///
    /// Riproduce la coppia misurata su `agenda-medica` il 2026-08-06: la porta
    /// 31926 e' registrata a `backend` con la sua unit, e un altro processo la
    /// dichiara nel proprio output presentandosi con un'altra identita'. La riga
    /// deve restare di `backend`, e non deve nascerne una seconda.
    ///
    /// MUTAZIONE che rende rosso (eseguita il 2026-08-06): far collassare
    /// `PortClaim::DiUnAltro` in `Registrabile` — cioe' tornare a trattare la
    /// porta come chiave d'identita' — e rimettere in `registra_porta_rilevata`
    /// la vecchia `ON CONFLICT (port) DO UPDATE SET project_id = $1, label = $3`.
    /// Le due mutazioni vanno INSIEME, ed e' la forma stessa del fix: con la sola
    /// query mutata il verdetto ferma comunque la scrittura, con la sola
    /// classificazione mutata la scrittura non riscrive nulla. Il test fallisce
    /// sul verdetto, che nomina il derubato (`label: "backend"`).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_porta_rilevata_non_ruba_la_riga_di_un_altro_servizio(pool: sqlx::PgPool) {
        let (_utente, progetto) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let porta = 31926i32;

        sqlx::query(
            "INSERT INTO nexus_port_allocations \
             (project_id, port, label, service_unit, allocation_mode) \
             VALUES ($1, $2, 'backend', 'agenda-medica-backend.service', 'adopted')",
        )
        .bind(progetto)
        .bind(porta)
        .execute(&pool)
        .await
        .expect("allocazione del backend");

        let claim = super::registra_porta_rilevata(&pool, progetto, "Service", porta).await;
        assert!(
            matches!(
                claim,
                nexus_tool_kit::ports::PortClaim::DiUnAltro { ref label, .. } if label == "backend"
            ),
            "la porta e' di 'backend': il verdetto deve nominarlo, {claim:?}"
        );

        let (label, unit): (String, Option<String>) =
            sqlx::query_as("SELECT label, service_unit FROM nexus_port_allocations WHERE port = $1")
                .bind(porta)
                .fetch_one(&pool)
                .await
                .expect("la riga esiste ancora");
        assert_eq!(label, "backend", "l'identita' della riga non si riscrive");
        assert_eq!(unit.as_deref(), Some("agenda-medica-backend.service"));

        // La seconda riga (31927 nel caso reale) nasceva perche' la
        // `find_or_allocate("backend")` successiva non trovava piu' la sua label.
        // Qui la trova: il registro resta a una riga per servizio.
        let righe: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_port_allocations WHERE project_id = $1",
        )
        .bind(progetto)
        .fetch_one(&pool)
        .await
        .expect("conteggio");
        assert_eq!(righe, 1, "nessuna riga in piu' e nessuna in meno");
    }

    /// L'AVVIO DI UN SERVIZIO NON RACCOGLIE LA PRENOTAZIONE DI UN ALTRO.
    ///
    /// E' il test che conta (regola O): attraversa `dedup_and_cleanup_ports`,
    /// cioe' ESATTAMENTE la funzione che `gate_pre_avvio` chiama a ogni
    /// `run_service` con `kind = "service"`, contro lo schema reale (compresa la
    /// colonna `prenotata_da_run` della mig 0741) e la `SELECT`/`DELETE` di
    /// produzione. Un test sul solo criterio puro non basta: era esattamente
    /// cio' che il primo giro del fix aveva, e questo raccoglitore restava col
    /// proprio criterio piu' debole.
    ///
    /// Le due righe sono nel BUCKET del progetto, `dynamic`, oltre la grace, con
    /// una label DIVERSA da quella in avvio e nessun processo attivo: prima del
    /// fix entrambe finivano nel `DELETE`, perche' l'unica domanda era «la porta
    /// e' bindabile?».
    ///
    /// Il test DISCRIMINA — una riga sopravvive e una viene raccolta — quindi
    /// non puo' essere verde per un preserva-tutto: se il criterio si spegnesse
    /// del tutto, o se i fatti non arrivassero, cadrebbe la seconda asserzione.
    ///
    /// MUTAZIONE che rende rosso: togliere la terza prova di vita da questo
    /// raccoglitore — in `raccolta_allocazione::giudica`, il ramo
    /// `fatti.prenotazione().await.tiene_in_vita()` — oppure smettere di passare
    /// `prenotata_da_run` nella `RigaAllocazione`. In entrambi i casi la porta
    /// prenotata sparisce all'avvio del servizio, che e' il difetto misurato il
    /// 18/08/2026 su biblioteca-18-08.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_avvio_di_un_servizio_non_raccoglie_una_prenotazione_viva(pool: sqlx::PgPool) {
        let (_utente, progetto) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let (bucket_start, _bucket_end) =
            crate::project_workspace::services::project_bucket_range(&progetto);
        let prenotata = bucket_start as i32;
        let residuo = (bucket_start + 1) as i32;
        let run_vivo = Uuid::new_v4();

        for (porta, label, run) in [
            (prenotata, "frontend", Some(run_vivo)),
            (residuo, "tentativo-fallito", None),
        ] {
            sqlx::query(
                "INSERT INTO nexus_port_allocations \
                   (project_id, port, label, allocation_mode, prenotata_da_run, created_at) \
                 VALUES ($1, $2, $3, 'dynamic', $4, NOW() - INTERVAL '1 hour')",
            )
            .bind(progetto)
            .bind(porta)
            .bind(label)
            .bind(run)
            .execute(&pool)
            .await
            .expect("seed allocazione");
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = crate::test_support::ctx_di_tool_test_su_db(
            dir.path().to_path_buf(),
            pool.clone(),
            progetto,
            Some(run_vivo),
        );

        // Il servizio che sta partendo si chiama `backend`: nessuna delle due
        // righe e' protetta dalla label in avvio, ed e' la condizione in cui il
        // difetto si manifestava.
        super::dedup_and_cleanup_ports(
            &ctx,
            "backend",
            "npm run dev",
            dir.path(),
            super::EsitoProcessi::Elenco(Vec::new()),
            &crate::test_support::RunFinto::con_vivo(run_vivo),
        )
        .await;

        let rimaste: Vec<i32> = sqlx::query_scalar(
            "SELECT port::int FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port",
        )
        .bind(progetto)
        .fetch_all(&pool)
        .await
        .expect("rilettura allocazioni");

        assert!(
            rimaste.contains(&prenotata),
            "la porta prenotata da un run VIVO deve sopravvivere all'avvio di un altro \
             servizio: il servizio che la usera' non esiste ancora, quindi non puo' avere \
             ne' listener ne' service_unit. Rimaste: {rimaste:?}"
        );
        assert!(
            !rimaste.contains(&residuo),
            "la riga senza alcuna prova di vita e' il residuo che questo raccoglitore \
             esiste per raccogliere: se sopravvive, il criterio non sta decidendo. \
             Rimaste: {rimaste:?}"
        );
    }

    /// Il raccoglitore dell'avvio ha ereditato anche le prove che NON erano sue.
    ///
    /// Con `list_processes` non interrogabile (DB del progetto irraggiungibile)
    /// non si cancella nulla: prima quel caso era un `if let Ok(...)` che
    /// saltava il cleanup in silenzio, ora e' un fatto dichiarato che PRESERVA —
    /// stesso esito, ma visibile e per il criterio.
    ///
    /// MUTAZIONE che rende rosso: far degradare
    /// `ImpiegoDellaLabel::NonInterrogabile` a `NessunProcesso` in
    /// `impiego_della_label`.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn senza_i_processi_del_progetto_non_si_cancella_niente(pool: sqlx::PgPool) {
        let (_utente, progetto) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let (bucket_start, _fine) =
            crate::project_workspace::services::project_bucket_range(&progetto);
        let porta = bucket_start as i32;

        sqlx::query(
            "INSERT INTO nexus_port_allocations \
               (project_id, port, label, allocation_mode, created_at) \
             VALUES ($1, $2, 'tentativo-fallito', 'dynamic', NOW() - INTERVAL '1 hour')",
        )
        .bind(progetto)
        .bind(porta)
        .execute(&pool)
        .await
        .expect("seed allocazione");

        super::cleanup_dead_process_ports(
            &pool,
            progetto,
            super::EsitoProcessi::NonInterrogabili("pool del progetto assente".into()),
            "backend",
            &crate::test_support::RunFinto::nessuno_vivo(),
        )
        .await;

        let rimaste: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM nexus_port_allocations WHERE project_id = $1")
                .bind(progetto)
                .fetch_one(&pool)
                .await
                .expect("conteggio");
        assert_eq!(
            rimaste, 1,
            "«non ho potuto leggere i processi» non e' «nessun processo la usa»: \
             sull'ignoto non si distrugge"
        );
    }

    /// I due casi in cui si scrive davvero, sempre contro lo schema vero: la
    /// porta libera nasce col richiedente, e la porta gia' propria si conferma
    /// senza duplicarsi.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_porta_libera_o_gia_propria_si_registra(pool: sqlx::PgPool) {
        let (_utente, progetto) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let porta = 31904i32;

        assert_eq!(
            super::registra_porta_rilevata(&pool, progetto, "frontend", porta).await,
            nexus_tool_kit::ports::PortClaim::Registrabile
        );
        let label: String =
            sqlx::query_scalar("SELECT label FROM nexus_port_allocations WHERE port = $1")
                .bind(porta)
                .fetch_one(&pool)
                .await
                .expect("la riga e' stata creata");
        assert_eq!(label, "frontend");

        assert_eq!(
            super::registra_porta_rilevata(&pool, progetto, "frontend", porta).await,
            nexus_tool_kit::ports::PortClaim::GiaSua,
            "il secondo giro dello stesso servizio e' una conferma, non un conflitto"
        );
        let righe: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM nexus_port_allocations WHERE project_id = $1",
        )
        .bind(progetto)
        .fetch_one(&pool)
        .await
        .expect("conteggio");
        assert_eq!(righe, 1);
    }

    /// CHI puo' registrare una porta, attraversando il produttore dell'identita'
    /// invece di fissare il `kind` a mano (regola O).
    ///
    /// Un comando one-shot riceve `kind='task'` e la label priva di scopo: e'
    /// legittimo come processo, ma quella label non ha nessun titolo per entrare
    /// nel registro delle porte — la porta che compare nel suo output e' di un
    /// altro.
    ///
    /// MUTAZIONE che rende rosso: far ritornare `true` a
    /// `registra_le_proprie_porte` per qualunque kind (com'era prima, quando
    /// `registra_o_audita_porta_rilevata` non guardava affatto il kind).
    #[test]
    fn un_task_non_registra_porte() {
        let root = std::path::Path::new("/progetti/agenda-medica");
        let (kind, label) = resolve_service_label(
            &serde_json::json!({}),
            "curl -s http://localhost:31926/api/appuntamenti",
            "service",
            root,
            root,
            uuid::Uuid::nil(),
            Some("agenda-medica".to_string()),
        )
        .classifica("service");

        assert_eq!(kind, "task", "un curl non e' un servizio di progetto");
        assert_eq!(
            label, LABEL_NON_SERVIZIO,
            "senza ruolo, l'identita' e' quella dichiarata priva di scopo"
        );
        assert!(
            !registra_le_proprie_porte(&kind),
            "la porta nell'output di un task e' di qualcun altro: registrarla \
             scriverebbe nel registro un'identita' senza significato"
        );
        // Il servizio vero, per contrasto: e' l'unico che registra.
        let (kind_servizio, _) = resolve_service_label(
            &serde_json::json!({"label": "backend"}),
            "npm run start",
            "service",
            root,
            root,
            uuid::Uuid::nil(),
            Some("agenda-medica".to_string()),
        )
        .classifica("service");
        assert!(registra_le_proprie_porte(&kind_servizio));
    }
}
