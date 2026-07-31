//! Tool servizio: avvio processi long-running, lettura output, stop, build immagine progetto.

use super::*;
use std::collections::HashMap;

/// Token che indicano "il comando avvia un server web". Tenuti a scope di
/// modulo (non dentro la funzione) per contenere la lunghezza di
/// `looks_like_web_service`; l'insieme e' invariato.
const WEB_TOKENS: &[&str] = &[
    "next dev",
    "next start",
    "vite",
    "vite dev",
    "vite preview",
    "webpack-dev-server",
    "webpack serve",
    "astro dev",
    "astro start",
    "astro preview",
    "nuxt dev",
    "nuxt start",
    "svelte-kit dev",
    "ng serve",            // Angular
    "react-scripts start", // CRA
    "expo start",          // React Native web
    "remix dev",
    "gunicorn",
    "uvicorn",
    "hypercorn",
    "daphne",
    "flask run",
    "django runserver",
    "manage.py runserver",
    "rails server",
    "rails s ",
    "sinatra",
    "node server",
    "node app",
    "node index",
    "node main",
    "ts-node server",
    "tsx server",
    "deno serve",
    "bun --hot",
    "bun run dev",
    "bun run start",
    "cargo run",
    "cargo watch",
    "go run",
    "dotnet run",
    "dotnet watch",
    "php -S",
    "ruby -run",
    "live-server",
    "http-server",
    "browser-sync",
    // Make/script wrapper noti
    "npm run dev",
    "npm run start",
    // `npm start` nudo e' l'alias built-in di `npm run start`: senza questo
    // token il servizio partiva SENZA PORT injection e leggeva la porta
    // stantia dal .env (incidente Beaty-Book 2026-07-02, EADDRINUSE).
    "npm start",
    "npm run serve",
    "pnpm dev",
    "pnpm start",
    "pnpm serve",
    "yarn dev",
    "yarn start",
    "yarn serve",
];

/// Heuristica: il comando avvia un server web/long-running che ha bisogno di
/// una porta TCP? Riconosce:
/// - script comuni: `next dev|start`, `vite`, `webpack-dev-server`, `astro dev`
/// - framework Python: `gunicorn`, `uvicorn`, `flask run`, `django runserver`
/// - Node generici: `node server.js`, `npm run dev|start|serve`
/// - Rust/Go/.NET dev server: `cargo run`, `go run`, `dotnet run`, `dotnet watch`
///
/// In caso di dubbio (es. `make foo`), NON inietta PORT: l'agente puo' chiamare
/// `request_port` esplicitamente e includere la porta nel comando.
pub(crate) fn looks_like_web_service(command: &str) -> bool {
    // One-shot build/install/test: non sono servizi web (regola L: is_long_oneshot).
    if is_long_oneshot(command) {
        return false;
    }
    let lc = command.to_lowercase();
    WEB_TOKENS.iter().any(|t| lc.contains(t))
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

/// Avvia un servizio/processo long-running direttamente sul server.
/// L'output viene catturato nel DB e mostrato nel pannello Output dell'IDE.
pub(super) async fn tool_run_service(ctx: &AgentToolContext, input: &Value, kind: &str) -> String {
    // Fase 1: validazione + label/work_dir (senza refuse: la dedup deve girare prima).
    let launch = match resolve_service_launch(ctx, input, kind).await {
        Ok(l) => l,
        Err(msg) => return msg,
    };
    let ServiceLaunch {
        command,
        kind,
        label,
        work_dir,
    } = launch;

    // ── Deduplicazione PRIMA del refuse (regola L, fix strutturale) ─────────
    // stop_similar + cleanup porte, poi liberazione LISTEN orfano/zombie sullo
    // stesso scopo. Solo DOPO si valuta refuse_if_same_scope_active: cosi' un
    // riavvio non resta bloccato da PID 12812/child ancora in ascolto mentre il
    // check refuse girava prima della dedup (incidente Vite 31754).
    dedup_and_cleanup_ports(ctx, &label, &command, &work_dir).await;

    if let Some(kind_hint) = derive_kind_hint(
        &command,
        &label,
        scope_dir(&ctx.root_path, &work_dir, &command).as_deref(),
    ) {
        if let Some(refuse_msg) = refuse_if_same_scope_active(ctx, kind_hint).await {
            return refuse_msg;
        }
    }

    // ── Quota container (PR hardening) ────────────────────────────────────
    // Solo per kind="service": i tool agente short-lived non contano contro
    // la quota container del progetto.
    if kind == "service" {
        if let Some(quota_msg) = check_container_quotas(ctx, &label, &command).await {
            return quota_msg;
        }
    }

    // ── Strato 1 hardening: auto-inject PORT per servizi web ────────────────
    // Se il comando avvia un server (next dev, vite, gunicorn, ecc.) Nexus
    // alloca automaticamente una porta nel bucket del progetto e la inietta
    // come PORT env. Cosi' il servizio non bindera' sulla porta hardcoded
    // (es. next dev → 3000 → conflitto con web-ide Nexus).
    let env_overrides = match allocate_web_port_env(ctx, &command, &label).await {
        Ok(env) => env,
        Err(msg) => return msg,
    };
    // Porta su cui il servizio DEVE mettersi in ascolto. E' il segnale che
    // distingue "il processo esiste" da "il servizio serve": senza, un avvio
    // fallito viene riportato come riuscito (vedi `attende_ascolto`).
    let porta_attesa = env_overrides
        .as_ref()
        .and_then(|e| e.get("PORT"))
        .and_then(|p| p.trim().parse::<u16>().ok());

    let process_id =
        match spawn_service_process(ctx, &label, &command, &work_dir, env_overrides, &kind).await {
            Ok(process_id) => process_id,
            Err(msg) => return msg,
        };

    // Servizio web: si attende l'ASCOLTO, non un tempo fisso. Uno sano risponde
    // spesso in meno di un secondo e il tool ritorna subito; uno morto consuma
    // l'intera finestra e viene riportato come fallito.
    // Servizio non web (nessuna porta): resta l'attesa a tempo per raccogliere
    // l'output iniziale, unico segnale disponibile.
    let ascolto = match porta_attesa {
        Some(port) => Some((port, attende_ascolto(port).await)),
        None => {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            None
        }
    };
    report_started_service(ctx, &label, process_id, ascolto).await
}

/// Finestra entro cui un servizio web deve mettersi in ascolto sulla sua porta.
const ATTESA_ASCOLTO_MS: u64 = 20_000;
/// Pausa fra due sondaggi mentre si aspetta l'ascolto.
const INTERVALLO_PROBE_MS: u64 = 400;

/// True se `port` entra in ascolto entro [`ATTESA_ASCOLTO_MS`].
///
/// Esiste perche' la vita del processo NON dimostra che il servizio funzioni:
/// `nodemon` sopravvive al crash dell'applicazione ("app crashed - waiting for
/// file changes") e resta `running` con la porta chiusa. Riportare "avviato" in
/// quel caso da' all'agente un esito falso, su cui costruisce i passi
/// successivi invece di correggere l'errore che gli sta gia' nello stderr.
/// L'ascolto e' il segnale strutturato del fatto (regola M).
///
/// Ritorna al PRIMO sondaggio riuscito, cosi' la finestra larga non costa nulla
/// ai servizi sani: la paga solo chi non parte, dove l'attesa e' il prezzo di
/// una diagnosi corretta invece di una conferma sbagliata.
async fn attende_ascolto(port: u16) -> bool {
    let scaduto_dopo = tokio::time::Instant::now() + std::time::Duration::from_millis(ATTESA_ASCOLTO_MS);
    loop {
        if crate::project_workspace::port_recovery::tcp_probe(port, INTERVALLO_PROBE_MS).await {
            return true;
        }
        if tokio::time::Instant::now() >= scaduto_dopo {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(INTERVALLO_PROBE_MS)).await;
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
async fn validate_service_command(ctx: &AgentToolContext, input: &Value) -> Result<String, String> {
    let command = match input.get("command").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return Err("\u{274C} [Errore: parametro 'command' mancante]".to_string()),
    };
    if command.trim().is_empty() {
        return Err("\u{274C} [Errore: comando vuoto]".to_string());
    }
    if let Some(msg) = crate::security::redaction_guard::enforce_no_redacted_placeholder(
        ctx,
        "run_service",
        "command",
        &command,
    )
    .await
    {
        return Err(msg);
    }
    Ok(command)
}

/// Fase 1 di `tool_run_service`: valida il comando, declassa one-shot, deriva
/// label e working directory. Il refuse per scopo duplicato avviene DOPO
/// `dedup_and_cleanup_ports` in `tool_run_service` (ordine vincolante).
async fn resolve_service_launch(
    ctx: &AgentToolContext,
    input: &Value,
    kind: &str,
) -> Result<ServiceLaunch, String> {
    let command = validate_service_command(ctx, input).await?;

    // Declassamento one-shot: install/build/test lunghi arrivano qui via
    // auto-routing di run_command (background=true, pattern long-running,
    // auto-probe) con kind="service", ma NON sono servizi del progetto.
    // Registrarli con kind='service' li fa comparire per sempre nel pannello
    // Servizi (list_services_windows). Il processo resta gestito identico
    // (stessa tabella, stop/read_output per id): cambia solo la classificazione.
    let kind =
        if kind == "service" && is_long_oneshot(&command) && !looks_like_web_service(&command) {
            "task"
        } else {
            kind
        };

    // Risoluzione working directory (punto unico riusato anche dal restart).
    let work_dir = resolve_service_work_dir(&ctx.root_path, input)?;

    // Derivazione identita' (refuse per scopo: dopo dedup in tool_run_service).
    // La conseguenza sulla classificazione la dichiara l'identita' stessa
    // (`classifica`), non il chiamante: un `kind='service'` senza identita' non
    // e' un servizio di progetto e viene declassato come il one-shot qui sopra.
    let (kind, label) = resolve_service_label(
        input,
        &command,
        kind,
        &ctx.root_path,
        &work_dir,
        ctx.project_id,
    )
    .classifica(kind);

    Ok(ServiceLaunch {
        command,
        kind,
        label,
        work_dir,
    })
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
) -> ServiceIdentity {
    let explicit_label = input
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| !crate::agent_processes::is_generic_service_label(s));
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
        return ServiceIdentity::SoloAncoraggio(format!(
            "service-{}",
            &project_id.simple().to_string()[..8]
        ));
    }
    ServiceIdentity::NonServizio
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

/// Deduplicazione servizi prima del refuse: ferma processi simili, cleanup
/// porte orfane, libera LISTEN residuo sullo stesso scopo (zombie/orfani non
/// tracciati in agent_processes). Delega stop a stop_similar_running_services
/// (punto unico, regola L).
async fn dedup_and_cleanup_ports(
    ctx: &AgentToolContext,
    label: &str,
    command: &str,
    work_dir: &std::path::Path,
) {
    let _ =
        crate::agent_processes::stop_similar_running_services(&ctx.db, ctx.project_id, label).await;
    if let Ok(existing) = crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        cleanup_dead_process_ports(&ctx.db, ctx.project_id, &existing, label).await;
    }
    if let Some(kind_hint) = derive_kind_hint(
        command,
        label,
        scope_dir(&ctx.root_path, work_dir, command).as_deref(),
    ) {
        free_listening_scope_port(ctx, kind_hint).await;
    }
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
) -> Result<Uuid, String> {
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
    .map_err(|e| format!("\u{274C} [Errore avvio servizio '{}': {}]", label, e))
}

/// Legge l'output iniziale di un servizio appena avviato, registra la porta
/// rilevata, emette gli eventi di pannello e compone il messaggio di ritorno.
async fn report_started_service(
    ctx: &AgentToolContext,
    label: &str,
    process_id: Uuid,
    ascolto: Option<(u16, bool)>,
) -> String {
    let info = match crate::agent_processes::read_process_output(
        &ctx.db,
        ctx.project_id,
        process_id,
        4000,
    )
    .await
    {
        Ok(info) => info,
        Err(e) => {
            return format!(
                "Servizio '{}' avviato (process_id: {}), ma errore lettura output: {}",
                label, process_id, e
            )
        }
    };

    // Auto-detect porta dall'output del servizio e registra
    // in nexus_port_allocations per il pannello Porte.
    // Che fare della porta rilevata: qui, dove il verdetto e' quello vero e non
    // una sua ricostruzione. La porta del bucket ALTRUI non si registra (vedi
    // `register_detected_port`), ma non sparisce: viene auditata, e restando
    // senza allocazione resta visibile al port_enforcer e al linter.
    let rilevata = detect_port_from_output(&ctx.project_id, &info.stdout, &info.stderr);
    let detected_port = match rilevata {
        Some(r) if r.esito == nexus_tool_kit::ports::PortRegistrability::Registrable => {
            register_detected_port(ctx, label, r.port as i32, info.pid).await;
            Some(r.port as i32)
        }
        Some(r) => {
            audita_porta_fuori_bucket(ctx, label, r, info.pid);
            None
        }
        None => None,
    };
    // Dispatcher: notifica avvio servizio → pannello Servizi aggiorna LED.
    // Se la porta attesa non e' mai entrata in ascolto il servizio NON e' su:
    // emettere l'avvio accenderebbe un LED verde su un servizio morto, cioe'
    // ripeterebbe nel pannello la stessa bugia detta all'agente.
    let in_ascolto = !matches!(ascolto, Some((_, false)));
    if in_ascolto {
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
    format_started_message(label, process_id, &info, ascolto)
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
    let mut entry = crate::security::AuditEntry::blocked(
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
    }))
    .with_actor_user(ctx.user_id);
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
async fn register_detected_port(ctx: &AgentToolContext, label: &str, port: i32, pid: Option<i32>) {
    let _ = sqlx::query(
        "INSERT INTO nexus_port_allocations (project_id, port, label, allocation_mode) \
         VALUES ($1, $2, $3, 'auto') ON CONFLICT (port) DO UPDATE SET \
         project_id = $1, label = $3, updated_at = NOW()",
    )
    .bind(ctx.project_id)
    .bind(port)
    .bind(label)
    .execute(&*ctx.db)
    .await;
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

/// Compone il messaggio testuale di avvio servizio (header + STDOUT/STDERR).
/// Messaggio per un servizio che non e' mai entrato in ascolto. Mette lo STDERR
/// per primo: e' li' che sta la causa, ed e' cio' che l'agente deve leggere per
/// correggere invece di proseguire.
fn format_ascolto_mancante(
    label: &str,
    process_id: Uuid,
    info: &crate::agent_processes::ProcessOutput,
    port: u16,
) -> String {
    let pid = info
        .pid
        .map(|p| p.to_string())
        .unwrap_or_else(|| "?".into());
    let mut msg = format!(
        "Servizio '{label}' NON avviato: nessun ascolto sulla porta {port} entro {} secondi \
         (process_id: {process_id}, pid: {pid}, status processo: {}).\n\
         Il processo puo' essere ancora vivo senza che il servizio funzioni: alcuni runner \
         (nodemon, watcher) sopravvivono al crash dell'applicazione. La causa e' nell'output \
         qui sotto; correggila e riavvia, invece di proseguire come se il servizio rispondesse.\n",
        ATTESA_ASCOLTO_MS / 1000,
        info.status,
    );
    if !info.stderr.is_empty() {
        msg.push_str(&format!("\nSTDERR:\n{}", info.stderr));
    }
    if !info.stdout.is_empty() {
        msg.push_str(&format!("\nSTDOUT:\n{}", info.stdout));
    }
    if info.stdout.is_empty() && info.stderr.is_empty() {
        msg.push_str(
            "\n(Nessun output: il processo non ha scritto nulla prima di smettere di rispondere.)",
        );
    }
    msg
}

fn format_started_message(
    label: &str,
    process_id: Uuid,
    info: &crate::agent_processes::ProcessOutput,
    ascolto: Option<(u16, bool)>,
) -> String {
    // Un servizio che non e' entrato in ascolto NON e' avviato, per quanto il
    // suo processo sia ancora vivo. Dirlo apertamente e' l'intero scopo del
    // controllo: l'agente deve leggere il fallimento e lo stderr che lo spiega,
    // non una conferma su cui costruire i passi successivi.
    if let Some((port, false)) = ascolto {
        return format_ascolto_mancante(label, process_id, info, port);
    }
    let mut msg = format!(
        "Servizio '{}' avviato (process_id: {}, pid: {}, status: {})\n",
        label,
        process_id,
        info.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into()),
        info.status,
    );
    if let Some((port, true)) = ascolto {
        msg.push_str(&format!("In ascolto sulla porta {port} (verificato).\n"));
    }
    if !info.stdout.is_empty() {
        msg.push_str(&format!("\nSTDOUT:\n{}", info.stdout));
    }
    if !info.stderr.is_empty() {
        msg.push_str(&format!("\nSTDERR:\n{}", info.stderr));
    }
    if info.stdout.is_empty() && info.stderr.is_empty() {
        msg.push_str("\n(Nessun output ancora. Usa read_service_output per controllare dopo qualche secondo.)");
    }
    msg
}

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
    input: &Value,
) -> Result<std::path::PathBuf, String> {
    let sub = input
        .get("working_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(sub) = sub else {
        return Ok(root.to_path_buf());
    };
    match resolve_relative_path(root, sub) {
        Ok(p) => Ok(p),
        Err(e) => Err(format!(
            "\u{274C} [Errore percorso: {}]",
            e.1["error"].as_str().unwrap_or("path error")
        )),
    }
}

/// Anti-duplicato convergente sul PUNTO UNICO resource_resolver (regola L).
/// Valutato DOPO `dedup_and_cleanup_ports`: refuse solo se, dopo stop_similar e
/// try_free_port, lo stesso scopo e' ancora in LISTEN (servizio legit da riusare).
async fn refuse_if_same_scope_active(ctx: &AgentToolContext, kind: &str) -> Option<String> {
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
            Some(format!(
                "\u{274C} [Errore: servizio '{}' di tipo {} gia' ATTIVO sulla porta {}. \
                 Riusalo invece di crearne uno nuovo (puoi accedere a http://localhost:{}). \
                 Se vuoi davvero riavviarlo usa `service_restart` con label='{}'.]",
                res.label, kind, port, port, res.label
            ))
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
/// Ritorna `Some(msg)` col messaggio d'errore da restituire se una quota e'
/// raggiunta; `None` se l'avvio puo' proseguire. Registra l'audit dei blocchi.
async fn check_container_quotas(
    ctx: &AgentToolContext,
    label: &str,
    command: &str,
) -> Option<String> {
    // Separazione DB: agent_processes (conteggi container/RAM) vive nel DB
    // del progetto; le quote restano nel meta. Punto unico project_db_routes.
    // Fail-closed: quota non verificabile -> avvio bloccato con messaggio
    // esplicito (lo spawn fallirebbe comunque sullo stesso DB).
    let run_pool =
        match crate::project_db_routes::project_data_pool_from(&ctx.db, ctx.project_id).await {
            Ok(p) => p,
            Err(e) => {
                return Some(format!(
                    "\u{274C} [Errore: DB del progetto non disponibile, quote container non verificabili: {e}]"
                ))
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
        return Some(format!("[Quota raggiunta: {}]", reason));
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
        return Some(format!("[Quota memoria raggiunta: {}]", reason));
    }
    None
}

/// Per i servizi web alloca una porta nel bucket del progetto e prepara gli
/// override d'ambiente (`PORT`/`HOST`). Ritorna `Ok(None)` per i comandi che
/// non sono server web (nessun override), `Err(msg)` se l'allocazione fallisce.
async fn allocate_web_port_env(
    ctx: &AgentToolContext,
    command: &str,
    label: &str,
) -> Result<Option<HashMap<String, String>>, String> {
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
    .map_err(|e| format!("\u{274C} [Errore porta per servizio '{}': {}]", label, e))?;
    Ok(Some(env))
}

/// Rilascia porte allocate (dynamic) il cui processo e' morto.
/// Controlla `agent_processes` per processi non-running di questo progetto
/// e rimuove le porte allocate che non hanno piu' un processo attivo.
async fn cleanup_dead_process_ports(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    processes: &[crate::agent_processes::ProcessSummary],
    preserve_label: &str,
) {
    // Raccogli le label dei processi ancora attivi
    let active_labels: std::collections::HashSet<String> = processes
        .iter()
        .filter(|p| p.status == "running" || p.status == "starting")
        .map(|p| p.label.clone())
        .collect();

    // Prendi le porte allocate dinamicamente per questo progetto
    let rows = sqlx::query_as::<_, (i32, String)>(
        "SELECT port, label FROM nexus_port_allocations \
         WHERE project_id = $1 AND allocation_mode = 'dynamic'",
    )
    .bind(project_id)
    .fetch_all(db)
    .await;

    let Ok(allocations) = rows else {
        return;
    };
    for (port, alloc_label) in allocations {
        // Non rilasciare l'allocazione del servizio che stiamo (ri)avviando
        // ora: la sua porta spenta serve all'adozione (deadlock allocazione
        // stantia), non va liberata. Salta anche le allocazioni con un processo
        // attivo corrispondente.
        if alloc_label == preserve_label || active_labels.contains(&alloc_label) {
            continue;
        }
        release_stale_port(db, project_id, port, &alloc_label).await;
    }
}

/// Rilascia una singola allocazione dinamica se la sua porta non e' realmente
/// in ascolto (bind test). Estratto da `cleanup_dead_process_ports` per tenerla
/// sotto soglia; comportamento invariato.
async fn release_stale_port(
    db: &sqlx::PgPool,
    project_id: uuid::Uuid,
    port: i32,
    alloc_label: &str,
) {
    // Verifica che la porta non sia effettivamente in uso (bind test).
    let port_in_use = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .is_err();
    if port_in_use {
        return;
    }
    let _ = sqlx::query("DELETE FROM nexus_port_allocations WHERE project_id = $1 AND port = $2")
        .bind(project_id)
        .bind(port)
        .execute(db)
        .await;
    tracing::info!(
        port = port,
        label = %alloc_label,
        "cleanup: porta dinamica rilasciata (processo morto)"
    );
}

/// Legge l'output di un servizio avviato con run_service
pub(super) async fn tool_read_service_output(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = input
        .get("process_id")
        .and_then(Value::as_str)
        .unwrap_or("");

    if process_id_str.is_empty() {
        // Se non specificato, leggi l'ultimo processo del progetto
        let rows = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
            Ok(r) => r,
            Err(e) => return format!("\u{274C} [Errore: {}]", e),
        };
        if rows.is_empty() {
            return "Nessun servizio avviato per questo progetto.".to_string();
        }
        let last = &rows[0];
        match crate::agent_processes::read_process_output(&ctx.db, ctx.project_id, last.id, 4000)
            .await
        {
            Ok(info) => format_process_output(&info),
            Err(e) => format!("\u{274C} [Errore lettura output: {}]", e),
        }
    } else {
        let process_id = match Uuid::parse_str(process_id_str) {
            Ok(id) => id,
            Err(_) => return "\u{274C} [Errore: process_id non valido]".to_string(),
        };
        match crate::agent_processes::read_process_output(&ctx.db, ctx.project_id, process_id, 4000)
            .await
        {
            Ok(info) => format_process_output(&info),
            Err(e) => format!("\u{274C} [Errore lettura output: {}]", e),
        }
    }
}

/// Ferma un servizio avviato con run_service
pub(super) async fn tool_stop_service(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = match input.get("process_id").and_then(Value::as_str) {
        Some(s) => s,
        None => return "\u{274C} [Errore: parametro 'process_id' mancante]".to_string(),
    };
    let process_id = match Uuid::parse_str(process_id_str) {
        Ok(id) => id,
        Err(_) => return "\u{274C} [Errore: process_id non valido]".to_string(),
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
            msg
        }
        Err(e) => format!("\u{274C} [Errore stop servizio: {}]", e),
    }
}

pub(super) async fn tool_build_project_image(ctx: &AgentToolContext) -> String {
    use crate::sandbox::build_project_service_image;
    match build_project_service_image(ctx.project_id, &ctx.root_path, &ctx.root_path).await {
        Ok(tag) => format!("Immagine Docker progetto buildata con successo: {}. I servizi avviati con run_service useranno questa immagine.", tag),
        Err(e) => format!("\u{274C} [Errore build immagine: {}]", e),
    }
}

/// Riavvia un servizio: ferma tutti i processi con la stessa label,
/// poi li riesegue con lo stesso comando. Attende output iniziale.
pub(super) async fn tool_service_restart(ctx: &AgentToolContext, input: &Value) -> String {
    let label = match input.get("label").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "\u{274C} [Errore: parametro 'label' obbligatorio]".to_string(),
    };

    // Cerca il processo esistente con questa label per recuperare il comando
    let existing = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return format!("\u{274C} [Errore lista processi: {}]", e),
    };

    let matching: Vec<_> = existing.iter().filter(|p| p.label == label).collect();
    if matching.is_empty() {
        return format!(
            "\u{274C} [Errore: nessun servizio trovato con label '{}'. Usa run_service per avviarlo.]",
            label
        );
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
    format!("Servizio '{}' riavviato.\n{}", label, result)
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
) -> Result<(), String> {
    for proc in matching
        .iter()
        .filter(|p| p.status == "running" || p.status == "starting")
    {
        if let Err(e) = crate::agent_processes::stop_process(&ctx.db, ctx.project_id, proc.id).await
        {
            return Err(format!(
                "\u{274C} [Errore restart '{}': stop del processo esistente non verificato: {}]",
                label, e
            ));
        }
    }
    Ok(())
}

/// Legge le ultime N righe di output di un servizio, con opzione di attesa
/// per catturare output aggiuntivo (simula follow per X secondi).
pub(super) async fn tool_tail_service_logs(ctx: &AgentToolContext, input: &Value) -> String {
    let process_id_str = input
        .get("process_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let max_chars = input
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(8000) as usize;
    let follow_secs = input
        .get("follow_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(60);

    // Risolvi process_id: specifico oppure ultimo del progetto (punto unico).
    let process_id = match resolve_process_id_or_last(ctx, process_id_str).await {
        Ok(id) => id,
        Err(msg) => return msg,
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
            Ok(info) => format_process_output(&info),
            Err(e) => format!("\u{274C} [Errore lettura output: {}]", e),
        };
    }

    follow_service_logs(ctx, process_id, max_chars, follow_secs).await
}

/// Risolve il process_id target: se `process_id_str` e' vuoto usa l'ultimo
/// processo del progetto, altrimenti lo parsa. Punto unico (regola L) condiviso
/// da tail/read. Ritorna `Err(msg)` col messaggio gia' pronto da restituire.
async fn resolve_process_id_or_last(
    ctx: &AgentToolContext,
    process_id_str: &str,
) -> Result<Uuid, String> {
    if !process_id_str.is_empty() {
        return Uuid::parse_str(process_id_str)
            .map_err(|_| "\u{274C} [Errore: process_id non valido]".to_string());
    }
    let rows = crate::agent_processes::list_processes(&ctx.db, ctx.project_id)
        .await
        .map_err(|e| format!("\u{274C} [Errore: {}]", e))?;
    match rows.first() {
        Some(p) => Ok(p.id),
        None => Err("Nessun servizio avviato per questo progetto.".to_string()),
    }
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

/// Lista i servizi/processi attivi per il progetto corrente.
pub(super) async fn tool_list_active_services(ctx: &AgentToolContext, _input: &Value) -> String {
    let rows = match crate::agent_processes::list_processes(&ctx.db, ctx.project_id).await {
        Ok(r) => r,
        Err(e) => return format!("\u{274C} [Errore: {}]", e),
    };

    if rows.is_empty() {
        return "Nessun servizio registrato per questo progetto.".to_string();
    }

    let mut output = String::new();
    let mut running_count = 0;
    let mut stopped_count = 0;

    for proc in &rows {
        let is_running = matches!(proc.status.as_str(), "running" | "starting");
        if is_running {
            running_count += 1;
        } else {
            stopped_count += 1;
        }
        output.push_str(&format_service_row(proc));
    }

    format!(
        "Servizi progetto: {} attivi, {} fermi (ultimi 20)\n\n{}",
        running_count, stopped_count, output
    )
}

/// Formatta una riga della lista servizi (icona stato + metadati + comando).
/// Estratto da `tool_list_active_services`; comportamento invariato.
fn format_service_row(proc: &crate::agent_processes::ProcessSummary) -> String {
    let status_icon = match proc.status.as_str() {
        "running" | "starting" => "[ATTIVO]",
        "stopped" | "exited" | "finished" => "[FERMO]",
        _ => "[?]",
    };
    let mut row = format!(
        "{} {} (id: {}, pid: {}, status: {}",
        status_icon,
        proc.label,
        proc.id,
        proc.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".into()),
        proc.status,
    );
    if let Some(code) = proc.exit_code {
        row.push_str(&format!(", exit: {}", code));
    }
    row.push_str(&format!(", avviato: {})\n", proc.created_at));
    row.push_str(&format!("  cmd: {}\n\n", proc.command));
    row
}

#[cfg(test)]
mod tests {
    use super::{
        derive_kind_hint, detect_port_from_output, existing_service_action, format_started_message,
        looks_like_web_service, resolve_service_label, resolve_service_work_dir, scope_dir,
        ExistingServiceAction, PortaRilevata, Uuid,
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
        let work_dir = resolve_service_work_dir(root, input).expect("working dir risolta");
        let (kind, label) =
            resolve_service_label(input, &command, kind, root, &work_dir, id("1a2b3c4d"))
                .classifica(kind);
        let unit = service_unit_name(&project_service_slug(nome_progetto), &label);
        (label, unit, kind)
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
        let msg = format_started_message("backend", uuid::Uuid::nil(), &info, Some((32976, false)));
        assert!(
            msg.contains("NON avviato"),
            "un servizio che non ascolta non va annunciato come avviato: {msg}"
        );
        assert!(msg.contains("32976"), "il messaggio deve nominare la porta attesa: {msg}");
        assert!(
            msg.contains("Cannot find module"),
            "lo stderr contiene la causa e deve raggiungere l'agente: {msg}"
        );
        // Il processo VIVO non deve mai diventare la prova che il servizio sia su.
        assert!(
            !msg.contains("Servizio 'backend' avviato"),
            "status 'running' non basta a dichiarare l'avvio: {msg}"
        );
    }

    #[test]
    fn con_ascolto_verificato_il_messaggio_lo_dichiara() {
        let info = uscita_processo("running", "server listening", "");
        let msg = format_started_message("backend", uuid::Uuid::nil(), &info, Some((32976, true)));
        assert!(msg.contains("avviato"), "{msg}");
        assert!(
            msg.contains("In ascolto sulla porta 32976 (verificato)"),
            "l'ascolto verificato va dichiarato, cosi' l'agente distingue una \
             conferma provata da una presunta: {msg}"
        );
    }

    /// Servizi senza porta (task, worker): nessun ascolto da attendere, quindi
    /// il messaggio resta quello storico. Il controllo non deve trasformare in
    /// fallimento cio' che non espone una porta.
    #[test]
    fn senza_porta_attesa_il_messaggio_resta_invariato() {
        let info = uscita_processo("running", "job avviato", "");
        let msg = format_started_message("worker", uuid::Uuid::nil(), &info, None);
        assert!(msg.contains("avviato"), "{msg}");
        assert!(!msg.contains("NON avviato"), "{msg}");
        assert!(!msg.contains("In ascolto"), "{msg}");
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
}
