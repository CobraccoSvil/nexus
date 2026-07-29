//! PUNTO UNICO (regola L) del CONTRATTO DI SUCCESSO di una riparazione
//! automatica di servizio: **cosa deve essere vero perche' una remediation possa
//! dirsi riuscita**, e come si scrive quell'esito.
//!
//! ## Il difetto che chiude
//!
//! La catena rileva -> ripara -> chiude esisteva gia', ma l'ultimo anello
//! misurava la cosa sbagliata. Caso reale (progetto gestione-spese, 2026-07-28):
//!
//! ```text
//! 21:24 service_observer: auto-debug avviato per gestione-spese-frontend.service (port_in_use)
//! 21:29 restart_project_unit: riavvio effettuato    msg=servizio 'frontend' avviato
//! ```
//!
//! e la diagnosi si chiudeva risolta. Due ore dopo, in ascolto c'era SOLO la
//! porta del backend; il frontend era morto, con un `.env` che dichiarava per se'
//! la porta del backend e un `vite.config.js` che leggeva una variabile diversa
//! da quella scritta nel `.env`. La riparazione non aveva riparato nulla: il
//! processo era nato per un istante ed era rimorto.
//!
//! I due segnali deboli su cui si chiudeva:
//!
//! 1. `restart_project_unit` -> "riavvio effettuato". Misura che il RIAVVIO e'
//!    avvenuto, cioe' la NASCITA di un processo, non il servizio che serve.
//! 2. `service_observer::resolve_open_crashes`, invocato al cambio del marcatore
//!    d'avvio: appena l'unit riparte, ogni diagnosi attiva diventa `resolved`.
//!    Anche qui: nascita del processo = guarigione.
//!
//! ## Il contratto (la parte 3 del modello: chiusura su verifica oggettiva)
//!
//! La rilevazione e' CERTA e GENERICA (bind error del SO, exit code, stato
//! strutturato del servizio) e la diagnosi e' delegata all'AI. Questo modulo non
//! aggiunge un solo controllo sulla VARIANTE osservata — niente confronto fra
//! `.env`, niente conoscenza di `VITE_PORT` o di vite: inseguire le varianti a
//! codice e' la strada persa, e una nuova variante troverebbe di nuovo il ciclo
//! muto. Si misura invece cio' che vale per QUALUNQUE variante:
//!
//! - **servizio web** (ha almeno una porta ALLOCATA a lui in
//!   `nexus_port_allocations`, legata dall'identita' `service_unit`): l'unit deve
//!   essere in stato `Running` e almeno una delle SUE porte deve RISPONDERE.
//! - **worker senza porta**: la sola liveness, che e' quanto si puo' misurare
//!   di un processo che non serve nulla.
//!
//! E per entrambi, due condizioni sul TEMPO, che sono la vera differenza
//! rispetto al segnale sostituito:
//!
//! - la conformita' deve **durare** [`STABILITY_SECONDS`] ininterrotti. "E'
//!   partito" si misura in un istante, "funziona" no: un processo che risponde
//!   al secondo 2 e muore al secondo 8 supera qualunque controllo istantaneo;
//! - deve **reggere un ulteriore riavvio**. Non e' zelo: nel caso reale il
//!   servizio e' morto proprio al secondo avvio, perche' il primo era riuscito
//!   su una porta che in quel momento era libera. Un solo avvio riuscito non
//!   distingue una configurazione sana da una che funziona per caso.
//!
//! "Esiste un processo con quel nome" NON e' il contratto, e non lo e' nemmeno
//! "il riavvio e' stato eseguito".
//!
//! ## Perche' "almeno una" delle porte allocate, e non tutte
//!
//! E' lo STESSO criterio con cui l'observer APRE la diagnosi
//! (`service_observer::any_port_listening`). Un criterio di chiusura piu' lasco
//! di quello di apertura chiude cio' che verrebbe riaperto al ciclo dopo; uno
//! piu' severo tiene aperto cio' che l'observer considera sano. In entrambi i
//! casi il ciclo oscilla. La grandezza misurata deve essere la stessa.
//!
//! ## Regola M
//!
//! Ogni segnale qui e' strutturato: [`ServiceState`] dal punto unico
//! [`super::service_manager`] (che su Windows riconcilia `process_alive` +
//! identita' del PID, su Linux legge `is-active`), lo status HTTP della risposta,
//! l'esito `acted` del riavvio. Nessuna decisione nasce dal testo dei log o dalla
//! prosa dell'agente: i log entrano solo nell'EVIDENZA, per la diagnosi umana e
//! per il prompt dell'AI.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::json;
use uuid::Uuid;

use super::service_manager::{self, ServiceBackend, ServiceState};
use crate::AppState;

/// Attesa massima (secondi) perche' un servizio appena riavviato diventi sano,
/// default del setting `agent.remediation.verify_readiness_seconds`.
const DEFAULT_READINESS_SECONDS: i64 = 45;
/// Intervallo fra due osservazioni durante l'attesa.
const OBSERVE_INTERVAL_MS: u64 = 2_000;
/// Per quanto la conformita' deve DURARE, ininterrotta, prima di valere.
///
/// E' il cuore della differenza fra questo contratto e il segnale che
/// sostituisce: "e' partito" si misura in un istante, "funziona" no. Vale per
/// tutti — un servizio web che risponde al secondo 2 e muore al secondo 8 non e'
/// riparato piu' di un worker che fa lo stesso.
const STABILITY_SECONDS: u64 = 15;
/// Timeout della prova TCP (stesso ordine di `service_observer::any_port_listening`).
const PROBE_TCP_TIMEOUT_MS: u64 = 400;
/// Timeout della prova HTTP: piu' lungo del TCP perche' comprende la risposta.
const PROBE_HTTP_TIMEOUT_MS: u64 = 2_000;
/// Caratteri di log conservati nell'evidenza.
const LOG_TAIL_CHARS: usize = 1_500;
/// Cap dei listener elencati nell'evidenza.
const MAX_LISTENERS_SHOWN: usize = 20;
/// Durata (ms) della notifica di esito: un fallimento resta piu' a lungo,
/// perche' chiede un intervento.
const NOTIFY_TTL_OK_MS: u64 = 15_000;
const NOTIFY_TTL_FAIL_MS: u64 = 60_000;

/// Le due fasi del contratto. Sono un vocabolario, non due stringhe: compaiono
/// nel [`RecoveryFailure`], nell'evidenza scritta sulla diagnosi e nei test, e
/// devono nominare la stessa cosa in tutti e tre.
const FASE_PRIMO_AVVIO: &str = "primo avvio dopo il rimedio";
const FASE_RIAVVIO_CONFERMA: &str = "riavvio di conferma";

/// Stato di una diagnosi con un rimedio IN CORSO. Ogni scrittura d'esito lo pone
/// come condizione: si chiude solo cio' che era davvero in verifica, e mai due
/// volte.
const STATO_DIAGNOSING: &str = "diagnosing";

/// Cosa ha risposto una porta (segnale strutturato, regola M).
///
/// La prova HTTP e' preferita perche' porta con se' uno status: distingue un
/// servizio che PARLA da una porta semplicemente aperta. Il ripiego TCP non e'
/// una toppa: un servizio di progetto puo' legittimamente non essere HTTP (un
/// socket, un DB di sviluppo), e pretendere HTTP da tutti produrrebbe falsi
/// fallimenti — cioe' lo stesso genere di bugia, col segno opposto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortAnswer {
    /// Ha risposto in HTTP con questo status. QUALUNQUE status vale come
    /// risposta: `404` sulla radice di un'API e' un servizio vivo. Non si usa
    /// qui la famiglia 2xx del final gate — vedi la nota sul riuso, in fondo.
    Http { status: u16 },
    /// Accetta connessioni TCP ma non ha risposto in HTTP (servizio non-web).
    Tcp,
    /// Nessuno in ascolto: la porta e' muta.
    Silence,
}

impl PortAnswer {
    /// `true` se qualcuno serve quella porta.
    pub(crate) fn answered(self) -> bool {
        !matches!(self, PortAnswer::Silence)
    }

    fn describe(self) -> String {
        match self {
            PortAnswer::Http { status } => format!("HTTP {status}"),
            PortAnswer::Tcp => "TCP aperta (nessuna risposta HTTP)".to_string(),
            PortAnswer::Silence => "MUTA (nessuno in ascolto)".to_string(),
        }
    }
}

/// Prova su una porta allocata all'unit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PortObservation {
    pub(crate) port: u16,
    pub(crate) answer: PortAnswer,
}

/// I TRE soli fatti su cui si decide (regola M). Tutto il resto raccolto da
/// [`collect_service_facts`] e' evidenza per gli umani e per l'AI, e non entra
/// mai nel giudizio.
#[derive(Debug, Clone)]
pub(crate) struct ServiceHealth {
    /// Stato normalizzato dal punto unico [`ServiceBackend::list`].
    pub(crate) state: ServiceState,
    /// Prove sulle porte ALLOCATE a questa unit. Vuoto = servizio senza porta
    /// (worker): il contratto ricade sulla sola liveness sostenuta.
    pub(crate) ports: Vec<PortObservation>,
    /// La conformita' e' DURATA almeno [`STABILITY_SECONDS`] senza interruzioni.
    /// Non e' un'interpretazione: e' cio' che l'osservazione ripetuta ha
    /// misurato, prodotto da [`stable_enough`].
    pub(crate) stable: bool,
}

impl ServiceHealth {
    /// `true` se il servizio ha una porta propria (contratto "web").
    fn is_web(&self) -> bool {
        !self.ports.is_empty()
    }

    /// Conformita' ISTANTANEA: vivo, e — se ha porte proprie — almeno una che
    /// risponde. E' quanto si puo' dire di una singola osservazione, ed e'
    /// esattamente quanto NON basta a chiudere.
    fn is_conforming(&self) -> bool {
        if self.state != ServiceState::Running {
            return false;
        }
        !self.is_web() || self.ports.iter().any(|p| p.answer.answered())
    }

    /// Il contratto: conforme, e per un tempo che non sia un istante.
    fn meets_contract(&self) -> bool {
        self.is_conforming() && self.stable
    }

    fn silent_ports(&self) -> Vec<u16> {
        self.ports
            .iter()
            .filter(|p| !p.answer.answered())
            .map(|p| p.port)
            .collect()
    }
}

/// Il fatto "e' durata abbastanza", separato dall'orologio che lo misura: la
/// funzione riceve la durata gia' trascorsa, non la legge. Cosi' il criterio si
/// esercita senza aspettare quindici secondi veri, e chi lo esercita passa
/// comunque dal produttore del fatto invece di scriverselo (regola O).
///
/// `None` = la conformita' non e' mai iniziata, o si e' interrotta.
pub(crate) fn stable_enough(conforming_for: Option<Duration>) -> bool {
    conforming_for.is_some_and(|d| d >= Duration::from_secs(STABILITY_SECONDS))
}

/// Una fase del contratto: un riavvio seguito da un'osservazione.
#[derive(Debug, Clone)]
pub(crate) struct RecoveryPhase {
    /// Etichetta della fase, per l'evidenza ("primo avvio" / "riavvio di prova").
    pub(crate) label: &'static str,
    /// Esito STRUTTURATO del riavvio (`ServiceActionOutcome::acted`): `false`
    /// significa che non e' stato riavviato NULLA, e su un nulla non si puo'
    /// affermare una guarigione.
    pub(crate) restarted: bool,
    /// Salute osservata a fine attesa. `None` se il riavvio non e' avvenuto.
    pub(crate) health: Option<ServiceHealth>,
}

/// Perche' il contratto non e' stato soddisfatto (strutturato, cosi' il
/// chiamante non deve leggere una frase per sapere cosa e' successo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryFailure {
    /// Il backend non ha riavviato nulla (unit sconosciuta, DB progetto assente).
    RestartNotPerformed { phase: &'static str },
    /// Il servizio non e' vivo a fine finestra.
    ServiceNotRunning {
        phase: &'static str,
        state: ServiceState,
    },
    /// Il servizio e' vivo ma NESSUNA delle porte allocate a lui risponde.
    AllocatedPortsSilent {
        phase: &'static str,
        ports: Vec<u16>,
    },
    /// Ha risposto, ma non ha retto la finestra: parte e muore.
    NotStable { phase: &'static str },
}

impl RecoveryFailure {
    /// Riga di evidenza: dice COSA non risponde e SU QUALE porta, che e' quanto
    /// serve a un umano (o all'AI del giro successivo) per ripartire.
    pub(crate) fn describe(&self) -> String {
        match self {
            RecoveryFailure::RestartNotPerformed { phase } => format!(
                "{phase}: il servizio non e' stato riavviato (nessuna unit gestita con questo nome): \
                 senza un riavvio effettivo non c'e' nulla da verificare"
            ),
            RecoveryFailure::ServiceNotRunning { phase, state } => format!(
                "{phase}: a fine finestra il servizio NON e' in esecuzione (stato: {state:?}). \
                 Il processo e' nato e non e' sopravvissuto"
            ),
            RecoveryFailure::AllocatedPortsSilent { phase, ports } => {
                let elenco = ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{phase}: il processo e' vivo ma NESSUNA porta allocata a questo servizio \
                     risponde (mute: {elenco}). Il servizio non serve la porta che gli e' \
                     assegnata nel registro"
                )
            }
            RecoveryFailure::NotStable { phase } => format!(
                "{phase}: il servizio ha risposto ma non ha retto {STABILITY_SECONDS} secondi \
                 ininterrotti. Parte e muore: la causa e' ancora li'"
            ),
        }
    }
}

/// Verdetto del contratto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryVerdict {
    Recovered,
    NotRecovered(RecoveryFailure),
}

impl RecoveryVerdict {
    pub(crate) fn recovered(&self) -> bool {
        matches!(self, RecoveryVerdict::Recovered)
    }
}

/// IL CRITERIO, puro e testabile: ogni fase deve avere riavviato davvero e aver
/// prodotto una salute conforme. Basta una fase che non regge e il verdetto e'
/// negativo — con la fase e il fatto che l'hanno determinato.
///
/// PURA per costruzione: riceve solo segnali gia' prodotti (lo stato dal punto
/// unico dei servizi, le risposte da [`probe_port`]). Cosi' un test puo'
/// arrivarci passando dai produttori veri invece di fabbricare l'esito che
/// vorrebbe verificare (regola O).
pub(crate) fn judge_recovery(phases: &[RecoveryPhase]) -> RecoveryVerdict {
    for phase in phases {
        if !phase.restarted {
            return RecoveryVerdict::NotRecovered(RecoveryFailure::RestartNotPerformed {
                phase: phase.label,
            });
        }
        let Some(health) = phase.health.as_ref() else {
            return RecoveryVerdict::NotRecovered(RecoveryFailure::RestartNotPerformed {
                phase: phase.label,
            });
        };
        if health.state != ServiceState::Running {
            return RecoveryVerdict::NotRecovered(RecoveryFailure::ServiceNotRunning {
                phase: phase.label,
                state: health.state,
            });
        }
        if health.is_web() && !health.is_conforming() {
            return RecoveryVerdict::NotRecovered(RecoveryFailure::AllocatedPortsSilent {
                phase: phase.label,
                ports: health.silent_ports(),
            });
        }
        if !health.stable {
            return RecoveryVerdict::NotRecovered(RecoveryFailure::NotStable { phase: phase.label });
        }
    }
    RecoveryVerdict::Recovered
}

// ── Prove sulle porte ────────────────────────────────────────────────────────

/// Client HTTP delle prove: nessun redirect seguito (interessa lo status che
/// DA' QUESTO servizio, non dove rimanda) e timeout corto.
fn probe_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(PROBE_HTTP_TIMEOUT_MS))
            .build()
            .unwrap_or_default()
    })
}

/// Prova UNA porta su loopback: prima HTTP (piu' informativo), poi TCP.
///
/// Il TCP e' interrogato solo se l'HTTP non ha prodotto uno status: una porta
/// che non parla HTTP non e' per questo morta.
pub(crate) async fn probe_port(port: u16) -> PortAnswer {
    let url = format!("http://127.0.0.1:{port}/");
    if let Ok(resp) = probe_client().get(&url).send().await {
        return PortAnswer::Http {
            status: resp.status().as_u16(),
        };
    }
    if super::port_recovery::tcp_probe(port, PROBE_TCP_TIMEOUT_MS).await {
        return PortAnswer::Tcp;
    }
    PortAnswer::Silence
}

// ── Raccolta dei fatti ───────────────────────────────────────────────────────

/// Fotografia GREZZA di un servizio: i due fatti su cui si decide
/// ([`ServiceHealth`]) piu' il contorno che serve a diagnosticare.
///
/// Un solo raccoglitore per due consumatori (regola L): il contratto, che ne
/// guarda la sola `health`, e il prompt di auto-debug, che ne rende tutto. La
/// ragione e' nel difetto: l'AI riceveva unit + log e doveva riscoprire un fatto
/// alla volta (quali porte le spettano, chi le occupa davvero) con una chiamata
/// tool per volta, quando quei fatti erano gia' tutti leggibili qui.
#[derive(Debug, Clone)]
pub(crate) struct ServiceFacts {
    pub(crate) unit: String,
    pub(crate) health: ServiceHealth,
    /// L'unit e' fra i servizi enumerati dal backend.
    pub(crate) known: bool,
    pub(crate) command: Option<String>,
    pub(crate) working_dir: Option<String>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) pid: Option<i32>,
    pub(crate) log_tail: String,
    /// Bucket di porte del progetto (estremi inclusivi, punto unico
    /// `nexus_tool_kit::ports`).
    pub(crate) bucket: (u16, u16),
    /// TUTTE le allocazioni del progetto: (porta, label, unit legata).
    pub(crate) allocations: Vec<(i32, String, Option<String>)>,
    /// Chi ascolta ORA nel bucket del progetto: (porta, pid, programma).
    pub(crate) listeners: Vec<(u16, u32, String)>,
}

impl ServiceFacts {
    /// Blocco di FATTI, senza interpretazione: e' quanto viene consegnato
    /// all'AI al momento dell'auto-debug e quanto resta scritto sulla diagnosi
    /// quando il contratto fallisce.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        self.render_identity(&mut out);
        self.render_ports(&mut out);
        if !self.log_tail.is_empty() {
            out.push_str(&format!("Ultime righe di log:\n{}\n", self.log_tail));
        }
        out
    }

    /// Chi e' il servizio e in che stato: unit, stato, PID, uscita, comando e
    /// directory da cui gira.
    fn render_identity(&self, out: &mut String) {
        out.push_str(&format!("Servizio: {}\n", self.unit));
        out.push_str(&format!(
            "Stato osservato: {:?}{}\n",
            self.health.state,
            if self.known {
                ""
            } else {
                " (unit non presente fra i servizi gestiti del progetto)"
            }
        ));
        if let Some(pid) = self.pid {
            out.push_str(&format!("PID: {pid}\n"));
        }
        if let Some(code) = self.exit_code {
            out.push_str(&format!("Exit code dell'ultima uscita: {code}\n"));
        }
        if let Some(cmd) = &self.command {
            out.push_str(&format!("Comando: {cmd}\n"));
        }
        if let Some(wd) = &self.working_dir {
            out.push_str(&format!("Working dir: {wd}\n"));
        }
    }

    /// Le porte, dai tre lati che vanno letti INSIEME: quelle che il registro
    /// assegna al progetto (e a chi), cosa risponde su quelle di questo
    /// servizio, e chi le sta occupando davvero adesso. Nel caso reale la
    /// diagnosi sta proprio nel confronto fra queste tre liste.
    fn render_ports(&self, out: &mut String) {
        out.push_str(&format!(
            "Bucket porte del progetto: {}-{}\n",
            self.bucket.0, self.bucket.1
        ));
        out.push_str("Porte allocate al progetto (registro nexus_port_allocations):\n");
        if self.allocations.is_empty() {
            out.push_str("  (nessuna allocazione registrata)\n");
        }
        for (port, label, unit) in &self.allocations {
            let etichetta = if label.is_empty() { "(senza label)" } else { label };
            let unit_txt = unit.as_deref().unwrap_or("(nessuna unit legata)");
            let mia = if unit.as_deref() == Some(self.unit.as_str()) {
                "  <-- allocata A QUESTO servizio"
            } else {
                ""
            };
            out.push_str(&format!("  {port} -> {etichetta} [{unit_txt}]{mia}\n"));
        }
        out.push_str("Prove sulle porte allocate a questo servizio:\n");
        if self.health.ports.is_empty() {
            out.push_str("  (nessuna porta allocata a questa unit: servizio senza porta)\n");
        }
        for p in &self.health.ports {
            out.push_str(&format!("  {} -> {}\n", p.port, p.answer.describe()));
        }
        out.push_str("In ascolto ORA nel bucket del progetto:\n");
        if self.listeners.is_empty() {
            out.push_str("  (nessun listener nel bucket)\n");
        }
        for (port, pid, program) in self.listeners.iter().take(MAX_LISTENERS_SHOWN) {
            out.push_str(&format!("  {port} <- pid {pid} ({program})\n"));
        }
    }
}

/// Raccoglie i fatti correnti di un servizio di progetto.
///
/// Le porte ATTESE vengono dal punto unico `service_observer::ports_for_unit`
/// (registro `nexus_port_allocations` legato per `service_unit`, piu' su Linux
/// le `Environment=` dell'unit): e' la stessa fonte che l'observer usa per
/// decidere che un servizio e' giu'. Se la chiusura interrogasse un'altra fonte,
/// aprirebbe e chiuderebbe su due idee diverse di "la sua porta".
pub(crate) async fn collect_service_facts(
    state: &AppState,
    project_id: Uuid,
    unit: &str,
) -> ServiceFacts {
    let target = ServiceRef::resolve(state, project_id, unit).await;
    facts_for(state, &target).await
}

/// Come [`collect_service_facts`], per chi ha gia' risolto l'identita' del
/// servizio (l'attesa la risolve una volta e la riusa a ogni giro).
async fn facts_for(state: &AppState, target: &ServiceRef) -> ServiceFacts {
    // `stable: false` per costruzione: una fotografia SINGOLA non sa nulla della
    // durata, e non deve poter sembrare che lo sappia. Il giudizio non passa mai
    // di qui — usa la salute misurata da `await_contract`, che la durata l'ha
    // osservata davvero; questi fatti servono all'evidenza.
    let health = observe_health(state, target, None).await;
    let known = health.state != ServiceState::Unknown;
    let project_id = target.project_id;

    let (command, working_dir, exit_code, pid, log_tail) =
        process_facts(state, project_id, &target.short).await;
    let bucket = nexus_tool_kit::ports::project_bucket_range(&project_id);
    let allocations = sqlx::query_as::<_, (i32, String, Option<String>)>(
        "SELECT port, label, service_unit FROM nexus_port_allocations \
         WHERE project_id = $1 ORDER BY port",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let listeners = service_manager::active()
        .listening_ports()
        .await
        .into_iter()
        .filter(|l| l.port >= bucket.0 && l.port <= bucket.1)
        .map(|l| (l.port, l.pid, l.program))
        .collect();

    ServiceFacts {
        unit: target.unit.clone(),
        health,
        known,
        command,
        working_dir,
        exit_code,
        pid,
        log_tail,
        bucket,
        allocations,
        listeners,
    }
}

/// Identita' di un servizio di progetto, risolta UNA volta e riusata a ogni
/// osservazione: unit completa, etichetta corta, slug di servizio e radice del
/// progetto.
///
/// Sta insieme perche' va usata insieme: il backend Linux enumera i servizi dai
/// file unit SOTTO la radice del progetto (`list_services_fallback(slug, root)`),
/// quindi un contesto costruito con una radice vuota — che su Windows non si
/// noterebbe, visto che li' l'enumerazione viene dal DB — renderebbe
/// l'osservazione cieca proprio dove il servizio esiste.
struct ServiceRef {
    project_id: Uuid,
    unit: String,
    slug: String,
    short: String,
    root: std::path::PathBuf,
}

impl ServiceRef {
    async fn resolve(state: &AppState, project_id: Uuid, unit: &str) -> Self {
        let (name, root) = project_name_and_root(state, project_id).await;
        let slug = super::services::project_service_slug(&name);
        let short = short_label(unit, &slug);
        Self {
            project_id,
            unit: unit.to_string(),
            slug,
            short,
            root: std::path::PathBuf::from(root),
        }
    }

    fn ctx<'a>(&'a self, state: &'a AppState) -> service_manager::ServiceContext<'a> {
        service_manager::ServiceContext {
            db: &state.db,
            port_registry: Some(&state.port_registry),
            project_id: self.project_id,
            slug: &self.slug,
            project_root: &self.root,
        }
    }
}

/// UNA osservazione dei soli fatti su cui si decide: stato normalizzato del
/// servizio e risposta di ciascuna porta allocata a lui. Leggera apposta —
/// l'attesa la ripete ogni pochi secondi, mentre il contorno diagnostico
/// (listener dell'host, comando, log) si raccoglie una volta sola alla fine.
///
/// `conforming_for` e' da quanto la conformita' dura ininterrotta secondo il
/// chiamante; `None` quando non c'e' una serie di osservazioni alle spalle.
async fn observe_health(
    state: &AppState,
    target: &ServiceRef,
    conforming_for: Option<Duration>,
) -> ServiceHealth {
    let ctx = target.ctx(state);
    let entry = service_manager::active()
        .list(&ctx)
        .await
        .into_iter()
        .find(|e| e.id == target.unit || e.label == target.short);
    let expected_ports =
        super::service_observer::ports_for_unit(&state.db, target.project_id, &target.unit).await;
    let mut ports = Vec::with_capacity(expected_ports.len());
    for port in expected_ports {
        ports.push(PortObservation {
            port,
            answer: probe_port(port).await,
        });
    }
    ServiceHealth {
        state: entry.map(|e| e.state).unwrap_or(ServiceState::Unknown),
        ports,
        stable: stable_enough(conforming_for),
    }
}

/// Nome e root del progetto (vuoti se il progetto non e' leggibile: i fatti
/// restano parziali, mai inventati).
async fn project_name_and_root(state: &AppState, project_id: Uuid) -> (String, String) {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT name, repository_root_path FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    match row {
        Some((name, root)) => (name, root.unwrap_or_default()),
        None => (String::new(), String::new()),
    }
}

/// Etichetta corta di un'unit `{slug}-{label}.service` (stessa derivazione di
/// `restart_project_unit`).
fn short_label(unit: &str, slug: &str) -> String {
    unit.strip_prefix(&format!("{slug}-"))
        .unwrap_or(unit)
        .strip_suffix(".service")
        .unwrap_or(unit)
        .to_string()
}

/// Comando, working dir, exit code, pid e coda di log del processo che serve il
/// servizio, dalla riga `agent_processes` piu' recente per quella label.
///
/// E' la fonte dei servizi di progetto su Windows (l'ambiente canonico); su
/// Linux, dove i servizi sono unit systemd, la riga non esiste e i campi restano
/// `None` — dichiarati assenti nell'evidenza, non riempiti con supposizioni.
async fn process_facts(
    state: &AppState,
    project_id: Uuid,
    short: &str,
) -> (Option<String>, Option<String>, Option<i32>, Option<i32>, String) {
    let Ok(pool) = crate::project_db_routes::project_data_pool_from(&state.db, project_id).await
    else {
        return (None, None, None, None, String::new());
    };
    let row: Option<(String, Option<String>, Option<i32>, Option<i32>, String, String)> =
        sqlx::query_as(
            "SELECT command, working_dir, pid, exit_code, output, error_output \
             FROM agent_processes \
             WHERE project_id = $1 AND label = $2 AND kind = 'service' \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(project_id)
        .bind(short)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();
    let Some((command, working_dir, pid, exit_code, output, error_output)) = row else {
        return (None, None, None, None, String::new());
    };
    let mut log = output;
    if !error_output.is_empty() {
        if !log.is_empty() {
            log.push('\n');
        }
        log.push_str(&error_output);
    }
    (
        Some(command).filter(|c| !c.is_empty()),
        working_dir.filter(|w| !w.is_empty()),
        exit_code,
        pid,
        tail_chars(&log, LOG_TAIL_CHARS),
    )
}

/// Coda di `max` caratteri (mai byte: spezzerebbe l'UTF-8).
fn tail_chars(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    s.chars().skip(total - max).collect()
}

// ── Il ciclo: riavvia, osserva, riavvia, osserva, giudica ────────────────────

/// Chiude il ciclo di una remediation di servizio verificandone l'esito
/// OSSERVABILE. E' il punto in cui la catena rileva -> ripara -> chiude smette
/// di credere alla nascita di un processo.
///
/// Due fasi, entrambe "riavvio + attesa + osservazione". La seconda esiste
/// perche' il caso reale e' morto al secondo avvio: un servizio la cui
/// configurazione e' ancora incoerente puo' partire una volta e non la
/// successiva. Se la prima fase gia' non regge si esce subito, senza infierire
/// con un secondo riavvio inutile.
pub(crate) async fn restart_and_verify(
    state: &AppState,
    project_id: Uuid,
    unit: &str,
) -> (RecoveryVerdict, ServiceFacts) {
    let readiness = readiness_window(state).await;
    let target = ServiceRef::resolve(state, project_id, unit).await;
    let mut phases: Vec<RecoveryPhase> = Vec::with_capacity(2);

    for label in [FASE_PRIMO_AVVIO, FASE_RIAVVIO_CONFERMA] {
        // Riavvio non avvenuto: la fase si chiude qui e senza salute. Osservare
        // un servizio che non e' stato riavviato direbbe qualcosa del suo stato
        // precedente, non di questo rimedio.
        let restarted = super::services::restart_project_unit(state, project_id, unit).await;
        if !restarted {
            phases.push(RecoveryPhase {
                label,
                restarted: false,
                health: None,
            });
            break;
        }
        // Una fase che non regge rende inutile la successiva: si esce con quanto
        // misurato, che e' gia' la ragione del verdetto.
        let health = await_contract(state, &target, readiness).await;
        let met = health.meets_contract();
        phases.push(RecoveryPhase {
            label,
            restarted: true,
            health: Some(health),
        });
        if !met {
            break;
        }
    }

    let verdict = judge_recovery(&phases);
    // I fatti completi si raccolgono UNA volta, a giudizio dato: servono
    // all'evidenza, non alla decisione (che ha gia' la sua salute misurata).
    let facts = facts_for(state, &target).await;
    (verdict, facts)
}

/// Osserva ripetutamente finche' il contratto e' soddisfatto o si esaurisce il
/// tempo, e ritorna la salute dell'ULTIMA osservazione.
///
/// Due orologi distinti, che rispondono a due domande diverse:
///
/// - `readiness` limita l'attesa che il servizio DIVENTI conforme (un avvio puo'
///   essere lento: build, migrazioni, warm-up);
/// - [`STABILITY_SECONDS`] misura da quanto lo E' senza interruzioni. Ogni
///   caduta azzera il conteggio: la finestra va superata di fila, altrimenti un
///   servizio che oscilla si dichiarerebbe guarito nel momento buono.
///
/// Il tetto assoluto (`readiness + STABILITY`) esiste perche' un servizio che
/// entra ed esce dalla conformita' non tenga occupato il verificatore per sempre:
/// scaduto quello, l'ultima osservazione parla da se' e — non avendo maturato la
/// stabilita' — non chiude.
async fn await_contract(
    state: &AppState,
    target: &ServiceRef,
    readiness: Duration,
) -> ServiceHealth {
    let start = Instant::now();
    let stability = Duration::from_secs(STABILITY_SECONDS);
    let mut conforming_since: Option<Instant> = None;
    loop {
        let conforming_for = conforming_since.map(|t| t.elapsed());
        let health = observe_health(state, target, conforming_for).await;
        if health.is_conforming() {
            if health.stable {
                return health;
            }
            conforming_since.get_or_insert_with(Instant::now);
        } else {
            conforming_since = None;
            if start.elapsed() >= readiness {
                return health; // non e' mai diventato conforme entro la finestra
            }
        }
        if start.elapsed() >= readiness + stability {
            return health;
        }
        tokio::time::sleep(Duration::from_millis(OBSERVE_INTERVAL_MS)).await;
    }
}

/// Finestra di readiness dal DB (regola G): `agent.remediation.verify_readiness_seconds`.
async fn readiness_window(state: &AppState) -> Duration {
    let secs = crate::settings::get_setting(&state.db, "agent.remediation.verify_readiness_seconds")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_READINESS_SECONDS)
        .max(STABILITY_SECONDS as i64);
    Duration::from_secs(secs as u64)
}

// ── Scrittura dell'esito ─────────────────────────────────────────────────────

/// Applica il verdetto a una diagnosi in `diagnosing` e ritorna lo stato scritto.
///
/// PUNTO UNICO della scrittura dell'esito di una remediation di servizio: chi
/// verifica e chi scrive sono la stessa catena, cosi' non puo' esistere un
/// percorso che chiude senza aver verificato. Un verdetto negativo NON chiude:
/// porta la diagnosi in `failed_remediation` (stato terminale gia' in uso dal
/// gemello sulle violazioni risorse) e le allega l'evidenza — cosa non risponde,
/// su quale porta — perche' il problema resti visibile e diagnosticabile nel
/// pannello Problemi invece di sparire come risolto.
pub(crate) async fn apply_recovery_verdict(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    verdict: &RecoveryVerdict,
    evidence: &str,
) -> Option<String> {
    match verdict {
        RecoveryVerdict::Recovered => sqlx::query_scalar::<_, String>(
            "UPDATE service_diagnoses \
                SET status = 'resolved', resolved_at = NOW(), updated_at = NOW() \
              WHERE id = $1 AND status = $2 \
              RETURNING status",
        )
        .bind(diagnosis_id)
        .bind(STATO_DIAGNOSING)
        .fetch_optional(db)
        .await
        .ok()
        .flatten(),
        RecoveryVerdict::NotRecovered(failure) => {
            mark_failed_remediation(db, diagnosis_id, failure, evidence).await
        }
    }
}

/// Stato terminale con l'evidenza in coda al `detail`: il problema resta nel
/// pannello e porta con se' cosa non risponde e su quale porta. In coda e non al
/// posto del testo esistente, perche' la diagnosi dell'AI e il log del guasto
/// restano il contesto che serve a leggerlo.
async fn mark_failed_remediation(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    failure: &RecoveryFailure,
    evidence: &str,
) -> Option<String> {
    let detail = format!(
        "VERIFICA DEL RIMEDIO FALLITA: {}\n\n{evidence}",
        failure.describe()
    );
    sqlx::query_scalar::<_, String>(
        "UPDATE service_diagnoses \
            SET status = 'failed_remediation', updated_at = NOW(), \
                detail = COALESCE(detail, '') || E'\\n\\n' || $3 \
          WHERE id = $1 AND status = $2 \
          RETURNING status",
    )
    .bind(diagnosis_id)
    .bind(STATO_DIAGNOSING)
    .bind(&detail)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Riporta a `open` le diagnosi di crash rimaste in `diagnosing` con il proprio
/// run di rimedio MORTO.
///
/// La verifica del contratto vive in un task IN MEMORIA: se mcp-core viene
/// riavviato mentre un rimedio e' in volo, quel task sparisce e nessuno chiudera'
/// piu' la diagnosi — da quando `resolve_open_crashes` non tocca piu' i
/// `diagnosing` (era proprio quella la chiusura bugiarda), resterebbe appesa per
/// sempre. `open` e non `failed_remediation`: il rimedio non ha fallito, e' stato
/// INTERROTTO; tornando aperta, l'observer la rivaluta col suo ciclo normale e
/// puo' ri-triggerarla. Il criterio "il rimedio e' ancora vivo?" e' il punto
/// unico condiviso col gemello sulle violazioni risorse (regola L).
pub(crate) async fn reap_interrupted_service_remediations(state: &AppState) {
    let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, project_id, triggered_run_id FROM service_diagnoses \
          WHERE signal_kind = 'crash' AND status = $1 \
            AND triggered_run_id IS NOT NULL",
    )
    .bind(STATO_DIAGNOSING)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    // Una riga per volta, e non una query sola: lo stato del run vive nel DB DEL
    // PROGETTO, che e' un database diverso per ciascun progetto (separazione dei
    // pool). Non c'e' JOIN possibile fra la diagnosi, che sta sul meta, e il run
    // che la giustifica. Le righe qui sono quelle rimaste appese a un riavvio di
    // mcp-core: unita', non migliaia.
    for (diag_id, project_id, run_id) in rows {
        reopen_if_remediation_dead(state, diag_id, project_id, run_id).await;
    }
}

/// Riapre UNA diagnosi se il run che la teneva in `diagnosing` non e' piu' vivo.
/// Estratta dal ciclo perche' il corpo resti leggibile e la ragione per cui la
/// domanda si pone riga per riga (vedi sopra) stia in un posto solo.
async fn reopen_if_remediation_dead(
    state: &AppState,
    diag_id: Uuid,
    project_id: Uuid,
    run_id: Uuid,
) {
    let vivo =
        super::resource_violation_remediation::remediation_run_is_active(state, project_id, run_id)
            .await;
    if vivo {
        return; // il task di verifica del run corrente se ne occupera'
    }
    let riaperta: Option<Uuid> = sqlx::query_scalar(
        "UPDATE service_diagnoses SET status = 'open', updated_at = NOW() \
          WHERE id = $1 AND status = $2 RETURNING id",
    )
    .bind(diag_id)
    .bind(STATO_DIAGNOSING)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if let Some(id) = riaperta {
        tracing::info!(
            project_id = %project_id, run_id = %run_id, diagnosis_id = %id,
            "service_recovery: rimedio interrotto (run terminato senza verifica), diagnosi riaperta"
        );
        crate::project_workspace::logs::emit_problems_panel_refresh(project_id, vec![id]);
    }
}

/// Testo, gravita' e durata della notifica dell'esito. Un fallimento resta piu'
/// a lungo sullo schermo e dice gia' nel titolo COSA non ha funzionato: e' la
/// prima cosa che un umano legge quando una riparazione non ha riparato.
fn notification_for(unit: &str, verdict: &RecoveryVerdict) -> (&'static str, String, u64) {
    match verdict {
        RecoveryVerdict::Recovered => (
            "success",
            format!("Riparazione automatica di {unit} verificata: il servizio risponde"),
            NOTIFY_TTL_OK_MS,
        ),
        RecoveryVerdict::NotRecovered(failure) => (
            "error",
            format!(
                "Riparazione automatica di {unit} NON verificata: {}",
                failure.describe()
            ),
            NOTIFY_TTL_FAIL_MS,
        ),
    }
}

/// Notifica + audit dell'esito di una remediation di servizio. Separata dalla
/// scrittura perche' la scrittura sia testabile su un pool nudo, senza bus
/// eventi ne' `AppState`.
pub(crate) fn announce_recovery(
    project_id: Uuid,
    unit: &str,
    verdict: &RecoveryVerdict,
    diagnosis_id: Option<Uuid>,
) {
    let (severity, message, ttl) = notification_for(unit, verdict);
    nexus_events::dispatcher::emit_global(
        project_id,
        nexus_events::event::ProjectEvent::Notification {
            severity: severity.to_string(),
            message,
            panel: Some("problems".to_string()),
            ttl_ms: Some(ttl),
            run_id: None,
        },
    );
    if let Some(id) = diagnosis_id {
        crate::project_workspace::logs::emit_problems_panel_refresh(project_id, vec![id]);
    }
    crate::security::record_audit(crate::security::AuditEntry {
        project_id,
        actor: "system",
        actor_user_id: None,
        actor_session_id: None,
        action: "service_remediation_verify".to_string(),
        resource_kind: "port",
        resource_id: None,
        outcome: if verdict.recovered() {
            "allowed"
        } else {
            "failed"
        },
        details: json!({
            "unit": unit,
            "verdict": format!("{verdict:?}"),
        }),
    });
}

// ── Nota sul riuso della primitiva di probe (regola L, punto 4 del mandato) ──
//
// `nexus_agent_graph::decisions::endpoint_probes` e' il punto unico di "quali
// chiamate HTTP prova il final gate": normalizza gli endpoint DICHIARATI
// dall'agente (`task_complete.endpoints`) in `CriterionSpec`, con metodo, corpo e
// status attesi. Non e' riusabile qui, per tre ragioni che riguardano la domanda,
// non la comodita':
//
//  1. la fonte. La', gli URL li dichiara l'agente; qui non c'e' alcuna
//     dichiarazione: l'unica cosa nota e' la porta che il REGISTRO assegna a
//     quell'unit. Non c'e' niente da normalizzare.
//  2. il criterio di successo. `DEFAULT_SUCCESS_STATUSES` (2xx) risponde a
//     "questo endpoint FUNZIONA". Qui la domanda e' "c'e' un servizio che serve
//     questa porta", e un 404 sulla radice di un'API la soddisfa in pieno.
//     Riusare quella lista trasformerebbe ogni backend senza rotta di root in un
//     rimedio fallito.
//  3. l'esecutore. Il runner di quei criteri
//     (`agent_graph_adapter::criteria_runner`) e' costruito su un
//     `Arc<dyn ToolExecutor>`, che esiste solo dentro un run agentico: un worker
//     di remediation non ne ha uno, e fabbricarne uno finto per una GET sarebbe
//     una deformazione di entrambi i lati.
//
// Cio' che invece si riusa davvero e' il principio (regola M): la decisione e'
// lo STATUS, il corpo non entra mai nel giudizio. E il TCP di ripiego non e' un
// secondo prober: e' il punto unico gia' esistente
// `port_recovery::tcp_probe`, lo stesso che usa l'observer.

#[cfg(test)]
mod tests {
    use super::*;

    /// Listener TCP effimero che risponde HTTP e poi chiude. Ritorna la porta.
    /// Serve a far nascere le [`PortAnswer`] dal PRODUTTORE vero ([`probe_port`])
    /// invece di scriverle a mano nel test: e' l'unico modo perche' il criterio
    /// venga esercitato sul segnale che vedra' in produzione (regola O).
    /// Status-line del servizio sano usato nelle prove.
    const STATUS_200: &str = "200 OK";
    /// Byte di richiesta letti prima di rispondere: basta la request-line.
    const RICHIESTA_MAX_BYTES: usize = 1024;

    async fn servizio_vivo_su_porta_effimera(status_line: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind effimero");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            // Due richieste: la prova della prima fase e quella della seconda.
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; RICHIESTA_MAX_BYTES];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        port
    }

    /// Una porta su cui nessuno ascolta: si prende una porta effimera e la si
    /// lascia chiudere, cosi' il numero e' certamente libero.
    async fn porta_muta() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind effimero");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    /// Salute con la conformita' DURATA a sufficienza: `stable` non e' scritto a
    /// mano, viene da [`stable_enough`] — lo stesso produttore che lo calcola in
    /// `await_contract` — e le risposte delle porte da [`probe_port`] su
    /// listener veri.
    async fn salute(state: ServiceState, porte: &[u16]) -> ServiceHealth {
        salute_durata(state, porte, Some(Duration::from_secs(STABILITY_SECONDS + 5))).await
    }

    async fn salute_durata(
        state: ServiceState,
        porte: &[u16],
        conforme_da: Option<Duration>,
    ) -> ServiceHealth {
        let mut ports = Vec::new();
        for &port in porte {
            ports.push(PortObservation {
                port,
                answer: probe_port(port).await,
            });
        }
        ServiceHealth {
            state,
            ports,
            stable: stable_enough(conforme_da),
        }
    }

    fn fase(label: &'static str, health: ServiceHealth) -> RecoveryPhase {
        RecoveryPhase {
            label,
            restarted: true,
            health: Some(health),
        }
    }

    #[tokio::test]
    async fn la_prova_distingue_una_porta_servita_da_una_muta() {
        let viva = servizio_vivo_su_porta_effimera(STATUS_200).await;
        assert!(
            matches!(probe_port(viva).await, PortAnswer::Http { status: 200 }),
            "un servizio che risponde deve produrre uno status"
        );
        assert_eq!(
            probe_port(porta_muta().await).await,
            PortAnswer::Silence,
            "su una porta senza ascoltatori la prova deve essere muta"
        );
    }

    /// Un 404 sulla radice e' un servizio VIVO: e' la ragione per cui il
    /// contratto non riusa la famiglia 2xx del final gate.
    #[tokio::test]
    async fn uno_status_qualunque_vale_come_risposta() {
        let porta = servizio_vivo_su_porta_effimera("404 Not Found").await;
        let health = salute(ServiceState::Running, &[porta]).await;
        assert!(
            health.meets_contract(),
            "un'API senza rotta di root risponde 404 e sta servendo la sua porta"
        );
    }

    /// IL CASO MINIMO del mandato: il servizio riparte (stato Running al primo
    /// avvio) ma la configurazione e' ancora rotta e al riavvio successivo non
    /// c'e' piu' nessuno sulla sua porta. Il verdetto deve essere negativo e
    /// dire QUALE porta e' muta.
    ///
    /// MUTAZIONE: riportando la chiusura al segnale debole — cioe' facendo
    /// ritornare `RecoveryVerdict::Recovered` a `judge_recovery` sulla sola base
    /// che le fasi abbiano `restarted: true`, com'era prima ("restart eseguito"
    /// = riuscito) — questo test rosseggia sulla prima asserzione.
    #[tokio::test]
    async fn un_servizio_che_non_sopravvive_al_riavvio_non_e_riparato() {
        let porta_viva = servizio_vivo_su_porta_effimera(STATUS_200).await;
        let primo = fase(
            FASE_PRIMO_AVVIO,
            salute(ServiceState::Running, &[porta_viva]).await,
        );
        // Seconda fase: il processo e' morto, la sua porta e' muta.
        let porta = porta_muta().await;
        let secondo = fase(
            FASE_RIAVVIO_CONFERMA,
            salute(ServiceState::Failed, &[porta]).await,
        );

        let verdetto = judge_recovery(&[primo, secondo]);
        assert!(
            !verdetto.recovered(),
            "il servizio non e' sopravvissuto al secondo avvio: non e' riparato"
        );
        let RecoveryVerdict::NotRecovered(failure) = verdetto else {
            unreachable!("appena verificato che non e' Recovered");
        };
        assert_eq!(
            failure,
            RecoveryFailure::ServiceNotRunning {
                phase: FASE_RIAVVIO_CONFERMA,
                state: ServiceState::Failed,
            },
            "l'evidenza deve dire in quale fase e con quale stato"
        );
    }

    /// L'altra faccia dello stesso caso: il processo e' VIVO ma la porta che il
    /// registro gli assegna non risponde (il servizio ascolta altrove, o non
    /// ascolta affatto). E' esattamente il frontend dell'incidente, la cui porta
    /// dichiarata era quella del backend.
    #[tokio::test]
    async fn un_processo_vivo_che_non_serve_la_sua_porta_non_e_riparato() {
        let porta = porta_muta().await;
        let fasi = [fase(
            FASE_PRIMO_AVVIO,
            salute(ServiceState::Running, &[porta]).await,
        )];
        let verdetto = judge_recovery(&fasi);
        assert_eq!(
            verdetto,
            RecoveryVerdict::NotRecovered(RecoveryFailure::AllocatedPortsSilent {
                phase: FASE_PRIMO_AVVIO,
                ports: vec![porta],
            }),
            "vivo non basta: il contratto e' la porta ALLOCATA a lui che risponde"
        );
    }

    /// Un riavvio che non e' avvenuto non e' una guarigione: e' un nulla.
    #[test]
    fn senza_riavvio_non_si_afferma_nulla() {
        let verdetto = judge_recovery(&[RecoveryPhase {
            label: FASE_PRIMO_AVVIO,
            restarted: false,
            health: None,
        }]);
        assert_eq!(
            verdetto,
            RecoveryVerdict::NotRecovered(RecoveryFailure::RestartNotPerformed {
                phase: FASE_PRIMO_AVVIO,
            })
        );
    }

    /// Due fasi sane: il contratto e' soddisfatto. Senza questo, un criterio che
    /// bocciasse sempre passerebbe tutti gli altri test.
    #[tokio::test]
    async fn due_avvii_sani_soddisfano_il_contratto() {
        let porta = servizio_vivo_su_porta_effimera(STATUS_200).await;
        let fasi = [
            fase(
                FASE_PRIMO_AVVIO,
                salute(ServiceState::Running, &[porta]).await,
            ),
            fase(
                FASE_RIAVVIO_CONFERMA,
                salute(ServiceState::Running, &[porta]).await,
            ),
        ];
        assert_eq!(judge_recovery(&fasi), RecoveryVerdict::Recovered);
    }

    /// LA CONSEGUENZA, non la stringa (regola O): un verdetto negativo deve
    /// arrivare fino alla riga di `service_diagnoses` e lasciarla NON risolta,
    /// con scritto cosa non risponde. E' il caso minimo del mandato percorso per
    /// intero — porta provata dal produttore vero, criterio, scrittura sullo
    /// schema META reale.
    ///
    /// MUTAZIONE: riportando la chiusura al segnale debole (`judge_recovery` che
    /// ritorna `Recovered` perche' il riavvio e' stato eseguito), qui si scrive
    /// `resolved`, `resolved_at` si valorizza e il detail non nomina piu' alcuna
    /// porta: rosseggiano tutte e tre le asserzioni.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_riparazione_non_verificata_lascia_la_diagnosi_aperta(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        // La riga come la lascia il trigger dell'auto-debug: in `diagnosing`,
        // agganciata al run che sta riparando.
        let diagnosi: Uuid = sqlx::query_scalar(
            "INSERT INTO service_diagnoses \
                (project_id, unit, signal_kind, metric, status, detail, triggered_run_id) \
             VALUES ($1, 'gestione-spese-frontend.service', 'crash', 'port_in_use', \
                     'diagnosing', 'porta gia'' occupata', $2) \
             RETURNING id",
        )
        .bind(project)
        .bind(Uuid::new_v4())
        .fetch_one(&pool)
        .await
        .expect("seed diagnosi");

        // Il servizio e' stato riavviato e risulta vivo, ma la porta che il
        // registro gli assegna non risponde: la configurazione e' ancora rotta.
        let porta = porta_muta().await;
        let verdetto = judge_recovery(&[fase(
            FASE_PRIMO_AVVIO,
            salute(ServiceState::Running, &[porta]).await,
        )]);
        let scritto = apply_recovery_verdict(&pool, diagnosi, &verdetto, "evidenza dei fatti").await;
        assert_eq!(
            scritto.as_deref(),
            Some("failed_remediation"),
            "una riparazione che non ripara non puo' chiudersi riuscita"
        );

        let (status, detail, resolved_at): (String, Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
            sqlx::query_as("SELECT status, detail, resolved_at FROM service_diagnoses WHERE id = $1")
                .bind(diagnosi)
                .fetch_one(&pool)
                .await
                .expect("rilettura diagnosi");
        assert_eq!(status, "failed_remediation");
        assert!(
            resolved_at.is_none(),
            "non e' risolta: nessuna data di risoluzione"
        );
        let detail = detail.unwrap_or_default();
        assert!(
            detail.contains(&porta.to_string()),
            "il problema resta visibile CON l'evidenza di quale porta e' muta: {detail}"
        );
    }

    /// Il gemello positivo: un contratto soddisfatto chiude davvero. Senza,
    /// una scrittura che non chiudesse mai passerebbe il test qui sopra.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_riparazione_verificata_chiude_la_diagnosi(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let diagnosi: Uuid = sqlx::query_scalar(
            "INSERT INTO service_diagnoses (project_id, unit, signal_kind, status) \
             VALUES ($1, 'gestione-spese-frontend.service', 'crash', 'diagnosing') RETURNING id",
        )
        .bind(project)
        .fetch_one(&pool)
        .await
        .expect("seed diagnosi");

        let porta = servizio_vivo_su_porta_effimera(STATUS_200).await;
        let verdetto = judge_recovery(&[
            fase(
                FASE_PRIMO_AVVIO,
                salute(ServiceState::Running, &[porta]).await,
            ),
            fase(
                FASE_RIAVVIO_CONFERMA,
                salute(ServiceState::Running, &[porta]).await,
            ),
        ]);
        assert_eq!(
            apply_recovery_verdict(&pool, diagnosi, &verdetto, "evidenza").await.as_deref(),
            Some("resolved")
        );
        let resolved_at: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT resolved_at FROM service_diagnoses WHERE id = $1")
                .bind(diagnosi)
                .fetch_one(&pool)
                .await
                .expect("rilettura");
        assert!(resolved_at.is_some(), "una chiusura reale timbra la data");
    }

    /// Servizio senza porte (worker): il contratto e' la sola liveness, e non
    /// diventa piu' lasco per questo — uno stato diverso da Running lo boccia,
    /// e nemmeno "vivo adesso" basta se non e' durato.
    #[tokio::test]
    async fn un_worker_senza_porte_e_giudicato_sulla_liveness_sostenuta() {
        let vivo = salute(ServiceState::Running, &[]).await;
        assert!(vivo.meets_contract());
        let morto = salute(ServiceState::Stopped, &[]).await;
        assert!(!morto.meets_contract());
        let appena_nato = salute_durata(ServiceState::Running, &[], None).await;
        assert!(
            appena_nato.is_conforming() && !appena_nato.meets_contract(),
            "vivo nell'istante dello spawn e' esattamente cio' che NON basta"
        );
    }

    /// Il caso reale del mandato in forma pura: il servizio risponde e poi
    /// muore. Nessuno dei due orologi da solo lo intercetta — la conformita'
    /// istantanea dice di si', la durata dice di no.
    #[tokio::test]
    async fn parte_e_muore_non_supera_la_finestra() {
        let porta = servizio_vivo_su_porta_effimera(STATUS_200).await;
        let lampo = salute_durata(
            ServiceState::Running,
            &[porta],
            Some(Duration::from_secs(2)),
        )
        .await;
        assert!(lampo.is_conforming(), "nell'istante risponde");
        assert_eq!(
            judge_recovery(&[fase(FASE_PRIMO_AVVIO, lampo)]),
            RecoveryVerdict::NotRecovered(RecoveryFailure::NotStable {
                phase: FASE_PRIMO_AVVIO
            }),
            "due secondi di vita non sono una riparazione"
        );
    }
}
