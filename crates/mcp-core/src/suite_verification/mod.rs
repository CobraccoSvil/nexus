//! Punto unico (regola L) della VERIFICA A SUITE: «qual e' l'esito della suite
//! di test per QUESTO stato del codice?».
//!
//! La domanda la ponevano TRE attori senza memoria condivisa, e ognuno la
//! rispondeva rieseguendo: il `final_gate` (la suite e' un criterio del gate,
//! via `criteria_runner::check_run_command` sugli step del profilo di verifica),
//! l'agente (`run_playwright_tests`, per auto-convincersi), e il ciclo review
//! dopo ogni rimando in correzione. Nessuno riconosceva l'esito dell'altro.
//!
//! MISURATO il 31/07/2026 sul progetto bacheca-attivita (tabella `jobs`, kind
//! `playwright_test`): 53 esecuzioni della stessa suite sulla stessa app in una
//! serata, 31 fallite e 21 passate. I rossi erano i due test sensibili al
//! cold-start di Vite — falliscono nei ~20 secondi dopo un riavvio del servizio,
//! passano a caldo (quattro verdi pieni consecutivi misurati). La catena si e'
//! ripetuta piu' volte: riavvio del servizio, suite subito, rosso, il ciclo di
//! correzione tratta il rosso come un difetto reale, il correttore MODIFICA
//! CODICE SANO e introduce un difetto vero (misurati: un `css_syntax_error` che
//! ha fatto crashare il frontend, e un TS2322 in `useActivitiesApi.ts`), nuovo
//! rosso — questa volta genuino — e altri cicli. La flakiness non ritardava solo
//! la chiusura: FABBRICAVA regressioni.
//!
//! Due fatti, percio' due presidi, entrambi qui:
//!
//! 1. **L'esito vale per lo STATO su cui e' girato.** Una richiesta di verifica
//!    con [`ChiaveDiStato`] gia' risolta ritorna l'esito memorizzato invece di
//!    rieseguire; una scrittura cambia la chiave e la invalida da se'. La chiave
//!    non e' solo il codice: una suite E2E interroga anche i SERVIZI, quindi vi
//!    entra la generazione d'ambiente (vedi [`state_key`]). Senza, un `passed`
//!    memorizzato sopravviverebbe allo spegnimento del servizio che lo aveva
//!    reso vero — un fail-open, cioe' l'errore peggiore di tutti quelli che
//!    questo modulo esiste per togliere.
//!
//! 2. **Il rosso non riprodotto non e' un difetto** (regola M). Un esito fallito
//!    i cui test falliti RIPASSANO alla riesecuzione mirata immediata, a chiave
//!    di stato IDENTICA, e' [`SuiteOutcome::Flaky`]: un esito PROPRIO (regola N,
//!    canonico `flaky`), che non apre il ciclo di correzione e non boccia il
//!    gate, ma resta scritto, conteggiato e visibile come cio' che e' — un
//!    debito di TEST, non un difetto dell'app.
//!
//!    NON e' un "ritenta finche' non passa" (sarebbe la toppa della regola H, e
//!    nasconderebbe i fallimenti veri): e' UNA riesecuzione, mirata ai soli test
//!    falliti, il cui unico scopo e' CLASSIFICARE. Un esito confermato resta
//!    fallito, e un caso non classificabile resta fallito — mai il contrario.
//!
//! Cosa questo modulo NON fa, di proposito: inseguire le cause di flakiness una
//! per una. Il cold-start di Vite era la causa di ieri; scrivere un'attesa per
//! quella causa lascerebbe muto il sistema alla prossima (regola H, "niente
//! varianti a codice"). Qui la flakiness si MISURA e si dichiara; chi vorra'
//! potra' poi scaldare il servizio al riavvio, e lo fara' con la misura in mano.

pub mod memo;
pub mod state_key;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

/// Tetto di tempo di UNA esecuzione di suite. E' anche il PAVIMENTO per chi
/// delega da un contesto con un tetto piu' stretto: i criteri del final gate
/// nascono col timeout dei comandi di build (180s), che per una suite E2E e'
/// un'interruzione, non una verifica — e un'interruzione, letta a valle, e' un
/// fallimento indistinguibile da un difetto vero.
pub const TIMEOUT_SUITE_DEFAULT_S: u64 = 600;

/// Eta' massima di default di un esito riusabile (mig 0663,
/// `agent.testing.suite_memo_ttl_seconds`). Vale solo a chiave assente.
pub const MEMO_TTL_DEFAULT_S: u64 = 900;

/// Esito canonico di una verifica a suite (regola N: identificatori in inglese,
/// uno per esito, validi sul wire, nel DB e nei log).
///
/// `Flaky` sta FRA `Passed` e `TestsFailed` e non e' riducibile a nessuno dei
/// due: dire "passata" nasconderebbe un debito reale, dire "fallita" manderebbe
/// il correttore a riparare codice sano — che e' esattamente il difetto
/// misurato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteOutcome {
    /// Suite verde: exit code 0.
    Passed,
    /// Fallita alla prima esecuzione, ripassata alla riesecuzione mirata a
    /// stato del codice invariato: instabilita' del TEST, non difetto dell'app.
    Flaky,
    /// La suite e' partita e almeno un test e' rosso, riprodotto.
    TestsFailed,
    /// Il runner non e' arrivato a eseguire un solo test (webServer non
    /// avviato, config rotta): non e' un risultato di test, e non va raccontato
    /// come "0 passati, 0 falliti" (regola M).
    SetupFailed,
}

/// Identificatori canonici degli esiti (regola N). Nominati una volta e usati
/// dalle tre facce del vocabolario — scrittura, stato della riga `jobs`,
/// rilettura — cosi' un refuso in una delle tre non puo' passare inosservato.
pub const ESITO_PASSED: &str = "passed";
pub const ESITO_FLAKY: &str = "flaky";
pub const ESITO_TESTS_FAILED: &str = "tests_failed";
pub const ESITO_SETUP_FAILED: &str = "setup_failed";
/// Stato della riga `jobs` per un esito che blocca (entrambi i fallimenti).
pub const STATO_FAILED: &str = "failed";

impl SuiteOutcome {
    /// Identificatore canonico (wire/DB/log).
    pub fn as_str(self) -> &'static str {
        match self {
            SuiteOutcome::Passed => ESITO_PASSED,
            SuiteOutcome::Flaky => ESITO_FLAKY,
            SuiteOutcome::TestsFailed => ESITO_TESTS_FAILED,
            SuiteOutcome::SetupFailed => ESITO_SETUP_FAILED,
        }
    }

    /// Valore della colonna `jobs.status`. `flaky` e' uno stato PROPRIO anche
    /// li': mapparlo su `passed` lo renderebbe invisibile (e la misura della
    /// flakiness impossibile), mapparlo su `failed` rimetterebbe il pannello a
    /// mostrare come difetti dell'app dei rossi che non lo sono.
    pub fn job_status(self) -> &'static str {
        match self {
            SuiteOutcome::Passed => ESITO_PASSED,
            SuiteOutcome::Flaky => ESITO_FLAKY,
            SuiteOutcome::TestsFailed | SuiteOutcome::SetupFailed => STATO_FAILED,
        }
    }

    /// `true` se questo esito deve BLOCCARE la chiusura: il criterio del gate
    /// fallisce e il ciclo di correzione parte. `Flaky` non blocca — e' la
    /// decisione centrale di questo modulo.
    pub fn blocca_la_chiusura(self) -> bool {
        matches!(self, SuiteOutcome::TestsFailed | SuiteOutcome::SetupFailed)
    }

    /// Ricostruisce l'esito dal suo identificatore canonico (lettura dalla
    /// memoria). Un valore ignoto ritorna `None`: la memoria di un vocabolario
    /// che non conosciamo non si interpreta a indovinare, si riesegue.
    pub fn da_str(s: &str) -> Option<Self> {
        match s {
            _ if s == ESITO_PASSED => Some(SuiteOutcome::Passed),
            _ if s == ESITO_FLAKY => Some(SuiteOutcome::Flaky),
            _ if s == ESITO_TESTS_FAILED => Some(SuiteOutcome::TestsFailed),
            _ if s == ESITO_SETUP_FAILED => Some(SuiteOutcome::SetupFailed),
            _ => None,
        }
    }
}

/// Conteggi di una esecuzione, dai segnali che il runner produce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuiteStats {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Flaky dichiarati da Playwright stesso (retry configurati nel progetto):
    /// distinto dalla NOSTRA classificazione, che nasce dalla riesecuzione
    /// mirata. Sommarli sarebbe confondere due misure diverse.
    pub flaky_reported: usize,
    /// Nomi dei test falliti, per la DESCRIZIONE (pannello, resoconto). Non
    /// decidono nulla: la classificazione guarda exit code e conteggi
    /// (regola M), mai questi.
    pub failed_tests: Vec<String>,
}

impl SuiteStats {
    /// Test effettivamente eseguiti. Distingue "la suite e' partita" da "il
    /// runner e' morto prima": e' il segnale che separa `TestsFailed` da
    /// `SetupFailed`.
    pub fn eseguiti(&self) -> usize {
        self.passed + self.failed + self.skipped
    }
}

/// Esito grezzo di UNA esecuzione del processo, prima di ogni classificazione.
#[derive(Debug, Clone, Default)]
pub struct SuiteRun {
    /// Exit code del processo. `None` = timeout o processo mai terminato.
    pub exit_code: Option<i32>,
    pub stats: SuiteStats,
    /// Testo pronto per il chiamante (report del runner). Mai analizzato per
    /// decidere: serve a MOSTRARE, non a classificare.
    pub testo: String,
    /// Riga `jobs` prodotta da questa esecuzione, se il runner l'ha registrata.
    pub job_id: Option<Uuid>,
}

/// Perche' si sta eseguendo: la riesecuzione mirata non e' una verifica, e' una
/// DOMANDA sulla precedente. Non entra in memoria come esito di suite e non
/// riapre a sua volta una classificazione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopoEsecuzione {
    Suite,
    RiesecuzioneMirata,
}

/// Cosa eseguire. Il comando e' quello del chiamante, INTERO: il gate esegue lo
/// step del profilo di verifica, l'agente il comando che il tool ha costruito.
/// Ricomporlo qui significherebbe una terza idea di "come si lancia la suite".
#[derive(Debug, Clone)]
pub struct SuiteInvocation {
    pub command: String,
    /// Directory relativa alla radice del run. `None` = radice.
    pub working_dir: Option<String>,
    pub timeout_s: u64,
    pub scopo: ScopoEsecuzione,
}

impl SuiteInvocation {
    /// Invocazione di una suite intera (scopo `Suite`): la riesecuzione mirata
    /// la costruisce il punto unico, non i chiamanti.
    pub fn suite(command: impl Into<String>, working_dir: Option<String>, timeout_s: u64) -> Self {
        Self {
            command: command.into(),
            working_dir,
            timeout_s,
            scopo: ScopoEsecuzione::Suite,
        }
    }
}

/// Esecutore del processo di test. UNA implementazione reale
/// ([`crate::agent_tools::testing::PlaywrightProcessExecutor`]): e' il punto in
/// cui la suite viene davvero lanciata, con l'ambiente (BASE_URL dalle porte
/// allocate) e la registrazione del job. Il trait esiste per i test, che devono
/// poter contare le esecuzioni senza avviare un browser.
#[async_trait]
pub trait SuiteExecutor: Send + Sync {
    async fn esegui(&self, inv: &SuiteInvocation) -> Result<SuiteRun, String>;
}

/// Cio' che la memoria restituisce su una chiave gia' risolta.
#[derive(Debug, Clone)]
pub struct EsitoMemorizzato {
    pub job_id: Uuid,
    pub outcome: SuiteOutcome,
    /// Da quanto e' stato registrato: entra nel testo, perche' un esito riusato
    /// senza dire quanto e' vecchio e' un esito di cui non ci si puo' fidare.
    pub eta: Duration,
    pub messaggio: String,
    pub test_instabili: Vec<String>,
    /// Conteggi dell'esecuzione che ha prodotto l'esito: un esito riusato che
    /// si presentasse con degli zeri verrebbe letto come "suite vuota".
    pub stats: SuiteStats,
}

/// Memoria degli esiti per (suite, stato). Impl reale su `jobs` del DB-progetto
/// ([`memo::PgSuiteMemo`]): il registro degli esiti esisteva gia', gli mancava
/// la chiave.
#[async_trait]
pub trait SuiteMemo: Send + Sync {
    async fn cerca(
        &self,
        suite_key: &str,
        state_key: &str,
        ttl: Duration,
    ) -> Option<EsitoMemorizzato>;

    /// Scrive l'esito FINALE sulla riga del run e, se la chiave e' calcolabile,
    /// la lega a quello stato.
    ///
    /// L'esito si scrive SEMPRE, anche a memoria disattivata: `flaky` nasce
    /// dopo la misura del runner, e se la sua scrittura dipendesse dal flag
    /// della memoria un'installazione con la memoria spenta mostrerebbe come
    /// fallimenti dell'app dei rossi gia' classificati come instabili. Il flag
    /// governa la LETTURA ([`SuiteMemo::cerca`]), non la registrazione.
    ///
    /// `job_id` e' la riga che il runner ha scritto; senza riga non c'e' niente
    /// da marcare, e la prossima verifica rieseguira' — comportamento sicuro.
    ///
    /// `chiavi` = `(suite_key, state_key)`; `None` significa chiave non
    /// calcolabile: l'esito si scrive ma non diventa riusabile.
    async fn registra_esito(
        &self,
        job_id: Uuid,
        outcome: SuiteOutcome,
        test_instabili: &[String],
        chiavi: Option<(&str, &str)>,
    );
}

/// Annuncio dell'esito FINALE ai pannelli, quando la classificazione ha
/// cambiato cio' che il runner aveva misurato.
///
/// Il runner emette il suo evento appena il processo termina, quindi con
/// l'esito GREZZO: per una suite poi riconosciuta instabile quell'evento dice
/// "falliti". Senza un secondo annuncio il pannello resterebbe rosso fino al
/// ricaricamento della lista — cioe' proprio il messaggio che questo presidio
/// esiste per non dare piu'.
#[async_trait]
pub trait EsitoAnnunciato: Send + Sync {
    async fn annuncia(&self, job_id: Uuid, outcome: SuiteOutcome, test_instabili: &[String]);
}

/// Chiave dello stato su cui una suite gira. Ricalcolabile: la classificazione
/// flaky pretende che lo stato sia lo STESSO prima e dopo la riesecuzione, e
/// l'unico modo di saperlo e' richiederla di nuovo.
#[async_trait]
pub trait ChiaveDiStato: Send + Sync {
    async fn chiave(&self) -> state_key::StateKey;
}

/// Configurazione DB-driven (regola G): letta a monte e passata.
#[derive(Debug, Clone, Copy)]
pub struct SuitePolicy {
    /// `agent.testing.suite_memo_enabled`.
    pub memo_abilitata: bool,
    /// `agent.testing.suite_memo_ttl_seconds`: tetto d'eta' di un esito
    /// riusabile. La chiave copre codice e generazione dei servizi, non il
    /// mondo intero (i dati di un DB, un file fuori dall'albero): il tetto e'
    /// il limite dichiarato di cio' che la chiave non puo' vedere.
    pub memo_ttl: Duration,
    /// `agent.testing.flaky_reclassify_enabled`.
    pub riclassificazione_abilitata: bool,
}

impl Default for SuitePolicy {
    /// Safe-default identici ai valori della migrazione 0663: valgono solo a
    /// chiave assente, mai come magic fallback nella logica.
    fn default() -> Self {
        Self {
            memo_abilitata: true,
            memo_ttl: Duration::from_secs(MEMO_TTL_DEFAULT_S),
            riclassificazione_abilitata: true,
        }
    }
}

impl SuitePolicy {
    /// Legge la policy dai `settings` (punto unico `settings::get_setting`).
    pub async fn dal_db(db: &sqlx::PgPool) -> Self {
        let d = Self::default();
        Self {
            memo_abilitata: booleano(db, "agent.testing.suite_memo_enabled", d.memo_abilitata)
                .await,
            memo_ttl: Duration::from_secs(
                intero(
                    db,
                    "agent.testing.suite_memo_ttl_seconds",
                    d.memo_ttl.as_secs() as i64,
                )
                .await
                .max(0) as u64,
            ),
            riclassificazione_abilitata: booleano(
                db,
                "agent.testing.flaky_reclassify_enabled",
                d.riclassificazione_abilitata,
            )
            .await,
        }
    }
}

async fn booleano(db: &sqlx::PgPool, key: &str, default: bool) -> bool {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off" | ""
            )
        })
        .unwrap_or(default)
}

async fn intero(db: &sqlx::PgPool, key: &str, default: i64) -> i64 {
    crate::settings::get_setting(db, key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// Da dove viene l'esito che il chiamante sta leggendo. Dichiarato SEMPRE: un
/// esito riusato presentato come appena misurato e' una bugia gentile, e a
/// valle nessuno potrebbe piu' distinguere "verificato ora" da "verificato
/// prima, su questo stesso codice".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrigineEsito {
    /// Suite eseguita adesso.
    Eseguita,
    /// Esito ripreso dalla memoria: nessuna esecuzione.
    Memoria { job_id: Uuid, eta_secondi: u64 },
}

/// Perche' un esito fallito NON e' stato classificato. Ogni ramo lascia l'esito
/// dov'era (fallito): non e' un vocabolario di scuse, e' cio' che il pannello e
/// il resoconto devono poter dire invece di tacere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotivoNonClassificato {
    /// Riclassificazione disattivata dalla configurazione.
    Disabilitata,
    /// La chiave di stato non e' calcolabile: senza sapere se il codice e'
    /// cambiato, un secondo verde non prova nulla.
    StatoIgnoto,
    /// Il codice (o l'ambiente) e' cambiato fra le due esecuzioni: le due
    /// misure non parlano dello stesso oggetto.
    StatoCambiato,
    /// Il comando non e' componibile con la riesecuzione mirata (pipe,
    /// concatenazioni, redirezioni): appendere un flag lo cambierebbe di senso.
    ComandoNonComponibile,
    /// La riesecuzione non ha eseguito nemmeno un test: non ha risposto alla
    /// domanda (tipico se il runner non sa quali fossero i falliti).
    NessunTestRieseguito,
    /// La riesecuzione non e' partita (errore del runner).
    RiesecuzioneNonEseguita,
}

impl MotivoNonClassificato {
    /// Testo del motivo, per il resoconto e per l'evidence del criterio.
    pub fn descrizione(self) -> &'static str {
        match self {
            Self::Disabilitata => {
                "riclassificazione flaky disattivata (agent.testing.flaky_reclassify_enabled)"
            }
            Self::StatoIgnoto => {
                "stato del codice non calcolabile: non si puo' accertare che la \
                 riesecuzione parli dello stesso codice"
            }
            Self::StatoCambiato => "il codice o i servizi sono cambiati fra le due esecuzioni",
            Self::ComandoNonComponibile => {
                "comando non componibile con la riesecuzione mirata \
                 (pipe/concatenazioni/redirezioni)"
            }
            Self::NessunTestRieseguito => "la riesecuzione mirata non ha eseguito alcun test",
            Self::RiesecuzioneNonEseguita => "la riesecuzione mirata non e' partita",
        }
    }
}

/// Verdetto della riesecuzione mirata. PURO: input i due esiti grezzi + il
/// fatto che lo stato sia rimasto lo stesso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Riclassificazione {
    /// I test falliti sono ripassati: instabilita' del test.
    Flaky,
    /// Sono falliti di nuovo: il difetto e' reale.
    ConfermatoFallito,
    /// Non si e' potuto dire: l'esito resta quello di prima (fallito).
    NonClassificato(MotivoNonClassificato),
}

/// Esito della verifica, quello che i tre chiamanti leggono.
#[derive(Debug, Clone)]
pub struct SuiteVerification {
    pub outcome: SuiteOutcome,
    pub origine: OrigineEsito,
    pub stats: SuiteStats,
    /// Test dichiarati instabili (valorizzato solo su [`SuiteOutcome::Flaky`]).
    pub test_instabili: Vec<String>,
    /// Perche' un fallito non e' stato riclassificato, quando non lo e' stato.
    pub motivo_non_classificato: Option<MotivoNonClassificato>,
    pub exit_code: Option<i32>,
    pub testo: String,
    pub job_id: Option<Uuid>,
    /// Chiave su cui vale questo esito. `None` = non calcolabile (nessuna
    /// memorizzazione, nessuna riclassificazione).
    pub state_key: Option<String>,
    pub suite_key: String,
}

impl SuiteVerification {
    /// Riga sintetica che DICHIARA cos'e' successo: usata nel testo del tool,
    /// nell'evidence del criterio del gate e nei log. Un solo punto, cosi' i
    /// tre chiamanti non raccontano tre versioni della stessa verifica.
    pub fn dichiarazione(&self) -> String {
        let base = match (&self.origine, self.outcome) {
            (OrigineEsito::Memoria { job_id, eta_secondi }, esito) => format!(
                "ESITO RIUSATO ({}): la suite era gia' stata eseguita su questo IDENTICO \
                 stato del codice {}s fa (job {}). Nessuna riesecuzione.",
                esito.as_str(),
                eta_secondi,
                job_id
            ),
            (OrigineEsito::Eseguita, SuiteOutcome::Flaky) => format!(
                "ESITO flaky: {} test falliti alla prima esecuzione sono RIPASSATI alla \
                 riesecuzione mirata, a codice e servizi INVARIATI. E' instabilita' dei \
                 TEST (debito di test), NON un difetto dell'applicazione: non modificare \
                 il codice dell'app per questo.{}",
                self.test_instabili.len(),
                if self.test_instabili.is_empty() {
                    String::new()
                } else {
                    format!(" Test instabili: {}", self.test_instabili.join(", "))
                }
            ),
            (OrigineEsito::Eseguita, esito) => {
                format!("ESITO {}: suite eseguita adesso.", esito.as_str())
            }
        };
        match self.motivo_non_classificato {
            Some(m) if self.outcome.blocca_la_chiusura() => {
                format!("{base} Fallimento NON riclassificato: {}.", m.descrizione())
            }
            _ => base,
        }
    }
}

/// Verifica una suite: memoria, esecuzione, classificazione. E' l'unica strada.
///
/// `chiave` viene interrogata DUE volte quando serve classificare: prima
/// dell'esecuzione e dopo la riesecuzione mirata. Due letture e non una perche'
/// la domanda della seconda non e' "qual e' lo stato" ma "e' lo stesso di
/// prima": un correttore che scrive mentre la suite gira renderebbe le due
/// misure incomparabili, e chiamare flaky quel caso sarebbe esattamente
/// l'errore che questo modulo esiste per non fare.
pub async fn verifica_suite(
    executor: &dyn SuiteExecutor,
    memo: Option<&dyn SuiteMemo>,
    chiave: &dyn ChiaveDiStato,
    policy: SuitePolicy,
    inv: &SuiteInvocation,
    annuncio: Option<&dyn EsitoAnnunciato>,
) -> Result<SuiteVerification, String> {
    let suite_key = suite_key(&inv.command, inv.working_dir.as_deref());
    let chiave_iniziale = chiave.chiave().await.valore();

    // ── 1. Memoria ───────────────────────────────────────────────────────────
    if policy.memo_abilitata {
        if let (Some(memo), Some(sk)) = (memo, chiave_iniziale.as_deref()) {
            if let Some(hit) = memo.cerca(&suite_key, sk, policy.memo_ttl).await {
                return Ok(esito_riusato(hit, sk, &suite_key));
            }
        }
    }

    // ── 2-3. Esecuzione e classificazione ────────────────────────────────────
    let (run, esito) =
        esegui_e_classifica(executor, chiave, policy, inv, chiave_iniziale.as_deref()).await?;
    let EsitoClassificato {
        outcome,
        outcome_grezzo,
        motivo_non_classificato,
    } = esito;
    let test_instabili = if outcome == SuiteOutcome::Flaky {
        run.stats.failed_tests.clone()
    } else {
        Vec::new()
    };

    let chiave_finale = chiave.chiave().await.valore();
    let chiave_memorizzabile =
        chiave_memorizzabile(&chiave_iniziale, &chiave_finale, &suite_key);

    let mut verifica = SuiteVerification {
        outcome,
        origine: OrigineEsito::Eseguita,
        stats: run.stats,
        test_instabili,
        motivo_non_classificato,
        exit_code: run.exit_code,
        testo: run.testo,
        job_id: run.job_id,
        state_key: chiave_memorizzabile,
        suite_key: suite_key.clone(),
    };

    // ── 4. Registrazione e annuncio ──────────────────────────────────────────
    registra_e_annuncia(memo, annuncio, &verifica, outcome, outcome_grezzo).await;

    verifica.testo = format!("{}\n{}", verifica.dichiarazione(), verifica.testo);
    Ok(verifica)
}

/// Scrive l'esito finale sulla riga del run e, se serve, lo annuncia ai
/// pannelli.
///
/// L'esito si scrive SEMPRE (e' l'unico posto in cui `flaky` esiste: il runner
/// ha misurato un fallimento, la classificazione e' arrivata dopo) e diventa
/// RIUSABILE solo se la chiave e' calcolabile. L'annuncio invece parte solo se
/// la classificazione ha SPOSTATO l'esito: l'evento del runner ha gia'
/// raccontato cio' che aveva misurato, e ripeterlo identico sarebbe un secondo
/// avviso per lo stesso fatto.
async fn registra_e_annuncia(
    memo: Option<&dyn SuiteMemo>,
    annuncio: Option<&dyn EsitoAnnunciato>,
    verifica: &SuiteVerification,
    outcome: SuiteOutcome,
    outcome_grezzo: SuiteOutcome,
) {
    let Some(job_id) = verifica.job_id else {
        return;
    };
    if let Some(memo) = memo {
        let chiavi = verifica
            .state_key
            .as_deref()
            .map(|sk| (verifica.suite_key.as_str(), sk));
        memo.registra_esito(job_id, outcome, &verifica.test_instabili, chiavi)
            .await;
    }
    if let Some(annuncio) = annuncio {
        if outcome != outcome_grezzo {
            annuncio
                .annuncia(job_id, outcome, &verifica.test_instabili)
                .await;
        }
    }
}

/// Esito di una esecuzione dopo la classificazione: quello finale, quello che
/// il runner aveva misurato (servono entrambi — l'annuncio parte solo se sono
/// diversi) e il motivo per cui un fallito non e' stato classificato.
struct EsitoClassificato {
    outcome: SuiteOutcome,
    outcome_grezzo: SuiteOutcome,
    motivo_non_classificato: Option<MotivoNonClassificato>,
}

/// Esegue la suite e, se e' fallita coi test, la sottopone alla riesecuzione
/// mirata.
///
/// Solo un fallimento di TEST si classifica: un setup mai partito non ha test
/// da rieseguire, e chiamare instabile un runner che non e' partito
/// nasconderebbe un'app che non si avvia.
async fn esegui_e_classifica(
    executor: &dyn SuiteExecutor,
    chiave: &dyn ChiaveDiStato,
    policy: SuitePolicy,
    inv: &SuiteInvocation,
    chiave_iniziale: Option<&str>,
) -> Result<(SuiteRun, EsitoClassificato), String> {
    let run = executor.esegui(inv).await?;
    let outcome_grezzo = classifica_esito(run.exit_code, run.stats.eseguiti());
    let riclassificazione = if outcome_grezzo == SuiteOutcome::TestsFailed {
        Some(riclassifica(executor, chiave, policy, inv, chiave_iniziale).await)
    } else {
        None
    };
    let esito = EsitoClassificato {
        outcome: match riclassificazione {
            Some(Riclassificazione::Flaky) => SuiteOutcome::Flaky,
            _ => outcome_grezzo,
        },
        outcome_grezzo,
        motivo_non_classificato: match riclassificazione {
            Some(Riclassificazione::NonClassificato(m)) => Some(m),
            _ => None,
        },
    };
    Ok((run, esito))
}

/// Confeziona un esito ripreso dalla memoria: nessuna esecuzione, origine
/// DICHIARATA (job e eta') perche' a valle "verificato ora" e "verificato prima
/// su questo stesso codice" restino distinguibili.
fn esito_riusato(hit: EsitoMemorizzato, state_key: &str, suite_key: &str) -> SuiteVerification {
    tracing::info!(
        target: "mcp_core::suite_verification",
        suite_key = %suite_key,
        outcome = hit.outcome.as_str(),
        eta_s = hit.eta.as_secs(),
        "verifica suite: esito riusato dalla memoria, nessuna riesecuzione"
    );
    let mut verifica = SuiteVerification {
        outcome: hit.outcome,
        origine: OrigineEsito::Memoria {
            job_id: hit.job_id,
            eta_secondi: hit.eta.as_secs(),
        },
        stats: hit.stats,
        test_instabili: hit.test_instabili,
        motivo_non_classificato: None,
        exit_code: None,
        testo: hit.messaggio,
        job_id: Some(hit.job_id),
        state_key: Some(state_key.to_string()),
        suite_key: suite_key.to_string(),
    };
    verifica.testo = format!("{}\n{}", verifica.dichiarazione(), verifica.testo);
    verifica
}

/// La chiave e' rimasta la stessa PER TUTTA la misura? Se qualcuno ha scritto
/// mentre la suite girava — un correttore in parallelo, un watcher che rigenera
/// — l'esito non appartiene ne' allo stato di prima ne' a quello di dopo: si
/// consegna al chiamante (e' pur sempre cio' che e' successo) ma non entra in
/// memoria, dove varrebbe come risposta per uno stato su cui non e' stato
/// misurato.
fn chiave_memorizzabile(
    iniziale: &Option<String>,
    finale: &Option<String>,
    suite_key: &str,
) -> Option<String> {
    match (iniziale, finale) {
        (Some(prima), Some(dopo)) if prima == dopo => Some(prima.clone()),
        (Some(_), _) => {
            tracing::info!(
                target: "mcp_core::suite_verification",
                suite_key = %suite_key,
                "verifica suite: lo stato e' cambiato durante l'esecuzione, l'esito non entra in memoria"
            );
            None
        }
        _ => None,
    }
}

/// UNA riesecuzione mirata ai soli test falliti, per classificare.
async fn riclassifica(
    executor: &dyn SuiteExecutor,
    chiave: &dyn ChiaveDiStato,
    policy: SuitePolicy,
    inv: &SuiteInvocation,
    chiave_iniziale: Option<&str>,
) -> Riclassificazione {
    if !policy.riclassificazione_abilitata {
        return Riclassificazione::NonClassificato(MotivoNonClassificato::Disabilitata);
    }
    let Some(chiave_prima) = chiave_iniziale else {
        return Riclassificazione::NonClassificato(MotivoNonClassificato::StatoIgnoto);
    };
    let Some(comando) = comando_riesecuzione_mirata(&inv.command) else {
        return Riclassificazione::NonClassificato(
            MotivoNonClassificato::ComandoNonComponibile,
        );
    };

    let mirata = SuiteInvocation {
        command: comando,
        working_dir: inv.working_dir.clone(),
        timeout_s: inv.timeout_s,
        scopo: ScopoEsecuzione::RiesecuzioneMirata,
    };
    let rerun = match executor.esegui(&mirata).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "mcp_core::suite_verification",
                error = %e,
                "riesecuzione mirata non partita: l'esito resta fallito"
            );
            return Riclassificazione::NonClassificato(
                MotivoNonClassificato::RiesecuzioneNonEseguita,
            );
        }
    };

    let chiave_dopo = chiave.chiave().await.valore();
    classifica_riesecuzione(&rerun, chiave_prima, chiave_dopo.as_deref())
}

/// Verdetto PURO sulla riesecuzione mirata (regola O: il test lo esercita da
/// qui, con gli stessi input del chiamante).
///
/// Verde vale SOLO se ha eseguito qualcosa: `--last-failed` senza l'elenco dei
/// falliti esce pulito avendo eseguito zero test, e prendere quello zero per
/// una conferma trasformerebbe questo presidio nel suo contrario — un modo per
/// dichiarare instabile qualunque fallimento.
pub fn classifica_riesecuzione(
    rerun: &SuiteRun,
    chiave_prima: &str,
    chiave_dopo: Option<&str>,
) -> Riclassificazione {
    match chiave_dopo {
        None => return Riclassificazione::NonClassificato(MotivoNonClassificato::StatoIgnoto),
        Some(dopo) if dopo != chiave_prima => {
            return Riclassificazione::NonClassificato(MotivoNonClassificato::StatoCambiato)
        }
        Some(_) => {}
    }
    if rerun.stats.eseguiti() == 0 {
        return Riclassificazione::NonClassificato(
            MotivoNonClassificato::NessunTestRieseguito,
        );
    }
    if rerun.exit_code == Some(0) {
        Riclassificazione::Flaky
    } else {
        Riclassificazione::ConfermatoFallito
    }
}

/// Classifica l'esito grezzo da exit code + test eseguiti (segnali strutturati,
/// regola M). Era in `agent_tools::testing`; vive qui perche' e' la stessa
/// domanda che si pongono i tre chiamanti.
pub fn classifica_esito(exit_code: Option<i32>, eseguiti: usize) -> SuiteOutcome {
    if exit_code == Some(0) {
        return SuiteOutcome::Passed;
    }
    if eseguiti == 0 {
        SuiteOutcome::SetupFailed
    } else {
        SuiteOutcome::TestsFailed
    }
}

/// Operatori di shell che rendono il comando una COMPOSIZIONE: appendere un
/// flag finirebbe sull'ultimo pezzo, non sulla suite.
const OPERATORI_SHELL: &[&str] = &["|", "&&", "||", ";", ">", "<", "$(", "`", "\n"];

/// Redirezione che NON compone: unisce stderr a stdout e non cambia cosa viene
/// eseguito. E' in coda a moltissimi comandi di verifica generati dal progetto
/// (`... 2>&1`), e trattarla come una composizione avrebbe reso non
/// classificabile proprio il caso piu' comune del gate.
const REDIREZIONE_STDERR: &str = "2>&1";

/// Comando della riesecuzione mirata, o `None` se non componibile.
///
/// `--last-failed` e' il segnale STRUTTURATO di Playwright (legge
/// `test-results/.last-run.json`, scritto dal runner stesso): i test da
/// rieseguire li sceglie lui. L'alternativa — rieseguire per NOME, coi titoli
/// estratti dall'output — sarebbe ricostruire da una prosa cio' che esiste gia'
/// in forma macchina (regola M).
pub fn comando_riesecuzione_mirata(command: &str) -> Option<String> {
    let c = command.trim();
    if c.is_empty() || c.contains("--last-failed") {
        return None;
    }
    // La sola redirezione di stderr si stacca e si rimette in coda: il flag va
    // al comando, non dopo la redirezione (dove la shell lo leggerebbe come
    // nome di file).
    let (corpo, coda) = match c.strip_suffix(REDIREZIONE_STDERR) {
        Some(corpo) => (corpo.trim_end(), format!(" {REDIREZIONE_STDERR}")),
        None => (c, String::new()),
    };
    if corpo.is_empty() || OPERATORI_SHELL.iter().any(|op| corpo.contains(op)) {
        return None;
    }
    Some(format!("{corpo} --last-failed{coda}"))
}

/// Lanciatori che precedono `playwright` e non dicono nulla su QUALI test
/// vengono eseguiti: il tool dell'agente usa `npx`, un profilo di verifica
/// inferito dall'LLM scrive spesso `pnpm exec`.
/// Nome del runner nel comando: e' cio' che separa il prefisso di lancio dai
/// selettori, e cio' che dice se un comando esegue una suite.
const RUNNER: &str = "playwright";

const LANCIATORI: &[&str] = &[
    "npx", "pnpm", "npm", "yarn", "bun", "bunx", "exec", "run", "dlx", "-s", "--silent",
];

/// Opzioni che governano COME si esegue, non QUALI test: scartate dall'identita'
/// della suite. `true` = l'opzione porta un valore separato da scartare a sua
/// volta.
const OPZIONI_DI_ESECUZIONE: &[(&str, bool)] = &[
    ("--timeout", true),
    ("--workers", true),
    ("-j", true),
    ("--retries", true),
    ("--max-failures", true),
    ("-x", false),
    ("--reporter", true),
    ("--output", true),
    ("--quiet", false),
    ("--last-failed", false),
    ("--forbid-only", false),
];

/// Identita' della suite: QUALI test vengono eseguiti e DOVE — non con quale
/// riga di comando.
///
/// E' la condizione perche' i tre attori si riconoscano, ed e' il motivo per cui
/// la chiave non e' il comando grezzo: l'agente lancia `npx playwright test
/// --timeout 10000 --workers 1 --reporter list`, il final_gate esegue lo step
/// del profilo di verifica (spesso `pnpm exec playwright test`). Sono la STESSA
/// suite sullo stesso codice, e con la chiave presa alla lettera non si sarebbero
/// riconosciuti mai — il presidio sarebbe stato scritto, testato e inerte
/// (regola O).
///
/// Restano nell'identita' i SELETTORI: filtro posizionale, `--project`,
/// `--grep`, `--config`. Due invocazioni che eseguono insiemi di test diversi
/// non devono rispondersi a vicenda.
///
/// Cio' che si SCARTA e' dichiarato: timeout per-test, parallelismo, reporter.
/// Il timeout puo' cambiare l'esito di un test lento, quindi un chiamante piu'
/// severo puo' riusare l'esito di uno piu' permissivo. E' la conseguenza voluta
/// del riconoscimento reciproco, e non e' nascosta: l'esito riusato viaggia
/// sempre con la sua origine (job e eta', vedi [`OrigineEsito`]).
pub fn suite_key(command: &str, working_dir: Option<&str>) -> String {
    let mut selettori: Vec<String> = Vec::new();
    let mut token = command.split_whitespace().peekable();
    let mut vista_playwright = false;

    while let Some(t) = token.next() {
        if !vista_playwright {
            if let Some(s) = token_del_prefisso(t, &mut vista_playwright) {
                selettori.push(s);
            }
            continue;
        }
        match opzione_di_esecuzione(t) {
            // `--opt=valore` porta il valore con se'; `--opt valore` no.
            Some(true) if !t.contains('=') => {
                let _ = token.next_if(|v| !v.starts_with('-'));
            }
            Some(_) => {}
            None => selettori.push(t.to_string()),
        }
    }

    let dir = working_dir.unwrap_or("").replace('\\', "/");
    let dir = dir.trim_matches('/').trim_start_matches("./");
    format!("{}|{}", dir, selettori.join(" "))
}

/// Tratta un token PRIMA che `playwright` sia comparso. Ritorna cio' che entra
/// nell'identita': niente per i lanciatori e le loro opzioni, `playwright`
/// normalizzato quando lo si incontra, il token stesso se e' un runner diverso
/// (uno script del progetto non e' la suite lanciata da npx).
fn token_del_prefisso(t: &str, vista_playwright: &mut bool) -> Option<String> {
    if t.contains(RUNNER) {
        *vista_playwright = true;
        return Some(RUNNER.to_string());
    }
    if LANCIATORI.contains(&t) || t.starts_with('-') {
        return None;
    }
    Some(t.to_string())
}

/// `Some(true)` se il token e' un'opzione di sola esecuzione con valore
/// separato, `Some(false)` se senza valore, `None` se e' un selettore.
fn opzione_di_esecuzione(t: &str) -> Option<bool> {
    OPZIONI_DI_ESECUZIONE
        .iter()
        .find(|(opt, _)| t == *opt || t.starts_with(&format!("{opt}=")))
        .map(|(_, con_valore)| *con_valore)
}

/// `true` se il comando lancia una suite Playwright. PUNTO UNICO del
/// riconoscimento (regola L): prima la stessa domanda aveva due risposte
/// diverse — `command.contains("playwright")` in `agent_tools::command`
/// (registrava come run di test anche un `npx playwright install`) e la lettura
/// del solo `playwright` in `agent_tools::privileged`.
///
/// Il criterio e' il comando, cioe' un DATO di configurazione strutturato, non
/// la prosa di un messaggio: `playwright` piu' il sottocomando `test`, con
/// l'esclusione esplicita dei sottocomandi che non eseguono test.
pub fn e_suite_playwright(command: &str) -> bool {
    let c = command.to_ascii_lowercase();
    if !c.contains(RUNNER) {
        return false;
    }
    // Sottocomandi che NON eseguono la suite (installazione browser/dipendenze,
    // codegen, apertura report): un `playwright install` registrato come run di
    // test produceva un esito di suite che nessuna suite aveva prodotto.
    for non_test in [
        " install",
        " codegen",
        " show-report",
        " open ",
        " merge-reports",
    ] {
        if c.contains(non_test) {
            return false;
        }
    }
    let token_test = c
        .split_whitespace()
        .any(|t| t == "test" || t.ends_with("/test"));
    token_test
}

/// Fabbrica dell'esecutore reale + memoria, condivisa dai chiamanti.
#[derive(Clone)]
pub struct SuiteDeps {
    pub executor: Arc<dyn SuiteExecutor>,
    pub memo: Option<Arc<dyn SuiteMemo>>,
    pub chiave: Arc<dyn ChiaveDiStato>,
    pub policy: SuitePolicy,
    /// `None` dove non c'e' un pannello ad ascoltare (il gate).
    pub annuncio: Option<Arc<dyn EsitoAnnunciato>>,
}

impl SuiteDeps {
    /// Verifica una suite con queste dipendenze. E' la porta d'ingresso dei
    /// chiamanti: nessuno di loro compone `verifica_suite` da se'.
    pub async fn verifica(&self, inv: &SuiteInvocation) -> Result<SuiteVerification, String> {
        verifica_suite(
            self.executor.as_ref(),
            self.memo.as_deref(),
            self.chiave.as_ref(),
            self.policy,
            inv,
            self.annuncio.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::state_key::StateKey;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Esecutore programmabile che CONTA le esecuzioni: e' la misura con cui i
    /// test dicono "non e' stata rieseguita", invece di dedurlo da un esito che
    /// sarebbe identico in entrambi i casi.
    struct EsecutoreFinto {
        esiti: Mutex<Vec<SuiteRun>>,
        esecuzioni: AtomicUsize,
        comandi: Mutex<Vec<String>>,
    }

    impl EsecutoreFinto {
        fn con(esiti: Vec<SuiteRun>) -> Self {
            Self {
                esiti: Mutex::new(esiti),
                esecuzioni: AtomicUsize::new(0),
                comandi: Mutex::new(Vec::new()),
            }
        }
        fn esecuzioni(&self) -> usize {
            self.esecuzioni.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl SuiteExecutor for EsecutoreFinto {
        async fn esegui(&self, inv: &SuiteInvocation) -> Result<SuiteRun, String> {
            self.esecuzioni.fetch_add(1, Ordering::SeqCst);
            self.comandi.lock().unwrap().push(inv.command.clone());
            let mut esiti = self.esiti.lock().unwrap();
            if esiti.is_empty() {
                return Err("esecuzione non prevista dal test".to_string());
            }
            Ok(esiti.remove(0))
        }
    }

    struct MemoriaInMemoria {
        righe: Mutex<Vec<(String, String, SuiteOutcome, Uuid, Vec<String>)>>,
    }

    impl MemoriaInMemoria {
        fn nuova() -> Self {
            Self {
                righe: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl SuiteMemo for MemoriaInMemoria {
        async fn cerca(
            &self,
            suite_key: &str,
            state_key: &str,
            _ttl: Duration,
        ) -> Option<EsitoMemorizzato> {
            self.righe
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(sk, stk, ..)| sk == suite_key && stk == state_key)
                .map(|(_, _, outcome, job, instabili)| EsitoMemorizzato {
                    job_id: *job,
                    outcome: *outcome,
                    eta: Duration::from_secs(12),
                    messaggio: "esito memorizzato".to_string(),
                    test_instabili: instabili.clone(),
                    stats: SuiteStats::default(),
                })
        }

        async fn registra_esito(
            &self,
            job_id: Uuid,
            outcome: SuiteOutcome,
            test_instabili: &[String],
            chiavi: Option<(&str, &str)>,
        ) {
            // Senza chiave l'esito e' scritto ma non riusabile: qui, dove la
            // riga non esiste, si traduce nel non registrarlo affatto.
            let Some((suite_key, state_key)) = chiavi else {
                return;
            };
            self.righe.lock().unwrap().push((
                suite_key.to_string(),
                state_key.to_string(),
                outcome,
                job_id,
                test_instabili.to_vec(),
            ));
        }
    }

    /// Chiave programmabile: emette in sequenza i valori dati, ripetendo
    /// l'ultimo. Serve a fabbricare il caso "lo stato e' cambiato durante".
    struct ChiaveFinta {
        valori: Mutex<Vec<Option<String>>>,
    }

    impl ChiaveFinta {
        fn fissa(v: &str) -> Self {
            Self {
                valori: Mutex::new(vec![Some(v.to_string())]),
            }
        }
        fn sequenza(v: Vec<Option<&str>>) -> Self {
            Self {
                valori: Mutex::new(v.into_iter().map(|s| s.map(str::to_string)).collect()),
            }
        }
    }

    #[async_trait]
    impl ChiaveDiStato for ChiaveFinta {
        async fn chiave(&self) -> StateKey {
            let mut valori = self.valori.lock().unwrap();
            let v = if valori.len() > 1 {
                valori.remove(0)
            } else {
                valori.first().cloned().flatten()
            };
            match v {
                Some(s) => StateKey::Calcolata(s),
                None => StateKey::NonCalcolabile("test"),
            }
        }
    }

    fn run(exit: i32, passed: usize, failed: usize, falliti: &[&str]) -> SuiteRun {
        SuiteRun {
            exit_code: Some(exit),
            stats: SuiteStats {
                passed,
                failed,
                skipped: 0,
                flaky_reported: 0,
                failed_tests: falliti.iter().map(|s| s.to_string()).collect(),
            },
            testo: "output della suite".to_string(),
            job_id: Some(Uuid::new_v4()),
        }
    }

    fn invocazione() -> SuiteInvocation {
        SuiteInvocation::suite("npx playwright test", Some("app".to_string()), 600)
    }

    /// La memoizzazione MORDE: due verifiche a chiave identica producono UNA
    /// sola esecuzione. La misura e' il contatore dell'esecutore, non l'esito
    /// (identico nei due casi, quindi cieco al difetto). Mutazione: togliendo
    /// il ramo memoria in `verifica_suite`, `esecuzioni()` diventa 2.
    #[tokio::test]
    async fn esito_memorizzato_non_riesegue_a_chiave_identica() {
        // DUE esiti a disposizione, di proposito: se la memoizzazione si
        // rompe, la seconda verifica trova un esito pronto e il test fallisce
        // sul CONTATORE (2 invece di 1), che e' la misura del difetto — non su
        // un esecutore rimasto a secco, che sarebbe un incidente della fixture.
        let executor = EsecutoreFinto::con(vec![run(0, 5, 0, &[]), run(0, 5, 0, &[])]);
        let memo = MemoriaInMemoria::nuova();
        let chiave = ChiaveFinta::fissa("stato-A");
        let policy = SuitePolicy::default();
        let inv = invocazione();

        let prima = verifica_suite(&executor, Some(&memo), &chiave, policy, &inv, None)
            .await
            .expect("prima verifica");
        assert_eq!(prima.outcome, SuiteOutcome::Passed);
        assert_eq!(prima.origine, OrigineEsito::Eseguita);

        let seconda = verifica_suite(&executor, Some(&memo), &chiave, policy, &inv, None)
            .await
            .expect("seconda verifica");
        assert_eq!(seconda.outcome, SuiteOutcome::Passed);
        assert!(matches!(seconda.origine, OrigineEsito::Memoria { .. }));

        assert_eq!(
            executor.esecuzioni(),
            1,
            "la suite deve essere stata eseguita UNA volta sola: la seconda verifica legge la memoria"
        );
    }

    /// Una scrittura cambia la chiave: la memoria non risponde piu' e la suite
    /// riparte. E' l'altra meta' del presidio — senza, un esito verde
    /// sopravviverebbe alle modifiche che doveva verificare.
    #[tokio::test]
    async fn chiave_diversa_riesegue() {
        let executor = EsecutoreFinto::con(vec![run(0, 5, 0, &[]), run(0, 6, 0, &[])]);
        let memo = MemoriaInMemoria::nuova();
        let policy = SuitePolicy::default();
        let inv = invocazione();

        let chiave_a = ChiaveFinta::fissa("stato-A");
        verifica_suite(&executor, Some(&memo), &chiave_a, policy, &inv, None)
            .await
            .expect("prima");
        let chiave_b = ChiaveFinta::fissa("stato-B");
        let seconda = verifica_suite(&executor, Some(&memo), &chiave_b, policy, &inv, None)
            .await
            .expect("seconda");

        assert_eq!(seconda.origine, OrigineEsito::Eseguita);
        assert_eq!(executor.esecuzioni(), 2);
    }

    /// Suite diverse non si rispondono a vicenda: stessa chiave di stato, ma
    /// comando diverso -> nessun riuso.
    #[tokio::test]
    async fn suite_diversa_non_riusa_l_esito() {
        let executor = EsecutoreFinto::con(vec![run(0, 5, 0, &[]), run(0, 2, 0, &[])]);
        let memo = MemoriaInMemoria::nuova();
        let chiave = ChiaveFinta::fissa("stato-A");
        let policy = SuitePolicy::default();

        verifica_suite(
            &executor,
            Some(&memo),
            &chiave,
            policy,
            &SuiteInvocation::suite("npx playwright test", None, 600),
            None,
        )
        .await
        .expect("suite intera");
        let altra = verifica_suite(
            &executor,
            Some(&memo),
            &chiave,
            policy,
            &SuiteInvocation::suite("npx playwright test auth", None, 600),
            None,
        )
        .await
        .expect("suite filtrata");

        assert_eq!(altra.origine, OrigineEsito::Eseguita);
        assert_eq!(executor.esecuzioni(), 2);
    }

    /// Il caso misurato: rosso alla prima, verde alla riesecuzione mirata a
    /// stato invariato -> `flaky`, che NON blocca la chiusura. Mutazione:
    /// facendo ritornare `ConfermatoFallito` alla classificazione, l'esito
    /// torna `tests_failed` e `blocca_la_chiusura()` torna vero.
    #[tokio::test]
    async fn fallito_che_ripassa_alla_riesecuzione_e_flaky() {
        let executor = EsecutoreFinto::con(vec![
            run(1, 17, 2, &["e2e/home.spec.ts:5:3", "e2e/list.spec.ts:9:1"]),
            run(0, 2, 0, &[]),
        ]);
        let memo = MemoriaInMemoria::nuova();
        let chiave = ChiaveFinta::fissa("stato-A");

        let v = verifica_suite(
            &executor,
            Some(&memo),
            &chiave,
            SuitePolicy::default(),
            &invocazione(),
            None,
        )
        .await
        .expect("verifica");

        assert_eq!(v.outcome, SuiteOutcome::Flaky);
        assert!(!v.outcome.blocca_la_chiusura());
        assert_eq!(v.test_instabili.len(), 2, "i test instabili restano scritti");
        assert_eq!(v.outcome.job_status(), "flaky");
        assert_eq!(executor.esecuzioni(), 2, "UNA riesecuzione, non un ritenta-finche'-verde");
        assert!(v.testo.contains("NON un difetto dell'applicazione"));
    }

    /// A memoria DISATTIVATA la suite si riesegue (nessun riuso) ma l'esito
    /// classificato si scrive lo stesso: se la registrazione dipendesse dal
    /// flag, un'installazione con la memoria spenta mostrerebbe come
    /// fallimenti dell'app dei rossi gia' riconosciuti instabili.
    #[tokio::test]
    async fn memoria_disattivata_riesegue_ma_registra_l_esito() {
        let executor = EsecutoreFinto::con(vec![
            run(1, 3, 1, &["e2e/home.spec.ts:5:3"]),
            run(0, 1, 0, &[]),
            run(1, 3, 1, &["e2e/home.spec.ts:5:3"]),
            run(0, 1, 0, &[]),
        ]);
        let memo = MemoriaInMemoria::nuova();
        let chiave = ChiaveFinta::fissa("stato-A");
        let policy = SuitePolicy {
            memo_abilitata: false,
            ..SuitePolicy::default()
        };

        let prima = verifica_suite(&executor, Some(&memo), &chiave, policy, &invocazione(), None)
            .await
            .expect("prima");
        assert_eq!(prima.outcome, SuiteOutcome::Flaky);
        assert_eq!(
            memo.righe.lock().unwrap().len(),
            1,
            "l'esito e' registrato anche a memoria disattivata"
        );

        let seconda = verifica_suite(&executor, Some(&memo), &chiave, policy, &invocazione(), None)
            .await
            .expect("seconda");
        assert_eq!(
            seconda.origine,
            OrigineEsito::Eseguita,
            "a memoria disattivata non si riusa nulla"
        );
        assert_eq!(executor.esecuzioni(), 4);
    }

    /// Un fallimento riprodotto resta fallito: il presidio non e' un modo per
    /// far passare i rossi.
    #[tokio::test]
    async fn fallito_riprodotto_resta_fallito() {
        let executor = EsecutoreFinto::con(vec![
            run(1, 17, 2, &["e2e/home.spec.ts:5:3"]),
            run(1, 0, 2, &["e2e/home.spec.ts:5:3"]),
        ]);
        let chiave = ChiaveFinta::fissa("stato-A");

        let v = verifica_suite(
            &executor,
            None,
            &chiave,
            SuitePolicy::default(),
            &invocazione(),
            None,
        )
        .await
        .expect("verifica");

        assert_eq!(v.outcome, SuiteOutcome::TestsFailed);
        assert!(v.outcome.blocca_la_chiusura());
        assert!(v.test_instabili.is_empty());
    }

    /// Setup fallito (zero test eseguiti) non passa dalla riclassificazione:
    /// non c'e' nessun test da rieseguire, e chiamare flaky un runner che non
    /// e' partito nasconderebbe un'app che non si avvia.
    #[tokio::test]
    async fn setup_fallito_non_viene_riclassificato() {
        let executor = EsecutoreFinto::con(vec![run(1, 0, 0, &[])]);
        let chiave = ChiaveFinta::fissa("stato-A");

        let v = verifica_suite(
            &executor,
            None,
            &chiave,
            SuitePolicy::default(),
            &invocazione(),
            None,
        )
        .await
        .expect("verifica");

        assert_eq!(v.outcome, SuiteOutcome::SetupFailed);
        assert_eq!(executor.esecuzioni(), 1, "nessuna riesecuzione mirata");
    }

    /// Se il codice cambia MENTRE la suite gira, le due misure non parlano
    /// dello stesso oggetto: l'esito resta fallito, col motivo dichiarato, e
    /// non entra in memoria (non appartiene a nessuno dei due stati).
    #[tokio::test]
    async fn stato_cambiato_durante_non_classifica() {
        let executor = EsecutoreFinto::con(vec![
            run(1, 3, 1, &["e2e/home.spec.ts:5:3"]),
            run(0, 1, 0, &[]),
        ]);
        let memo = MemoriaInMemoria::nuova();
        let chiave = ChiaveFinta::sequenza(vec![Some("stato-A"), Some("stato-B")]);

        let v = verifica_suite(
            &executor,
            Some(&memo),
            &chiave,
            SuitePolicy::default(),
            &invocazione(),
            None,
        )
        .await
        .expect("verifica");

        assert_eq!(v.outcome, SuiteOutcome::TestsFailed);
        assert_eq!(
            v.motivo_non_classificato,
            Some(MotivoNonClassificato::StatoCambiato)
        );
        assert_eq!(v.state_key, None, "esito non riferibile a uno stato");
        assert!(
            memo.righe.lock().unwrap().is_empty(),
            "un esito misurato su uno stato in movimento non entra in memoria"
        );
    }

    /// Una scrittura arrivata DOPO l'esecuzione (suite verde, poi il codice
    /// cambia) non produce un esito memorizzabile: memorizzarlo sotto la chiave
    /// di prima lo renderebbe riusabile per uno stato su cui non e' mai stato
    /// misurato per intero.
    #[tokio::test]
    async fn scrittura_durante_una_suite_verde_non_memorizza() {
        let executor = EsecutoreFinto::con(vec![run(0, 5, 0, &[])]);
        let memo = MemoriaInMemoria::nuova();
        let chiave = ChiaveFinta::sequenza(vec![Some("stato-A"), Some("stato-B")]);

        let v = verifica_suite(
            &executor,
            Some(&memo),
            &chiave,
            SuitePolicy::default(),
            &invocazione(),
            None,
        )
        .await
        .expect("verifica");

        assert_eq!(v.outcome, SuiteOutcome::Passed);
        assert_eq!(v.state_key, None);
        assert!(memo.righe.lock().unwrap().is_empty());
    }

    /// `--last-failed` che non esegue nulla NON e' una conferma: senza test
    /// eseguiti la riesecuzione non ha risposto, e l'esito resta fallito.
    #[test]
    fn riesecuzione_a_zero_test_non_classifica() {
        let rerun = SuiteRun {
            exit_code: Some(0),
            stats: SuiteStats::default(),
            testo: String::new(),
            job_id: None,
        };
        assert_eq!(
            classifica_riesecuzione(&rerun, "stato-A", Some("stato-A")),
            Riclassificazione::NonClassificato(MotivoNonClassificato::NessunTestRieseguito)
        );
    }

    #[test]
    fn comando_mirato_solo_se_componibile() {
        assert_eq!(
            comando_riesecuzione_mirata("npx playwright test"),
            Some("npx playwright test --last-failed".to_string())
        );
        // La redirezione di stderr non compone: il flag precede la coda,
        // altrimenti la shell lo leggerebbe come nome di file.
        assert_eq!(
            comando_riesecuzione_mirata("npx playwright test 2>&1"),
            Some("npx playwright test --last-failed 2>&1".to_string())
        );
        assert_eq!(
            comando_riesecuzione_mirata("npx playwright test | tee out.txt"),
            None
        );
        assert_eq!(
            comando_riesecuzione_mirata("cd app && npx playwright test"),
            None
        );
        assert_eq!(
            comando_riesecuzione_mirata("npx playwright test --last-failed"),
            None
        );
    }

    #[test]
    fn riconosce_solo_le_suite() {
        assert!(e_suite_playwright("npx playwright test"));
        assert!(e_suite_playwright("pnpm exec playwright test --workers 1"));
        assert!(!e_suite_playwright("npx playwright install chromium"));
        assert!(!e_suite_playwright("npx playwright show-report"));
        assert!(!e_suite_playwright("npm run build"));
    }

    /// IL test del riconoscimento reciproco: il comando dell'agente e quello
    /// del profilo di verifica del gate sono la stessa suite. Senza questa
    /// normalizzazione i due non si riconoscerebbero MAI, e il presidio
    /// sarebbe inerte pur essendo scritto e verde (regola O). Mutazione:
    /// togliendo `--timeout`/`--workers`/`--reporter` da OPZIONI_DI_ESECUZIONE
    /// (o i lanciatori dal prefisso) questo test diventa rosso.
    #[test]
    fn agente_e_gate_riconoscono_la_stessa_suite() {
        let agente = suite_key(
            "npx playwright test --timeout 10000 --workers 1 --reporter list",
            Some("app"),
        );
        let gate = suite_key("pnpm exec playwright test", Some("app/"));
        assert_eq!(agente, gate);
    }

    #[test]
    fn suite_key_normalizza_spazi_e_directory() {
        assert_eq!(
            suite_key("npx   playwright  test", Some("app/")),
            suite_key("npx playwright test", Some("app"))
        );
        assert_ne!(
            suite_key("npx playwright test", Some("app")),
            suite_key("npx playwright test", Some("altra"))
        );
    }

    /// I SELETTORI restano nell'identita': una suite filtrata non e' la suite
    /// intera, e riusarne l'esito direbbe "verificato" su test mai eseguiti.
    #[test]
    fn i_selettori_distinguono_le_suite() {
        let intera = suite_key("npx playwright test", None);
        assert_ne!(intera, suite_key("npx playwright test auth", None));
        assert_ne!(
            intera,
            suite_key("npx playwright test --project firefox", None)
        );
        assert_ne!(
            intera,
            suite_key("npx playwright test --grep @smoke", None)
        );
        assert_ne!(
            intera,
            suite_key("npx playwright test --config app/pw.config.ts", None)
        );
    }

    /// Il valore separato di un'opzione di esecuzione non deve restare nella
    /// chiave facendo da finto selettore (`--workers 4` -> il `4`).
    #[test]
    fn il_valore_di_una_opzione_scartata_non_diventa_selettore() {
        assert_eq!(
            suite_key("npx playwright test --workers 4", None),
            suite_key("npx playwright test --workers=8", None)
        );
        assert_eq!(
            suite_key("npx playwright test --workers 4", None),
            suite_key("npx playwright test", None)
        );
    }

    #[test]
    fn vocabolario_esito_canonico() {
        for o in [
            SuiteOutcome::Passed,
            SuiteOutcome::Flaky,
            SuiteOutcome::TestsFailed,
            SuiteOutcome::SetupFailed,
        ] {
            assert_eq!(SuiteOutcome::da_str(o.as_str()), Some(o));
        }
        assert_eq!(SuiteOutcome::da_str("verde"), None);
    }
}
