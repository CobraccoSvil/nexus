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

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nexus_agent_graph::decisions::{path_in_scope, WriteFact};
use serde_json::json;
use uuid::Uuid;

use super::service_manager::{self, ServiceBackend, ServiceState};
use crate::AppState;

/// Attesa massima (secondi) perche' un servizio appena riavviato diventi sano,
/// default del setting `agent.remediation.verify_readiness_seconds`.
const DEFAULT_READINESS_SECONDS: i64 = 45;
/// Intervallo fra due osservazioni durante l'attesa.
const OBSERVE_INTERVAL_MS: u64 = 2_000;
/// Per quanto la conformita' deve DURARE, ininterrotta, prima di valere PER LA
/// REMEDIATION.
///
/// E' il cuore della differenza fra questo contratto e il segnale che
/// sostituisce: "e' partito" si misura in un istante, "funziona" no. Vale per
/// tutti — un servizio web che risponde al secondo 2 e muore al secondo 8 non e'
/// riparato piu' di un worker che fa lo stesso.
///
/// NON e' piu' una costante del ciclo: e' il valore che questo consumatore
/// sceglie. La durata e' diventata un PARAMETRO ([`stable_enough`],
/// [`await_port_ready`]) perche' i consumatori del criterio pongono la stessa
/// domanda con esigenze diverse, e la variante corretta e' un parametro, non un
/// secondo criterio (regola L). Chi rimedia e chi lancia una suite pretendono un
/// servizio CALDO; `run_service` chiede "e' salito?" subito dopo lo spawn, e
/// pretendere quindici secondi li' costerebbe quel tempo a ogni avvio sano per
/// rispondere a una domanda che nessuno ha posto.
pub(crate) const STABILITY_SECONDS: u64 = 15;

/// La durata di stabilita' del contratto di remediation, per i consumatori che
/// vogliono ESATTAMENTE quella e non un numero ricopiato.
pub(crate) fn stabilita_di_remediation() -> Duration {
    Duration::from_secs(STABILITY_SECONDS)
}
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
/// Diagnosi visibile e non presa in carico da nessuno: e' da qui che il presidio
/// la preleva, ed e' qui che torna un tentativo fallito che ne ha ancora.
const STATO_APERTA: &str = "open";
/// Stato terminale: si e' tentato quanto era previsto e il servizio non risponde.
/// Non e' `resolved` — il problema resta nel pannello, con l'evidenza.
const STATO_FALLITA: &str = "failed_remediation";
/// L'unico stato che timbra `resolved_at`.
const STATO_RISOLTA: &str = "resolved";

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

    pub(crate) fn describe(self) -> String {
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

    /// Riga di evidenza di cio' che si e' MISURATO, non di cio' che si conclude:
    /// lo stato e cosa ha risposto su ciascuna porta allocata. E' quanto resta
    /// scritto sulla diagnosi quando la si chiude senza aver riparato nulla, ed
    /// e' la sola cosa che rende quella chiusura verificabile a posteriori.
    pub(crate) fn describe(&self) -> String {
        let stato = format!("stato {:?}", self.state);
        if self.ports.is_empty() {
            return format!("{stato}, nessuna porta allocata a questa unit");
        }
        let porte = self
            .ports
            .iter()
            .map(|p| format!("{} -> {}", p.port, p.answer.describe()))
            .collect::<Vec<_>>()
            .join("; ");
        format!("{stato}, {porte}")
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
///
/// `stability` e' quanto deve durare per QUESTO consumatore: la remediation e il
/// gate della suite passano [`stabilita_di_remediation`], l'avvio di un servizio
/// passa zero perche' li' la domanda e' "risponde?" e non "e' caldo?". Zero non
/// degrada il criterio a un `bool`: il ciclo osserva comunque due volte, quindi
/// resta il fatto "ha risposto, e un istante dopo rispondeva ancora".
pub(crate) fn stable_enough(conforming_for: Option<Duration>, stability: Duration) -> bool {
    conforming_for.is_some_and(|d| d >= stability)
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
    if super::port_recovery::port_listening(port).await {
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
        stable: stable_enough(conforming_for, stabilita_di_remediation()),
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
/// Il ciclo vero e' [`await_observed`]: qui c'e' solo l'osservazione (la salute
/// dell'unit coi suoi allocati).
async fn await_contract(
    state: &AppState,
    target: &ServiceRef,
    readiness: Duration,
) -> ServiceHealth {
    await_observed(readiness, stabilita_di_remediation(), |conforming_for| {
        Box::pin(observe_health(state, target, conforming_for))
    })
    .await
}

/// I due fatti su cui il ciclo dei due orologi decide, qualunque sia l'oggetto
/// osservato (una unit coi suoi allocati, una singola porta). `stable` e' il
/// fatto-durata gia' prodotto da [`stable_enough`], mai ricalcolato dal ciclo.
trait ObservedContract {
    /// Conforme ORA: quanto una singola osservazione puo' dire.
    fn conforming(&self) -> bool;
    /// Conforme da abbastanza: la finestra di stabilita' e' maturata.
    fn stable(&self) -> bool;
}

impl ObservedContract for ServiceHealth {
    fn conforming(&self) -> bool {
        self.is_conforming()
    }
    fn stable(&self) -> bool {
        self.stable
    }
}

/// Il ciclo dei due orologi, parametrico sull'osservazione (punto unico: la
/// stessa attesa serve la remediation, che osserva una unit, e il runner
/// Playwright, che osserva la porta bersaglio della suite).
///
/// Due orologi distinti, che rispondono a due domande diverse:
///
/// - `readiness` limita l'attesa che l'osservato DIVENTI conforme (un avvio puo'
///   essere lento: build, migrazioni, warm-up);
/// - [`STABILITY_SECONDS`] misura da quanto lo E' senza interruzioni. Ogni
///   caduta azzera il conteggio: la finestra va superata di fila, altrimenti un
///   servizio che oscilla si dichiarerebbe guarito nel momento buono.
///
/// Il tetto assoluto (`readiness + STABILITY`) esiste perche' un servizio che
/// entra ed esce dalla conformita' non tenga occupato il verificatore per sempre:
/// scaduto quello, l'ultima osservazione parla da se' e — non avendo maturato la
/// stabilita' — non chiude.
///
/// `observe` riceve da quanto l'osservato e' conforme senza interruzioni (`None`
/// = non lo e', o ha appena smesso): e' l'input di [`stable_enough`], che
/// l'osservazione usa per produrre il proprio fatto-durata. Il future e' boxed
/// (`BoxFuture`, un'allocazione ogni [`OBSERVE_INTERVAL_MS`]: irrilevante) e
/// NON per comodita': con `AsyncFnMut` il `Send` dei future resi non e'
/// esprimibile, e ogni `tokio::spawn` a valle della catena smette di compilare.
async fn await_observed<'a, T, F>(readiness: Duration, stability: Duration, mut observe: F) -> T
where
    T: ObservedContract,
    F: FnMut(Option<Duration>) -> futures::future::BoxFuture<'a, T>,
{
    let start = Instant::now();
    let mut conforming_since: Option<Instant> = None;
    loop {
        let conforming_for = conforming_since.map(|t| t.elapsed());
        let observed = observe(conforming_for).await;
        if observed.conforming() {
            if observed.stable() {
                return observed;
            }
            conforming_since.get_or_insert_with(Instant::now);
        } else {
            conforming_since = None;
            if start.elapsed() >= readiness {
                return observed; // non e' mai diventato conforme entro la finestra
            }
        }
        if start.elapsed() >= readiness + stability {
            return observed;
        }
        tokio::time::sleep(Duration::from_millis(OBSERVE_INTERVAL_MS)).await;
    }
}

/// Esito dell'attesa di readiness su UNA porta: cosa ha risposto l'ultima
/// osservazione, e se la risposta e' durata la finestra di stabilita'.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PortReadiness {
    pub(crate) answer: PortAnswer,
    pub(crate) stable: bool,
}

impl PortReadiness {
    /// Pronta: risponde (HTTP o TCP), e da abbastanza da non essere un istante.
    pub(crate) fn ready(&self) -> bool {
        self.answer.answered() && self.stable
    }
}

impl ObservedContract for PortReadiness {
    fn conforming(&self) -> bool {
        self.answer.answered()
    }
    fn stable(&self) -> bool {
        self.stable
    }
}

/// Attende che una porta RISPONDA stabilmente (il contratto della remediation,
/// ridotto alla sola porta: niente stato dell'unit). E' il gate di readiness del
/// runner Playwright: una suite lanciata a t+0 da un riavvio trova un servizio
/// che risponde ma sta ancora scaldando (Vite che ritrasforma le dipendenze), e
/// produce rossi flaky su codice sano. Criterio e durata sono i punti unici gia'
/// esistenti ([`probe_port`], [`stable_enough`]); l'attesa e' [`await_observed`].
///
/// `stability` e' l'unica cosa che cambia fra i consumatori, ed e' percio' un
/// parametro: il gate della suite pretende [`stabilita_di_remediation`], l'avvio
/// di un servizio passa zero (vedi [`stable_enough`]). Un secondo ciclo di
/// attesa con un'altra soglia sarebbe un secondo criterio della vita, cioe' il
/// difetto che questo punto unico esiste per togliere (regola L).
pub(crate) async fn await_port_ready(
    port: u16,
    readiness: Duration,
    stability: Duration,
) -> PortReadiness {
    await_observed(readiness, stability, move |conforming_for| {
        Box::pin(async move {
            PortReadiness {
                answer: probe_port(port).await,
                stable: stable_enough(conforming_for, stability),
            }
        })
    })
    .await
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

/// Come si e' concluso il presidio di una diagnosi. E' un vocabolario, non un
/// booleano: le quattro conclusioni scrivono stati diversi perche' dicono cose
/// diverse, e appiattirle rimetterebbe in circolo proprio la bugia che questo
/// modulo esiste per togliere ("e' andata" / "non e' andata").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepairOutcome {
    /// Il contratto era gia' soddisfatto: NESSUN rimedio e' stato eseguito, e
    /// non serviva. La rilevazione era superata.
    NotNeeded,
    /// Un rimedio e' stato eseguito e giudicato. `retry_left` distingue "non ha
    /// funzionato, restano tentativi" da "non funziona, serve un umano": senza
    /// quella distinzione il primo intoppo transitorio chiuderebbe per sempre la
    /// riga, che e' la stessa forma di difetto — una decisione presa una volta —
    /// che il presidio toglie a monte.
    Judged {
        verdict: RecoveryVerdict,
        retry_left: bool,
    },
    /// I tentativi previsti sono finiti e non se ne fara' un altro. Nasce dal
    /// gate, non da un rimedio: la riga e' ancora `open`.
    Exhausted { attempts: i64 },
}

/// Applica l'esito a una diagnosi e ritorna lo stato scritto.
///
/// PUNTO UNICO della scrittura dell'esito di una remediation di servizio: chi
/// verifica e chi scrive sono la stessa catena, cosi' non puo' esistere un
/// percorso che chiude senza aver verificato. Un verdetto negativo definitivo
/// NON chiude: porta la diagnosi in `failed_remediation` (stato terminale gia'
/// in uso dal gemello sulle violazioni risorse) e le allega l'evidenza — cosa
/// non risponde, su quale porta — perche' il problema resti visibile e
/// diagnosticabile nel pannello Problemi invece di sparire come risolto.
///
/// Lo stato di PARTENZA lo sceglie l'esito, non il chiamante: `NotNeeded` e
/// `Judged` chiudono un lease (`diagnosing`), `Exhausted` interviene su una riga
/// ancora `open`. Se la riga non e' nello stato atteso non si scrive nulla e si
/// ritorna `None`: qualcun altro l'ha gia' presa.
pub(crate) async fn apply_repair_outcome(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    outcome: &RepairOutcome,
    evidence: &str,
) -> Option<String> {
    match outcome {
        RepairOutcome::NotNeeded => {
            let detail = format!(
                "NESSUN RIMEDIO NECESSARIO: il servizio soddisfa il contratto ({evidence}). \
                 La rilevazione era superata."
            );
            close_diagnosis(db, diagnosis_id, &detail).await
        }
        RepairOutcome::Judged {
            verdict: RecoveryVerdict::Recovered,
            ..
        } => {
            let detail = format!("RIPARAZIONE VERIFICATA: {evidence}");
            close_diagnosis(db, diagnosis_id, &detail).await
        }
        RepairOutcome::Judged {
            verdict: RecoveryVerdict::NotRecovered(failure),
            retry_left: true,
        } => reopen_for_retry(db, diagnosis_id, failure).await,
        RepairOutcome::Judged {
            verdict: RecoveryVerdict::NotRecovered(failure),
            retry_left: false,
        } => {
            let detail = format!(
                "VERIFICA DEL RIMEDIO FALLITA: {}\n\n{evidence}",
                failure.describe()
            );
            mark_terminal(db, diagnosis_id, STATO_DIAGNOSING, &detail).await
        }
        RepairOutcome::Exhausted { attempts } => {
            let detail = format!(
                "RIPARAZIONE AUTOMATICA ESAURITA dopo {attempts} tentativi: il servizio non \
                 soddisfa il contratto (stato in esecuzione e almeno una porta allocata a lui \
                 che risponde). Serve un intervento."
            );
            mark_terminal(db, diagnosis_id, STATO_APERTA, &detail).await
        }
    }
}

/// Chiusura: `resolved` con la data, e in coda al `detail` il perche'. In coda e
/// non al posto del testo esistente, perche' la diagnosi dell'AI e il log del
/// guasto restano il contesto che serve a leggerlo.
async fn close_diagnosis(db: &sqlx::PgPool, diagnosis_id: Uuid, detail: &str) -> Option<String> {
    scrivi_esito(db, diagnosis_id, STATO_DIAGNOSING, STATO_RISOLTA, detail).await
}

/// Stato terminale (`failed_remediation`) con l'evidenza in coda: il problema
/// resta nel pannello e porta con se' cosa non risponde e su quale porta.
async fn mark_terminal(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    from: &str,
    detail: &str,
) -> Option<String> {
    scrivi_esito(db, diagnosis_id, from, STATO_FALLITA, detail).await
}

/// Il tentativo non ha riparato ma ce ne sono altri: la riga torna VISIBILE e
/// disponibile (`open`), con scritto cosa non ha funzionato. Il `cooldown_until`
/// scritto dal lease resta: il prossimo tentativo e' spaziato, non immediato.
async fn reopen_for_retry(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    failure: &RecoveryFailure,
) -> Option<String> {
    let detail = format!(
        "TENTATIVO DI RIPARAZIONE NON RIUSCITO: {}",
        failure.describe()
    );
    scrivi_esito(db, diagnosis_id, STATO_DIAGNOSING, STATO_APERTA, &detail).await
}

/// L'unica UPDATE che scrive un esito su `service_diagnoses`.
///
/// Lo stato di partenza e' una CONDIZIONE, non un'informazione: si scrive solo
/// cio' che era davvero in quello stato, e mai due volte. E la data di
/// risoluzione la decide lo stato di arrivo qui dentro, in un punto solo: un
/// chiamante che potesse timbrarla per conto proprio potrebbe timbrare una
/// chiusura che non e' avvenuta.
async fn scrivi_esito(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    from: &str,
    to: &str,
    detail: &str,
) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "UPDATE service_diagnoses \
            SET status = $3, \
                resolved_at = CASE WHEN $3 = $5 THEN NOW() ELSE resolved_at END, \
                updated_at = NOW(), \
                detail = COALESCE(detail, '') || E'\\n\\n' || $4 \
          WHERE id = $1 AND status = $2 \
          RETURNING status",
    )
    .bind(diagnosis_id)
    .bind(from)
    .bind(to)
    .bind(detail)
    .bind(STATO_RISOLTA)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

// ── Presa in carico: chi decide di riparare, e quando ────────────────────────
//
// IL DIFETTO CHE CHIUDE. Il contratto qui sopra esisteva, ma nessuno lo
// interrogava se non DOPO un run dell'AI riuscito: [`restart_and_verify`] era
// raggiungibile solo da `service_observer_remediation::spawn_verifica_esito`,
// che nasce dallo spawn del Debugger. E quello spawn e' UN SOLO COLPO, deciso in
// memoria: `service_observer::detect_structural_failure` registra il problema
// una volta per (unit, marcatore d'avvio, reason) e non lo rivede finche' la
// firma non cambia — cioe' finche' il servizio non riparte. Ogni guardia che
// dice "non ora" (boot-grace, run gia' attivo sulla sessione, cap orario,
// nessuna sessione chat) consumava quel colpo per SEMPRE: un rinvio diventava
// una rinuncia, e nessuno se ne accorgeva perche' la riga restava li', aperta.
//
// MISURATO il 30-31/07/2026 su bacheca-attivita: tre diagnosi `crash` aperte
// alle 20:51:53, ancora aperte sette ore dopo, con le anomalie gemelle riscritte
// 1806 volte (una ogni 15s) e `triggered_run_id` NULL su tutte. Il log del
// processo riavviato alle 04:17:40 mostra la meccanica per intero: cinque righe
// "boot-grace attivo, skip auto-debug", tutte alle 04:18:08, e poi mai piu' una
// sola interrogazione del trigger.
//
// Non era sfortuna. Il primo ciclo dell'observer e' a +25s dall'avvio
// (`STARTUP_DELAY_S`) e la boot-grace dura 90s: per QUALUNQUE servizio gia' giu'
// quando mcp-core riparte, l'unico colpo cade dentro la finestra in cui il
// trigger e' per costruzione inerte. Zero riparazioni, garantite.
//
// Da qui la presa in carico non e' piu' una decisione presa una volta e tenuta a
// mente: e' lo STATO DELLA RIGA (`status`, `triggered_run_id`, `ts`,
// `remediation_attempts`, `cooldown_until`), riletto a ogni ciclo. Un rinvio
// resta un rinvio, mai una rinuncia.
//
// PRIMA VERSIONE DI QUESTO FIX (scartata in review, mai committata): il
// presidio riavviava direttamente via `restart_and_verify`, senza passare
// dall'AI. Verificato che la corsa era REALE e SISTEMATICA, non solo sulle
// diagnosi rimaste bloccate a lungo: `register_structural_problem` non attende
// `service_log_diagnose::spawn_diagnosis` (un `tokio::spawn` non bloccante che
// fa una chiamata LLM fino a 25s prima di interrogare
// `maybe_trigger_debugger`), e il presidio girava nello STESSO ciclo
// dell'observer subito dopo la registrazione — un semplice UPDATE Postgres
// locale vince quasi sempre contro una chiamata HTTP con timeout di secondi.
// Il guard "rimedio_in_corso" di `maybe_trigger_debugger` (`status =
// 'diagnosing'`) non distingue un `diagnosing` scritto dal SUO trigger da uno
// scritto dal lease del presidio, quindi il riavvio cieco arrivava per primo e
// il Debugger — l'unico che puo' correggere una causa nel CODICE — non veniva
// piu' invocato per (quasi) nessun crash nuovo. Avrebbe contraddetto il mandato
// di questo stesso modulo: "la diagnosi e' delegata all'AI... non si aggiunge
// un controllo sulla VARIANTE osservata".
//
// Il presidio quindi NON sostituisce l'AI: la RITENTA. Per ogni diagnosi
// ancora aperta, se l'AI non ha mai avuto un run per lei (`triggered_run_id
// IS NULL`) ed e' abbastanza recente, si richiama `maybe_trigger_debugger` —
// stesso punto unico, stessi gate (cooldown per firma, boot-grace, cap orario,
// sessione, run attivo), nessuna logica duplicata (regola L). Il riavvio
// deterministico resta un RIPIEGO, per le sole diagnosi rimaste [`STUCK`] —
// vecchie abbastanza da escludere qualunque gate transitorio, con l'AI mai
// partita (o partita e interrotta da un riavvio di mcp-core, che riapre senza
// azzerare `triggered_run_id`): la' un plain restart non correggera' un bug
// reale, ma e' comunque meglio del silenzio di prima, e la sua eventuale
// incapacita' di riparare si dichiara nel pannello (`failed_remediation`),
// mai la si maschera.
//
// [`STUCK`]: vedi `RepairPolicy::ai_trigger_stuck_after_seconds`.

/// Diagnosi prese in carico in un giro, per progetto. Ognuna, se ammissibile al
/// ripiego deterministico, costa due riavvii e due finestre di osservazione: si
/// lavora a poche per volta, le piu' vecchie prima, e le altre restano per il
/// giro successivo.
const MAX_CRASHES_PER_PASS: i64 = 3;
/// Tentativi di riparazione DETERMINISTICA (ripiego, non AI) prima di
/// dichiarare che serve un umano.
const DEFAULT_MAX_ATTEMPTS: i64 = 3;
/// Distanza minima fra due tentativi deterministici sulla stessa diagnosi.
const DEFAULT_RETRY_COOLDOWN_S: i64 = 600;
/// Quanto una diagnosi resta affidata SOLO ai ritentativi del trigger AI prima
/// di diventare ammissibile al ripiego deterministico. Ampio apposta: deve
/// escludere con margine qualunque gate transitorio di `maybe_trigger_debugger`
/// (boot-grace, cap orario), cosi' il ripiego scatta solo su un blocco che i
/// ritentativi non hanno superato, non su un gate che stava per sciogliersi.
const DEFAULT_AI_TRIGGER_STUCK_AFTER_S: i64 = 1800;

/// Parametri del presidio (regola G: dal DB, mai da env; i default valgono a
/// chiave assente e sono identici ai valori della migrazione 0661).
struct RepairPolicy {
    enabled: bool,
    max_attempts: i64,
    retry_cooldown_seconds: i64,
    ai_trigger_stuck_after_seconds: i64,
}

async fn repair_policy(db: &sqlx::PgPool) -> RepairPolicy {
    async fn intero(db: &sqlx::PgPool, key: &str, default: i64) -> i64 {
        crate::settings::get_setting(db, key)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default)
    }
    let enabled = crate::settings::get_setting(db, "agent.remediation.auto_restart_enabled")
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    RepairPolicy {
        enabled,
        max_attempts: intero(db, "agent.remediation.max_restart_attempts", DEFAULT_MAX_ATTEMPTS)
            .await
            .max(1),
        retry_cooldown_seconds: intero(
            db,
            "agent.remediation.retry_cooldown_seconds",
            DEFAULT_RETRY_COOLDOWN_S,
        )
        .await
        .max(0),
        ai_trigger_stuck_after_seconds: intero(
            db,
            "agent.remediation.ai_trigger_stuck_after_seconds",
            DEFAULT_AI_TRIGGER_STUCK_AFTER_S,
        )
        .await
        .max(0),
    }
}

/// Una diagnosi di crash che aspetta un presidio, con quanto e' gia' stato
/// tentato su di lei e cio' che serve a decidere fra ritentare l'AI o ricadere
/// sul ripiego deterministico.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingCrash {
    pub(crate) id: Uuid,
    pub(crate) unit: String,
    pub(crate) attempts: i64,
    /// Classificazione della diagnosi (colonna `metric`): passata a
    /// `maybe_trigger_debugger` come `kind`, per rifare la stessa richiesta che
    /// avrebbe fatto la registrazione originale.
    pub(crate) metric: Option<String>,
    /// Ultimo dettaglio noto (colonna `detail`): passato come `last_log`.
    pub(crate) detail: Option<String>,
    pub(crate) error_signature_hash: Option<String>,
    /// `true` se questa diagnosi e' ammissibile al ripiego deterministico:
    /// l'AI ha gia' avuto un run (`triggered_run_id` valorizzato, anche se poi
    /// riaperta perche' interrotta da un riavvio di mcp-core) OPPURE e' aperta
    /// da piu' di [`RepairPolicy::ai_trigger_stuck_after_seconds`] senza che
    /// l'AI sia mai partita. Finche' e' `false` la diagnosi si affida SOLO ai
    /// ritentativi del trigger esistente.
    pub(crate) fallback_eligible: bool,
}

/// Le diagnosi di crash che aspettano: aperte, fuori dal cooldown dell'ultimo
/// tentativo, le piu' vecchie prima.
///
/// SOLO `open` (regola L): `diagnosing` significa che un rimedio e' in volo —
/// quello dell'AI o il lease di questo stesso presidio — e la sua chiusura
/// appartiene a chi l'ha preso; `resolved` e `failed_remediation` sono terminali.
/// Prenderne una in `diagnosing` significherebbe riavviare un servizio sotto i
/// piedi di chi lo sta gia' verificando.
pub(crate) async fn pending_service_crashes(
    db: &sqlx::PgPool,
    project_id: Uuid,
    limit: i64,
    ai_trigger_stuck_after_seconds: i64,
) -> Vec<PendingCrash> {
    sqlx::query_as::<_, (Uuid, String, i32, Option<String>, Option<String>, Option<String>, bool)>(
        "SELECT id, unit, remediation_attempts, metric, detail, error_signature_hash, \
                (triggered_run_id IS NOT NULL \
                 OR ts < NOW() - make_interval(secs => $4)) AS fallback_eligible \
           FROM service_diagnoses \
          WHERE project_id = $1 AND signal_kind = 'crash' AND status = $2 \
            AND (cooldown_until IS NULL OR cooldown_until < NOW()) \
          ORDER BY ts ASC LIMIT $3",
    )
    .bind(project_id)
    .bind(STATO_APERTA)
    .bind(limit)
    .bind(ai_trigger_stuck_after_seconds as f64)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(
        |(id, unit, attempts, metric, detail, error_signature_hash, fallback_eligible)| {
            PendingCrash {
                id,
                unit,
                attempts: attempts as i64,
                metric,
                detail,
                error_signature_hash,
                fallback_eligible,
            }
        },
    )
    .collect()
}

/// Cosa fare di una diagnosi aperta. Puro e testabile: e' la regola, separata
/// dall'orologio e dal database che la alimentano.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepairDecision {
    /// Prendila in carico.
    Attend,
    /// Il presidio e' spento: la riga resta visibile, nessuno la tocca.
    Disabled,
    /// I tentativi previsti sono finiti: si smette di riavviare e lo si dichiara.
    AttemptsExhausted,
    /// Un agente sta gia' lavorando su questo progetto: non si riavvia niente
    /// sotto i suoi piedi. La diagnosi resta `open` e verra' ripresa al giro
    /// successivo, quando il campo sara' libero.
    AgenteAlLavoro,
}

/// La decisione, dato anche il fatto «un agente sta lavorando su questo
/// progetto».
///
/// ROOT CAUSE del parametro, misurata il 03/08/2026 su DUE progetti: il guard
/// «c'e' gia' un run attivo» esisteva solo sul percorso che SPAWNA un run di
/// diagnosi (`service_observer_remediation`), non su quello che RIAVVIA il
/// servizio. Il worker e l'agente finivano cosi' a muovere gli stessi servizi
/// nello stesso momento:
///   - agenda-corsi, 01:17:32 — riparazione di
///     `agenda-corsi-agenda-corsi-frontend.service.service` mentre il run
///     d293ae40 era attivo (01:10:01-01:30:53), chiuso `blocked_needs_input`;
///   - bacheca-attivita, 02:22:58 — riparazione di
///     `bacheca-attivita-frontend.service` mentre il run 90924d59 era attivo
///     (02:14:43-02:36:55), chiuso `failed_diagnosed`.
/// Su 1405 diagnosi, 78 hanno tentato un riavvio: non e' un caso di confine.
///
/// Il criterio e' il PROGETTO, non la sessione: un run agentico avvia, ferma e
/// riavvia qualunque servizio del progetto su cui lavora, quindi un riavvio
/// concorrente su un'altra unit e' altrettanto capace di togliergli il terreno
/// (la dedup per scopo ferma processi che il worker non stava guardando). Il
/// guard dell'auto-debug, che invece ragiona per sessione, risponde a un'altra
/// domanda — «questa conversazione ha gia' un agente?» — e resta dov'e'.
///
/// Non e' un rinvio a vuoto: la diagnosi non viene consumata, resta `open` e il
/// prossimo giro la riprende. Se il servizio e' ancora giu' quando l'agente ha
/// finito, viene riparato allora; se e' l'agente ad averlo rimesso in piedi, la
/// verifica del contratto chiudera' la diagnosi senza toccare nulla.
pub(crate) fn repair_decision_con_agente(
    enabled: bool,
    attempts: i64,
    max_attempts: i64,
    agente_al_lavoro: bool,
) -> RepairDecision {
    if !enabled {
        return RepairDecision::Disabled;
    }
    if attempts >= max_attempts {
        return RepairDecision::AttemptsExhausted;
    }
    // DOPO l'esaurimento: una diagnosi che ha finito i tentativi va dichiarata
    // tale anche mentre un agente lavora, o resterebbe `open` per sempre senza
    // che nessuno spieghi perche'.
    if agente_al_lavoro {
        return RepairDecision::AgenteAlLavoro;
    }
    RepairDecision::Attend
}

#[cfg(test)]
pub(crate) fn repair_decision(enabled: bool, attempts: i64, max_attempts: i64) -> RepairDecision {
    repair_decision_con_agente(enabled, attempts, max_attempts, false)
}

/// `true` se il progetto ha un run agentico in corso.
///
/// Interroga `agent_runs` sul pool del PROGETTO (tabella migrata). Un errore
/// non diventa «campo libero»: se non si e' potuto guardare si risponde
/// «occupato», perche' la conseguenza di sbagliare in un verso (un riavvio
/// rimandato di un giro) e' incomparabilmente piu' lieve di quella nell'altro
/// (un servizio riavviato sotto i piedi di un agente che ci sta lavorando).
async fn agente_al_lavoro_sul_progetto(state: &AppState, project_id: Uuid) -> bool {
    let pool = match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "presidio servizi: DB progetto non interrogabile, considero il campo occupato"
            );
            return true;
        }
    };
    match sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM agent_runs \
          WHERE status IN ('running', 'awaiting_confirmation', 'awaiting_subagents')",
    )
    .fetch_one(&pool)
    .await
    {
        Ok(n) => n > 0,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                error = %e,
                "presidio servizi: run attivi non leggibili, considero il campo occupato"
            );
            true
        }
    }
}

/// Prende in carico UNA diagnosi: `open` -> `diagnosing`, col cooldown del
/// prossimo tentativo gia' scritto. Atomico e condizionato allo stato, perche'
/// due cicli dell'observer che si accavallano — o il trigger dell'AI in corsa
/// sulla stessa unit — non possano riavviare lo stesso servizio due volte.
///
/// `false` = la riga non era piu' `open`: qualcun altro l'ha presa, e questo
/// giro non fa nulla.
pub(crate) async fn lease_for_repair(
    db: &sqlx::PgPool,
    diagnosis_id: Uuid,
    cooldown_seconds: i64,
) -> bool {
    sqlx::query_scalar::<_, Uuid>(
        "UPDATE service_diagnoses \
            SET status = $2, updated_at = NOW(), \
                cooldown_until = NOW() + make_interval(secs => $3) \
          WHERE id = $1 AND status = $4 \
          RETURNING id",
    )
    .bind(diagnosis_id)
    .bind(STATO_DIAGNOSING)
    .bind(cooldown_seconds as f64)
    .bind(STATO_APERTA)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

/// Registra che un tentativo di riparazione e' stato ESEGUITO. Separato dal
/// lease apposta: una diagnosi che si chiude perche' il servizio era gia' sano
/// non ha consumato alcun tentativo, e contarla renderebbe il conteggio una
/// misura di quante volte ci si e' guardati intorno.
async fn note_attempt(db: &sqlx::PgPool, diagnosis_id: Uuid) {
    let _ = sqlx::query(
        "UPDATE service_diagnoses SET remediation_attempts = remediation_attempts + 1 \
          WHERE id = $1",
    )
    .bind(diagnosis_id)
    .execute(db)
    .await;
}

/// Il contratto SENZA rimedio: questo servizio lo soddisfa GIA'?
///
/// Stessa misura del dopo-rimedio ([`await_contract`]: stesso stato, stesse
/// prove sulle stesse porte, stessa durata) con la finestra di readiness a ZERO,
/// perche' qui la domanda e' diversa: non si aspetta che un servizio DIVENTI
/// sano — nessuno l'ha appena riavviato — si guarda se lo E'. Un secondo
/// criterio di "servizio sano" farebbe dell'apertura e della chiusura due idee
/// diverse della stessa cosa (regola L), e il ciclo oscillerebbe.
pub(crate) async fn contract_without_repair(
    state: &AppState,
    project_id: Uuid,
    unit: &str,
) -> ServiceHealth {
    let target = ServiceRef::resolve(state, project_id, unit).await;
    await_contract(state, &target, Duration::ZERO).await
}

/// Presidia le diagnosi di crash aperte di un progetto: e' l'INGRESSO che
/// mancava alla catena rileva -> ripara -> chiude.
///
/// Chiamata a ogni ciclo dell'observer con i parametri del trigger AI gia'
/// caricati dal chiamante (`ObserverConfig`, un solo caricamento per ciclo):
/// non li ricarica una seconda volta, e non ne duplica il significato.
///
/// Non trattiene il ciclo: ogni azione (ritentativo del trigger, o presidio
/// deterministico) vive in un task a se'; il lease sul DB — o, per il
/// ritentativo, i gate gia' dentro `maybe_trigger_debugger` — sono cio' che
/// impedisce a due cicli di agire due volte sulla stessa riga.
pub(crate) async fn process_open_service_crashes(
    state: &AppState,
    project_id: Uuid,
    auto_diagnose_enabled: bool,
    diagnose_cooldown_seconds: i64,
    diagnose_max_per_hour: i64,
) {
    // Boot-grace: RINVIO, non consumo. Subito dopo un riavvio di mcp-core i
    // servizi sono nel transitorio e "giu'" non distingue un guasto da una
    // stabilizzazione. La riga resta `open` e il giro dopo si riprova — che e'
    // esattamente cio' che prima non accadeva.
    if super::service_observer_remediation::within_boot_grace(state).await {
        return;
    }
    // RI-ARMO prima della presa in carico: una diagnosi FALLITA la cui causa
    // puo' essere cambiata (scrittura registrata su un file del servizio,
    // posteriore al fallimento) torna `open` e rientra nel giro qui sotto.
    rearm_failed_remediations(state, project_id).await;
    let policy = repair_policy(&state.db).await;
    let crashes = pending_service_crashes(
        &state.db,
        project_id,
        MAX_CRASHES_PER_PASS,
        policy.ai_trigger_stuck_after_seconds,
    )
    .await;
    for crash in crashes {
        if !crash.fallback_eligible {
            retry_ai_trigger(
                state,
                project_id,
                crash,
                auto_diagnose_enabled,
                diagnose_cooldown_seconds,
                diagnose_max_per_hour,
            );
            continue;
        }
        dispatch_repair_decision(state, project_id, &policy, crash).await;
    }
}

/// Applica a UNA diagnosi la decisione di riparazione: esaurimento dichiarato
/// (con refresh del pannello), presa in carico con lease e presidio in
/// background, o nulla se la riparazione automatica e' spenta. Estratta dal
/// ciclo di `process_open_service_crashes`, che resta la sola a decidere QUALI
/// diagnosi entrano nel giro.
async fn dispatch_repair_decision(
    state: &AppState,
    project_id: Uuid,
    policy: &RepairPolicy,
    crash: PendingCrash,
) {
    // Il fatto si misura QUI, fuori dalla regola (regola G: l'I/O al chiamante,
    // la decisione al punto unico). Si legge solo se la riparazione e' accesa e
    // ci sono ancora tentativi: interrogare il DB per una diagnosi che verrebbe
    // comunque dichiarata esaurita sarebbe lavoro a vuoto a ogni giro.
    let agente_al_lavoro = if policy.enabled && crash.attempts < policy.max_attempts {
        agente_al_lavoro_sul_progetto(state, project_id).await
    } else {
        false
    };
    match repair_decision_con_agente(
        policy.enabled,
        crash.attempts,
        policy.max_attempts,
        agente_al_lavoro,
    ) {
        RepairDecision::AgenteAlLavoro => {
            tracing::debug!(
                unit = %crash.unit,
                project_id = %project_id,
                "presidio servizi: un agente sta lavorando sul progetto, riparazione rimandata"
            );
        }
        RepairDecision::Disabled => {}
        RepairDecision::AttemptsExhausted => {
            let esito = RepairOutcome::Exhausted {
                attempts: crash.attempts,
            };
            if apply_repair_outcome(&state.db, crash.id, &esito, "").await.is_some() {
                tracing::warn!(
                    unit = %crash.unit, attempts = crash.attempts,
                    "service_recovery: tentativi di riparazione esauriti, serve un intervento"
                );
                crate::project_workspace::logs::emit_problems_panel_refresh(
                    project_id,
                    vec![crash.id],
                );
            }
        }
        RepairDecision::Attend => {
            if !lease_for_repair(&state.db, crash.id, policy.retry_cooldown_seconds).await {
                return;
            }
            let ultimo = crash.attempts + 1 >= policy.max_attempts;
            let state = state.clone();
            tokio::spawn(async move { presidia(state, project_id, crash, ultimo).await });
        }
    }
}

/// Ritenta il trigger AI ESISTENTE (`maybe_trigger_debugger`, punto unico
/// invariato) per una diagnosi non ancora ammissibile al ripiego. Nessuna
/// scrittura nostra: se il trigger scatta, e' lui a portare la riga in
/// `diagnosing` con `triggered_run_id`; se un gate lo respinge ancora (es.
/// boot-grace non ancora scaduta altrove, cooldown per firma), la riga resta
/// `open` e il ciclo successivo la ripresenta — che e' esattamente il
/// comportamento che il difetto originale non aveva.
///
/// In un task a se': `maybe_trigger_debugger` fa diverse query e puo' spawnare
/// un run: non deve trattenere il ciclo dell'observer, come gia' non lo
/// trattiene quando parte da `service_log_diagnose::spawn_diagnosis`.
fn retry_ai_trigger(
    state: &AppState,
    project_id: Uuid,
    crash: PendingCrash,
    auto_diagnose_enabled: bool,
    diagnose_cooldown_seconds: i64,
    diagnose_max_per_hour: i64,
) {
    let state = state.clone();
    let PendingCrash {
        id,
        unit,
        metric,
        detail,
        error_signature_hash,
        ..
    } = crash;
    let kind = metric.unwrap_or_else(|| "unknown".to_string());
    let last_log = detail.unwrap_or_default();
    let sig = error_signature_hash.unwrap_or_default();
    tokio::spawn(async move {
        super::service_observer_remediation::maybe_trigger_debugger(
            &state,
            auto_diagnose_enabled,
            diagnose_cooldown_seconds,
            diagnose_max_per_hour,
            project_id,
            &unit,
            &kind,
            &last_log,
            &sig,
            Some(id),
        )
        .await;
    });
}

/// Il presidio DETERMINISTICO di UNA diagnosi (ripiego, non AI), in
/// background: prima guarda, e solo se serve ripara.
///
/// L'ordine non e' prudenza, e' il mandato: una rilevazione superata (il
/// servizio risponde, la riga e' rimasta indietro) si chiude su un FATTO
/// OSSERVATO — la porta che gli spetta risponde, e continua a farlo — non sul
/// fatto che il ciclo sia passato di nuovo (regola M). E riavviare un servizio
/// sano sarebbe il danno che la rilevazione sbagliata, da sola, non faceva.
async fn presidia(
    state: AppState,
    project_id: Uuid,
    crash: PendingCrash,
    ultimo_tentativo: bool,
) {
    let salute = contract_without_repair(&state, project_id, &crash.unit).await;
    if salute.meets_contract() {
        let esito = RepairOutcome::NotNeeded;
        let scritto =
            apply_repair_outcome(&state.db, crash.id, &esito, &salute.describe()).await;
        tracing::info!(
            unit = %crash.unit, stato_diagnosi = ?scritto,
            "service_recovery: diagnosi superata, il servizio soddisfa il contratto senza rimedi"
        );
        crate::project_workspace::logs::emit_problems_panel_refresh(project_id, vec![crash.id]);
        return;
    }

    note_attempt(&state.db, crash.id).await;
    let (verdict, facts) = restart_and_verify(&state, project_id, &crash.unit).await;
    let esito = RepairOutcome::Judged {
        verdict: verdict.clone(),
        retry_left: !ultimo_tentativo,
    };
    let scritto = apply_repair_outcome(&state.db, crash.id, &esito, &facts.render()).await;
    tracing::info!(
        unit = %crash.unit, tentativo = crash.attempts + 1, verdetto = ?verdict,
        stato_diagnosi = ?scritto,
        "service_recovery: riparazione automatica tentata e verificata sul servizio"
    );
    announce_recovery(project_id, &crash.unit, &verdict, Some(crash.id));
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

    // Il presidio deterministico non ha un run da interrogare: il suo lease e'
    // tenuto da un task in memoria, e a un riavvio di mcp-core quel task muore
    // come l'altro. Il criterio qui non e' un timeout scelto a occhio: e' l'AVVIO
    // DI QUESTO PROCESSO. Un lease piu' vecchio non puo' appartenere a nessun
    // task vivo, perche' i task vivi sono nati dopo.
    let orfane = reopen_orphan_repair_leases(&state.db, state.boot_at.elapsed().as_secs() as i64)
        .await;
    if !orfane.is_empty() {
        tracing::info!(
            righe = orfane.len(),
            "service_recovery: lease di riparazione orfani riaperti (mcp-core riavviato a rimedio in volo)"
        );
        crate::project_workspace::logs::emit_problems_panel_refresh_batch(&orfane);
    }
}

/// Riapre le diagnosi leasate dal presidio deterministico (`diagnosing` SENZA
/// run) il cui lease e' anteriore all'avvio di questo processo.
///
/// `open` e non `failed_remediation`, per la stessa ragione del gemello con run:
/// il rimedio non ha fallito, e' stato INTERROTTO. E il tentativo non viene
/// scontato — `remediation_attempts` era gia' stato incrementato solo se un
/// riavvio era davvero partito.
pub(crate) async fn reopen_orphan_repair_leases(
    db: &sqlx::PgPool,
    boot_elapsed_seconds: i64,
) -> Vec<(Uuid, Uuid)> {
    sqlx::query_as::<_, (Uuid, Uuid)>(
        "UPDATE service_diagnoses SET status = $1, updated_at = NOW() \
          WHERE signal_kind = 'crash' AND status = $2 \
            AND triggered_run_id IS NULL \
            AND updated_at < NOW() - make_interval(secs => $3) \
          RETURNING project_id, id",
    )
    .bind(STATO_APERTA)
    .bind(STATO_DIAGNOSING)
    .bind(boot_elapsed_seconds as f64)
    .fetch_all(db)
    .await
    .unwrap_or_default()
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

// ── Ri-armo: una diagnosi FALLITA torna ammissibile quando la causa cambia ──
//
// IL DIFETTO CHE CHIUDE. `failed_remediation` era TERMINALE in senso assoluto:
// nessun percorso del codice la rileggeva. Corretta la causa nel codice — da un
// run successivo o dall'utente — la diagnosi restava FALLITA, il servizio
// spento, la porta allocata libera e inutilizzata, e il pannello Problemi
// mostrava "Riparazione automatica FALLITA" su un difetto che non esisteva piu'.
//
// MISURATO il 31/07/2026 sera su bacheca-attivita: frontend in crash per
// `css_syntax_error` (rilevazione corretta), riavvio ri-fallito (giusto: un
// errore di sintassi non si guarisce riavviando), riga marcata
// `failed_remediation` (onesto, per contratto). POI un run di correzione ha
// sistemato il css — `npx vite build` pulito, 138 moduli — e la diagnosi e'
// rimasta FALLITA, con la porta 24804 libera. Nel frattempo l'agente in chat,
// non potendo far ripartire il servizio, ha aggirato: backend avviato come
// processo nudo sulla porta 3001, FUORI dal bucket del progetto — esattamente
// il ripiego che la governance delle porte esiste per impedire.
//
// IL TRIGGER E' UN SEGNALE STRUTTURATO (regola M), mai un orologio: una
// SCRITTURA registrata su un file del servizio (`file_mutations`, DB META, con
// gli hash del contenuto prima/dopo) posteriore al fallimento, oppure la
// richiesta ESPLICITA dal pannello Problemi. Un retry a tempo cieco sarebbe la
// toppa (regola H): riavvierebbe per sempre un servizio la cui causa e' ancora
// li', e trasformerebbe lo stato terminale in un eufemismo.
//
// Il ri-armo NON ripara niente: riporta la riga in `open`, e da li' il
// presidio esistente (`process_open_service_crashes`) la riprende con il SUO
// contratto invariato — Running, porta allocata che risponde, stabilita',
// riavvio di conferma. Il criterio "questa scrittura conta?" delega ai punti
// unici (regola L): [`WriteFact::cambia_il_contenuto`] (una riscrittura
// identica non e' un cambiamento) e [`path_in_scope`] (la scrittura deve
// cadere nell'area del servizio).

/// Diagnosi FALLITE riesaminate per giro. Poche per costruzione: una riga
/// entra qui solo dopo aver esaurito i tentativi del presidio.
const MAX_REARM_PER_PASS: i64 = 10;
/// Tetto di mutazioni lette per diagnosi, le piu' RECENTI prima: sono le
/// candidate piu' probabili a essere la correzione, e le scritture nuove
/// entrano nella finestra da sole al giro successivo.
const MAX_REARM_FACTS: i64 = 500;

/// Una diagnosi in stato terminale `failed_remediation`, col momento del
/// fallimento (l'`updated_at` timbrato da [`scrivi_esito`]: dopo quella
/// scrittura la riga non viene piu' toccata, quindi e' il watermark esatto
/// oltre il quale una mutazione e' "posteriore al fallimento").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedDiagnosis {
    pub(crate) id: Uuid,
    pub(crate) unit: String,
    pub(crate) failed_at: chrono::DateTime<chrono::Utc>,
}

/// Una scrittura registrata, coi suoi FATTI (regola M): il path e gli hash del
/// contenuto prima/dopo, cosi' come `record_mutation` li ha calcolati.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MutationSeen {
    pub(crate) file_path: String,
    pub(crate) fact: WriteFact,
    pub(crate) at: chrono::DateTime<chrono::Utc>,
}

/// Le diagnosi di crash in stato terminale FALLITO di un progetto, le piu'
/// vecchie prima. Solo `signal_kind = 'crash'`: il gemello sulle violazioni
/// risorse ha il suo ciclo e la sua semantica.
pub(crate) async fn failed_service_diagnoses(
    db: &sqlx::PgPool,
    project_id: Uuid,
    limit: i64,
) -> Vec<FailedDiagnosis> {
    sqlx::query_as::<_, (Uuid, String, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, unit, updated_at FROM service_diagnoses \
          WHERE project_id = $1 AND signal_kind = 'crash' AND status = $2 \
          ORDER BY updated_at ASC LIMIT $3",
    )
    .bind(project_id)
    .bind(STATO_FALLITA)
    .bind(limit)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id, unit, failed_at)| FailedDiagnosis {
        id,
        unit,
        failed_at,
    })
    .collect()
}

/// Le scritture registrate DOPO un istante, per progetto, le piu' recenti
/// prima. I fatti NON sono filtrati in SQL (stessa scelta, e stessa ragione,
/// di `agent_graph_adapter::mutation_progress`): il criterio "cambia il
/// contenuto?" vive nel punto unico [`WriteFact::cambia_il_contenuto`], e un
/// `WHERE before_sha256 IS DISTINCT FROM after_sha256` ne farebbe una seconda
/// copia in SQL (regola L).
pub(crate) async fn mutations_since(
    db: &sqlx::PgPool,
    project_id: Uuid,
    after: chrono::DateTime<chrono::Utc>,
) -> Vec<MutationSeen> {
    sqlx::query_as::<_, (String, Option<String>, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT file_path, before_sha256, after_sha256, created_at \
           FROM file_mutations \
          WHERE project_id = $1 AND created_at > $2 \
          ORDER BY created_at DESC LIMIT $3",
    )
    .bind(project_id)
    .bind(after)
    .bind(MAX_REARM_FACTS)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(file_path, before_sha256, after_sha256, at)| MutationSeen {
        file_path,
        fact: WriteFact {
            before_sha256,
            after_sha256,
            // Il ri-armo chiede "il codice e' cambiato dopo il crash?", e li'
            // una riscrittura di soli fine-riga NON e' un cambiamento: la
            // colonna e' pero' fuori da questo SELECT, che legge quattro campi
            // scelti per un'altra domanda. `None` mantiene il comportamento
            // storico (confronto sugli hash); se un giorno il ri-armo dovesse
            // distinguere anche questo caso, la colonna e' li' dalla mig 0680.
            solo_fine_riga: None,
        },
        at,
    })
    .collect()
}

/// IL CRITERIO del ri-armo, puro e testabile: la prima mutazione che CAMBIA il
/// contenuto ([`WriteFact::cambia_il_contenuto`]: una riscrittura identica non
/// e' un segnale) e cade nell'AREA del servizio ([`path_in_scope`]). `None` =
/// nessun segnale, la diagnosi resta terminale.
pub(crate) fn rearm_evidence<'a>(
    mutations: &'a [MutationSeen],
    service_area: &[String],
) -> Option<&'a MutationSeen> {
    mutations
        .iter()
        .find(|m| m.fact.cambia_il_contenuto() && path_in_scope(&m.file_path, service_area))
}

/// L'AREA di un servizio come scope relativo alla root del progetto, per
/// [`path_in_scope`] (che ri-normalizza: separatori, case, confine di
/// segmento). La fonte e' la working dir REGISTRATA del processo
/// (`agent_processes`, la stessa di [`process_facts`]), mai il nome del
/// programma (lezione di `service_ownership`: `node` non dice quale servizio
/// sia).
///
/// Working dir ignota, fuori root o uguale alla root -> `"."` (tutto il
/// progetto): non sapendo QUALE area appartenga al servizio, qualunque
/// scrittura del progetto e' un segnale ammissibile che la causa PUO' essere
/// cambiata. Non e' lasco: il ri-armo non dichiara niente riparato, riapre il
/// ciclo — ed e' il contratto a valle a verificare sul servizio vero.
pub(crate) fn relative_service_area(working_dir: Option<&str>, project_root: &str) -> Vec<String> {
    fn norm(p: &str) -> String {
        p.trim()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    }
    let tutto = vec![".".to_string()];
    let Some(wd) = working_dir else {
        return tutto;
    };
    let (wd, root) = (norm(wd), norm(project_root));
    if root.is_empty() || wd == root {
        return tutto;
    }
    match wd.strip_prefix(&format!("{root}/")) {
        Some(rel) if !rel.is_empty() => vec![rel.to_string()],
        _ => tutto,
    }
}

/// Risolve l'area di un servizio dal suo processo registrato.
async fn service_area(state: &AppState, project_id: Uuid, unit: &str) -> Vec<String> {
    let target = ServiceRef::resolve(state, project_id, unit).await;
    let (_, working_dir, _, _, _) = process_facts(state, project_id, &target.short).await;
    relative_service_area(working_dir.as_deref(), &target.root.to_string_lossy())
}

/// PUNTO UNICO della scrittura di ri-armo: `failed_remediation` -> `open`, coi
/// tentativi AZZERATI (la causa e' cambiata: il nuovo ciclo ha diritto ai
/// suoi; la storia dei precedenti resta nel detail) e il cooldown rimosso (il
/// segnale e' un fatto nuovo, non un retry dello stesso tentativo). Entrambi i
/// trigger — la scrittura registrata e la richiesta esplicita dal pannello —
/// delegano qui, col loro motivo.
///
/// Condizionata allo stato E al progetto: si ri-arma solo cio' che era davvero
/// terminale-fallito, mai due volte, e mai una diagnosi di un altro progetto
/// (il chiamante HTTP passa l'id dalla URL: senza questa condizione potrebbe
/// ri-armare righe altrui).
pub(crate) async fn rearm_diagnosis(
    db: &sqlx::PgPool,
    project_id: Uuid,
    diagnosis_id: Uuid,
    motivo: &str,
) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "UPDATE service_diagnoses \
            SET status = $4, remediation_attempts = 0, cooldown_until = NULL, \
                updated_at = NOW(), \
                detail = COALESCE(detail, '') || E'\\n\\n' || $3 \
          WHERE id = $1 AND project_id = $2 AND signal_kind = 'crash' AND status = $5 \
          RETURNING id",
    )
    .bind(diagnosis_id)
    .bind(project_id)
    .bind(motivo)
    .bind(STATO_APERTA)
    .bind(STATO_FALLITA)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Il motivo scritto sulla diagnosi al ri-armo esplicito dal pannello. Qui e
/// non nel layer HTTP, perche' il vocabolario dei motivi stia accanto alla
/// scrittura che li persiste.
pub(crate) const REARM_EXPLICIT_REASON: &str =
    "RIPARAZIONE RI-ARMATA su richiesta esplicita dal pannello: nuovo ciclo di verifica \
     richiesto dall'utente.";

/// Un giro di ri-armo sulle diagnosi FALLITE gia' selezionate, con le aree dei
/// servizi gia' risolte. Ritorna gli id ri-armati. Separata dall'orchestratore
/// con `AppState` perche' i test la attraversino sullo schema META reale
/// (regola O) passando dai produttori veri (`apply_repair_outcome` per la
/// diagnosi fallita, `record_mutation` per la scrittura).
pub(crate) async fn rearm_pass(
    db: &sqlx::PgPool,
    project_id: Uuid,
    failed: &[FailedDiagnosis],
    aree: &HashMap<String, Vec<String>>,
) -> Vec<Uuid> {
    // Area assente dalla mappa = area non risolta: vale il ripiego dichiarato
    // di `relative_service_area` (tutto il progetto), per la stessa ragione.
    let tutto = vec![".".to_string()];
    let mut riarmate = Vec::new();
    for diag in failed {
        let mutations = mutations_since(db, project_id, diag.failed_at).await;
        if mutations.is_empty() {
            continue;
        }
        let area = aree.get(&diag.unit).unwrap_or(&tutto);
        let Some(seen) = rearm_evidence(&mutations, area) else {
            continue;
        };
        let motivo = format!(
            "RIPARAZIONE RI-ARMATA: scrittura registrata su {} alle {}, posteriore al \
             fallimento: la causa puo' essere cambiata, il ciclo di verifica riparte da capo.",
            seen.file_path,
            seen.at.to_rfc3339()
        );
        if rearm_diagnosis(db, project_id, diag.id, &motivo).await.is_some() {
            tracing::info!(
                unit = %diag.unit, diagnosis_id = %diag.id, file = %seen.file_path,
                "service_recovery: diagnosi fallita ri-armata da una scrittura sul servizio"
            );
            riarmate.push(diag.id);
        }
    }
    riarmate
}

/// Ri-armo automatico di un progetto: interrogato a ogni ciclo dell'observer
/// (da [`process_open_service_crashes`], PRIMA della presa in carico, cosi'
/// una riga ri-armata rientra nel giro immediatamente successivo).
pub(crate) async fn rearm_failed_remediations(state: &AppState, project_id: Uuid) {
    let failed = failed_service_diagnoses(&state.db, project_id, MAX_REARM_PER_PASS).await;
    if failed.is_empty() {
        return;
    }
    let mut aree: HashMap<String, Vec<String>> = HashMap::new();
    for diag in &failed {
        if !aree.contains_key(&diag.unit) {
            let area = service_area(state, project_id, &diag.unit).await;
            aree.insert(diag.unit.clone(), area);
        }
    }
    let riarmate = rearm_pass(&state.db, project_id, &failed, &aree).await;
    if !riarmate.is_empty() {
        crate::project_workspace::logs::emit_problems_panel_refresh(project_id, riarmate);
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
// `port_recovery::port_listening`, lo stesso che usa l'observer.

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
            stable: stable_enough(conforme_da, stabilita_di_remediation()),
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

    /// Osservazione sintetica per esercitare il CICLO ([`await_observed`]) a
    /// tempo virtuale (`start_paused`): la porta "risponde" dal giro
    /// `risponde_dal` in poi, e il fatto-durata viene dal produttore vero
    /// ([`stable_enough`]) — la closure sintetizza solo cio' che la rete
    /// risponderebbe, mai il criterio (regola O). Niente I/O reale: con il clock
    /// pausato l'auto-advance di tokio e l'I/O vero andrebbero in gara.
    fn osservazione_a_scatti(
        risponde_dal: usize,
        giri: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> impl FnMut(Option<Duration>) -> futures::future::BoxFuture<'static, PortReadiness> {
        move |conforming_for| {
            let giri = giri.clone();
            futures::future::FutureExt::boxed(async move {
                let giro = giri.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let answer = if giro >= risponde_dal {
                    PortAnswer::Http { status: 200 }
                } else {
                    PortAnswer::Silence
                };
                PortReadiness {
                    answer,
                    stable: stable_enough(conforming_for, stabilita_di_remediation()),
                }
            })
        }
    }

    /// La risposta di UN istante non basta: il ciclo deve trattenere il
    /// verdetto finche' la conformita' non ha retto l'intera finestra di
    /// stabilita'. E' il caso del runner Playwright dopo un riavvio: la porta
    /// risponde subito, ma la suite deve partire solo quando risponde DA
    /// abbastanza.
    ///
    /// MUTAZIONE: facendo ritornare il ciclo alla prima osservazione conforme
    /// (cioe' togliendo l'attesa della stabilita'), `stable` e' false e il
    /// numero di giri e' 1: entrambe le asserzioni rosseggiano.
    #[tokio::test(start_paused = true)]
    async fn la_conformita_deve_durare_prima_che_il_ciclo_chiuda() {
        let giri = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let esito = await_observed(
            Duration::from_secs(60),
            stabilita_di_remediation(),
            osservazione_a_scatti(0, giri.clone()),
        )
        .await;
        assert!(esito.ready(), "risponde dal primo giro: deve maturare");
        let osservazioni = giri.load(std::sync::atomic::Ordering::SeqCst);
        let minimo = (STABILITY_SECONDS * 1000 / OBSERVE_INTERVAL_MS) as usize;
        assert!(
            osservazioni > minimo,
            "la finestra di {STABILITY_SECONDS}s a passi di {OBSERVE_INTERVAL_MS}ms \
             richiede piu' di {minimo} osservazioni, misurate {osservazioni}"
        );
    }

    /// Il caso del runner a freddo: la porta e' muta per i primi giri (il
    /// servizio sta ripartendo), poi risponde. Il ciclo deve attendere che
    /// DIVENTI conforme e POI che la conformita' duri — mai chiudere sul
    /// primo silenzio, mai sulla prima risposta.
    #[tokio::test(start_paused = true)]
    async fn una_porta_che_nasce_muta_e_poi_risponde_matura_la_finestra() {
        let giri = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let muta_per = 5usize;
        let esito = await_observed(
            Duration::from_secs(60),
            stabilita_di_remediation(),
            osservazione_a_scatti(muta_per, giri.clone()),
        )
        .await;
        assert!(esito.ready(), "dopo il transitorio muto deve maturare");
        let osservazioni = giri.load(std::sync::atomic::Ordering::SeqCst);
        let minimo = muta_per + (STABILITY_SECONDS * 1000 / OBSERVE_INTERVAL_MS) as usize;
        assert!(
            osservazioni > minimo,
            "il transitorio muto ({muta_per} giri) non conta nella finestra: \
             servono piu' di {minimo} osservazioni, misurate {osservazioni}"
        );
    }

    /// Scaduta la readiness senza che l'osservato sia MAI diventato conforme,
    /// il ciclo ritorna l'ultima osservazione cosi' com'e': non pronta, e con
    /// la risposta che spiega perche' (qui: muta).
    ///
    /// MUTAZIONE: se il ciclo ignorasse la finestra di readiness questo test
    /// non terminerebbe (l'osservazione non diventa mai conforme).
    #[tokio::test(start_paused = true)]
    async fn scaduta_la_readiness_una_porta_sempre_muta_non_e_pronta() {
        let giri = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let esito = await_observed(
            Duration::from_secs(30),
            stabilita_di_remediation(),
            osservazione_a_scatti(usize::MAX, giri.clone()),
        )
        .await;
        assert!(!esito.ready(), "mai conforme: non puo' essere pronta");
        assert_eq!(
            esito.answer,
            PortAnswer::Silence,
            "l'ultima osservazione deve dire cosa (non) ha risposto"
        );
    }

    /// Una caduta a meta' finestra azzera il conteggio: la stabilita' va
    /// maturata DI FILA. Un servizio che oscilla non deve dichiararsi pronto
    /// nel momento buono.
    #[tokio::test(start_paused = true)]
    async fn una_caduta_a_meta_finestra_azzera_il_conteggio() {
        let giri = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let giri_visti = giri.clone();
        // Conforme, MA con un buco al giro 3: risponde ai giri 0-2, tace al 3,
        // risponde dal 4 in poi.
        let osservazione = move |conforming_for: Option<Duration>| {
            let giri = giri_visti.clone();
            futures::future::FutureExt::boxed(async move {
                let giro = giri.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let answer = if giro == 3 {
                    PortAnswer::Silence
                } else {
                    PortAnswer::Http { status: 200 }
                };
                PortReadiness {
                    answer,
                    stable: stable_enough(conforming_for, stabilita_di_remediation()),
                }
            })
        };
        let esito = await_observed(
            Duration::from_secs(60),
            stabilita_di_remediation(),
            osservazione,
        )
        .await;
        assert!(esito.ready(), "dopo la ricaduta la finestra rimatura");
        let osservazioni = giri.load(std::sync::atomic::Ordering::SeqCst);
        // 4 giri bruciati (3 conformi + la caduta) + una finestra INTERA dopo.
        let minimo = 4 + (STABILITY_SECONDS * 1000 / OBSERVE_INTERVAL_MS) as usize;
        assert!(
            osservazioni > minimo,
            "i giri prima della caduta non contano: servono piu' di {minimo} \
             osservazioni, misurate {osservazioni}"
        );
    }

    /// La composizione vera ([`await_port_ready`]: probe reale + ciclo) sul
    /// caso immediato e deterministico: porta muta e readiness a zero. Il
    /// primo giro osserva il silenzio e la finestra e' gia' scaduta: niente
    /// attese, niente timer in gara con l'I/O.
    #[tokio::test]
    async fn await_port_ready_su_porta_muta_ritorna_subito_non_pronta() {
        let esito = await_port_ready(
            porta_muta().await,
            Duration::ZERO,
            stabilita_di_remediation(),
        )
        .await;
        assert!(!esito.ready(), "nessuno in ascolto: non e' pronta");
        assert_eq!(esito.answer, PortAnswer::Silence);
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
        let esito = RepairOutcome::Judged {
            verdict: verdetto,
            retry_left: false,
        };
        let scritto = apply_repair_outcome(&pool, diagnosi, &esito, "evidenza dei fatti").await;
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
        let esito = RepairOutcome::Judged {
            verdict: verdetto,
            retry_left: false,
        };
        assert_eq!(
            apply_repair_outcome(&pool, diagnosi, &esito, "evidenza")
                .await
                .as_deref(),
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

    // ── Presa in carico ──────────────────────────────────────────────────────

    /// Semina una diagnosi di crash come la scrive l'observer.
    async fn semina_crash(
        pool: &sqlx::PgPool,
        project: Uuid,
        unit: &str,
        status: &str,
        attempts: i32,
        cooldown_secs: Option<i64>,
    ) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO service_diagnoses \
                (project_id, unit, signal_kind, metric, status, detail, \
                 remediation_attempts, cooldown_until) \
             VALUES ($1, $2, 'crash', 'service_failed', $3, 'servizio non operativo', $4, \
                     CASE WHEN $5::bigint IS NULL THEN NULL \
                          ELSE NOW() + make_interval(secs => $5::bigint) END) \
             RETURNING id",
        )
        .bind(project)
        .bind(unit)
        .bind(status)
        .bind(attempts)
        .bind(cooldown_secs)
        .fetch_one(pool)
        .await
        .expect("seed crash")
    }

    async fn stato_di(pool: &sqlx::PgPool, id: Uuid) -> (String, Option<String>, i32) {
        sqlx::query_as("SELECT status, detail, remediation_attempts FROM service_diagnoses WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("rilettura diagnosi")
    }

    /// La regola, nuda: finiti i tentativi non si riavvia piu'. E' il freno che
    /// impedisce al presidio di diventare un riavviatore perpetuo di un servizio
    /// che non puo' partire.
    ///
    /// MUTAZIONE: togliendo il confronto sui tentativi (`repair_decision` che
    /// ritorna sempre `Attend` a presidio acceso) rosseggia la terza asserzione,
    /// col valore del difetto reale — un servizio riavviato all'infinito.
    /// IL CONFLITTO, riprodotto: il presidio non riavvia un servizio mentre un
    /// agente lavora sul progetto.
    ///
    /// Misurato il 03/08/2026 su DUE progetti indipendenti. agenda-corsi: il
    /// worker ripara alle 01:17:32 mentre il run d293ae40 e' attivo
    /// (01:10:01-01:30:53), che chiude `blocked_needs_input`. bacheca-attivita:
    /// riparazione alle 02:22:58 durante il run 90924d59 (02:14:43-02:36:55),
    /// che chiude `failed_diagnosed`. Su 1405 diagnosi, 78 hanno tentato un
    /// riavvio: la sovrapposizione non e' un caso di confine.
    ///
    /// L'ordine dei controlli e' parte del contratto: l'esaurimento dei
    /// tentativi viene PRIMA, o una diagnosi finita resterebbe `open` per
    /// sempre ogni volta che un agente lavora, senza che nessuno dichiari
    /// perche'.
    ///
    /// MUTAZIONE: togliere il ramo `agente_al_lavoro` fa tornare `Attend` alla
    /// prima asserzione, cioe' il riavvio sotto i piedi dell'agente.
    #[test]
    fn il_presidio_non_riavvia_mentre_un_agente_lavora() {
        assert_eq!(
            repair_decision_con_agente(true, 0, 3, true),
            RepairDecision::AgenteAlLavoro,
            "campo occupato: la diagnosi resta open e si riprende dopo"
        );
        assert_eq!(
            repair_decision_con_agente(true, 0, 3, false),
            RepairDecision::Attend,
            "campo libero: si ripara"
        );
        // L'esaurimento vince sul campo occupato: va dichiarato comunque.
        assert_eq!(
            repair_decision_con_agente(true, 3, 3, true),
            RepairDecision::AttemptsExhausted
        );
        // E il presidio spento resta spento.
        assert_eq!(
            repair_decision_con_agente(false, 0, 3, true),
            RepairDecision::Disabled
        );
    }

    #[test]
    fn il_presidio_smette_di_riavviare_quando_i_tentativi_sono_finiti() {
        assert_eq!(repair_decision(false, 0, 3), RepairDecision::Disabled);
        assert_eq!(repair_decision(true, 0, 3), RepairDecision::Attend);
        assert_eq!(repair_decision(true, 2, 3), RepairDecision::Attend);
        assert_eq!(
            repair_decision(true, 3, 3),
            RepairDecision::AttemptsExhausted
        );
    }

    /// Cosa si prende in carico e cosa no. `diagnosing` in particolare NON si
    /// tocca: e' una riga che qualcun altro sta gia' verificando, e prenderla
    /// significherebbe riavviargli il servizio sotto i piedi.
    ///
    /// MUTAZIONE: allargando la selezione a `status IN ('open','diagnosing')` —
    /// la forma che aveva `resolve_open_crashes` prima del fix del 28/07 —
    /// rosseggia il conteggio, con due righe invece di una.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn si_prende_in_carico_solo_cio_che_e_aperto_e_fuori_cooldown(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let attesa = semina_crash(&pool, project, "app-backend.service", "open", 0, None).await;
        semina_crash(&pool, project, "app-in-cooldown.service", "open", 1, Some(600)).await;
        semina_crash(&pool, project, "app-in-corso.service", "diagnosing", 1, None).await;
        semina_crash(&pool, project, "app-chiusa.service", "resolved", 0, None).await;
        semina_crash(
            &pool,
            project,
            "app-terminale.service",
            "failed_remediation",
            3,
            None,
        )
        .await;
        sqlx::query(
            "INSERT INTO service_diagnoses (project_id, unit, signal_kind, metric, status) \
             VALUES ($1, 'app-backend.service', 'anomaly', 'down', 'open')",
        )
        .bind(project)
        .execute(&pool)
        .await
        .expect("seed anomalia");

        let pendenti = pending_service_crashes(&pool, project, 10, 1800).await;
        assert_eq!(
            pendenti.len(),
            1,
            "una sola riga e' presidiabile: {pendenti:?}"
        );
        assert_eq!(pendenti[0].id, attesa);
        assert_eq!(pendenti[0].unit, "app-backend.service");
        assert_eq!(pendenti[0].attempts, 0);
    }

    /// Il lease e' cio' che rende innocuo un ciclo che si accavalla con un
    /// altro: la seconda presa in carico della stessa riga non avviene.
    ///
    /// MUTAZIONE: togliendo `AND status = 'open'` dalla UPDATE del lease,
    /// entrambe le chiamate ritornano `true` e due task riavvierebbero lo stesso
    /// servizio insieme — rosseggia la seconda asserzione.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn due_cicli_non_possono_prendere_la_stessa_diagnosi(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let id = semina_crash(&pool, project, "app-backend.service", "open", 0, None).await;

        assert!(
            lease_for_repair(&pool, id, 600).await,
            "la prima presa in carico riesce"
        );
        assert!(
            !lease_for_repair(&pool, id, 600).await,
            "la seconda non trova piu' una riga aperta: qualcun altro l'ha presa"
        );

        let (status, _, attempts) = stato_di(&pool, id).await;
        assert_eq!(status, "diagnosing");
        assert_eq!(
            attempts, 0,
            "prendere in carico non e' tentare: il tentativo si conta quando si riavvia"
        );
        // Il cooldown del prossimo tentativo e' gia' scritto: la riga, tornando
        // aperta, non viene ripresa dal ciclo successivo di quindici secondi.
        assert!(
            pending_service_crashes(&pool, project, 10, 1800)
                .await
                .is_empty(),
            "in carico e in cooldown: nessuno la ripresenta"
        );
    }

    /// Una diagnosi appena aperta NON e' ancora ammissibile al ripiego
    /// deterministico: l'AI non ha ancora avuto la sua occasione, e finche' la
    /// soglia non e' trascorsa il presidio deve limitarsi a ritentare il
    /// trigger esistente (vedi retry_ai_trigger), mai riavviare da solo.
    /// Trascorsa la soglia, senza che l'AI sia mai partita, diventa ammissibile.
    ///
    /// MUTAZIONE: togliendo il confronto sull'eta' (`fallback_eligible` sempre
    /// `true`) rosseggia la prima asserzione — e il difetto sarebbe la corsa
    /// gia' trovata in review: il riavvio deterministico precederebbe l'AI su
    /// ogni crash nuovo.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_diagnosi_appena_aperta_attende_i_ritentativi_ai_prima_del_ripiego(
        pool: sqlx::PgPool,
    ) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let id = semina_crash(&pool, project, "app-backend.service", "open", 0, None).await;

        let pendenti = pending_service_crashes(&pool, project, 10, 1800).await;
        assert_eq!(pendenti.len(), 1);
        assert!(
            !pendenti[0].fallback_eligible,
            "appena aperta: l'AI non ha ancora avuto la sua occasione"
        );

        sqlx::query("UPDATE service_diagnoses SET ts = NOW() - INTERVAL '1 hour' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("invecchia la diagnosi");
        let pendenti = pending_service_crashes(&pool, project, 10, 1800).await;
        assert!(
            pendenti[0].fallback_eligible,
            "trascorsa la soglia senza che l'AI sia mai partita: ammissibile al ripiego"
        );
    }

    /// Una diagnosi che l'AI ha GIA' preso in carico (`triggered_run_id`
    /// valorizzato) e che poi torna `open` — l'unico modo e' un'interruzione
    /// da riavvio di mcp-core, vedi `reopen_orphan_repair_leases` — e'
    /// ammissibile al ripiego SUBITO, senza aspettare la soglia d'eta': l'AI
    /// ha gia' avuto il suo turno, non ha senso ritentare lo stesso trigger
    /// all'infinito su un run che non concludera' mai.
    ///
    /// MUTAZIONE: togliendo `triggered_run_id IS NOT NULL` dalla condizione SQL
    /// rosseggia l'asserzione, con `fallback_eligible = false` nonostante l'AI
    /// abbia gia' un run per questa diagnosi.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_diagnosi_gia_affidata_all_ai_e_poi_riaperta_e_subito_ammissibile(
        pool: sqlx::PgPool,
    ) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let id = semina_crash(&pool, project, "app-backend.service", "open", 0, None).await;
        sqlx::query("UPDATE service_diagnoses SET triggered_run_id = $2 WHERE id = $1")
            .bind(id)
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .expect("simula un run AI interrotto e poi riaperto");

        let pendenti = pending_service_crashes(&pool, project, 10, 1800).await;
        assert_eq!(pendenti.len(), 1);
        assert!(
            pendenti[0].fallback_eligible,
            "l'AI ha gia' avuto un run per questa diagnosi: si passa al ripiego, non si ritenta all'infinito"
        );
    }

    /// Un tentativo che non ripara, quando ne restano altri, NON e' terminale:
    /// la riga torna visibile e disponibile, con scritto cosa non ha funzionato.
    ///
    /// MUTAZIONE: facendo scrivere `failed_remediation` anche al ramo con
    /// tentativi residui (cioe' ignorando `retry_left`) rosseggia la prima
    /// asserzione — e il difetto sarebbe reale: un intoppo transitorio
    /// chiuderebbe la riga per sempre al primo colpo, che e' la stessa forma di
    /// difetto che questo lavoro toglie a monte.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_tentativo_fallito_con_altri_a_disposizione_torna_visibile(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let id = semina_crash(&pool, project, "app-backend.service", "open", 0, None).await;
        assert!(lease_for_repair(&pool, id, 600).await);
        note_attempt(&pool, id).await;

        // Il servizio e' vivo ma la porta che il registro gli assegna e' muta:
        // verdetto dal giudice vero, su una prova fatta dal produttore vero.
        let porta = porta_muta().await;
        let verdetto = judge_recovery(&[fase(
            FASE_PRIMO_AVVIO,
            salute(ServiceState::Running, &[porta]).await,
        )]);
        let esito = RepairOutcome::Judged {
            verdict: verdetto,
            retry_left: true,
        };
        assert_eq!(
            apply_repair_outcome(&pool, id, &esito, "fatti").await.as_deref(),
            Some("open"),
            "restano tentativi: la diagnosi torna aperta, non chiusa in fallimento"
        );

        let (status, detail, attempts) = stato_di(&pool, id).await;
        assert_eq!(status, "open");
        assert_eq!(attempts, 1, "il tentativo eseguito e' contato");
        let detail = detail.unwrap_or_default();
        assert!(
            detail.contains(&porta.to_string()),
            "il tentativo lascia detto cosa non ha risposto: {detail}"
        );

        // Al giro dopo la riga NON viene ripresa subito: il cooldown scritto dal
        // lease spazia i tentativi.
        assert!(pending_service_crashes(&pool, project, 10, 1800)
            .await
            .is_empty());
    }

    /// IL FALSO POSITIVO del mandato: il servizio risponde, la diagnosi e'
    /// rimasta indietro. Si chiude su cio' che si e' MISURATO — quale porta ha
    /// risposto e come — e senza riavviare niente.
    ///
    /// MUTAZIONE: facendo chiudere la riga senza passare dal contratto (cioe'
    /// scrivendo `resolved` perche' il ciclo e' passato di nuovo) il detail non
    /// nomina piu' alcuna porta e rosseggia l'ultima asserzione: resterebbe una
    /// chiusura non verificabile a posteriori, che e' esattamente il segnale
    /// debole che mig 0654 ha tolto.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_diagnosi_superata_si_chiude_su_cio_che_ha_risposto(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let id = semina_crash(&pool, project, "app-backend.service", "open", 0, None).await;
        assert!(lease_for_repair(&pool, id, 600).await);

        // La salute viene dai produttori veri: stato + `probe_port` su un
        // listener che risponde davvero, durata da `stable_enough`.
        let porta = servizio_vivo_su_porta_effimera(STATUS_200).await;
        let salute = salute(ServiceState::Running, &[porta]).await;
        assert!(
            salute.meets_contract(),
            "premessa del caso: il servizio soddisfa il contratto"
        );

        let scritto = apply_repair_outcome(&pool, id, &RepairOutcome::NotNeeded, &salute.describe())
            .await;
        assert_eq!(scritto.as_deref(), Some("resolved"));

        let (_, detail, attempts) = stato_di(&pool, id).await;
        assert_eq!(
            attempts, 0,
            "non si e' riparato niente: nessun tentativo consumato"
        );
        let detail = detail.unwrap_or_default();
        assert!(
            detail.contains(&porta.to_string()) && detail.contains("200"),
            "la chiusura porta con se' la prova che l'ha decisa: {detail}"
        );
    }

    /// Il presidio vive in un task in memoria: a mcp-core riavviato, un lease
    /// piu' vecchio dell'avvio non puo' appartenere a nessun task vivo e la
    /// diagnosi va rimessa in circolo. Un lease piu' recente e' di questo
    /// processo e non si tocca.
    ///
    /// MUTAZIONE: rimuovendo il confronto sull'avvio (riaprendo qualunque
    /// `diagnosing` senza run) rosseggia la seconda asserzione, e il difetto
    /// sarebbe una riga strappata a un rimedio in corso.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn un_lease_anteriore_all_avvio_del_processo_viene_riaperto(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let vecchia = semina_crash(&pool, project, "app-vecchia.service", "open", 1, None).await;
        let recente = semina_crash(&pool, project, "app-recente.service", "open", 0, None).await;
        assert!(lease_for_repair(&pool, vecchia, 600).await);
        assert!(lease_for_repair(&pool, recente, 600).await);
        // Il lease della prima e' anteriore all'avvio di questo processo.
        sqlx::query("UPDATE service_diagnoses SET updated_at = NOW() - INTERVAL '10 minutes' WHERE id = $1")
            .bind(vecchia)
            .execute(&pool)
            .await
            .expect("invecchia il lease");

        // Processo avviato 60 secondi fa: tutto cio' che e' stato leasato prima
        // e' orfano per costruzione.
        let riaperte = reopen_orphan_repair_leases(&pool, 60).await;
        assert_eq!(riaperte, vec![(project, vecchia)]);
        assert_eq!(stato_di(&pool, vecchia).await.0, "open");
        assert_eq!(
            stato_di(&pool, recente).await.0,
            "diagnosing",
            "il lease di questo processo ha ancora il suo task: non si tocca"
        );
    }

    // ── Ri-armo su cambiamento della causa ───────────────────────────────────

    /// Produce una diagnosi FALLITA passando dal produttore vero (regola O): una
    /// riga in verifica chiusa da [`apply_repair_outcome`] con verdetto negativo
    /// e tentativi esauriti — la stessa strada di `spawn_verifica_esito` e del
    /// presidio. Poi RETRODATA il fallimento di un minuto, perche' il confronto
    /// "mutazione posteriore al fallimento" non dipenda dalla risoluzione del
    /// clock del DB fra due statement consecutivi.
    async fn diagnosi_fallita(pool: &sqlx::PgPool, project: Uuid, unit: &str) -> Uuid {
        let id = semina_crash(pool, project, unit, "diagnosing", 3, None).await;
        let esito = RepairOutcome::Judged {
            verdict: RecoveryVerdict::NotRecovered(RecoveryFailure::NotStable {
                phase: FASE_PRIMO_AVVIO,
            }),
            retry_left: false,
        };
        let scritto = apply_repair_outcome(pool, id, &esito, "evidenza del fallimento").await;
        assert_eq!(
            scritto.as_deref(),
            Some(STATO_FALLITA),
            "premessa: la diagnosi e' in stato terminale fallito"
        );
        sqlx::query(
            "UPDATE service_diagnoses SET updated_at = updated_at - INTERVAL '1 minute' \
              WHERE id = $1",
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("retrodata il fallimento");
        id
    }

    /// Scrive una mutazione col produttore vero (`record_mutation`, regola O):
    /// gli hash che il criterio confronta sono quelli derivati dai contenuti.
    async fn scrittura(
        pool: &sqlx::PgPool,
        project: Uuid,
        user: Uuid,
        path: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) {
        crate::file_mutations::record_mutation(
            pool,
            project,
            None,
            Some(Uuid::new_v4()),
            Some(user),
            path,
            "write_file",
            before,
            after,
            crate::file_mutations::ScopeAudit::none(),
        )
        .await
        .expect("mutazione registrata");
    }

    /// IL CASO DEL MANDATO (bacheca-attivita, 31/07/2026): il css viene corretto
    /// da un run successivo, e la diagnosi FALLITA deve tornare ammissibile al
    /// ciclo di verifica — `open`, tentativi azzerati, cooldown rimosso, motivo
    /// scritto — e rientrare nella selezione del presidio.
    ///
    /// MUTAZIONE: rompendo il ri-armo — [`rearm_evidence`] che ignora
    /// [`WriteFact::cambia_il_contenuto`] e ritorna `None`, oppure il watermark
    /// invertito in [`mutations_since`] (`created_at < $2`) — questo test
    /// rosseggia sulla prima asserzione, col valore del difetto reale: il
    /// servizio resta spento con una diagnosi fallita stantia mentre il codice
    /// e' sano.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_scrittura_sul_servizio_riarma_la_diagnosi_fallita(pool: sqlx::PgPool) {
        let (user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let unit = "bacheca-attivita-frontend.service";
        let id = diagnosi_fallita(&pool, project, unit).await;

        // La correzione: contenuto DIVERSO, dentro l'area del servizio.
        scrittura(
            &pool,
            project,
            user,
            "frontend/src/style.css",
            Some(".x{color:red;;}"),
            Some(".x{color:red}"),
        )
        .await;

        let failed = failed_service_diagnoses(&pool, project, MAX_REARM_PER_PASS).await;
        assert_eq!(failed.len(), 1, "la diagnosi fallita e' candidata al ri-armo");
        let aree = HashMap::from([(unit.to_string(), vec!["frontend".to_string()])]);
        assert_eq!(
            rearm_pass(&pool, project, &failed, &aree).await,
            vec![id],
            "una scrittura efficace nell'area del servizio ri-arma la diagnosi"
        );

        let (status, detail, attempts) = stato_di(&pool, id).await;
        assert_eq!(status, "open");
        assert_eq!(attempts, 0, "la causa e' cambiata: il nuovo ciclo ha i suoi tentativi");
        let detail = detail.unwrap_or_default();
        assert!(
            detail.contains("RI-ARMATA") && detail.contains("frontend/src/style.css"),
            "il motivo del ri-armo resta scritto sulla riga: {detail}"
        );
        let cooldown: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT cooldown_until FROM service_diagnoses WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("cooldown");
        assert!(cooldown.is_none(), "il segnale e' un fatto nuovo, non un retry in attesa");

        // Il cerchio si chiude: la riga rientra nella selezione del presidio.
        let pending = pending_service_crashes(&pool, project, 10, 1800).await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
    }

    /// I due NON-segnali: una riscrittura a contenuto IDENTICO (l'attivita' che
    /// non produce niente, il caso che `cambia_il_contenuto` esiste per vedere)
    /// e una scrittura ANTERIORE al fallimento (il lavoro che il fallimento ha
    /// gia' giudicato). Nessuno dei due deve riaprire il ciclo: un ri-armo su
    /// questi sarebbe il retry cieco che la regola H vieta, solo travestito.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn riscritture_identiche_e_scritture_anteriori_non_riarmano(pool: sqlx::PgPool) {
        let (user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let unit = "app-frontend.service";

        // Scrittura ANTERIORE (efficace, in area): registrata PRIMA che il
        // fallimento venga timbrato, quindi gia' compresa nel giudizio.
        scrittura(
            &pool,
            project,
            user,
            "frontend/vite.config.js",
            Some("export default {}"),
            Some("export default { server: {} }"),
        )
        .await;
        // Il fallimento arriva DOPO quella scrittura; niente retrodatazione qui,
        // il watermark deve restare successivo alla mutazione.
        let id = semina_crash(&pool, project, unit, "diagnosing", 3, None).await;
        let esito = RepairOutcome::Judged {
            verdict: RecoveryVerdict::NotRecovered(RecoveryFailure::NotStable {
                phase: FASE_PRIMO_AVVIO,
            }),
            retry_left: false,
        };
        assert_eq!(
            apply_repair_outcome(&pool, id, &esito, "fallita dopo la scrittura")
                .await
                .as_deref(),
            Some(STATO_FALLITA)
        );

        // Riscrittura POSTERIORE ma IDENTICA: il write c'e', il contenuto no.
        scrittura(
            &pool,
            project,
            user,
            "frontend/src/style.css",
            Some(".x{color:red}"),
            Some(".x{color:red}"),
        )
        .await;

        let failed = failed_service_diagnoses(&pool, project, MAX_REARM_PER_PASS).await;
        let aree = HashMap::from([(unit.to_string(), vec!["frontend".to_string()])]);
        assert!(
            rearm_pass(&pool, project, &failed, &aree).await.is_empty(),
            "ne' la scrittura anteriore ne' la riscrittura identica sono un segnale"
        );
        assert_eq!(stato_di(&pool, id).await.0, STATO_FALLITA);
    }

    /// L'AREA decide: la stessa scrittura efficace ri-arma il servizio la cui
    /// area la contiene e NON quello di un'altra area. Con area ignota (`"."`)
    /// qualunque scrittura del progetto e' ammissibile — dichiarato, non lasco:
    /// il contratto a valle verifica comunque sul servizio vero.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_scrittura_fuori_dall_area_del_servizio_non_riarma(pool: sqlx::PgPool) {
        let (user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let unit = "app-frontend.service";
        let id = diagnosi_fallita(&pool, project, unit).await;

        scrittura(
            &pool,
            project,
            user,
            "backend/server.js",
            Some("const p = 1;"),
            Some("const p = 2;"),
        )
        .await;

        let failed = failed_service_diagnoses(&pool, project, MAX_REARM_PER_PASS).await;
        let stretta = HashMap::from([(unit.to_string(), vec!["frontend".to_string()])]);
        assert!(
            rearm_pass(&pool, project, &failed, &stretta).await.is_empty(),
            "una scrittura sul backend non dice niente del frontend"
        );
        assert_eq!(stato_di(&pool, id).await.0, STATO_FALLITA);

        // Stessa scrittura, area ignota: il segnale e' ammissibile.
        let tutto = HashMap::from([(unit.to_string(), vec![".".to_string()])]);
        assert_eq!(rearm_pass(&pool, project, &failed, &tutto).await, vec![id]);
        assert_eq!(stato_di(&pool, id).await.0, "open");
    }

    /// Il ri-armo ESPLICITO dal pannello: la richiesta umana e' il segnale
    /// strutturato, ma vale solo per una diagnosi davvero in stato terminale
    /// fallito e davvero di quel progetto.
    ///
    /// MUTAZIONE: togliendo da [`rearm_diagnosis`] la condizione sullo stato
    /// (`AND status = 'failed_remediation'`) rosseggia la prima asserzione — e
    /// il difetto sarebbe un bottone capace di strappare una riga a un rimedio
    /// in corso; togliendo quella sul progetto rosseggia l'ultima, e il difetto
    /// sarebbe un endpoint che ri-arma diagnosi altrui.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_ritentativo_esplicito_riarma_solo_una_diagnosi_fallita(pool: sqlx::PgPool) {
        let (_user, project) = nexus_migrations_embedded::seed_identita_meta(&pool).await;
        let aperta = semina_crash(&pool, project, "app-aperta.service", "open", 0, None).await;
        assert!(
            rearm_diagnosis(&pool, project, aperta, REARM_EXPLICIT_REASON)
                .await
                .is_none(),
            "una diagnosi non fallita non si ri-arma: non c'e' niente da riaprire"
        );
        assert_eq!(stato_di(&pool, aperta).await.0, "open");

        let fallita = diagnosi_fallita(&pool, project, "app-rotta.service").await;
        assert_eq!(
            rearm_diagnosis(&pool, project, fallita, REARM_EXPLICIT_REASON).await,
            Some(fallita)
        );
        let (status, detail, attempts) = stato_di(&pool, fallita).await;
        assert_eq!(status, "open");
        assert_eq!(attempts, 0);
        assert!(detail.unwrap_or_default().contains("richiesta esplicita"));

        // Progetto sbagliato: la riga non si tocca.
        let altra = diagnosi_fallita(&pool, project, "app-altra.service").await;
        assert!(
            rearm_diagnosis(&pool, Uuid::new_v4(), altra, REARM_EXPLICIT_REASON)
                .await
                .is_none()
        );
        assert_eq!(stato_di(&pool, altra).await.0, STATO_FALLITA);
    }

    /// L'area del servizio dalla working dir registrata: relativa alla root,
    /// robusta a separatori e case di Windows, col confine di SEGMENTO (una
    /// root `d:/proj` non contiene `d:/proj-x`). Working dir ignota, uguale
    /// alla root o fuori root -> `"."`, il ripiego dichiarato.
    #[test]
    fn l_area_del_servizio_e_relativa_alla_root() {
        assert_eq!(
            relative_service_area(Some(r"D:\proj\frontend"), r"D:\proj"),
            vec!["frontend".to_string()]
        );
        assert_eq!(
            relative_service_area(Some(r"d:\PROJ\Frontend\"), r"D:\proj"),
            vec!["frontend".to_string()],
            "il filesystem di Windows e' case-insensitive: l'area deve esserlo"
        );
        for (wd, root) in [
            (None, r"D:\proj"),
            (Some(r"D:\proj"), r"D:\proj"),
            (Some(r"C:\altro\posto"), r"D:\proj"),
            (Some(r"D:\proj-x\api"), r"D:\proj"),
            (Some(r"D:\proj\x"), ""),
        ] {
            assert_eq!(
                relative_service_area(wd, root),
                vec![".".to_string()],
                "area non determinabile ({wd:?}, {root:?}): tutto il progetto"
            );
        }
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
